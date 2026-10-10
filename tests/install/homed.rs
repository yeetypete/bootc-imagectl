//! Check the user systemd-homed's first boot wizard created.

use std::process::Command;

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;

/// The user the xtask passes a credential for.
const USER: &str = "alice";

/// systemd-homed manages the user.
pub(crate) fn creates_wizard_user() -> Result<()> {
    Command::new("homectl")
        .args(["inspect", USER])
        .output_string()?;
    Ok(())
}
