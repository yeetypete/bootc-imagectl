//! Install a bootc image onto a disk, boot the disk and run tests in it.
//!
//! `bootc-imagectl install` runs in a bcvk VM of the image, and
//! systemd-vmspawn boots the installed disk.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use rustix::process::{Pid, Signal, kill_process};
use serde_json::json;
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use xshell::{Cmd, Shell, cmd};

use crate::{bound_binary, target_dir};

/// Where the installer VM sees the work directory. bcvk mounts a bind named
/// `install` there.
const WORK: &str = "/run/virtiofs-mnt-install";

/// Where the installer VM sees the disk, which bcvk attaches as `target`.
const DISK: &str = "/dev/disk/by-id/virtio-target";

/// The size of the disk file: 10 GiB.
const DISK_SIZE: u64 = 10 * 1024 * 1024 * 1024;

/// The environment variable naming the root filesystem to install with.
pub(crate) const FILESYSTEM_ENV: &str = "BOOTC_IMAGECTL_TEST_FILESYSTEM";

/// The root filesystems CI installs with. The first is the default.
pub(crate) const FILESYSTEMS: [&str; 2] = ["ext4", "btrfs"];

/// The passphrase of the encrypted root.
const PASSPHRASE: &str = "passphrase";

/// The user systemd-homed's first boot wizard creates from a credential.
const HOME_USER: &str = "alice";

/// The password of `HOME_USER`.
const HOME_PASSWORD: &str = "password";

/// `HOME_PASSWORD` hashed with `openssl passwd -6 -salt bootc-imagectl password`.
const HOME_HASHED_PASSWORD: &str = "$6$bootc-imagectl$bT.qMyfQznhEE5jMYfRNoTWpw9ilj.EVYUTCuvm6ykWREwP3sTwVpJXgbHpSfwhFZ5E2.s3xsMzYJnpC9pVPn0";

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
pub(crate) fn run(
    sh: &Shell,
    image: &str,
    transient_etc: bool,
    binary: &Path,
    args: &[OsString],
) -> Result<()> {
    let work = target_dir(binary)?.join("install");
    let disk = install_disk(sh, image, &work, Passphrase::Fixed)?;
    // With a transient /etc, the tests run on the second boot, to check that
    // the state of the first boot survived.
    if transient_etc {
        let mut vm = Vm::boot(binary, &work, &disk)?;
        vm.wait_for_ssh(sh)?;
        vm.wait_for_boot(sh);
        vm.power_off(sh)?;
        vm.check_console()?;
    }
    let mut vm = Vm::boot(binary, &work, &disk)?;
    vm.wait_for_ssh(sh)?;
    vm.run_tests(sh, binary, args)?;
    vm.power_off(sh)?;
    vm.check_console()
}

/// How `bootc-imagectl install` gets the passphrase of the disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Passphrase {
    /// `PASSPHRASE`, from a key file.
    Fixed,
    /// The installer asks for it on this terminal.
    Prompt,
}

/// Install the image onto a fresh disk in `work` and return the disk.
pub(crate) fn install_disk(
    sh: &Shell,
    image: &str,
    work: &Path,
    passphrase: Passphrase,
) -> Result<PathBuf> {
    remove_instances(sh)?;
    if work.exists() {
        fs::remove_dir_all(work).with_context(|| format!("removing {}", work.display()))?;
    }
    fs::create_dir_all(work)?;
    let layout = work.join("image");
    cmd!(
        sh,
        "podman save --quiet --format oci-dir --output {layout} {image}"
    )
    .run()?;
    if passphrase == Passphrase::Fixed {
        write_private(&work.join("passphrase"), PASSPHRASE)?;
    }
    let disk = work.join("disk.raw");
    File::create(&disk)?.set_len(DISK_SIZE)?;
    install(sh, image, work, &disk, passphrase)?;
    Ok(disk)
}

/// Boot the installed disk in a VM with a graphical console, and wait for
/// it to power off. The boot will prompt for the disk's passphrase.
pub(crate) fn boot_gui(disk: &Path) -> Result<()> {
    // Registered before systemd-vmspawn starts, so no Ctrl-C is lost.
    let mut signals = Signals::new([SIGINT, SIGTERM])?;
    let mut vmspawn = vmspawn(disk)
        .arg("--console=gui")
        .arg("--network-user-mode")
        .stdin(Stdio::null())
        // Ctrl-C in the terminal would also reach QEMU, which would quit
        // before systemd-vmspawn shuts the VM down.
        .process_group(0)
        .spawn()
        .map_err(vmspawn_error)?;
    let pid = Pid::from_child(&vmspawn);
    let handle = signals.handle();
    // Returns whether it forwarded a signal.
    let forward = thread::spawn(move || {
        let mut interrupted = false;
        for _ in signals.forever() {
            interrupted = true;
            let _ = kill_process(pid, Signal::TERM);
        }
        interrupted
    });
    let status = vmspawn.wait().context("waiting for systemd-vmspawn")?;
    handle.close();
    let interrupted = forward.join().unwrap_or(false);
    ensure!(
        status.success() || interrupted,
        "systemd-vmspawn failed: {status}"
    );
    Ok(())
}

