//! Check `bootc-imagectl finalize` on Arch Linux.

use anyhow::Result;

use crate::accounts::{self, MovedAccounts};
use crate::finalize::{ROOT, names, var_tmpfiles};

#[test]
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

/// The image's package users and its regular user.
const ARCH: MovedAccounts = MovedAccounts {
    system_users: &[("avahi", 969), ("uuidd", 970)],
    user: ("archie", 1000),
    group: "wheel",
};

#[test]
fn locks_uids_of_package_users() {
    accounts::locks_uids(&ARCH);
}

#[test]
fn moves_users_out_of_etc_and_resolves_them_through_nss() -> Result<()> {
    accounts::moves_users_out_of_etc(&ARCH)
}

#[test]
fn writes_user_records_to_userdb() -> Result<()> {
    accounts::writes_user_records(&ARCH)
}

#[test]
fn writes_privileged_records_for_users_with_passwords() -> Result<()> {
    accounts::writes_privileged_records(&ARCH)
}

#[test]
fn writes_membership_files_for_primary_and_auxiliary_groups() -> Result<()> {
    accounts::writes_membership_files(&ARCH)
}

#[test]
fn keeps_only_runtime_directories_in_var() -> Result<()> {
    assert_eq!(names("var")?, ["lock", "run", "tmp"]);
    Ok(())
}

#[test]
fn records_mail_spool_in_var_tmpfiles() -> Result<()> {
    let var = var_tmpfiles()?;
    assert!(var.contains("L /var/mail - - - - spool/mail\n"), "{var}");
    assert!(
        var.contains("d /var/spool/mail 1777 root root -\n"),
        "{var}"
    );
    Ok(())
}

#[test]
fn initramfs_contains_user_records() -> Result<()> {
    accounts::initramfs_contains_user_records(&ARCH)
}
