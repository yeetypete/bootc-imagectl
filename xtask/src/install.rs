//! Install a bootc image onto a disk, boot the disk and run tests in it.
//!
//! `bootc-imagectl install` runs in a bcvk VM of the image, and
//! systemd-vmspawn boots the installed disk.

use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rustix::process::{Pid, Signal, kill_process};
use xshell::{Shell, cmd};

use crate::{bound_binary, target_dir};

/// Where the installer VM sees the work directory. bcvk mounts a bind named
/// `install` there.
const WORK: &str = "/run/virtiofs-mnt-install";

/// Where the installer VM sees the disk, which bcvk attaches as `target`.
const DISK: &str = "/dev/disk/by-id/virtio-target";

/// The size of the disk file: 10 GiB.
const DISK_SIZE: u64 = 10 * 1024 * 1024 * 1024;

/// The environment variable naming the root filesystem to install with.
/// The tests in the installed system check the root against it.
const FILESYSTEM_ENV: &str = "BOOTC_IMAGECTL_TEST_FILESYSTEM";

/// The passphrase of the encrypted root.
const PASSPHRASE: &str = "passphrase";

/// How long the installed system has to boot and start sshd.
const BOOT_TIMEOUT: Duration = Duration::from_secs(300);

/// How long systemd-vmspawn has to stop the VM before its scope is stopped.
const STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the installed system has to power off.
const POWEROFF_TIMEOUT: Duration = Duration::from_secs(60);

/// Where the installed system sees the target directory, which holds the
/// test binary.
const TARGET: &str = "/run/bootc-imagectl-test";

/// The name of the installer VM's container.
const INSTALLER_CONTAINER: &str = "bootc-imagectl-install";

/// Name assigned to the systemd-vmspawn VM.
const MACHINE: &str = "bootc-imagectl-test";

/// Install the image onto a fresh disk, boot it and run the test binary in
/// it over ssh.
pub(crate) fn run(sh: &Shell, image: &str, binary: &Path, args: &[OsString]) -> Result<()> {
    let work = target_dir(binary)?.join("install");
    remove_instances(sh)?;
    if work.exists() {
        fs::remove_dir_all(&work).with_context(|| format!("removing {}", work.display()))?;
    }
    fs::create_dir_all(&work)?;
    let layout = work.join("image");
    cmd!(
        sh,
        "podman save --quiet --format oci-dir --output {layout} {image}"
    )
    .run()?;
    write_private(&work.join("passphrase"), PASSPHRASE)?;
    let disk = work.join("disk.raw");
    File::create(&disk)?.set_len(DISK_SIZE)?;

    install(sh, image, &work, &disk)?;
    let mut vm = Vm::boot(binary, &work, &disk)?;
    vm.wait_for_ssh(sh)?;
    vm.run_tests(sh, binary, args)?;
    vm.power_off(sh)?;
    vm.check_console()
}

/// Remove a leftover installer container and VM.
fn remove_instances(sh: &Shell) -> Result<()> {
    cmd!(sh, "podman rm --force --ignore {INSTALLER_CONTAINER}")
        .quiet()
        .ignore_stdout()
        .run()?;
    stop_machine();
    Ok(())
}

/// Stop the VM.
fn stop_machine() {
    let _ = Command::new("systemctl")
        .args(["--user", "stop", &format!("{MACHINE}.scope")])
        .stderr(Stdio::null())
        .status();
}

/// The root filesystem to install with, ext4 unless set otherwise.
fn filesystem() -> String {
    env::var(FILESYSTEM_ENV).unwrap_or_else(|_| "ext4".into())
}

/// Write a file only its owner can read.
fn write_private(path: &Path, content: &str) -> Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| file.write_all(content.as_bytes()))
        .with_context(|| format!("writing {}", path.display()))
}