/// A systemd-vmspawn command booting `disk`.
fn vmspawn(disk: &Path) -> Command {
    let mut command = Command::new("systemd-vmspawn");
    command
        .arg(format!("--image={}", disk.display()))
        .arg(format!("--machine={MACHINE}"))
        // The VMs do not need TPM.
        .args(["--ram=4G", "--register=no", "--tpm=no"]);
    command
}

/// Explain a failure to start systemd-vmspawn.
fn vmspawn_error(e: io::Error) -> anyhow::Error {
    match e.kind() {
        io::ErrorKind::NotFound => anyhow::anyhow!("systemd-vmspawn is not installed"),
        _ => anyhow::Error::from(e).context("starting systemd-vmspawn"),
    }
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

/// The root filesystem to install with.
fn filesystem() -> String {
    env::var(FILESYSTEM_ENV).unwrap_or_else(|_| FILESYSTEMS[0].into())
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
fn install(
    sh: &Shell,
    image: &str,
    work: &Path,
    disk: &Path,
    passphrase: Passphrase,
) -> Result<()> {
    // Bound read-write: bcvk's read-only binds fail on kernels without
    // SELinux. TODO: report upstream and fix.
    let bind = format!("{}:install", work.display());
    let disk = format!("{}:target", disk.display());
    let filesystem = format!("--filesystem={}", filesystem());
    let key_file =
        (passphrase == Passphrase::Fixed).then(|| format!("--key-file={WORK}/passphrase"));
    let install = cmd!(
        sh,
        "bcvk ephemeral run-ssh --rm --name {INSTALLER_CONTAINER} --bind {bind} --mount-disk-file {disk}
            --karg systemd.firstboot=no {image}
            /usr/libexec/bootc-imagectl install --yes {filesystem} {key_file...}
            --source-imgref=oci:{WORK}/image --target-imgref={image} {DISK}"
    );
    match passphrase {
        Passphrase::Fixed => install.run()?,
        // xshell would connect stdin to /dev/null.
        Passphrase::Prompt => {
            let status = Command::from(install).status().context("running bcvk")?;
            ensure!(status.success(), "bcvk failed: {status}");
        }
    }
    Ok(())
}

/// The `home.create.` credential of `HOME_USER`.
fn home_credential() -> String {
    let record = json!({
        "secret": { "password": [HOME_PASSWORD] },
        "privileged": { "hashedPassword": [HOME_HASHED_PASSWORD] },
    });
    format!("home.create.{HOME_USER}:{record}")
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

        let journal = work.join("vm.journal");
        let cid = random_cid();
        let vmspawn = vmspawn(disk)
            .env("RUNTIME_DIRECTORY", &runtime_dir)
            .arg("--console=read-only")
            .arg(format!("--vsock-cid={cid}"))
            .arg(format!(
                "--set-credential=cryptsetup.passphrase:{PASSPHRASE}"
            ))
            .arg(format!("--set-credential={}", home_credential()))
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
            .map_err(vmspawn_error)?;
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
        loop {
            if self.ssh(sh, ["true"]).quiet().ignore_stderr().run().is_ok() {
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
        let test = bound_binary(binary, TARGET)?;
        let term = format!("TERM={}", env::var("TERM").unwrap_or_default());
        let filesystem = format!("{FILESYSTEM_ENV}={}", filesystem());
        self.ssh(sh, ["env", &term, &filesystem])
            .arg(test)
            .args(args)
            .run()?;
        Ok(())
    }

    /// Wait for the boot to finish.
    fn wait_for_boot(&self, sh: &Shell) {
        let _ = self
            .ssh(sh, ["systemctl", "is-system-running", "--wait"])
            .quiet()
            .ignore_stdout()
            .run();
    }

    /// Power off the installed system and wait for systemd-vmspawn to exit.
    fn power_off(&mut self, sh: &Shell) -> Result<()> {
        // ssh may lose the connection before systemctl exits.
        let _ = self
            .ssh(sh, ["systemctl", "poweroff"])
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

    /// Build the ssh command that runs `command` in the VM as root.
    fn ssh<'a>(
        &self,
        sh: &'a Shell,
        command: impl IntoIterator<Item = impl AsRef<OsStr>>,
    ) -> Cmd<'a> {
        let key = &self.key;
        let host = format!("root@vsock/{}", self.cid);
        cmd!(
            sh,
            "ssh -i {key} -o BatchMode=yes -o ConnectTimeout=5 -o LogLevel=ERROR {host}"
        )
        .args(command)
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
