//! Turn the rootfs into a bootc image.

use anyhow::{Result, bail};

use crate::cli::FinalizeOpts;

/// Run the `finalize` subcommand.
///
/// # Errors
///
/// Fails until the subcommand is implemented.
pub fn finalize(_opts: FinalizeOpts) -> Result<()> {
    bail!("finalize is not yet implemented")
}
