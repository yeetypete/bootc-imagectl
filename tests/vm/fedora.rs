//! Check the booted Fedora image.

use anyhow::Result;

use crate::accounts::{self, MovedUser};
use crate::units;

/// The base image's regular user.
const FEDORA: MovedUser = MovedUser {
    name: "fedora",
    uid: 1000,
    group: "wheel",
    password: "fedora",
    system_user: ("systemd-oom", 998),
};

#[test]
fn resolves_moved_users_through_nss() -> Result<()> {
    accounts::resolves_through_nss(&FEDORA)
}

#[test]
fn accepts_password_of_moved_user() -> Result<()> {
    accounts::accepts_password(&FEDORA)
}

#[test]
fn keeps_account_files_unchanged_after_first_boot() -> Result<()> {
    accounts::account_files_unchanged()
}

#[test]
fn keeps_enabled_units_after_first_boot() -> Result<()> {
    units::are_enabled(&["sshd.service", "systemd-timesyncd.service"])
}
