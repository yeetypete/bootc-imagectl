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
use uzers::UsersCache;

use crate::cli::FinalizeOpts;
use crate::distro;

mod identity;
mod initramfs;
mod layout;
mod lint;
mod tmpfiles;
mod var;

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

    initramfs::build_initramfs(&root).context("building the initramfs")?;

    identity::remove_machine_identity(&root, distro).context("removing machine identity")?;
    distro
        .move_package_database(&root)
        .context("moving the package database")?;
    distro
        .remove_repository_indexes(&root)
        .context("removing repository indexes")?;
    layout::layout_toplevel(&root).context("laying out the toplevel")?;
    tmpfiles::patch_tmpfiles(&root).context("patching tmpfiles.d")?;
    var::write_var_tmpfiles(&root, &UsersCache::new())
        .context("generating /var tmpfiles.d entries")?;
    var::empty_var(&root).context("emptying /var")?;

    lint::bootc_lint()
}
