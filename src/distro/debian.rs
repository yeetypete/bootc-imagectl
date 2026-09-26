//! Debian and derivatives.

use std::collections::BTreeSet;
use std::process::Command;
use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};
use cap_std_ext::camino::Utf8Path;
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::CapStdExtDirExtUtf8;
use tracing::debug;

use super::{Distro, PackageName, SYSIMAGE};
use crate::fs::move_dir;

const DATABASE: &str = "var/lib/dpkg";

/// The package state under /var and where it moves under [`SYSIMAGE`].
const STATE: [(&str, &str); 6] = [
    (DATABASE, "usr/lib/sysimage/dpkg"),
    ("var/cache/debconf", "usr/lib/sysimage/debconf"),
    ("var/lib/ucf", "usr/lib/sysimage/ucf"),
    ("var/lib/pam", "usr/lib/sysimage/pam"),
    (
        "var/lib/systemd/deb-systemd-helper-enabled",
        "usr/lib/sysimage/deb-systemd-helper-enabled",
    ),
    (
        "var/lib/systemd/deb-systemd-user-helper-enabled",
        "usr/lib/sysimage/deb-systemd-user-helper-enabled",
    ),
];

/// Directories apt refuses to run without, which finalize recreates in the
/// emptied /var for derived builds.
const APT_DIRS: [&str; 1] = ["var/log/apt"];

/// A package's state in the dpkg database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageState {
    NotInstalled,
    ConfigFiles,
    HalfInstalled,
    Unpacked,
    HalfConfigured,
    TriggersAwaiting,
    TriggersPending,
    Installed,
}

impl PackageState {
    fn is_installed(self) -> bool {
        matches!(
            self,
            Self::Installed | Self::TriggersAwaiting | Self::TriggersPending
        )
    }
}

impl FromStr for PackageState {
    type Err = anyhow::Error;

    fn from_str(state: &str) -> Result<Self> {
        Ok(match state {
            "not-installed" => Self::NotInstalled,
            "config-files" => Self::ConfigFiles,
            "half-installed" => Self::HalfInstalled,
            "unpacked" => Self::Unpacked,
            "half-configured" => Self::HalfConfigured,
            "triggers-awaiting" => Self::TriggersAwaiting,
            "triggers-pending" => Self::TriggersPending,
            "installed" => Self::Installed,
            _ => bail!("{state:?} is not a dpkg package state"),
        })
    }
}

/// The symlink target from `path` to `target`, as a relative path.
fn link_target(path: &str, target: &str) -> String {
    let depth = path.matches('/').count();
    format!("{}{target}", "../".repeat(depth))
}

/// Symlink the state directories to their homes under /usr, replacing any
/// link already there.
fn link_state(root: &Dir) -> Result<()> {
    for (path, target) in STATE {
        let (dir, _) = path.rsplit_once('/').context("a state path has a parent")?;
        root.create_dir_all(dir)?;
        root.remove_file_optional(path)?;
        root.symlink(link_target(path, target), path)
            .with_context(|| format!("linking /{path}"))?;
    }
    Ok(())
}

/// Debian and derivatives, which use dpkg and apt.
#[derive(Debug)]
pub(super) struct Debian;

