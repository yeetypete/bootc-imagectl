//! Distribution-specific behaviour.

use anyhow::{Context, Result};
use cap_std_ext::camino::Utf8Path;
use cap_std_ext::cap_std::fs_utf8::Dir;

mod arch;
mod os_release;

/// Finalize steps whose behaviour depends on the distribution's package
/// manager and packaging conventions. Derivatives share their parent's
/// implementation (e.g. Manjaro uses `Arch`).
pub trait Distro: std::fmt::Debug {
    /// Lowercase name for logs and errors.
    fn name(&self) -> &'static str;

    /// Delete the machine identity this distribution's packages generate at
    /// build time. Distributions whose packages generate none need no
    /// implementation.
    ///
    /// # Errors
    ///
    /// Fails on any filesystem error in the rootfs.
    fn remove_machine_identity(&self, _root: &Dir) -> Result<()> {
        Ok(())
    }

    /// Move the state describing the installed packages out of /var, which
    /// finalize empties, to where the package manager finds it on the
    /// installed system, and delete the repository indexes the build
    /// downloaded.
    ///
    /// # Errors
    ///
    /// Fails if the state is missing or cannot be moved.
    fn relocate_package_state(&self, root: &Dir) -> Result<()>;

    /// The name of the installed package that owns `path`, an absolute path
    /// in the image, or `None` if no package does.
    ///
    /// # Errors
    ///
    /// Fails if the package manager cannot answer.
    fn package_owning(&self, path: &Utf8Path) -> Result<Option<String>>;

    /// Whether the package `name` is installed.
    ///
    /// # Errors
    ///
    /// Fails if the package manager cannot answer.
    fn is_installed(&self, name: &str) -> Result<bool>;
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
    use indoc::indoc;

    use super::*;
    use crate::testutil::rootfs;

    fn with_os_release(content: &str) -> Result<cap_std_ext::cap_tempfile::utf8::TempDir> {
        let root = rootfs()?;
        root.create_dir_all("etc")?;
        root.write("etc/os-release", content)?;
        Ok(root)
    }

    #[test]
    fn detects_arch() -> Result<()> {
        let root = with_os_release(indoc! {"
            ID=arch
            BUILD_ID=rolling
        "})?;
        assert_eq!(detect(&root)?.name(), "arch");
        Ok(())
    }

    #[test]
    fn maps_derivatives_to_their_family() -> Result<()> {
        let root = with_os_release(indoc! {r#"
            ID=manjaro
            ID_LIKE="manjaro arch"
        "#})?;
        assert_eq!(detect(&root)?.name(), "arch");
        let root = with_os_release(indoc! {"
            ID=endeavouros
            ID_LIKE=arch
        "})?;
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
