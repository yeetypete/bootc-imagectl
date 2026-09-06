//! Arch Linux and derivatives.

use anyhow::{Context, Result};
use cap_std_ext::cap_std::fs::Dir;
use cap_std_ext::dirext::CapStdExtDirExt;

use super::Distro;

/// Where pacman keeps its database unless `pacman.conf` says otherwise.
const DEFAULT_DB_PATH: &str = "var/lib/pacman";

/// pacman's database directory, relative to the rootfs. A bootc image
/// normally relocates it under `/usr` with the `DBPath` option.
fn db_path(root: &Dir) -> Result<String> {
    let conf = root
        .read_to_string_optional("etc/pacman.conf")
        .context("reading /etc/pacman.conf")?
        .unwrap_or_default();
    let configured = conf.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "DBPath").then(|| value.trim())
    });
    Ok(configured
        .unwrap_or(DEFAULT_DB_PATH)
        .trim_matches('/')
        .to_owned())
}

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

    /// Remove pacman repository indexes. The local database is not removed.
    fn remove_repository_indexes(&self, root: &Dir) -> Result<()> {
        let sync = format!("{}/sync", db_path(root)?);
        let Some(dir) = root.open_dir_optional(&sync)? else {
            return Ok(());
        };
        for entry in dir.entries()? {
            let name = entry?.file_name();
            dir.remove_all_optional(&name)
                .with_context(|| format!("removing /{sync}/{}", name.display()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::rootfs;

    #[test]
    fn reads_db_path_from_pacman_conf() -> Result<()> {
        let root = rootfs()?;
        root.create_dir("etc")?;
        root.write(
            "etc/pacman.conf",
            "[options]\n# DBPath = /commented/out\nDBPath      = /usr/lib/sysimage/pacman/\n",
        )?;
        assert_eq!(db_path(&root)?, "usr/lib/sysimage/pacman");
        Ok(())
    }

    #[test]
    fn defaults_db_path_without_pacman_conf() -> Result<()> {
        let root = rootfs()?;
        assert_eq!(db_path(&root)?, DEFAULT_DB_PATH);
        Ok(())
    }

    #[test]
    fn removes_indexes_and_keeps_the_local_database() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("var/lib/pacman/sync")?;
        root.create_dir_all("var/lib/pacman/local/pacman-7.0.0-1")?;
        root.write("var/lib/pacman/sync/core.db", b"index")?;
        root.write("var/lib/pacman/sync/extra.db", b"index")?;
        root.write("var/lib/pacman/local/ALPM_DB_VERSION", b"9")?;
        root.write(
            "var/lib/pacman/local/pacman-7.0.0-1/desc",
            b"%NAME%\npacman\n",
        )?;

        Arch.remove_repository_indexes(&root)?;

        assert!(root.is_dir("var/lib/pacman/sync"));
        assert_eq!(root.read_dir("var/lib/pacman/sync")?.count(), 0);
        assert!(root.exists("var/lib/pacman/local/ALPM_DB_VERSION"));
        assert!(root.exists("var/lib/pacman/local/pacman-7.0.0-1/desc"));
        Ok(())
    }

    #[test]
    fn tolerates_a_missing_sync_directory() -> Result<()> {
        let root = rootfs()?;
        Arch.remove_repository_indexes(&root)?;
        Ok(())
    }
}