impl Distro for Debian {
    fn name(&self) -> &'static str {
        "debian"
    }

    /// Stage the kernel where bootc looks for it. The kernel package installs
    /// it under /boot.
    fn stage_kernel(&self, root: &Dir, kver: &str) -> Result<()> {
        let staged = format!("usr/lib/modules/{kver}/vmlinuz");
        let installed = format!("boot/vmlinuz-{kver}");
        if root.exists(&staged) || !root.exists(&installed) {
            return Ok(());
        }
        debug!("staging /{installed} at /{staged}");
        root.copy(&installed, root, &staged)
            .with_context(|| format!("copying /{installed} to /{staged}"))?;
        Ok(())
    }

    /// Remove ssl-cert's snakeoil certificate, which ssl-cert.service
    /// regenerates when its key is missing.
    fn remove_machine_identity(&self, root: &Dir) -> Result<()> {
        if let Some(certs) = root.open_dir_optional("etc/ssl/certs")? {
            for entry in certs.entries()? {
                let name = entry?.file_name()?;
                let target = certs.read_link_contents(&name).ok();
                if target.as_deref() == Some(Utf8Path::new("ssl-cert-snakeoil.pem")) {
                    certs
                        .remove_file(&name)
                        .with_context(|| format!("removing /etc/ssl/certs/{name}"))?;
                }
            }
        }
        root.remove_file_optional("etc/ssl/certs/ssl-cert-snakeoil.pem")?;
        root.remove_file_optional("etc/ssl/private/ssl-cert-snakeoil.key")?;
        // Only present on Ubuntu's rockcraft-built base images, no-op otherwise.
        root.remove_all_optional(".rock")?;
        Ok(())
    }

    /// Move dpkg's database and the maintainer scripts' state under /usr.
    /// symlink their old paths to the new locations.
    fn relocate_package_state(&self, root: &Dir) -> Result<()> {
        ensure!(root.exists(DATABASE), "no dpkg database at /{DATABASE}");
        root.create_dir_all(SYSIMAGE)?;
        for (path, target) in STATE {
            let moved = root
                .symlink_metadata_optional(path)?
                .is_some_and(|meta| meta.is_symlink());
            if moved {
                continue;
            }
            if root.is_dir(path) {
                move_dir(root, path, target)
                    .with_context(|| format!("moving /{path} to /{target}"))?;
            } else {
                root.create_dir_all(target)?;
            }
        }
        link_state(root)
    }

    /// Recreate the directories required by apt to exist.
    fn restore_package_state(&self, root: &Dir) -> Result<()> {
        for dir in APT_DIRS {
            root.create_dir_all(dir)?;
        }
        Ok(())
    }

    fn package_owning(&self, path: &Utf8Path) -> Result<Option<PackageName>> {
        let Some(output) = query(&["-S", path.as_str()], "no path found matching")? else {
            return Ok(None);
        };
        owner_in(&output, path)
    }

    fn is_installed(&self, name: &PackageName) -> Result<bool> {
        // One line for each instance of a `Multi-Arch: same` package.
        let Some(states) = query(
            &["-W", "-f=${db:Status-Status}\\n", name.as_str()],
            "no packages found matching",
        )?
        else {
            return Ok(false);
        };
        let states = states
            .lines()
            .map(str::parse)
            .collect::<Result<Vec<PackageState>>>()?;
        Ok(states.iter().any(|state| state.is_installed()))
    }
}

/// The package that owns `path`, from `dpkg-query -S` output.
fn owner_in(output: &str, path: &Utf8Path) -> Result<Option<PackageName>> {
    let suffix = format!(": {path}");
    let mut owners = None;
    for line in output
        .lines()
        .filter(|line| !line.starts_with("diversion by "))
    {
        let found = line
            .strip_suffix(suffix.as_str())
            .with_context(|| format!("dpkg-query -S {path} printed {line:?}"))?;
        owners = Some(found);
    }
    let Some(owners) = owners else {
        return Ok(None);
    };
    let mut names = owners
        .split(", ")
        // Drop the `:arch` suffix of `Multi-Arch: same` and foreign packages.
        .map(|owner| owner.split_once(':').map_or(owner, |(name, _)| name))
        .collect::<BTreeSet<_>>()
        .into_iter();
    let (Some(name), None) = (names.next(), names.next()) else {
        bail!("multiple packages own {path}: {owners}");
    };
    name.parse().map(Some)
}

