//! Check what bootc reports about the installed system.

use std::process::Command;

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;
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

/// The booted deployment updates from `imgref` in a registry.
pub(crate) fn updates_from(imgref: &str) -> Result<()> {
    let host: Host = Command::new("bootc")
        .args(["status", "--json"])
        .run_and_parse_json()?;
    let reference = host.status.booted.image.image;
    assert_eq!(reference.image, imgref);
    assert_eq!(reference.transport, "registry");
    Ok(())
}
