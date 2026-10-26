//! Check that the installed system booted with Secure Boot on.

use std::fs;

use anyhow::Result;
use bootc_imagectl::testing::{Feature, IntegrationTest, Requirement, integration_test};

/// The `SecureBoot` EFI variable.
const SECURE_BOOT: &str =
    "/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c";

/// Secure Boot is on if the byte after the variable's 4-byte attributes
/// is 1.
fn boots_with_secure_boot() -> Result<()> {
    assert_eq!(fs::read(SECURE_BOOT)?.get(4), Some(&1));
    Ok(())
}

const REQUIRES: &[Requirement] = &[Requirement::Feature(Feature::SecureBoot)];

pub(crate) const TESTS: &[IntegrationTest] = &[integration_test!(boots_with_secure_boot, REQUIRES)];
