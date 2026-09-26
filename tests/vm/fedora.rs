//! Check the booted Fedora image.

use std::process::Command;

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;

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

#[test]
fn sysusers_changes_nothing_at_boot() -> Result<()> {
    accounts::sysusers_changed_nothing(&FEDORA)
}

#[test]
fn keeps_enabled_units_after_first_boot() -> Result<()> {
    let state = Command::new("systemctl")
        .args(["is-enabled", "systemd-timesyncd.service"])
        .output_string()?;
    assert_eq!(state.trim(), "enabled");
    Ok(())
}
