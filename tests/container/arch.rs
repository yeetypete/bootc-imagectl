//! Check `bootc-imagectl finalize` on Arch Linux.

use anyhow::Result;

use crate::finalize::{ROOT, names, var_tmpfiles};
use bootc_imagectl::testing::{Requirement, Test};
use bootc_imagectl::trial;

fn moves_pacman_database_and_removes_its_indexes() -> Result<()> {
    let pacman_conf = ROOT.read_to_string("etc/pacman.conf")?;
    assert!(
        pacman_conf
            .lines()
            .any(|line| line == "DBPath = /usr/lib/sysimage/pacman/"),
        "{pacman_conf}"
    );
    assert!(ROOT.exists("usr/lib/sysimage/pacman/local/ALPM_DB_VERSION"));
    assert!(names("usr/lib/sysimage/pacman/sync")?.is_empty());
    Ok(())
}

fn keeps_only_expected_entries_in_var() -> Result<()> {
    assert_eq!(names("var")?, ["lock", "run", "tmp"]);
    Ok(())
}

fn records_var_entries_in_tmpfiles() -> Result<()> {
    let var = var_tmpfiles()?;
    assert!(var.contains("L /var/mail - - - - spool/mail\n"), "{var}");
    assert!(
        var.contains("d /var/spool/mail 1777 root root -\n"),
        "{var}"
    );
    Ok(())
}

const REQUIRES: &[Requirement] = &[Requirement::Distro(&["arch"])];

pub(crate) const TESTS: &[Test] = &[
    trial!(moves_pacman_database_and_removes_its_indexes, REQUIRES),
    trial!(keeps_only_expected_entries_in_var, REQUIRES),
    trial!(records_var_entries_in_tmpfiles, REQUIRES),
];
