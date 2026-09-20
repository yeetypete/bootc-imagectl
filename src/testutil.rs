//! Helpers shared by unit tests.

use anyhow::Result;
use cap_std_ext::cap_std::ambient_authority;
use cap_std_ext::cap_tempfile::utf8::TempDir;

/// An empty rootfs in a temporary directory.
pub(crate) fn rootfs() -> Result<TempDir> {
    Ok(TempDir::new(ambient_authority())?)
}
