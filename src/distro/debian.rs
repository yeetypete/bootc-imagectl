//! Debian and derivatives.

use std::collections::BTreeSet;
use std::fmt::{self, Write as _};
use std::io::Read;
use std::process::Command;
use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};
use cap_std_ext::camino::{Utf8Path, Utf8PathBuf};
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::{CapStdExtDirExt, CapStdExtDirExtUtf8};
use flate2::bufread::GzDecoder;
use tracing::debug;

use uzers::{Groups, Users, UsersCache};

use super::{Components, Distro, PackageName, SYSIMAGE};
use crate::fs::move_dir;
use crate::id::{Gid, Uid};
use crate::passwd::{Entry as _, Group, Passwd};

const DATABASE: &str = "var/lib/dpkg";

/// dpkg's statoverride file, which overrides the ownership and mode of paths,
/// once [`Debian::relocate_package_state`] moved the database under /usr.
const STATOVERRIDE: &str = "usr/lib/sysimage/dpkg/statoverride";

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
/// emptied /var for package installs after finalize.
const APT_DIRS: [&str; 1] = ["var/log/apt"];

/// The `dpkg-query -W` format of each package and its source package, e.g.
/// `libc6:amd64\tglibc`.
const COMPONENT_FORMAT: &str = "-f=${binary:Package}\\t${source:Package}\\n";

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

/// The user or group of a stat override, by name or by number with a leading
/// `#`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StatOwner<T> {
    Name(String),
    Id(T),
}

impl<T> StatOwner<T> {
    /// Refer to the owner by the ID `id_of` returns for its name, if any.
    fn refer_by_id(&mut self, id_of: impl FnOnce(&str) -> Result<Option<T>>) -> Result<()> {
        if let Self::Name(name) = self
            && let Some(id) = id_of(name)?
        {
            *self = Self::Id(id);
        }
        Ok(())
    }
}

impl<T: FromStr<Err = anyhow::Error>> FromStr for StatOwner<T> {
    type Err = anyhow::Error;

    fn from_str(field: &str) -> Result<Self> {
        match field.strip_prefix('#') {
            Some(id) => id.parse().map(Self::Id),
            None => Ok(Self::Name(field.to_owned())),
        }
    }
}

impl<T: fmt::Display> fmt::Display for StatOwner<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(name) => f.write_str(name),
            Self::Id(id) => write!(f, "#{id}"),
        }
    }
}

/// A stat override, one line of [`STATOVERRIDE`] in the form:
/// user, group, mode and path, e.g. `root ssl-cert 710 /etc/ssl/private`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StatOverride {
    user: StatOwner<Uid>,
    group: StatOwner<Gid>,
    mode: String,
    /// The rest of the line, as paths may contain spaces.
    path: Utf8PathBuf,
}

impl FromStr for StatOverride {
    type Err = anyhow::Error;

    fn from_str(line: &str) -> Result<Self> {
        let [user, group, mode, path] = line.splitn(4, ' ').collect::<Vec<_>>()[..] else {
            bail!("malformed stat override {line:?}");
        };
        Ok(Self {
            user: user.parse()?,
            group: group.parse()?,
            mode: mode.to_owned(),
            path: path.into(),
        })
    }
}

impl fmt::Display for StatOverride {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            user,
            group,
            mode,
            path,
        } = self;
        write!(f, "{user} {group} {mode} {path}")
    }
}

/// Write the stat override owners missing from /etc/passwd and /etc/group,
/// which finalize moved to userdb, as `#ID`. dpkg 1.22.16 and later look the
/// owners up in those files instead of through NSS, and abort on any they
/// don't find. They accept and keep `#ID`.
fn refer_to_moved_owners_by_id(root: &Dir, nss: &(impl Users + Groups)) -> Result<()> {
    let Some(content) = root
        .as_cap_std()
        .read_to_string_optional(STATOVERRIDE)
        .with_context(|| format!("reading /{STATOVERRIDE}"))?
    else {
        return Ok(());
    };
    let users = Passwd::read_all(root)?;
    let groups = Group::read_all(root)?;
    let mut numbered = String::new();
    for line in content.lines() {
        let mut entry: StatOverride = line
            .parse()
            .with_context(|| format!("parsing /{STATOVERRIDE}"))?;
        entry.user.refer_by_id(|name| {
            if users.iter().any(|user| user.name.as_str() == name) {
                return Ok(None);
            }
            nss.get_user_by_name(name)
                .and_then(|user| Uid::new(user.uid()))
                .map(Some)
                .with_context(|| format!("the stat override user {name} does not resolve"))
        })?;
        entry.group.refer_by_id(|name| {
            if groups.iter().any(|group| group.name.as_str() == name) {
                return Ok(None);
            }
            nss.get_group_by_name(name)
                .and_then(|group| Gid::new(group.gid()))
                .map(Some)
                .with_context(|| format!("the stat override group {name} does not resolve"))
        })?;
        writeln!(numbered, "{entry}")?;
    }
    if numbered != content {
        debug!("referring to the owners in /{STATOVERRIDE} by ID");
        let perms = root.metadata(STATOVERRIDE)?.permissions();
        root.atomic_write_with_perms(STATOVERRIDE, numbered, perms)
            .with_context(|| format!("writing /{STATOVERRIDE}"))?;
    }
    Ok(())
}

