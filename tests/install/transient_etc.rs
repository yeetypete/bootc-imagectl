//! Check that a transient /etc keeps the first boot's state across a reboot.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;
use bootc_imagectl::finalize::ssh_host_keys;
use bootc_imagectl::testing::{boot_time, written_before_boot};
use camino::{Utf8Path, Utf8PathBuf};

use crate::homed::user_record;

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
    assert!(last_change < boot_time());
    Ok(())
}

/// The machine ID matches the first boot's.
pub(crate) fn keeps_machine_id() -> Result<()> {
    let machine_id = fs::read_to_string("/etc/machine-id")?;
    let machine_ids: Vec<String> = user_record()?.binding.into_keys().collect();
    assert_eq!(machine_ids, [machine_id.trim()]);
    Ok(())
}

/// sshd uses the host keys in /var from the first boot.
pub(crate) fn keeps_ssh_host_keys() -> Result<()> {
    let keys: Vec<Utf8PathBuf> = ssh_host_keys().collect();
    let config = Command::new("sshd").arg("-T").output_string()?;
    let loaded: Vec<&Utf8Path> = config
        .lines()
        .filter_map(|line| line.strip_prefix("hostkey "))
        .map(Utf8Path::new)
        .collect();
    assert_eq!(loaded, keys);
    for key in &keys {
        assert!(written_before_boot(key.as_std_path())?, "{key}");
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
