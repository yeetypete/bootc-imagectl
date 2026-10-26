//! Check that the UKI of an image signed for Secure Boot carries a signed PCR
//! policy.

use std::process::Command;

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;
use bootc_imagectl::testing::{Feature, IntegrationTest, Requirement, integration_test};
use serde::Deserialize;

use crate::finalize::uki;

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

/// The `.pcrsig` signatures for the sha256 bank, one per boot phase.
#[derive(Debug, Deserialize)]
struct PcrSignatures {
    sha256: Vec<PcrSignature>,
}

/// A signature of expected PCR values.
#[derive(Debug, Deserialize)]
struct PcrSignature {
    pcrs: Vec<u32>,
}

/// The UKI carries the public key and signatures of its expected PCR 11
/// values.
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
    Ok(())
}

const REQUIRES: &[Requirement] = &[Requirement::Feature(Feature::SecureBoot)];

pub(crate) const TESTS: &[IntegrationTest] = &[integration_test!(signs_pcr_policy, REQUIRES)];
