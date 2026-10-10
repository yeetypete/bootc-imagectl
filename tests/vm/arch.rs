//! Check the booted Arch Linux image.

use anyhow::Result;

use crate::accounts::{self, MovedUser};
use crate::units;

/// The base image's regular user.
const ARCHIE: MovedUser = MovedUser {
    name: "archie",
    uid: 1000,
    group: "wheel",
    password: "archie",
    system_user: ("avahi", 969),
};

#[test]
fn resolves_moved_users_through_nss() -> Result<()> {
    accounts::resolves_through_nss(&ARCHIE)
}

#[test]
fn accepts_password_of_moved_user() -> Result<()> {
    accounts::accepts_password(&ARCHIE)
}

#[test]
fn keeps_account_files_unchanged_after_first_boot() -> Result<()> {
    accounts::account_files_unchanged()
}

#[test]
fn keeps_enabled_units_after_first_boot() -> Result<()> {
    units::are_enabled(&[
        "systemd-networkd.service",
        "sshd.service",
        "remote-fs.target",
    ])
}
