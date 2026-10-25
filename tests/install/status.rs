//! Check what bootc reports about the installed system.

use std::process::Command;

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;
use bootc_imagectl::testing::{self, Test};
use bootc_imagectl::trial;
use serde::Deserialize;

/// The host status, as reported by `bootc status --json`.
#[derive(Debug, Deserialize)]
struct Host {
    status: HostStatus,
}

#[derive(Debug, Deserialize)]
struct HostStatus {
    booted: BootEntry,
}

#[derive(Debug, Deserialize)]
struct BootEntry {
    image: ImageStatus,
}

#[derive(Debug, Deserialize)]
struct ImageStatus {
    image: ImageReference,
}

#[derive(Debug, Deserialize)]
struct ImageReference {
    image: String,
    transport: String,
}

/// The booted deployment updates from the image under test in a registry.
fn updates_from_target_imgref() -> Result<()> {
    let host: Host = Command::new("bootc")
        .args(["status", "--json"])
        .run_and_parse_json()?;
    let reference = host.status.booted.image.image;
    assert_eq!(reference.image, testing::image().reference);
    assert_eq!(reference.transport, "registry");
    Ok(())
}

pub(crate) const TESTS: &[Test] = &[trial!(updates_from_target_imgref)];
