//! Arch Linux and derivatives.

use anyhow::Result;
use cap_std_ext::cap_std::fs::Dir;

use super::Distro;

/// Arch Linux and derivatives, which use pacman.
#[derive(Debug)]
pub(super) struct Arch;

impl Distro for Arch {
    fn name(&self) -> &'static str {
        "arch"
    }

    /// Arch packages generate no distribution-specific per-machine state.
    fn remove_machine_identity(&self, _root: &Dir) -> Result<()> {
        Ok(())
    }
}
