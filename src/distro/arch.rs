//! Arch Linux and derivatives.

use std::process::Command;

use anyhow::{Context, Result, bail};
use cap_std_ext::camino::Utf8Path;
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::{CapStdExtDirExt, CapStdExtDirExtUtf8};

use super::Distro;
use crate::fs::move_dir;

/// Where pacman keeps its database unless `pacman.conf` says otherwise.
const DEFAULT_DB_PATH: &str = "var/lib/pacman";

/// Where finalize moves the package database.
const USR_DB_PATH: &str = "usr/lib/sysimage/pacman";

/// pacman's database directory, relative to the rootfs. A bootc image
/// normally relocates it under `/usr` with the `DBPath` option.
fn db_path(root: &Dir) -> Result<String> {
    let conf = root
        .as_cap_std()
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

    /// Move pacman's database under /usr and point `DBPath` at it. A database
    /// already outside /var is left where it is.
    fn move_package_database(&self, root: &Dir) -> Result<()> {
        let from = db_path(root)?;
        if !Utf8Path::new(&from).starts_with("var") {
            return Ok(());
        }
        root.create_dir_all("usr/lib/sysimage")?;
        move_dir(root, &from, USR_DB_PATH)
            .with_context(|| format!("moving /{from} to /{USR_DB_PATH}"))?;

        // Replace the DBPath line, which pacman ships commented out.
        let conf = root
            .read_to_string("etc/pacman.conf")
            .context("reading /etc/pacman.conf")?;
        let db_path_line = format!("DBPath = /{USR_DB_PATH}/");
        let mut lines: Vec<&str> = conf.lines().collect();
        let is_db_path = |line: &&str| {
            line.trim_start()
                .trim_start_matches('#')
                .trim_start()
                .starts_with("DBPath")
        };
        let i = lines
            .iter()
            .position(is_db_path)
            .context("/etc/pacman.conf has no DBPath line")?;
        lines[i] = &db_path_line;
        root.write("etc/pacman.conf", lines.join("\n") + "\n")
            .context("writing /etc/pacman.conf")
    }

    /// Remove pacman repository indexes. The local database is not removed.
    fn remove_repository_indexes(&self, root: &Dir) -> Result<()> {
        let sync = format!("{}/sync", db_path(root)?);
        let Some(dir) = root.open_dir_optional(&sync)? else {
            return Ok(());
        };
        for entry in dir.entries()? {
            let name = entry?.file_name()?;
            dir.remove_all_optional(&name)
                .with_context(|| format!("removing /{sync}/{name}"))?;
        }
        Ok(())
    }

    fn package_owning(&self, path: &Utf8Path) -> Result<Option<String>> {
        let owner = query(&["-Qqo", path.as_str()], "No package owns")?;
        Ok(owner.map(|owner| owner.trim().to_owned()))
    }

    fn is_installed(&self, name: &str) -> Result<bool> {
        Ok(query(&["-Q", name], "was not found")?.is_some())
    }
}

/// Query the local pacman database. Returns pacman's output, or `None` if
/// it fails with `not_found` in its error message.
fn query(args: &[&str], not_found: &str) -> Result<Option<String>> {
    let output = Command::new("pacman")
        .args(args)
        .output()
        .context("running pacman")?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        return Ok(Some(String::from_utf8(output.stdout)?));
    }
    if stderr.contains(not_found) {
        return Ok(None);
    }
    bail!(
        "pacman {} failed with {}: {}",
        args.join(" "),
        output.status,
        stderr.trim()
    )
}

#[cfg(test)]
mod tests {
    use indoc::indoc;

    use super::*;
    use crate::testutil::rootfs;

