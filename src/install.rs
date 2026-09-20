//! Install a finalized bootc image onto a disk.

use anyhow::{Result, bail};

use crate::cli::InstallOpts;

/// Run the `install` subcommand.
///
/// # Errors
///
/// Fails until the subcommand is implemented.
pub fn install(_opts: &InstallOpts) -> Result<()> {
    bail!("install is not yet implemented")
}
