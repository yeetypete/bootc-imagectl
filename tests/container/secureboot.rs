//! Check that the UKI of an image signed for Secure Boot carries a signed PCR
//! policy.

use std::process::Command;

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;
use bootc_imagectl::testing::{Feature, IntegrationTest, Requirement, integration_test};
use serde::Deserialize;

use crate::finalize::{initramfs_paths, uki};

/// The UKI's PCR sections, from `ukify inspect --json`.
#[derive(Debug, Deserialize)]
struct PcrSections {
    #[serde(rename = ".pcrpkey")]
    pcrpkey: Section,
    #[serde(rename = ".pcrsig")]
    pcrsig: Section,
}

/// A text section of the UKI, from `ukify inspect --json`.
#[derive(Debug, Deserialize)]
struct Section {
    text: String,
}

/// The `.pcrsig` signatures for the sha256 bank.
#[derive(Debug, Deserialize)]
struct PcrSignatures {
    sha256: Vec<PcrSignature>,
}

/// A signature of expected PCR values.
#[derive(Debug, Deserialize)]
struct PcrSignature {
    pcrs: Vec<u32>,
    /// The TPM2 policy digest of the expected PCR values.
    #[serde(rename = "pol")]
    policy_digest: String,
}

/// The UKI carries the public key and signatures of its expected PCR 11
/// values, all for the initrd alone.
fn signs_pcr_policy() -> Result<()> {
    let sections: PcrSections = Command::new("ukify")
        .arg("inspect")
        .arg(uki()?)
        .args([
            "--json=short",
            "--section",
            ".pcrpkey:text",
            "--section",
            ".pcrsig:text",
        ])
        .run_and_parse_json()?;
    assert_eq!(pem::parse(&sections.pcrpkey.text)?.tag(), "PUBLIC KEY");
    let signatures = serde_json::from_str::<PcrSignatures>(&sections.pcrsig.text)?.sha256;
    assert!(!signatures.is_empty());
    assert!(signatures.iter().all(|signature| signature.pcrs == [11]));
    assert!(
        signatures
            .iter()
            .all(|signature| signature.policy_digest == signatures[0].policy_digest),
        "{signatures:?}"
    );
    Ok(())
}

/// The initrd measures entering it into PCR 11, which the signed PCR policy
/// expects.
fn initramfs_measures_boot_phase() -> Result<()> {
    assert!(
        initramfs_paths()?
            .iter()
            .any(|path| path.ends_with("/systemd-pcrphase-initrd.service")),
        "no systemd-pcrphase-initrd.service in the initramfs"
    );
    Ok(())
}

const REQUIRES: &[Requirement] = &[Requirement::Feature(Feature::SecureBoot)];

pub(crate) const TESTS: &[IntegrationTest] = &[
    integration_test!(signs_pcr_policy, REQUIRES),
    integration_test!(initramfs_measures_boot_phase, REQUIRES),
];