/// The symlink target from `path` to `target`, as a relative path.
fn link_target(path: &str, target: &str) -> String {
    let depth = path.matches('/').count();
    format!("{}{target}", "../".repeat(depth))
}

/// Symlink the state directories to their homes under /usr, replacing any
/// symlink already there.
fn link_state(root: &Dir) -> Result<()> {
    for (path, target) in STATE {
        let (dir, _) = path.rsplit_once('/').context("a state path has a parent")?;
        root.create_dir_all(dir)?;
        root.remove_file_optional(path)?;
        root.symlink(link_target(path, target), path)
            .with_context(|| format!("creating symlink /{path}"))?;
    }
    Ok(())
}

/// Debian and derivatives, which use dpkg and apt.
#[derive(Debug)]
pub(super) struct Debian {
    name: &'static str,
    default_user_groups: &'static [&'static str],
}

pub(super) const DEBIAN: Debian = Debian {
    name: "debian",
    default_user_groups: &[
        "audio",
        "cdrom",
        "dip",
        "floppy",
        "video",
        "plugdev",
        "netdev",
        "scanner",
        "bluetooth",
        "debian-tor",
        "lpadmin",
    ],
};

pub(super) const UBUNTU: Debian = Debian {
    name: "ubuntu",
    default_user_groups: &[
        "adm",
        "cdrom",
        "dip",
        "lpadmin",
        "plugdev",
        "sambashare",
        "debian-tor",
        "users",
        "lxd",
    ],
};

