//! Install a finalized bootc image onto a disk.
//!
//! Runs from the image, as a privileged container with the host's devices:
//!
//! ```text
//! podman run --rm -it --privileged --pid=host --ipc=host \
//!     -v /dev:/dev -v /run/udev:/run/udev:ro \
//!     -v /var/lib/containers:/var/lib/containers \
//!     IMAGE /usr/libexec/bootc-imagectl install /dev/nvme0n1
//! ```

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::process::Command;

use anyhow::{Context, Result, ensure};
use cap_std_ext::camino::{Utf8Path, Utf8PathBuf};
use rustix::mount::{MountFlags, UnmountFlags};
use serde::Deserialize;
use tracing::{info, warn};

use crate::cli::{Encrypt, InstallOpts};
use crate::command::CommandRunExt;

mod repart;

/// Where systemd-boot's EFI binaries are, which bootc installs to the ESP.
const SYSTEMD_BOOT_DIR: &str = "/usr/lib/systemd/boot/efi";

/// The device-mapper name of the unlocked root partition.
const MAPPING: &str = "bootc-imagectl-root";

/// Run the `install` subcommand.
///
/// # Errors
///
/// Fails if the image lacks a tool the install needs, the user does not
/// confirm, or a step fails.
pub fn install(opts: &InstallOpts) -> Result<()> {
    ensure!(
        opts.encrypt == Encrypt::Passphrase || opts.key_file.is_none(),
        "--key-file needs --encrypt passphrase"
    );
    ensure!(
        rustix::process::geteuid().is_root(),
        "install must run as root"
    );
    let device = match &opts.device {
        Some(device) => device.clone(),
        None => prompt_disk()?,
    };
    let device = &device;
    let metadata = fs::metadata(device).with_context(|| format!("reading {device}"))?;
    ensure!(
        metadata.file_type().is_block_device(),
        "{device} is not a block device"
    );
    check_image(opts.encrypt)?;

    if !opts.yes {
        confirm(device)?;
    }

    // Kept out of /tmp, where bootc mounts a tmpfs over the target mount.
    let tmp_dir = tempfile::Builder::new()
        .prefix("bootc-imagectl-")
        .tempdir_in("/run")
        .context("creating a temporary directory in /run")?;
    let tmp = Utf8Path::from_path(tmp_dir.path()).context("temporary directory is not UTF-8")?;
    let key_file = match (opts.encrypt, &opts.key_file) {
        (Encrypt::Off, _) => None,
        (Encrypt::Passphrase, Some(path)) => Some(path.clone()),
        (Encrypt::Passphrase, None) => Some(write_key_file(tmp, &prompt_passphrase()?)?),
    };

    info!("partitioning {device}");
    // systemd-repart updates the kernel's partitions one by one and fails when
    // a new partition overlaps an old one it has not updated yet, e.g. a larger
    // ESP. Without old partitions it only adds new ones.
    remove_partitions(device)?;
    let definitions = repart::write_definitions(tmp, opts.encrypt)?;
    let partitions = repart::partition(device, &definitions, key_file.as_deref())?;
    wait_for_udev(&partitions.esp)?;
    wait_for_udev(&partitions.root)?;

    // Dropped in reverse order: unmounted, then locked.
    let mapping = key_file
        .as_deref()
        .map(|key_file| Mapping::open(&partitions.root, key_file))
        .transpose()?;
    let filesystem = mapping.as_ref().map_or(&partitions.root, |m| &m.device);
    let target = tmp.join("target");
    fs::create_dir(&target).with_context(|| format!("creating {target}"))?;
    let _mount = Mount::ext4(filesystem, &target)?;

    let source_not_in_registry = opts
        .source_imgref
        .as_deref()
        .is_some_and(|imgref| !is_registry(imgref));
    if source_not_in_registry && opts.target_imgref.is_none() {
        warn!(
            "the source is not in a registry and no --target-imgref is given so the installed system will have no update source"
        );
    }
    info!(
        "installing {}",
        opts.source_imgref.as_deref().unwrap_or("this image")
    );
    let mut bootc = Command::new("bootc");
    bootc.args([
        "install",
        "to-filesystem",
        "--composefs-backend",
        "--bootloader=systemd",
        // An empty --root-mount-spec makes bootc omit the root= karg, so that
        // systemd-gpt-auto-generator finds the root partition by its type and
        // unlocks it. Without it, bootc adds root=UUID= of the filesystem
        // inside the LUKS volume, which does not exist until the volume is
        // unlocked.
        "--root-mount-spec=",
    ]);
    if let Some(imgref) = &opts.source_imgref {
        bootc.arg(format!("--source-imgref={imgref}"));
    }
    if let Some(imgref) = &opts.target_imgref {
        bootc.arg(format!("--target-imgref={imgref}"));
    }
    bootc.arg(&target).run()?;

    info!("installed to {device}");
    if mapping.is_some() {
        info!(
            "to unlock the root with the TPM, run systemd-cryptenroll --tpm2-device=auto {}",
            partitions.root
        );
    }
    Ok(())
}

