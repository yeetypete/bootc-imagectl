//! Check that a transient /etc keeps the first boot's state across a reboot.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use bootc_imagectl::command::CommandRunExt;

use crate::homed::user_record;

/// The time the system booted.
fn boot_time() -> Result<SystemTime> {
    let stat = fs::read_to_string("/proc/stat")?;
    let seconds: u64 = stat
        .lines()
        .find_map(|line| line.strip_prefix("btime "))
        .context("no btime in /proc/stat")?
        .trim()
        .parse()?;
    Ok(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
}

/// Whether `path` was last written before this boot.
fn written_before_boot(path: &str) -> Result<bool> {
    let modified = fs::metadata(path)
        .with_context(|| format!("reading the metadata of {path}"))?
        .modified()?;
    Ok(modified < boot_time()?)
}

/// bootc mounted /etc as a transient overlay.
pub(crate) fn mounts_etc_transient() -> Result<()> {
    let mount = Command::new("findmnt")
        .args(["--noheadings", "--output=FSTYPE,SOURCE", "/etc"])
        .output_string()?;
    assert_eq!(
        mount.split_whitespace().collect::<Vec<_>>(),
        ["overlay", "transient"]
    );
    Ok(())
}

/// The wizard's user created on the first boot is still present.
pub(crate) fn keeps_wizard_user() -> Result<()> {
    let last_change =
        SystemTime::UNIX_EPOCH + Duration::from_micros(user_record()?.last_change_usec);
    assert!(last_change < boot_time()?);
    Ok(())
}

/// The machine ID matches the first boot's.
pub(crate) fn keeps_machine_id() -> Result<()> {
    let machine_id = fs::read_to_string("/etc/machine-id")?;
    let machine_ids: Vec<String> = user_record()?.binding.into_keys().collect();
    assert_eq!(machine_ids, [machine_id.trim()]);
    Ok(())
}

/// The SSH host keys are from the first boot.
pub(crate) fn keeps_ssh_host_keys() -> Result<()> {
    for key_type in ["ecdsa", "ed25519", "rsa"] {
        let key = format!("/var/lib/ssh/ssh_host_{key_type}_key");
        assert!(written_before_boot(&key)?, "{key}");
    }
    Ok(())
}

/// bootc's pull secret is linked into /run.
pub(crate) fn links_pull_secret() -> Result<()> {
    assert_eq!(
        fs::read_link("/run/ostree/auth.json")?,
        Path::new("/var/lib/ostree/auth.json")
    );
    Ok(())
}
