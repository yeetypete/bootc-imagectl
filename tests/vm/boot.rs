//! Check the image booted cleanly.

use std::process::Command;

use anyhow::Result;

#[test]
fn boots_to_running() -> Result<()> {
    let output = Command::new("systemctl")
        .arg("is-system-running")
        .output()?;
    let state = String::from_utf8(output.stdout)?;
    assert_eq!(state.trim(), "running");
    Ok(())
}
