//! Check `bootc-imagectl finalize` on Debian without a regular user.

use anyhow::Result;

use crate::homed;

#[test]
fn adds_wizard_user_to_groups() -> Result<()> {
    homed::adds_wizard_user_to(&[
        "sudo", "audio", "cdrom", "dip", "floppy", "video", "plugdev", "lpadmin",
    ])
}
