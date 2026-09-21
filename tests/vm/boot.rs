//! Check the image booted cleanly.

use std::process::Command;

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;

#[test]
fn boots_to_running() -> Result<()> {
    let state = Command::new("systemctl")
        .arg("is-system-running")
        .output_string()?;
    assert_eq!(state.trim(), "running");
    Ok(())
}
