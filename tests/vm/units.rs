//! Check the enabled units in the booted image.

use std::process::Command;

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;

/// The units the image enabled are still enabled after the first boot
/// applied the systemd presets.
pub(crate) fn are_enabled(units: &[&str]) -> Result<()> {
    for unit in units {
        let state = Command::new("systemctl")
            .args(["is-enabled", unit])
            .output_string()?;
        assert_eq!(state.trim(), "enabled", "{unit}");
    }
    Ok(())
}