/// Fail if the image lacks tooling install needs.
fn check_image(encrypt: Encrypt) -> Result<()> {
    let mut tools = vec![
        "bootc",
        "bootctl",
        "lsblk",
        "mkfs.ext4",
        "mkfs.vfat",
        "systemd-repart",
        "udevadm",
        "wipefs",
    ];
    if encrypt == Encrypt::Passphrase {
        tools.push("cryptsetup");
    }
    let missing: Vec<_> = tools
        .into_iter()
        .filter(|tool| which::which(tool).is_err())
        .collect();
    ensure!(
        missing.is_empty(),
        "the image lacks tools install needs: {}",
        missing.join(", ")
    );
    ensure!(
        fs::exists(SYSTEMD_BOOT_DIR)?,
        "the image lacks systemd-boot's EFI binaries in {SYSTEMD_BOOT_DIR}"
    );
    Ok(())
}

/// A disk, as reported by `lsblk --json`.
#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Disk {
    path: Utf8PathBuf,
    size: String,
    model: Option<String>,
    #[serde(rename = "rm")]
    removable: bool,
    #[serde(rename = "type")]
    kind: String,
}

/// The output of `lsblk --json`.
#[derive(Debug, Deserialize)]
struct Lsblk {
    blockdevices: Vec<Disk>,
}

/// The whole disks in `lsblk --json` output.
fn disks(lsblk_json: &str) -> Result<Vec<Disk>> {
    let lsblk: Lsblk = serde_json::from_str(lsblk_json).context("parsing lsblk's output")?;
    Ok(lsblk
        .blockdevices
        .into_iter()
        .filter(|disk| disk.kind == "disk" && !disk.path.as_str().starts_with("/dev/zram"))
        .collect())
}

/// List the disks and ask which one to install to.
fn prompt_disk() -> Result<Utf8PathBuf> {
    let output = Command::new("lsblk")
        .args(["--json", "--nodeps", "--output", "PATH,SIZE,MODEL,RM,TYPE"])
        .output_string()?;
    let mut disks = disks(&output)?;
    ensure!(!disks.is_empty(), "found no disk to install to");
    println!("Disks:");
    for (number, disk) in disks.iter().enumerate() {
        let model = disk.model.as_deref().unwrap_or("");
        let removable = if disk.removable { " (removable)" } else { "" };
        println!(
            "  {}) {}  {}  {model}{removable}",
            number + 1,
            disk.path,
            disk.size
        );
    }
    print!("Disk to install to [1-{}]: ", disks.len());
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let number: usize = answer.trim().parse().context("not a number")?;
    ensure!(
        (1..=disks.len()).contains(&number),
        "no disk {number} in the list"
    );
    Ok(disks.swap_remove(number - 1).path)
}

/// Show what the install destroys and make the user type the disk's name.
fn confirm(device: &Utf8Path) -> Result<()> {
    println!("This destroys every partition on {device}:");
    Command::new("lsblk")
        .args(["--output", "NAME,SIZE,FSTYPE,LABEL"])
        .arg(device)
        .run()?;
    print!("Type {device} to confirm: ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    ensure!(answer.trim() == device, "aborted");
    Ok(())
}

/// Ask for the passphrase that unlocks the root partition, twice.
fn prompt_passphrase() -> Result<String> {
    let passphrase = rpassword::prompt_password("Passphrase for the encrypted root: ")?;
    ensure!(!passphrase.is_empty(), "the passphrase must not be empty");
    ensure!(
        passphrase == rpassword::prompt_password("Repeat the passphrase: ")?,
        "the passphrases do not match"
    );
    Ok(passphrase)
}

/// Write a passphrase into `dir` as a key file systemd-repart and
/// cryptsetup read.
fn write_key_file(dir: &Utf8Path, passphrase: &str) -> Result<Utf8PathBuf> {
    let path = dir.join("passphrase");
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut file| file.write_all(passphrase.as_bytes()))
        .with_context(|| format!("writing {path}"))?;
    Ok(path)
}

