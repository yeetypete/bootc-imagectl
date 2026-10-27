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

/// The `.pcrsig` signatures for the sha256 bank, one per boot phase.
#[derive(Debug, Deserialize)]
struct PcrSignatures {
    sha256: Vec<PcrSignature>,
}

/// A signature of expected PCR values.
#[derive(Debug, Deserialize)]
struct PcrSignature {
    pcrs: Vec<u32>,
    /// The policy reference, such as `initrd`.
    #[serde(rename = "ref")]
    policyref: Option<String>,
}

/// The UKI carries the public key and signatures of its expected PCR 11
/// values. One of them is valid only in the initrd, so that the root can be
/// bound to it. systemd 262 and later mark it with the `initrd` policy
/// reference. Older versions sign only the initrd phase.
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
    let initrd = signatures
        .iter()
        .filter(|signature| signature.policyref.as_deref() == Some("initrd"))
        .count();
    assert!(initrd == 1 || signatures.len() == 1, "{signatures:?}");
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
