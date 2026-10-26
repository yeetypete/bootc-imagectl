//! Check that the installed system booted with Secure Boot on.

use std::fs;

use anyhow::Result;
use bootc_imagectl::testing::{Feature, IntegrationTest, Requirement, integration_test};
use uuid::uuid;

/// The `SecureBoot` EFI variable.
const SECURE_BOOT: &str =
    "/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c";

/// The KEK and db EFI variables.
const KEK: &str = "/sys/firmware/efi/efivars/KEK-8be4df61-93ca-11d2-aa0d-00e098032b8c";
const DB: &str = "/sys/firmware/efi/efivars/db-d719b2cb-3d3a-4596-a3bc-dad00e67656f";

/// The owner of Microsoft's certificates in a signature list.
const MICROSOFT_GUID: [u8; 16] = uuid!("77fa9abd-0359-4d32-bd60-28f4e78f784b").to_bytes_le();

/// Secure Boot is on if the byte after the variable's 4-byte attributes
/// is 1.
fn boots_with_secure_boot() -> Result<()> {
    assert_eq!(fs::read(SECURE_BOOT)?.get(4), Some(&1));
    Ok(())
}

/// systemd-boot enrolled Microsoft's certificates next to ours.
fn enrolls_microsoft_certificates() -> Result<()> {
    for variable in [KEK, DB] {
        let data = fs::read(variable)?;
        assert!(
            data.windows(MICROSOFT_GUID.len())
                .any(|window| window == MICROSOFT_GUID),
            "{variable}"
        );
    }
    Ok(())
}

const REQUIRES: &[Requirement] = &[Requirement::Feature(Feature::SecureBoot)];

pub(crate) const TESTS: &[IntegrationTest] = &[
    integration_test!(boots_with_secure_boot, REQUIRES),
    integration_test!(enrolls_microsoft_certificates, REQUIRES),
];
