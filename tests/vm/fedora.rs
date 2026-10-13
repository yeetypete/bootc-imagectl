//! Check the booted Fedora image.

use anyhow::Result;

use crate::accounts::{self, MovedUser};

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
