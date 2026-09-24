//! Helpers shared by unit tests.

use anyhow::Result;
use cap_std_ext::camino::Utf8Path;
use cap_std_ext::cap_std::ambient_authority;
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::cap_tempfile::utf8::TempDir;

use crate::distro::Distro;

/// An empty rootfs in a temporary directory.
pub(crate) fn rootfs() -> Result<TempDir> {
    Ok(TempDir::new(ambient_authority())?)
}

/// A distribution whose packages generate no machine identity files, and
/// whose package manager answers from the fields.
#[derive(Debug, Default)]
pub(crate) struct TestDistro {
    /// The package that owns every path in the image, if any.
    pub(crate) owner: Option<&'static str>,
    /// The packages that are not installed.
    pub(crate) not_installed: &'static [&'static str],
}

impl Distro for TestDistro {
    fn name(&self) -> &'static str {
        "test"
    }

    fn relocate_package_state(&self, _root: &Dir) -> Result<()> {
        Ok(())
    }

    fn package_owning(&self, _path: &Utf8Path) -> Result<Option<String>> {
        Ok(self.owner.map(str::to_owned))
    }

    fn is_installed(&self, name: &str) -> Result<bool> {
        Ok(!self.not_installed.contains(&name))
    }
}