    #[test]
    fn reads_db_path_from_pacman_conf() -> Result<()> {
        let root = rootfs()?;
        root.create_dir("etc")?;
        root.write(
            "etc/pacman.conf",
            indoc! {"
                [options]
                # DBPath = /commented/out
                DBPath      = /usr/lib/sysimage/pacman/
            "},
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
    fn moves_database_under_usr_and_adjusts_pacman_conf() -> Result<()> {
        let root = rootfs()?;
        root.create_dir("etc")?;
        root.write(
            "etc/pacman.conf",
            indoc! {"
                [options]
                #DBPath      = /var/lib/pacman/
                HoldPkg     = pacman glibc
            "},
        )?;
        root.create_dir_all("var/lib/pacman/local/pacman-7.1.0-2")?;
        root.create_dir_all("var/lib/pacman/sync")?;
        root.write("var/lib/pacman/local/ALPM_DB_VERSION", b"9")?;
        root.write(
            "var/lib/pacman/local/pacman-7.1.0-2/desc",
            indoc! {b"
                %NAME%
                pacman
            "},
        )?;
        root.write("var/lib/pacman/sync/core.db", b"index")?;

        Arch.move_package_database(&root)?;

        assert!(!root.exists("var/lib/pacman"));
        assert!(root.exists("usr/lib/sysimage/pacman/local/ALPM_DB_VERSION"));
        assert!(root.exists("usr/lib/sysimage/pacman/local/pacman-7.1.0-2/desc"));
        assert!(root.exists("usr/lib/sysimage/pacman/sync/core.db"));
        assert_eq!(
            root.read_to_string("etc/pacman.conf")?,
            indoc! {"
                [options]
                DBPath = /usr/lib/sysimage/pacman/
                HoldPkg     = pacman glibc
            "}
        );
        assert_eq!(db_path(&root)?, USR_DB_PATH);
        Ok(())
    }

    #[test]
    fn keeps_database_already_outside_var() -> Result<()> {
        let root = rootfs()?;
        root.create_dir("etc")?;
        let conf = indoc! {"
            [options]
            DBPath = /usr/lib/pacman/
        "};
        root.write("etc/pacman.conf", conf)?;
        root.create_dir_all("usr/lib/pacman/local")?;

        Arch.move_package_database(&root)?;

        assert!(root.exists("usr/lib/pacman/local"));
        assert_eq!(root.read_to_string("etc/pacman.conf")?, conf);
        Ok(())
    }

    #[test]
    fn fails_without_database() -> Result<()> {
        let root = rootfs()?;
        root.create_dir("etc")?;
        root.write(
            "etc/pacman.conf",
            indoc! {"
                [options]
                #DBPath      = /var/lib/pacman/
            "},
        )?;
        let err = format!("{:#}", Arch.move_package_database(&root).unwrap_err());
        assert!(err.contains("moving /var/lib/pacman"), "{err}");
        Ok(())
    }

    #[test]
    fn removes_indexes_and_keeps_local_database() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("var/lib/pacman/sync")?;
        root.create_dir_all("var/lib/pacman/local/pacman-7.0.0-1")?;
        root.write("var/lib/pacman/sync/core.db", b"index")?;
        root.write("var/lib/pacman/sync/extra.db", b"index")?;
        root.write("var/lib/pacman/local/ALPM_DB_VERSION", b"9")?;
        root.write(
            "var/lib/pacman/local/pacman-7.0.0-1/desc",
            indoc! {b"
                %NAME%
                pacman
            "},
        )?;

        Arch.remove_repository_indexes(&root)?;

        assert!(root.is_dir("var/lib/pacman/sync"));
        assert_eq!(root.read_dir("var/lib/pacman/sync")?.count(), 0);
        assert!(root.exists("var/lib/pacman/local/ALPM_DB_VERSION"));
        assert!(root.exists("var/lib/pacman/local/pacman-7.0.0-1/desc"));
        Ok(())
    }

    #[test]
    fn tolerates_missing_sync_directory() -> Result<()> {
        let root = rootfs()?;
        Arch.remove_repository_indexes(&root)?;
        Ok(())
    }
}
