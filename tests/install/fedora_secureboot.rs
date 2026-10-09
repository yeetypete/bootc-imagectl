//! Check the installed Fedora system signed for Secure Boot.

use anyhow::Result;

use crate::{secureboot, status};

#[test]
fn updates_from_target_imgref() -> Result<()> {
    status::updates_from("localhost/bootc-imagectl-test:fedora-secureboot")
}

#[test]
fn boots_with_secure_boot() -> Result<()> {
    assert!(secureboot::enabled()?);
    Ok(())
}
