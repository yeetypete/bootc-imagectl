//! Check the booted Arch Linux image.

use anyhow::Result;

use crate::accounts::{self, MovedUser};

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
