//! Check the installed Debian system.

use anyhow::Result;

use crate::{secureboot, status};

#[test]
fn updates_from_target_imgref() -> Result<()> {
    status::updates_from("localhost/bootc-imagectl-test:debian")
}

#[test]
fn boots_without_secure_boot() -> Result<()> {
    assert!(!secureboot::enabled()?);
    Ok(())
}
