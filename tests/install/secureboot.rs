//! Check that the installed system booted with Secure Boot on.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use bootc_imagectl::finalize::signature_databases;
use bootc_imagectl::testing::{Feature, IntegrationTest, Requirement, integration_test};

/// The directory the kernel exposes the EFI variables in.
const EFIVARS: &str = "/sys/firmware/efi/efivars";

/// The vendor GUID of the PK, KEK and `SecureBoot` variables.
const GLOBAL_VARIABLE: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";

/// The vendor GUID of the db variable.
const IMAGE_SECURITY_DATABASE: &str = "d719b2cb-3d3a-4596-a3bc-dad00e67656f";

/// The certificate xtask signs the test images with, relative to the target
/// directory the test binary runs from.
const CERTIFICATE: &str = "secureboot/public/secureboot-certificate.pem";

/// The contents of the EFI variable `name`, after its 4-byte attributes.
fn variable(name: &str) -> Result<Vec<u8>> {
    let guid = if name == "db" {
        IMAGE_SECURITY_DATABASE
    } else {
        GLOBAL_VARIABLE
    };
    Ok(fs::read(format!("{EFIVARS}/{name}-{guid}"))?.split_off(4))
}

/// The DER form of the certificate the image was signed with.
fn certificate() -> Result<Vec<u8>> {
    let binary = std::env::current_exe()?;
    let target = binary
        .parent()
        .and_then(Path::parent)
        .context("finding the target directory")?;
    let pem = pem::parse(fs::read(target.join(CERTIFICATE))?)?;
    assert_eq!(pem.tag(), "CERTIFICATE");
    Ok(pem.into_contents())
}

/// Secure Boot is enabled.
fn boots_with_secure_boot() -> Result<()> {
    assert_eq!(variable("SecureBoot")?, [1]);
    Ok(())
}

/// The firmware enrolled the PK, KEK and db finalize wrote, with the
/// image's certificate and Microsoft's.
fn enrolls_signature_databases() -> Result<()> {
    for (name, database) in signature_databases(&certificate()?, true) {
        assert!(variable(name)? == database, "{name} differs");
    }
    Ok(())
}

const REQUIRES: &[Requirement] = &[Requirement::Feature(Feature::SecureBoot)];

pub(crate) const TESTS: &[IntegrationTest] = &[
    integration_test!(boots_with_secure_boot, REQUIRES),
    integration_test!(enrolls_signature_databases, REQUIRES),
];
