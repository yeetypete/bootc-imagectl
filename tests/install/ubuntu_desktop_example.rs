//! Check the installed Ubuntu desktop example.

use std::process::Command;

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;

use crate::{homed, status};

#[test]
fn updates_from_target_imgref() -> Result<()> {
    status::updates_from("localhost/bootc-imagectl-test:ubuntu-desktop-example")
}

#[test]
fn adds_wizard_user_to_groups() -> Result<()> {
    homed::adds_wizard_user_to(&["sudo", "adm", "cdrom", "dip", "plugdev", "users"])
}

#[test]
fn stores_wizard_user_unencrypted() -> Result<()> {
    homed::stores_wizard_user_unencrypted()
}

#[test]
fn starts_display_manager() -> Result<()> {
    Command::new("systemctl")
        .args(["is-active", "graphical.target", "gdm.service"])
        .output_string()?;
    Ok(())
}
