//! Distribution-specific behaviour.

use anyhow::{Context, Result};
use cap_std_ext::cap_std::fs::Dir;

mod arch;
mod os_release;

/// Finalize steps whose behaviour depends on the distribution's package
/// manager and packaging conventions. Derivatives share their parent's
/// implementation (e.g. Manjaro uses `Arch`).
pub trait Distro: std::fmt::Debug {
    /// Lowercase name for logs and errors.
    fn name(&self) -> &'static str;

    /// Delete the machine identity this distribution's packages generate at
    /// build time.
    ///
    /// # Errors
    ///
    /// Fails on any filesystem error in the rootfs.
    fn remove_machine_identity(&self, root: &Dir) -> Result<()>;

    /// Delete the repository indexes the package manager downloaded during
    /// the build. The database of installed packages is not removed.
    ///
    /// # Errors
    ///
    /// Fails on any filesystem error in the rootfs.
    fn remove_repository_indexes(&self, root: &Dir) -> Result<()>;
}

/// The distribution an os-release `ID` or `ID_LIKE` entry names, if supported.
fn from_id(id: &str) -> Option<&'static dyn Distro> {
    match id {
        "arch" => Some(&arch::Arch),
        // Other distributions go here.
        _ => None,
    }
}

/// Detect the distribution from the rootfs's os-release.
///
/// # Errors
///
/// Fails if os-release is missing or reports an unsupported distribution.
pub fn detect(root: &Dir) -> Result<&'static dyn Distro> {
    let release = os_release::read(root)?;
    release.lineage().find_map(from_id).with_context(|| {
        format!(
            "unsupported distribution {:?} (ID_LIKE={:?})",
            release.id, release.id_like
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::rootfs;

    fn with_os_release(content: &str) -> Result<cap_std_ext::cap_tempfile::TempDir> {
        let root = rootfs()?;
        root.create_dir_all("etc")?;
        root.write("etc/os-release", content)?;
        Ok(root)
    }

    #[test]
    fn detects_arch() -> Result<()> {
        let root = with_os_release("ID=arch\nBUILD_ID=rolling\n")?;
        assert_eq!(detect(&root)?.name(), "arch");
        Ok(())
    }

    #[test]
    fn maps_derivatives_to_their_family() -> Result<()> {
        let root = with_os_release("ID=manjaro\nID_LIKE=\"manjaro arch\"\n")?;
        assert_eq!(detect(&root)?.name(), "arch");
        let root = with_os_release("ID=endeavouros\nID_LIKE=arch\n")?;
        assert_eq!(detect(&root)?.name(), "arch");
        Ok(())
    }

    #[test]
    fn rejects_unsupported_distributions() -> Result<()> {
        let root = with_os_release("ID=fedora\n")?;
        let err = detect(&root).unwrap_err().to_string();
        assert!(err.contains("fedora"), "{err}");
        Ok(())
    }
}
