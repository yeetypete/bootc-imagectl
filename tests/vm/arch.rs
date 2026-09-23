//! Check the booted Arch Linux image.

use std::process::Command;

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;

use crate::accounts::{self, MovedUser};

#[test]
fn pacman_lists_packages() -> Result<()> {
    let packages = Command::new("pacman").arg("-Q").output_string()?;
    assert!(
        packages.lines().any(|line| line.starts_with("pacman ")),
        "{packages}"
    );
    Ok(())
}

/// The base image's regular user.
const ARCHIE: MovedUser = MovedUser {
    name: "archie",
    uid: 1000,
    group: "wheel",
    password: "password",
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
fn sysusers_changes_nothing_at_boot() -> Result<()> {
    accounts::sysusers_changed_nothing(&ARCHIE)
}
