//! Check `bootc-imagectl finalize` on Arch Linux.

use std::process::Command;
use std::sync::LazyLock;

use anyhow::Result;

use crate::finalize::{ROOT, names};

#[test]
fn moves_pacman_database_and_removes_its_indexes() -> Result<()> {
    let pacman_conf = ROOT.read_to_string("etc/pacman.conf")?;
    assert!(
        pacman_conf.contains("\nDBPath = /usr/lib/sysimage/pacman/\n"),
        "{pacman_conf}"
    );
    assert!(ROOT.exists("usr/lib/sysimage/pacman/local/ALPM_DB_VERSION"));
    assert!(names("usr/lib/sysimage/pacman/sync")?.is_empty());
    Ok(())
}

#[test]
fn pacman_lists_packages_from_moved_database() -> Result<()> {
    LazyLock::force(&ROOT);
    let output = Command::new("pacman").arg("-Q").output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let packages = String::from_utf8(output.stdout)?;
    assert!(
        packages.lines().any(|line| line.starts_with("pacman ")),
        "{packages}"
    );
    Ok(())
}
