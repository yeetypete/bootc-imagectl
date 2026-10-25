//! Check that a transient /etc keeps the first boot's state across a reboot.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;
use bootc_imagectl::finalize::{DBUS_MACHINE_ID, PULL_SECRET, PULL_SECRET_LINK, SSH_HOST_KEY_DIR};
use bootc_imagectl::testing::{boot_time, written_before_boot};
use camino::Utf8Path;

use crate::homed::user_record;

/// bootc mounted /etc as a transient overlay.
pub(crate) fn mounts_etc_transient() -> Result<()> {
    let mount = Command::new("findmnt")
        .args(["--noheadings", "--output=FSTYPE,SOURCE", "/etc"])
        .run_get_string()?;
    assert_eq!(
        mount.split_whitespace().collect::<Vec<_>>(),
        ["overlay", "transient"]
    );
    Ok(())
}

/// The wizard's user created on the first boot is still present.
pub(crate) fn keeps_wizard_user() -> Result<()> {
    assert!(user_record()?.last_change < boot_time());
    Ok(())
}

/// The machine ID matches the first boot's.
pub(crate) fn keeps_machine_id() -> Result<()> {
    let machine_id = fs::read_to_string("/etc/machine-id")?;
    let machine_ids: Vec<String> = user_record()?.binding.into_keys().collect();
    assert_eq!(machine_ids, [machine_id.trim()]);
    Ok(())
}

/// The first boot persisted the machine ID in /var.
pub(crate) fn persists_machine_id_in_var() -> Result<()> {
    let path = Path::new(DBUS_MACHINE_ID);
    assert!(path.symlink_metadata()?.is_file());
    assert!(written_before_boot(path)?);
    assert_eq!(
        fs::read_to_string(path)?,
        fs::read_to_string("/etc/machine-id")?
    );
    Ok(())
}

/// Every host key sshd loads links to one in /var from the first boot.
pub(crate) fn keeps_ssh_host_keys() -> Result<()> {
    let config = Command::new("sshd").arg("-T").run_get_string()?;
    let keys: Vec<&Utf8Path> = config
        .lines()
        .filter_map(|line| line.split_once(' '))
        // Newer OpenSSH prints the keywords in camel case.
        .filter(|(keyword, _)| keyword.eq_ignore_ascii_case("hostkey"))
        .map(|(_, path)| Utf8Path::new(path))
        .collect();
    assert!(!keys.is_empty());
    for key in keys {
        for link in [key.to_owned(), key.with_extension("pub")] {
            let target = Utf8Path::new(SSH_HOST_KEY_DIR).join(link.file_name().unwrap());
            assert_eq!(fs::read_link(&link)?, target.as_std_path(), "{link}");
            assert!(written_before_boot(target.as_std_path())?, "{target}");
        }
    }
    Ok(())
}

/// bootc's pull secret is linked into /run.
pub(crate) fn links_pull_secret() -> Result<()> {
    assert_eq!(fs::read_link(PULL_SECRET_LINK)?, Path::new(PULL_SECRET));
    Ok(())
}
