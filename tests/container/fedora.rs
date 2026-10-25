//! Check `bootc-imagectl finalize` on Fedora.

use std::path::Path;

use anyhow::Result;

use crate::finalize::{ROOT, names, preset_changes, var_tmpfiles};
use bootc_imagectl::testing::{Requirement, Test};
use bootc_imagectl::trial;

fn applies_full_preset_policy() -> Result<()> {
    // finalize runs `systemctl preset-all --preset-mode=enable-only`, which
    // never disables a unit. On Fedora, the first boot runs `systemctl
    // preset-all --preset-mode=full`, which also disables the units that the
    // preset files disable. Check that it changes no unit of the image.
    let changes = preset_changes("full")?;
    assert!(changes.is_empty(), "{changes:#?}");
    Ok(())
}

fn keeps_rpm_database_under_usr() -> Result<()> {
    assert!(ROOT.exists("usr/lib/sysimage/rpm/rpmdb.sqlite"));
    assert_eq!(
        ROOT.read_link("var/lib/rpm")?,
        Path::new("../../usr/lib/sysimage/rpm")
    );
    Ok(())
}

fn keeps_only_expected_entries_in_var() -> Result<()> {
    assert_eq!(names("var")?, ["lib", "lock", "run", "tmp"]);
    Ok(())
}

fn records_var_entries_in_tmpfiles() -> Result<()> {
    let var = var_tmpfiles()?;
    assert!(
        var.contains("L /var/lib/rpm - - - - ../../usr/lib/sysimage/rpm\n"),
        "{var}"
    );
    assert!(
        var.contains("d /var/spool/mail 0775 root mail -\n"),
        "{var}"
    );
    Ok(())
}

const REQUIRES: &[Requirement] = &[Requirement::Distro(&["fedora"])];

pub(crate) const TESTS: &[Test] = &[
    trial!(applies_full_preset_policy, REQUIRES),
    trial!(keeps_rpm_database_under_usr, REQUIRES),
    trial!(keeps_only_expected_entries_in_var, REQUIRES),
    trial!(records_var_entries_in_tmpfiles, REQUIRES),
];