/// Run `bootc-imagectl install` in a VM of the image against `disk`.
fn install(sh: &Shell, image: &str, work: &Path, disk: &Path) -> Result<()> {
    // Bound read-write: bcvk's read-only binds fail on kernels without
    // SELinux. TODO: report upstream and fix.
    let bind = format!("{}:install", work.display());
    let disk = format!("{}:target", disk.display());
    let filesystem = format!("--filesystem={}", filesystem());
    cmd!(
        sh,
        "bcvk ephemeral run-ssh --rm --name {INSTALLER_CONTAINER} --bind {bind} --mount-disk-file {disk}
            --karg systemd.firstboot=no {image}
            /usr/libexec/bootc-imagectl install --yes {filesystem} --key-file={WORK}/passphrase
            --source-imgref=oci:{WORK}/image --target-imgref={image} {DISK}"
    )
    .run()?;
    Ok(())
}

/// A systemd-vmspawn VM booted from the installed disk.
#[derive(Debug)]
#[must_use = "stops the VM when dropped"]
struct Vm {
    vmspawn: Child,
    /// The VM's vsock CID, which ssh connects to.
    cid: u32,
    /// systemd-vmspawn's runtime directory.
    runtime_dir: PathBuf,
    /// The private SSH key systemd-vmspawn generates for the VM.
    key: PathBuf,
    /// The work directory, which holds the VM's logs.
    work: PathBuf,
}

impl Vm {
    fn boot(binary: &Path, work: &Path, disk: &Path) -> Result<Self> {
        // Short, since systemd-vmspawn creates sockets in it.
        let runtime_dir =
            Path::new(&env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?)
                .join(MACHINE);
        if runtime_dir.exists() {
            fs::remove_dir_all(&runtime_dir)
                .with_context(|| format!("removing {}", runtime_dir.display()))?;
        }
        fs::create_dir(&runtime_dir)?;
        let journal = work.join("journal");
        fs::create_dir(&journal)?;
        let cid = random_cid();
        let vmspawn = Command::new("systemd-vmspawn")
            .env("RUNTIME_DIRECTORY", &runtime_dir)
            .arg(format!("--image={}", disk.display()))
            .arg(format!("--machine={MACHINE}"))
            .args(["--ram=4G", "--register=no", "--console=read-only"])
            .arg(format!("--vsock-cid={cid}"))
            // The tests do not need TPM.
            .arg("--tpm=no")
            .arg(format!(
                "--set-credential=cryptsetup.passphrase:{PASSPHRASE}"
            ))
            .arg(format!(
                "--bind-ro={}:{TARGET}",
                target_dir(binary)?.display()
            ))
            .arg(format!("--forward-journal={}", journal.display()))
            .arg("systemd.firstboot=no")
            .arg("systemd.log_color=0")
            .stdin(Stdio::null())
            .stdout(File::create(work.join("console.log"))?)
            .stderr(File::create(work.join("vmspawn.log"))?)
            .spawn()
            .map_err(|e| match e.kind() {
                io::ErrorKind::NotFound => {
                    anyhow::anyhow!("systemd-vmspawn is not installed")
                }
                _ => anyhow::Error::from(e).context("starting systemd-vmspawn"),
            })?;
        Ok(Self {
            vmspawn,
            cid,
            key: runtime_dir.join(format!("{MACHINE}-ed25519")),
            runtime_dir,
            work: work.to_owned(),
        })
    }

    /// Wait until the VM accepts ssh logins with the key systemd-vmspawn
    /// generated.
    fn wait_for_ssh(&mut self, sh: &Shell) -> Result<()> {
        let start = Instant::now();
        let (options, host) = (&self.ssh_options(), self.ssh_host());
        loop {
            if cmd!(sh, "ssh {options...} {host} true")
                .quiet()
                .ignore_stderr()
                .run()
                .is_ok()
            {
                return Ok(());
            }
            if let Some(status) = self.vmspawn.try_wait()? {
                bail!(
                    "systemd-vmspawn exited with {status}, see the logs in {}",
                    self.work.display()
                );
            }
            if start.elapsed() > BOOT_TIMEOUT {
                bail!(
                    "the installed system did not accept ssh logins within {BOOT_TIMEOUT:?}, see the logs in {}",
                    self.work.display()
                );
            }
            thread::sleep(Duration::from_secs(1));
        }
    }

