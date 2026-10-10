//! Check the booted Debian image.

use anyhow::Result;

use crate::accounts::{self, MovedUser};
use crate::units;

/// The image's regular user.
const DEBIAN: MovedUser = MovedUser {
    name: "debian",
    uid: 1000,
    group: "sudo",
    password: "debian",
    system_user: ("sshd", 990),
};

#[test]
fn resolves_moved_users_through_nss() -> Result<()> {
    accounts::resolves_through_nss(&DEBIAN)
}

#[test]
fn accepts_password_of_moved_user() -> Result<()> {
    accounts::accepts_password(&DEBIAN)
}

#[test]
fn keeps_account_files_unchanged_after_first_boot() -> Result<()> {
    accounts::account_files_unchanged()
}

#[test]
fn keeps_enabled_units_after_first_boot() -> Result<()> {
    units::are_enabled(&["systemd-networkd.service", "ssh.service"])
}
