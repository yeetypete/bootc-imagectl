//! Check the installed Fedora system without a regular user.

use anyhow::Result;

use crate::{homed, status};

#[test]
fn updates_from_target_imgref() -> Result<()> {
    status::updates_from("localhost/bootc-imagectl-test:fedora-homed")
}

#[test]
fn creates_wizard_user() -> Result<()> {
    homed::creates_wizard_user()
}