/// Remove the disk's partitions from the kernel by wiping its partition table.
fn remove_partitions(device: &Utf8Path) -> Result<()> {
    // wipefs fails if a partition is in use and rereads the partition table.
    Command::new("wipefs")
        .args(["--all", "--quiet"])
        .arg(device)
        .run()
}

/// Wait for udev to process a new partition. bootc reads partition types
/// from udev's database.
fn wait_for_udev(node: &Utf8Path) -> Result<()> {
    Command::new("udevadm")
        .args(["wait", "--timeout=10"])
        .arg(node)
        .run()
}

/// Whether `imgref` names an image in a registry, as `docker://` in
/// containers-transports(5) or bootc's `registry:`.
fn is_registry(imgref: &str) -> bool {
    imgref.starts_with("docker://") || imgref.starts_with("registry:")
}

/// The unlocked root partition, locked again when dropped.
#[derive(Debug)]
#[must_use = "closes the mapping when dropped"]
struct Mapping {
    device: Utf8PathBuf,
}

impl Mapping {
    fn open(partition: &Utf8Path, key_file: &Utf8Path) -> Result<Self> {
        info!("unlocking {partition}");
        Command::new("cryptsetup")
            .arg("open")
            .arg(format!("--key-file={key_file}"))
            .args([partition.as_str(), MAPPING])
            .run()?;
        Ok(Self {
            device: Utf8Path::new("/dev/mapper").join(MAPPING),
        })
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        if let Err(e) = Command::new("cryptsetup").args(["close", MAPPING]).run() {
            warn!("closing {MAPPING}: {e:#}");
        }
    }
}

/// A mounted filesystem, detached with everything mounted below it when
/// dropped.
#[derive(Debug)]
#[must_use = "unmounts when dropped"]
struct Mount {
    target: Utf8PathBuf,
}

impl Mount {
    /// Mount the ext4 filesystem the root definition formats.
    fn ext4(device: &Utf8Path, target: &Utf8Path) -> Result<Self> {
        rustix::mount::mount(
            device.as_str(),
            target.as_str(),
            "ext4",
            MountFlags::empty(),
            None,
        )
        .with_context(|| format!("mounting {device} at {target}"))?;
        Ok(Self {
            target: target.to_owned(),
        })
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        if let Err(e) = rustix::mount::unmount(self.target.as_str(), UnmountFlags::DETACH) {
            warn!("unmounting {}: {e}", self.target);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command};

    fn install_opts(args: &[&str]) -> InstallOpts {
        let args = ["bootc-imagectl", "install"].iter().chain(args);
        let Command::Install(opts) = Cli::parse_from(args).command else {
            unreachable!()
        };
        opts
    }

    #[test]
    fn encrypts_by_default() {
        assert_eq!(install_opts(&["/dev/vdb"]).encrypt, Encrypt::Passphrase);
    }

    #[test]
    fn writes_key_file_only_owner_can_read() {
        let dir = tempfile::tempdir().unwrap();
        let dir = Utf8Path::from_path(dir.path()).unwrap();
        let path = write_key_file(dir, "secret").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "secret");
        let mode = fs::metadata(&path).unwrap().permissions();
        assert_eq!(mode.mode() & 0o777, 0o600);
    }

    #[test]
    fn lists_only_whole_disks() {
        let output = r#"{"blockdevices": [
            {"path":"/dev/loop0", "size":"1.2G", "model":null, "rm":false, "type":"loop"},
            {"path":"/dev/nvme0n1", "size":"1.8T", "model":"Samsung SSD", "rm":false, "type":"disk"},
            {"path":"/dev/sda", "size":"14.9G", "model":"Flash", "rm":true, "type":"disk"},
            {"path":"/dev/sr0", "size":"2.6G", "model":"DVD", "rm":true, "type":"rom"},
            {"path":"/dev/zram0", "size":"4G", "model":null, "rm":false, "type":"disk"}
        ]}"#;
        let paths: Vec<_> = disks(output)
            .unwrap()
            .into_iter()
            .map(|disk| disk.path)
            .collect();
        assert_eq!(paths, ["/dev/nvme0n1", "/dev/sda"]);
    }

    #[test]
    fn detects_registry_imgrefs() {
        assert!(is_registry("docker://docker.io/example/image:latest"));
        assert!(is_registry("registry:docker.io/example/image:latest"));
        assert!(!is_registry("oci:/images/example"));
        assert!(!is_registry("containers-storage:localhost/example"));
    }
}
