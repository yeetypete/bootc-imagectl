//! Turn the rootfs the package manager built into a bootc image.
//!
//! Runs inside the image being built, after the last package install:
//!
//! ```text
//! RUN bootc-imagectl finalize
//! ```

use anyhow::{Context, Result};
use cap_std_ext::cap_std::ambient_authority;
use cap_std_ext::cap_std::fs::Dir;
use tracing::info;

use crate::cli::FinalizeOpts;
use crate::distro;

mod identity;
mod layout;

/// Run the `finalize` subcommand.
///
/// # Errors
///
/// Fails if the rootfs cannot be opened or a step fails. The error names
/// the step.
pub fn finalize(_opts: FinalizeOpts) -> Result<()> {
    let root = Dir::open_ambient_dir("/", ambient_authority()).context("opening /")?;
    let distro = distro::detect(&root).context("detecting the distribution")?;
    info!("finalizing a {} rootfs", distro.name());
    identity::remove_machine_identity(&root, distro).context("removing machine identity")?;
    layout::layout_toplevel(&root).context("laying out the toplevel")?;
    Ok(())
}
