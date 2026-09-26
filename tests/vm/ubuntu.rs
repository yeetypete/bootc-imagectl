//! Check the booted Ubuntu image.

use anyhow::Result;

use crate::accounts::{self, MovedUser};

/// The base image's regular user.
const UBUNTU: MovedUser = MovedUser {
    name: "ubuntu",
    uid: 1000,
    group: "sudo",
    password: "ubuntu",
    system_user: ("sshd", 992),
};

#[test]
fn resolves_moved_users_through_nss() -> Result<()> {
    accounts::resolves_through_nss(&UBUNTU)
}

#[test]
fn accepts_password_of_moved_user() -> Result<()> {
    accounts::accepts_password(&UBUNTU)
}

#[test]
fn sysusers_changes_nothing_at_boot() -> Result<()> {
    accounts::sysusers_changed_nothing(&UBUNTU)
}
