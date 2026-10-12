//! Check `bootc-imagectl finalize` on Fedora.

use std::path::Path;

use anyhow::Result;

use crate::accounts::{self, MovedAccounts};
use crate::finalize::{ROOT, names, preset_changes, var_tmpfiles};

#[test]
fn applies_full_preset_policy() -> Result<()> {
    // finalize runs `systemctl preset-all --preset-mode=enable-only`, which
    // never disables a unit. On Fedora, the first boot runs `systemctl
    // preset-all --preset-mode=full`, which also disables the units that the
    // preset files disable. Check that it changes no unit of the image.
    let changes = preset_changes("full")?;
    assert!(changes.is_empty(), "{changes:#?}");
    Ok(())
}

#[test]
fn keeps_rpm_database_under_usr() -> Result<()> {
    assert!(ROOT.exists("usr/lib/sysimage/rpm/rpmdb.sqlite"));
    assert_eq!(
        ROOT.read_link("var/lib/rpm")?,
        Path::new("../../usr/lib/sysimage/rpm")
    );
    Ok(())
}

/// The image's package users and its regular user.
const FEDORA: MovedAccounts = MovedAccounts {
    system_users: &[("systemd-coredump", 997), ("systemd-oom", 998)],
    user: ("fedora", 1000),
    group: "wheel",
};

#[test]
fn locks_uids_of_package_users() {
    accounts::locks_uids(&FEDORA);
}

#[test]
fn keeps_removed_accounts_locked() -> Result<()> {
    accounts::keeps_removed_accounts_locked()
}

#[test]
fn moves_accounts_out_of_etc_and_resolves_them_through_nss() -> Result<()> {
    accounts::moves_accounts_out_of_etc(&FEDORA)
}

#[test]
fn writes_user_records_to_userdb() -> Result<()> {
    accounts::writes_user_records(&FEDORA)
}

#[test]
fn writes_group_records_to_userdb() -> Result<()> {
    accounts::writes_group_records(&FEDORA)
}

#[test]
fn writes_privileged_records_for_users_with_passwords() -> Result<()> {
    accounts::writes_privileged_records(&FEDORA)
}

#[test]
fn writes_membership_files_for_primary_and_auxiliary_groups() -> Result<()> {
    accounts::writes_membership_files(&FEDORA)
}

#[test]
fn keeps_only_expected_entries_in_var() -> Result<()> {
    assert_eq!(names("var")?, ["lib", "lock", "run", "tmp"]);
    Ok(())
}

#[test]
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

#[test]
fn initramfs_contains_user_records() -> Result<()> {
    accounts::initramfs_contains_user_records(&FEDORA)
}
