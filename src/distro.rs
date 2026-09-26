//! Distribution-specific behaviour.

use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, ensure};
use cap_std_ext::camino::Utf8Path;
use cap_std_ext::cap_std::fs_utf8::Dir;

mod arch;
mod debian;
mod os_release;

/// The name of a package, as defined by its package manager.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageName(String);

impl PackageName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for PackageName {
    type Err = anyhow::Error;

    fn from_str(name: &str) -> Result<Self> {
        ensure!(!name.is_empty(), "the package name is empty");
        ensure!(
            !name.contains(char::is_whitespace),
            "the package name {name:?} contains whitespace"
        );
        Ok(Self(name.to_owned()))
    }
}

impl fmt::Display for PackageName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where package managers keep their state on an image, relative to the
/// rootfs.
const SYSIMAGE: &str = "usr/lib/sysimage";

/// Finalize steps whose behaviour depends on the distribution's package
/// manager and packaging conventions. Derivatives share their parent's
/// implementation (e.g. Manjaro uses `Arch`).
pub trait Distro: std::fmt::Debug {
    /// Lowercase name for logs and errors.
    fn name(&self) -> &'static str;

    /// Put the kernel image at `/usr/lib/modules/<kver>/vmlinuz`, where bootc
    /// looks for it. Distributions whose kernel package installs it there
    /// need no implementation.
    ///
    /// # Errors
    ///
    /// Fails on any filesystem error in the rootfs.
    fn stage_kernel(&self, _root: &Dir, _kver: &str) -> Result<()> {
        Ok(())
    }

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

    /// Recreate in the emptied /var what the package manager needs in a
    /// derived build. finalize already keeps the symlinks from /var into
    /// /usr. Package managers that need nothing else need no implementation.
    ///
    /// # Errors
    ///
    /// Fails on any filesystem error in the rootfs.
    fn restore_package_state(&self, _root: &Dir) -> Result<()> {
        Ok(())
    }

    /// The name of the installed package that owns `path`, an absolute path
    /// in the image, or `None` if no package does.
    ///
    /// # Errors
    ///
    /// Fails if the package manager cannot answer.
    fn package_owning(&self, path: &Utf8Path) -> Result<Option<PackageName>>;

    /// Whether the package `name` is installed.
    ///
    /// # Errors
    ///
    /// Fails if the package manager cannot answer.
    fn is_installed(&self, name: &PackageName) -> Result<bool>;
}

/// The distribution an os-release `ID` or `ID_LIKE` entry names, if supported.
fn from_id(id: &str) -> Option<&'static dyn Distro> {
    match id {
        "arch" => Some(&arch::Arch),
        "debian" => Some(&debian::Debian),
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
    fn detects_debian_and_ubuntu() -> Result<()> {
        let root = with_os_release("ID=debian\n")?;
        assert_eq!(detect(&root)?.name(), "debian");
        let root = with_os_release("ID=ubuntu\nID_LIKE=debian\n")?;
        assert_eq!(detect(&root)?.name(), "debian");
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
