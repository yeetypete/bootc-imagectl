//! Fedora and derivatives.

use std::collections::BTreeSet;
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use cap_std_ext::camino::Utf8Path;
use cap_std_ext::cap_std::fs_utf8::Dir;

use super::{Distro, PackageName};

/// rpm's database, which Fedora already keeps under /usr and links to from
/// /var/lib/rpm.
const DATABASE: &str = "usr/lib/sysimage/rpm";

#[derive(Debug)]
pub(super) struct Fedora;

impl Distro for Fedora {
    fn name(&self) -> &'static str {
        "fedora"
    }

    /// rpm and dnf already keep their state under /usr, so only check that
    /// the database is there.
    fn relocate_package_state(&self, root: &Dir) -> Result<()> {
        ensure!(root.is_dir(DATABASE), "no rpm database at /{DATABASE}");
        Ok(())
    }

    fn package_owning(&self, path: &Utf8Path) -> Result<Option<PackageName>> {
        let Some(output) = query(&["-qf", "--qf", "%{NAME}\\n", path.as_str()])? else {
            return Ok(None);
        };
        owner_in(&output, path)
    }

    fn is_installed(&self, name: &PackageName) -> Result<bool> {
        Ok(query(&["-q", name.as_str()])?.is_some())
    }
}

/// The package that owns `path`, from `rpm -qf` output with one name per
/// line.
fn owner_in(output: &str, path: &Utf8Path) -> Result<Option<PackageName>> {
    // Each architecture of a multilib package prints its own line.
    let names: BTreeSet<&str> = output.lines().collect();
    let mut names = names.into_iter();
    match (names.next(), names.next()) {
        (Some(name), None) => name.parse().map(Some),
        (None, _) => Ok(None),
        (Some(_), Some(_)) => bail!("multiple packages own {path}: {}", output.trim()),
    }
}

/// Query rpm's database. Returns rpm's output, or `None` if the database has
/// no such path or package.
fn query(args: &[&str]) -> Result<Option<String>> {
    let output = Command::new("rpm")
        .args(args)
        .output()
        .context("running rpm")?;
    let stdout = String::from_utf8(output.stdout)?;
    if output.status.success() {
        return Ok(Some(stdout));
    }
    // rpm reports both on stdout.
    if stdout.contains("is not owned by any package") || stdout.contains("is not installed") {
        return Ok(None);
    }
    bail!(
        "rpm {} failed with {}: {}",
        args.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::rootfs;

    #[test]
    fn finds_owner_in_rpm_output() -> Result<()> {
        let path = Utf8Path::new("/usr/lib/sysusers.d/foo.conf");
        let owner = |output: &str| owner_in(output, path);
        let name = |name: &str| name.parse::<PackageName>().map(Some);

        assert_eq!(owner("foo\n")?, name("foo")?);
        assert_eq!(owner("libfoo\nlibfoo\n")?, name("libfoo")?);
        assert_eq!(owner("")?, None);

        let err = owner("foo\nbar\n").unwrap_err();
        assert!(err.to_string().contains("multiple packages own"), "{err}");
        Ok(())
    }

    #[test]
    fn relocates_package_state() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all(DATABASE)?;
        Fedora.relocate_package_state(&root)?;

        // A second run, e.g. in a derived build, leaves the database alone.
        Fedora.relocate_package_state(&root)?;
        assert!(root.is_dir(DATABASE));
        Ok(())
    }

    #[test]
    fn fails_without_database() -> Result<()> {
        let root = rootfs()?;
        let err = Fedora.relocate_package_state(&root).unwrap_err();
        assert!(err.to_string().contains("/usr/lib/sysimage/rpm"), "{err}");
        Ok(())
    }
}
