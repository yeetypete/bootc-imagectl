//! Check the booted Arch Linux image.

use std::process::Command;

use anyhow::Result;

#[test]
fn pacman_lists_packages() -> Result<()> {
    let output = Command::new("pacman").arg("-Q").output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let packages = String::from_utf8(output.stdout)?;
    assert!(
        packages.lines().any(|line| line.starts_with("pacman ")),
        "{packages}"
    );
    Ok(())
}