    /// Run the test binary in the VM, from the bound target directory.
    fn run_tests(&self, sh: &Shell, binary: &Path, args: &[OsString]) -> Result<()> {
        let (options, host) = (&self.ssh_options(), self.ssh_host());
        let test = bound_binary(binary, TARGET)?;
        let term = format!("TERM={}", env::var("TERM").unwrap_or_default());
        let filesystem = format!("{FILESYSTEM_ENV}={}", filesystem());
        cmd!(
            sh,
            "ssh {options...} {host} env {term} {filesystem} {test} {args...}"
        )
        .run()?;
        Ok(())
    }

    /// Power off the installed system and wait for systemd-vmspawn to exit.
    fn power_off(&mut self, sh: &Shell) -> Result<()> {
        let (options, host) = (&self.ssh_options(), self.ssh_host());
        // ssh may lose the connection before systemctl exits.
        let _ = cmd!(sh, "ssh {options...} {host} systemctl poweroff")
            .quiet()
            .ignore_stderr()
            .run();
        let start = Instant::now();
        loop {
            if let Some(status) = self.vmspawn.try_wait()? {
                if !status.success() {
                    bail!(
                        "systemd-vmspawn exited with {status}, see the logs in {}",
                        self.work.display()
                    );
                }
                return Ok(());
            }
            if start.elapsed() > POWEROFF_TIMEOUT {
                bail!(
                    "the installed system did not power off within {POWEROFF_TIMEOUT:?}, see the logs in {}",
                    self.work.display()
                );
            }
            thread::sleep(Duration::from_secs(1));
        }
    }

    /// Fail on any unit systemd reported as failed on the console, from the
    /// initrd to the end of shutdown.
    fn check_console(&self) -> Result<()> {
        let path = self.work.join("console.log");
        let console = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let console = String::from_utf8_lossy(&console);
        let bad: Vec<&str> = console
            .lines()
            .map(str::trim)
            .filter(|line| {
                JobResult::ALL
                    .iter()
                    .any(|result| line.contains(result.status()))
            })
            .collect();
        if !bad.is_empty() {
            bail!(
                "systemd reported failures on the console:\n{}",
                bad.join("\n")
            );
        }
        Ok(())
    }

    /// ssh options used to log into the VM.
    fn ssh_options(&self) -> Vec<OsString> {
        let mut options = vec!["-i".into(), self.key.clone().into_os_string()];
        for option in ["BatchMode=yes", "ConnectTimeout=5", "LogLevel=ERROR"] {
            options.extend(["-o".into(), option.into()]);
        }
        options
    }

    /// The ssh destination of the VM's root account.
    fn ssh_host(&self) -> String {
        format!("root@vsock/{}", self.cid)
    }
}

/// The job results systemd reports as failures.
#[derive(Debug, Clone, Copy)]
enum JobResult {
    Timeout,
    Failed,
    Dependency,
    Assert,
    Unsupported,
}

impl JobResult {
    const ALL: [Self; 5] = [
        Self::Timeout,
        Self::Failed,
        Self::Dependency,
        Self::Assert,
        Self::Unsupported,
    ];

    /// The status systemd prints on the console for the result.
    fn status(self) -> &'static str {
        match self {
            Self::Timeout => "[ TIME ]",
            Self::Failed => "[FAILED]",
            Self::Dependency => "[DEPEND]",
            Self::Assert => "[ASSERT]",
            Self::Unsupported => "[UNSUPP]",
        }
    }
}

/// A random vsock CID, excluding the reserved CIDs 0 to 2 and `u32::MAX`.
fn random_cid() -> u32 {
    rand::random_range(3..u32::MAX)
}

impl Drop for Vm {
    fn drop(&mut self) {
        if matches!(self.vmspawn.try_wait(), Ok(None)) {
            let _ = kill_process(Pid::from_child(&self.vmspawn), Signal::TERM);
        }
        let start = Instant::now();
        while matches!(self.vmspawn.try_wait(), Ok(None)) && start.elapsed() < STOP_TIMEOUT {
            thread::sleep(Duration::from_millis(100));
        }
        stop_machine();
        let _ = self.vmspawn.wait();
        let _ = fs::remove_dir_all(&self.runtime_dir);
    }
}
