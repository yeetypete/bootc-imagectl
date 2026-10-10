//! Check the installed Debian system without a regular user.

use anyhow::Result;

use crate::{homed, status};

#[test]
fn updates_from_target_imgref() -> Result<()> {
    status::updates_from("localhost/bootc-imagectl-test:debian-homed")
}

#[test]
fn adds_wizard_user_to_groups() -> Result<()> {
    homed::adds_wizard_user_to(&[
        "sudo", "audio", "cdrom", "dip", "floppy", "video", "plugdev", "lpadmin",
    ])
}
