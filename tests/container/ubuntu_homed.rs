//! Check `bootc-imagectl finalize` on Ubuntu without a regular user.

use anyhow::Result;

use crate::homed;

#[test]
fn adds_wizard_user_to_groups() -> Result<()> {
    homed::adds_wizard_user_to(&["sudo", "adm", "cdrom", "dip", "plugdev", "users"])
}
