//! Filesystem operations.

use anyhow::{Result, bail};
use cap_std_ext::cap_std::fs_utf8::Dir;

/// Copy the tree under `from` into `to`, which exists and is empty.
pub(crate) fn copy_dir(from: &Dir, to: &Dir) -> Result<()> {
    for entry in from.entries()? {
        let entry = entry?;
        let name = entry.file_name()?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            to.create_dir(&name)?;
            to.set_permissions(&name, entry.metadata()?.permissions())?;
            copy_dir(&from.open_dir(&name)?, &to.open_dir(&name)?)?;
        } else if file_type.is_file() {
            from.copy(&name, to, &name)?;
        } else {
            bail!("{name} is neither a file nor a directory");
        }
    }
    Ok(())
}

/// Move the directory `from` to `to` by copying. overlayfs cannot rename a
/// directory a lower layer holds.
pub(crate) fn move_dir(root: &Dir, from: &str, to: &str) -> Result<()> {
    root.create_dir(to)?;
    root.set_permissions(to, root.metadata(from)?.permissions())?;
    copy_dir(&root.open_dir(from)?, &root.open_dir(to)?)?;
    Ok(root.remove_dir_all(from)?)
}

#[cfg(test)]
mod tests {
    use cap_std_ext::cap_std::fs::{MetadataExt, Permissions, PermissionsExt};

    use super::*;
    use crate::testutil::rootfs;

    #[test]
    fn moves_directory_tree() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("from/sub")?;
        root.set_permissions("from", Permissions::from_mode(0o750))?;
        root.set_permissions("from/sub", Permissions::from_mode(0o700))?;
        root.write("from/sub/file", b"content")?;
        root.write("from/top", b"")?;
        root.set_permissions("from/top", Permissions::from_mode(0o600))?;

        move_dir(&root, "from", "to")?;

        assert!(!root.exists("from"));
        assert_eq!(root.read("to/sub/file")?, b"content");
        assert_eq!(root.metadata("to")?.mode() & 0o777, 0o750);
        assert_eq!(root.metadata("to/sub")?.mode() & 0o777, 0o700);
        assert_eq!(root.metadata("to/top")?.mode() & 0o777, 0o600);
        Ok(())
    }

    #[test]
    fn fails_on_symlink() -> Result<()> {
        let root = rootfs()?;
        root.create_dir("from")?;
        root.symlink("top", "from/link")?;
        let err = format!("{:#}", move_dir(&root, "from", "to").unwrap_err());
        assert!(err.contains("link is neither"), "{err}");
        Ok(())
    }
}
