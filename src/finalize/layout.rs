//! Lay out the filesystem as bootc expects.

use anyhow::{Context, Result};
use cap_std_ext::cap_std::fs::Dir;
use cap_std_ext::dirext::CapStdExtDirExt;
use tracing::debug;

/// Toplevel directories that live under `/var`, `/run` or `/sysroot` on a
/// bootc system, and their symlinks. tmpfiles.d creates the targets at boot.
const TOPLEVEL_SYMLINKS: &[(&str, &str)] = &[
    ("home", "var/home"),
    ("root", "var/roothome"),
    ("srv", "var/srv"),
    ("mnt", "var/mnt"),
    ("media", "run/media"),
    ("ostree", "sysroot/ostree"),
];

/// Lay out the toplevel directories and symlinks.
pub(crate) fn layout_toplevel(root: &Dir) -> Result<()> {
    debug!("laying out the toplevel");
    for (link, target) in TOPLEVEL_SYMLINKS {
        root.remove_all_optional(link)
            .with_context(|| format!("removing /{link}"))?;
        root.symlink(target, link)
            .with_context(|| format!("linking /{link} -> {target}"))?;
    }

    // The composefs digest sealed into the UKI excludes the UKI itself but
    // not its parent directory, so the directory has to exist in the image.
    root.remove_all_optional("boot").context("removing /boot")?;
    root.create_dir_all("boot/EFI/Linux")
        .context("creating /boot/EFI/Linux")?;

    root.create_dir_all("sysroot")
        .context("creating /sysroot")?;
    root.create_dir_all("var").context("creating /var")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::testutil::rootfs;

    #[test]
    fn replaces_directories_and_stale_links_with_symlinks() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("home/user")?;
        root.write("home/user/file", b"x")?;
        root.symlink("elsewhere", "srv")?;
        root.write("mnt", b"a regular file")?;

        layout_toplevel(&root)?;

        for (link, target) in TOPLEVEL_SYMLINKS {
            assert_eq!(root.read_link(link)?, Path::new(target), "/{link}");
        }
        Ok(())
    }

    #[test]
    fn recreates_boot_and_creates_mount_points() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("boot/EFI/Linux")?;
        root.write("boot/EFI/Linux/stale.efi", b"x")?;
        root.write("boot/vmlinuz", b"x")?;

        layout_toplevel(&root)?;

        assert!(root.is_dir("boot/EFI/Linux"));
        assert_eq!(root.read_dir("boot/EFI/Linux")?.count(), 0);
        assert_eq!(root.read_dir("boot")?.count(), 1);
        assert!(root.is_dir("sysroot"));
        assert!(root.is_dir("var"));
        Ok(())
    }

    #[test]
    fn is_idempotent() -> Result<()> {
        let root = rootfs()?;
        layout_toplevel(&root)?;
        layout_toplevel(&root)?;
        assert_eq!(root.read_link("home")?, Path::new("var/home"));
        Ok(())
    }
}
