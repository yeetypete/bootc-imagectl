//! Check `bootc-imagectl finalize` on Fedora without a regular user.

use anyhow::Result;

use crate::homed;

#[test]
fn adds_wizard_user_to_groups() -> Result<()> {
    homed::adds_wizard_user_to(&["wheel"])
}