/// Query dpkg's database. Returns dpkg-query's output, or `None` if the
/// database has no such path or package.
fn query(args: &[&str], not_found: &str) -> Result<Option<String>> {
    let output = Command::new("dpkg-query")
        .args(args)
        .output()
        .context("running dpkg-query")?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        return Ok(Some(String::from_utf8(output.stdout)?));
    }
    if stderr.contains(not_found) {
        return Ok(None);
    }
    bail!(
        "dpkg-query {} failed with {}: {}",
        args.join(" "),
        output.status,
        stderr.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::rootfs;

    #[test]
    fn finds_owner_in_dpkg_query_output() -> Result<()> {
        let path = Utf8Path::new("/usr/lib/sysusers.d/foo.conf");
        let owner = |output: &str| owner_in(output, path);
        let name = |name: &str| name.parse::<PackageName>().map(Some);

        assert_eq!(owner("foo: /usr/lib/sysusers.d/foo.conf\n")?, name("foo")?);
        assert_eq!(
            owner("libfoo:amd64, libfoo:i386: /usr/lib/sysusers.d/foo.conf\n")?,
            name("libfoo")?
        );
        let diverted = indoc::indoc! {"
            diversion by bar from: /usr/lib/sysusers.d/foo.conf
            diversion by bar to: /usr/lib/sysusers.d/foo.conf.distrib
            foo: /usr/lib/sysusers.d/foo.conf
        "};
        assert_eq!(owner(diverted)?, name("foo")?);
        let only_diverted = "diversion by bar from: /usr/lib/sysusers.d/foo.conf\n";
        assert_eq!(owner(only_diverted)?, None);

        let err = owner("foo, bar: /usr/lib/sysusers.d/foo.conf\n").unwrap_err();
        assert!(err.to_string().contains("multiple packages own"), "{err}");
        assert!(owner("something else\n").is_err());
        Ok(())
    }

    #[test]
    fn parses_package_states() -> Result<()> {
        assert!("installed".parse::<PackageState>()?.is_installed());
        assert!("triggers-pending".parse::<PackageState>()?.is_installed());
        assert!(!"config-files".parse::<PackageState>()?.is_installed());
        assert!(!"unpacked".parse::<PackageState>()?.is_installed());
        let err = "removed".parse::<PackageState>().unwrap_err().to_string();
        assert!(err.contains("not a dpkg package state"), "{err}");
        Ok(())
    }

    #[test]
    fn relocates_package_state() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("var/lib/dpkg")?;
        root.write("var/lib/dpkg/status", "Package: bash\n")?;
        Debian.relocate_package_state(&root)?;
        assert_eq!(
            root.read_to_string("usr/lib/sysimage/dpkg/status")?,
            "Package: bash\n"
        );
        assert_eq!(
            root.read_link_contents("var/lib/dpkg")?,
            "../../usr/lib/sysimage/dpkg"
        );
        assert!(root.is_dir("usr/lib/sysimage/ucf"));
        assert_eq!(
            root.read_link_contents("var/lib/systemd/deb-systemd-helper-enabled")?,
            "../../../usr/lib/sysimage/deb-systemd-helper-enabled"
        );
        assert!(root.is_file("var/lib/dpkg/status"));

        // A second run, as in a derived build, leaves the moved state alone.
        Debian.relocate_package_state(&root)?;
        assert!(root.is_file("var/lib/dpkg/status"));
        Ok(())
    }

    #[test]
    fn restores_package_state() -> Result<()> {
        let root = rootfs()?;
        root.create_dir("var")?;
        Debian.restore_package_state(&root)?;
        assert!(root.is_dir("var/log/apt"));
        Ok(())
    }

    #[test]
    fn stages_kernel() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("boot")?;
        root.create_dir_all("usr/lib/modules/7.0.0-31-generic")?;
        root.write("boot/vmlinuz-7.0.0-31-generic", "kernel")?;
        Debian.stage_kernel(&root, "7.0.0-31-generic")?;
        assert_eq!(
            root.read_to_string("usr/lib/modules/7.0.0-31-generic/vmlinuz")?,
            "kernel"
        );
        // A kernel already staged, or none installed, is left alone.
        root.write("boot/vmlinuz-7.0.0-31-generic", "newer")?;
        Debian.stage_kernel(&root, "7.0.0-31-generic")?;
        assert_eq!(
            root.read_to_string("usr/lib/modules/7.0.0-31-generic/vmlinuz")?,
            "kernel"
        );
        Debian.stage_kernel(&root, "7.0.0-32-generic")?;
        Ok(())
    }

    #[test]
    fn removes_machine_identity() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("etc/ssl/certs")?;
        root.create_dir_all("etc/ssl/private")?;
        root.write("etc/ssl/certs/ssl-cert-snakeoil.pem", "cert")?;
        root.write("etc/ssl/private/ssl-cert-snakeoil.key", "key")?;
        root.symlink("ssl-cert-snakeoil.pem", "etc/ssl/certs/abcd1234.0")?;
        root.write("etc/ssl/certs/ca-certificates.crt", "keep")?;
        root.create_dir_all(".rock")?;
        Debian.remove_machine_identity(&root)?;
        assert_eq!(root.read_dir("etc/ssl/certs")?.count(), 1);
        assert!(!root.exists("etc/ssl/private/ssl-cert-snakeoil.key"));
        assert!(!root.exists(".rock"));
        Ok(())
    }
}
