//! Check `bootc-imagectl finalize` on Ubuntu.

use anyhow::Result;

use crate::accounts::{self, MovedAccounts};
use crate::finalize::{ROOT, names, var_tmpfiles};

#[test]
fn moves_dpkg_database_and_links_it_back() -> Result<()> {
    assert!(ROOT.exists("usr/lib/sysimage/dpkg/status"));
    assert_eq!(
        ROOT.read_link("var/lib/dpkg")?,
        std::path::Path::new("../../usr/lib/sysimage/dpkg")
    );
    Ok(())
}

/// The image's package users and its regular user.
const UBUNTU: MovedAccounts = MovedAccounts {
    system_users: &[("sshd", 992), ("systemd-network", 998)],
    user: ("ubuntu", 1000),
    group: "sudo",
};

#[test]
fn locks_uids_of_package_users() {
    accounts::locks_uids(&UBUNTU);
}

#[test]
fn moves_users_out_of_etc_and_resolves_them_through_nss() -> Result<()> {
    accounts::moves_users_out_of_etc(&UBUNTU)
}

#[test]
fn writes_user_records_to_userdb() -> Result<()> {
    accounts::writes_user_records(&UBUNTU)
}

#[test]
fn writes_privileged_records_for_users_with_passwords() -> Result<()> {
    accounts::writes_privileged_records(&UBUNTU)
}

#[test]
fn writes_membership_files_for_primary_and_auxiliary_groups() -> Result<()> {
    accounts::writes_membership_files(&UBUNTU)
}

#[test]
fn keeps_package_state_links_in_var() -> Result<()> {
    assert_eq!(names("var")?, ["cache", "lib", "lock", "log", "run", "tmp"]);
    assert!(ROOT.is_dir("var/log/apt"));
    assert_eq!(
        ROOT.read_link("var/cache/debconf")?,
        std::path::Path::new("../../usr/lib/sysimage/debconf")
    );
    Ok(())
}

#[test]
fn records_package_state_links_in_var_tmpfiles() -> Result<()> {
    let var = var_tmpfiles()?;
    assert!(
        var.contains("L /var/lib/dpkg - - - - ../../usr/lib/sysimage/dpkg\n"),
        "{var}"
    );
    assert!(var.contains("d /var/mail 2775 root mail -\n"), "{var}");
    Ok(())
}

#[test]
fn initramfs_contains_user_records() -> Result<()> {
    accounts::initramfs_contains_user_records(&UBUNTU)
}
