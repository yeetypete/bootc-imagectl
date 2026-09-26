//! Turn the rootfs the package manager built into a bootc image.
//!
//! Runs inside the image being built, after the last package install:
//!
//! ```text
//! RUN bootc-imagectl finalize --sysusers-lock /usr/lib/sysusers.d/00-bootc-imagectl.conf
//! ```

use anyhow::{Context, Result};
use cap_std_ext::cap_std::ambient_authority;
use cap_std_ext::cap_std::fs_utf8::Dir;
use tracing::info;

use crate::cli::FinalizeOpts;
use crate::distro;

mod accounts;
mod identity;
mod initramfs;
mod layout;
mod lint;
mod presets;
mod tmpfiles;
mod var;

/// Run the `finalize` subcommand.
///
/// # Errors
///
/// Fails if the rootfs cannot be opened or a step fails. The error names
/// the step.
pub fn finalize(opts: &FinalizeOpts) -> Result<()> {
    let root = Dir::open_ambient_dir("/", ambient_authority()).context("opening /")?;
    let distro = distro::detect(&root).context("detecting the distribution")?;
    info!("finalizing a {} rootfs", distro.name());

    accounts::finalize(&root, distro, &opts.sysusers_lock).context("finalizing the accounts")?;
    initramfs::finalize(&root, distro).context("building the initramfs")?;
    identity::finalize(&root, distro).context("removing the machine identity")?;
    layout::finalize(&root).context("laying out the filesystem")?;
    var::finalize(&root, distro).context("finalizing /var")?;
    presets::finalize(&root).context("recording enabled units in a preset file")?;

    lint::bootc_lint()
}
