//! Check the booted Ubuntu image.

use std::process::Command;

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;

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
fn dpkg_lists_packages_from_moved_database() -> Result<()> {
    let status = Command::new("dpkg-query")
        .args(["-W", "-f=${db:Status-Status}", "bash"])
        .output_string()?;
    assert_eq!(status, "installed");
    Ok(())
}

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
