//! Check `bootc-imagectl finalize` on Debian.

use std::path::Path;

use anyhow::Result;

use crate::accounts::{self, MovedAccounts};
use crate::finalize::{ROOT, names, var_tmpfiles};

#[test]
fn moves_dpkg_database_and_links_it_back() -> Result<()> {
    assert!(ROOT.exists("usr/lib/sysimage/dpkg/status"));
    assert_eq!(
        ROOT.read_link("var/lib/dpkg")?,
        Path::new("../../usr/lib/sysimage/dpkg")
    );
    assert_eq!(
        ROOT.read_link("var/cache/debconf")?,
        Path::new("../../usr/lib/sysimage/debconf")
    );
    Ok(())
}

#[test]
fn keeps_apt_log_directory_in_var() {
    assert!(ROOT.is_dir("var/log/apt"));
}

#[test]
fn removes_snakeoil_certificate() -> Result<()> {
    assert!(!ROOT.exists("etc/ssl/certs/ssl-cert-snakeoil.pem"));
    assert!(!ROOT.exists("etc/ssl/private/ssl-cert-snakeoil.key"));
    for name in names("etc/ssl/certs")? {
        let target = ROOT.read_link(Path::new("etc/ssl/certs").join(&name)).ok();
        assert_ne!(
            target.as_deref(),
            Some(Path::new("ssl-cert-snakeoil.pem")),
            "{name:?}"
        );
    }
    Ok(())
}

#[test]
fn records_enabled_units_in_presets() {
    // Debian's presets enable every unit the image enables.
    assert!(!ROOT.exists("usr/lib/systemd/system-preset/10-bootc-imagectl.preset"));
}

/// The image's package users and its regular user.
const DEBIAN: MovedAccounts = MovedAccounts {
    system_users: &[("sshd", 990), ("systemd-network", 997)],
    user: ("debian", 1000),
    group: "sudo",
};

#[test]
fn locks_uids_of_package_users() {
    accounts::locks_uids(&DEBIAN);
}

#[test]
fn keeps_removed_accounts_locked() -> Result<()> {
    accounts::keeps_removed_accounts_locked()
}

#[test]
fn moves_users_out_of_etc_and_resolves_them_through_nss() -> Result<()> {
    accounts::moves_users_out_of_etc(&DEBIAN)
}

#[test]
fn writes_user_records_to_userdb() -> Result<()> {
    accounts::writes_user_records(&DEBIAN)
}

#[test]
fn writes_privileged_records_for_users_with_passwords() -> Result<()> {
    accounts::writes_privileged_records(&DEBIAN)
}

#[test]
fn writes_membership_files_for_primary_and_auxiliary_groups() -> Result<()> {
    accounts::writes_membership_files(&DEBIAN)
}

#[test]
fn keeps_only_expected_entries_in_var() -> Result<()> {
    assert_eq!(names("var")?, ["cache", "lib", "lock", "log", "run", "tmp"]);
    Ok(())
}

#[test]
fn records_var_entries_in_tmpfiles() -> Result<()> {
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
    accounts::initramfs_contains_user_records(&DEBIAN)
}