impl Distro for Debian {
    fn name(&self) -> &'static str {
        self.name
    }

    fn admin_group(&self) -> &'static str {
        "sudo"
    }

    fn default_user_groups(&self) -> &'static [&'static str] {
        self.default_user_groups
    }

    /// Stage the kernel where bootc looks for it. The kernel package installs
    /// it under /boot.
    ///
    /// NOTE: Ubuntu 24.04's arm64 kernel packages install a gzip-compressed EFI
    /// image, which systemd-stub cannot boot from a UKI. It is staged
    /// decompressed, which keeps the signature of the EFI image inside.
    fn stage_kernel(&self, root: &Dir, kver: &str) -> Result<()> {
        let staged = format!("usr/lib/modules/{kver}/vmlinuz");
        let installed = format!("boot/vmlinuz-{kver}");
        if root.exists(&staged) || !root.exists(&installed) {
            return Ok(());
        }
        let contents = root
            .read(&installed)
            .with_context(|| format!("reading /{installed}"))?;
        let mut decoder = GzDecoder::new(contents.as_slice());
        let kernel = if decoder.header().is_some() {
            debug!("decompressing /{installed}");
            let mut kernel = Vec::new();
            decoder
                .read_to_end(&mut kernel)
                .with_context(|| format!("decompressing /{installed}"))?;
            kernel
        } else {
            contents
        };
        debug!("staging /{installed} at /{staged}");
        root.write(&staged, kernel)
            .with_context(|| format!("writing /{staged}"))?;
        let permissions = root.metadata(&installed)?.permissions();
        root.set_permissions(&staged, permissions)
            .with_context(|| format!("setting the mode of /{staged}"))?;
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

    /// Recreate the directories required by apt to exist, and refer to the
    /// stat override owners finalize moved to userdb by ID.
    fn restore_package_state(&self, root: &Dir) -> Result<()> {
        for dir in APT_DIRS {
            root.create_dir_all(dir)?;
        }
        refer_to_moved_owners_by_id(root, &UsersCache::new())
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

    /// Group each package's files by its source package.
    fn package_components(&self, root: &Dir) -> Result<Components> {
        let output = query(&["-W", COMPONENT_FORMAT], "no packages found")?.unwrap_or_default();
        let mut components = Components::new();
        for line in output.lines() {
            let (package, source) = line
                .split_once('\t')
                .with_context(|| format!("dpkg-query printed {line:?}"))?;
            let list = format!("{DATABASE}/info/{package}.list");
            let listed = root
                .read_to_string(&list)
                .with_context(|| format!("reading /{list}"))?;
            components
                .entry(source.parse()?)
                .or_default()
                .extend(listed.lines().map(Utf8PathBuf::from));
        }
        Ok(components)
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
    use std::io::Write;

    use cap_std_ext::cap_std::fs_utf8::{Permissions, PermissionsExt};
    use flate2::Compression;
    use flate2::write::GzEncoder;

    use super::*;
    use cap_std_ext::cap_tempfile::utf8::TempDir;
    use uzers::mock::MockUsers;
    use uzers::{Group as MockGroup, User};

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

    /// A rootfs whose /etc has root, and whose stat overrides name root, the
    /// moved messagebus and ssl-cert, and an ID.
    fn stat_override_rootfs() -> Result<TempDir> {
        let root = rootfs()?;
        root.create_dir_all("etc")?;
        root.write("etc/passwd", "root:x:0:0:root:/root:/bin/bash\n")?;
        root.write("etc/group", "root:x:0:\n")?;
        root.create_dir_all("usr/lib/sysimage/dpkg")?;
        root.write(
            STATOVERRIDE,
            "root ssl-cert 710 /etc/ssl/private\n\
             messagebus #5 4754 /usr/lib/dbus helper\n",
        )?;
        Ok(root)
    }

    #[test]
    fn refers_to_moved_stat_override_owners_by_id() -> Result<()> {
        let root = stat_override_rootfs()?;
        let mut nss = MockUsers::with_current_uid(0);
        nss.add_user(User::new(991, "messagebus", 991));
        nss.add_group(MockGroup::new(102, "ssl-cert"));
        refer_to_moved_owners_by_id(&root, &nss)?;
        assert_eq!(
            root.read_to_string(STATOVERRIDE)?,
            "root #102 710 /etc/ssl/private\n#991 #5 4754 /usr/lib/dbus helper\n"
        );
        Ok(())
    }

    #[test]
    fn rejects_unresolved_stat_override_owners() -> Result<()> {
        let root = stat_override_rootfs()?;
        let nss = MockUsers::with_current_uid(0);
        assert!(refer_to_moved_owners_by_id(&root, &nss).is_err());
        Ok(())
    }

    #[test]
    fn rejects_malformed_stat_overrides() {
        assert!("root ssl-cert 710".parse::<StatOverride>().is_err());
        assert!(
            "root #x 710 /etc/ssl/private"
                .parse::<StatOverride>()
                .is_err()
        );
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
        DEBIAN.relocate_package_state(&root)?;
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

        // A second run leaves the moved state alone.
        DEBIAN.relocate_package_state(&root)?;
        assert!(root.is_file("var/lib/dpkg/status"));
        Ok(())
    }

    #[test]
    fn fails_without_database() -> Result<()> {
        let root = rootfs()?;
        let err = DEBIAN.relocate_package_state(&root).unwrap_err();
        assert!(err.to_string().contains("/var/lib/dpkg"), "{err}");
        Ok(())
    }

    #[test]
    fn restores_package_state() -> Result<()> {
        let root = rootfs()?;
        root.create_dir("var")?;
        DEBIAN.restore_package_state(&root)?;
        assert!(root.is_dir("var/log/apt"));
        Ok(())
    }

    #[test]
    fn stages_kernel() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("boot")?;
        root.create_dir_all("usr/lib/modules/7.0.0-31-generic")?;
        root.write("boot/vmlinuz-7.0.0-31-generic", "kernel")?;
        DEBIAN.stage_kernel(&root, "7.0.0-31-generic")?;
        assert_eq!(
            root.read_to_string("usr/lib/modules/7.0.0-31-generic/vmlinuz")?,
            "kernel"
        );
        // A kernel already staged, or none installed, is left alone.
        root.write("boot/vmlinuz-7.0.0-31-generic", "newer")?;
        DEBIAN.stage_kernel(&root, "7.0.0-31-generic")?;
        assert_eq!(
            root.read_to_string("usr/lib/modules/7.0.0-31-generic/vmlinuz")?,
            "kernel"
        );
        DEBIAN.stage_kernel(&root, "7.0.0-32-generic")?;
        Ok(())
    }

    #[test]
    fn stages_compressed_kernel_decompressed() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("boot")?;
        root.create_dir_all("usr/lib/modules/7.0.0-31-generic")?;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"MZ kernel")?;
        root.write("boot/vmlinuz-7.0.0-31-generic", encoder.finish()?)?;
        root.set_permissions(
            "boot/vmlinuz-7.0.0-31-generic",
            Permissions::from_mode(0o600),
        )?;
        DEBIAN.stage_kernel(&root, "7.0.0-31-generic")?;
        let staged = "usr/lib/modules/7.0.0-31-generic/vmlinuz";
        assert_eq!(root.read(staged)?, b"MZ kernel");
        assert_eq!(root.metadata(staged)?.permissions().mode() & 0o777, 0o600);
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
        DEBIAN.remove_machine_identity(&root)?;
        assert_eq!(root.read_dir("etc/ssl/certs")?.count(), 1);
        assert!(!root.exists("etc/ssl/private/ssl-cert-snakeoil.key"));
        assert!(!root.exists(".rock"));
        Ok(())
    }
}
