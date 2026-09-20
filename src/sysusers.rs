//! Read and write sysusers.d(5) configuration files.
//!
//! systemd-sysusers creates system users and groups from `*.conf` files in
//! the sysusers.d directories. `finalize` reads the same files to learn
//! which package specifies each account, and reads and writes the image's
//! sysusers lock file (see `lockfile`), which uses the same syntax.
//!
//! One line specifies one entry:
//!
//! ```text
//! #Type Name     ID             GECOS                 Home directory Shell
//! u!    httpd    404            "HTTP User"
//! u     _authd   /usr/bin/authd "Authorization user"
//! u     postgres -              "Postgresql Database" /var/lib/pgsql /usr/libexec/postgresdb
//! g     input    -              -
//! m     _authd   input
//! r     -        500-900
//! ```

use std::collections::BTreeMap;
use std::fmt::{self, Write};
use std::ops::RangeInclusive;
use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};
use cap_std_ext::camino::{Utf8Path, Utf8PathBuf};
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::CapStdExtDirExtUtf8;

use word::WHITESPACE;

pub(crate) mod lockfile;
mod parse;
mod word;

/// The directories systemd-sysusers reads, relative to the rootfs, in order
/// of precedence. A file in an earlier directory overrides a file of the same
/// name in a later one.
const CONFIG_DIRS: &[&str] = &[
    "etc/sysusers.d",
    "run/sysusers.d",
    "usr/local/lib/sysusers.d",
    "usr/lib/sysusers.d",
];

/// The longest user or group name systemd accepts.
const MAX_NAME_LEN: usize = 31;

/// IDs with a special meaning that systemd refuses to allocate: the 16-bit
/// and 32-bit `(uid_t) -1`.
const PLACEHOLDER_IDS: [u32; 2] = [65535, u32::MAX];

/// A user or group name systemd accepts: a letter or `_` followed by
/// letters, digits, `_` and `-`, at most 31 characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name(String);

impl Name {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Name {
    type Err = anyhow::Error;

    fn from_str(name: &str) -> Result<Self> {
        let mut chars = name.chars();
        let first = chars.next().context("empty user or group name")?;
        ensure!(
            first.is_ascii_alphabetic() || first == '_',
            "the name {name:?} does not start with a letter or _"
        );
        if let Some(c) = chars.find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))) {
            bail!("the name {name:?} contains {c:?}, only letters, digits, _ and - are allowed");
        }
        ensure!(
            name.len() <= MAX_NAME_LEN,
            "the name {name:?} is longer than {MAX_NAME_LEN} characters"
        );
        Ok(Self(name.to_owned()))
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The ID field of a `u` or `g` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Id {
    /// `-`: systemd-sysusers allocates a free ID when it creates the account.
    Automatic,
    /// A fixed UID or GID.
    Fixed(u32),
    /// An absolute path. The account takes the UID or GID of the path's
    /// owner or group.
    FromPath(Utf8PathBuf),
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Automatic => f.write_str("-"),
            Self::Fixed(id) => write!(f, "{id}"),
            Self::FromPath(path) => write!(f, "{}", Quoted(path.as_str())),
        }
    }
}

impl FromStr for Id {
    type Err = anyhow::Error;

    fn from_str(field: &str) -> Result<Self> {
        Ok(match field {
            "-" => Self::Automatic,
            path if path.starts_with('/') => Self::FromPath(path.into()),
            id => Self::Fixed(parse_id(id)?),
        })
    }
}

/// The primary group of a user, from the `UID:GID` or `UID:groupname` form
/// of the ID field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrimaryGroup {
    Gid(u32),
    Name(Name),
}

impl fmt::Display for PrimaryGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gid(gid) => write!(f, "{gid}"),
            Self::Name(name) => write!(f, "{name}"),
        }
    }
}

impl FromStr for PrimaryGroup {
    type Err = anyhow::Error;

    fn from_str(group: &str) -> Result<Self> {
        Ok(match group.parse() {
            Ok(name) => Self::Name(name),
            // A name never starts with a digit.
            Err(_) => Self::Gid(
                parse_id(group)
                    .with_context(|| format!("{group:?} is neither a GID nor a group name"))?,
            ),
        })
    }
}

/// A `u` line: a system user, and a group of the same name unless
/// `primary_group` names another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub name: Name,
    pub uid: Id,
    /// The primary group. `None` means the group with the user's name.
    pub primary_group: Option<PrimaryGroup>,
    /// The GECOS field, a short description of the account.
    pub gecos: Option<String>,
    /// The home directory. systemd-sysusers uses `/` if unset.
    pub home: Option<Utf8PathBuf>,
    /// The login shell. systemd-sysusers uses the nologin shell if unset.
    pub shell: Option<Utf8PathBuf>,
    /// `u!`: the account is fully locked, not just without a password.
    pub locked: bool,
}

/// A `g` line: a system group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub name: Name,
    pub gid: Id,
}

/// An `m` line: `user` is a member of `group`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Membership {
    pub user: Name,
    pub group: Name,
}

/// One line of a sysusers.d file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    User(User),
    Group(Group),
    Membership(Membership),
    /// An `r` line: a range of UIDs and GIDs systemd-sysusers allocates
    /// from.
    Range(RangeInclusive<u32>),
}

impl Entry {
    /// The type column of the entry.
    fn kind(&self) -> Kind {
        match self {
            Self::User(user) => Kind::User {
                locked: user.locked,
            },
            Self::Group(_) => Kind::Group,
            Self::Membership(_) => Kind::Membership,
            Self::Range(_) => Kind::Range,
        }
    }
}

/// The type column of a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    User { locked: bool },
    Group,
    Membership,
    Range,
}

impl FromStr for Kind {
    type Err = anyhow::Error;

    fn from_str(kind: &str) -> Result<Self> {
        Ok(match kind {
            "u" => Self::User { locked: false },
            "u!" => Self::User { locked: true },
            "g" => Self::Group,
            "m" => Self::Membership,
            "r" => Self::Range,
            "g!" | "m!" | "r!" => bail!("the ! modifier applies to u lines only"),
            kind => bail!("unknown type {kind:?}"),
        })
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::User { locked: false } => "u",
            Self::User { locked: true } => "u!",
            Self::Group => "g",
            Self::Membership => "m",
            Self::Range => "r",
        })
    }
}

/// A free-form field as written in a sysusers.d line. A field that is empty
/// or contains whitespace, quotes or backslashes is enclosed in double
/// quotes, with quotes and backslashes escaped. `%` is converted to `%%`,
/// since systemd expands specifiers.
struct Quoted<'a>(&'a str);

impl fmt::Display for Quoted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let field = self.0;
        let needs_quotes = field.is_empty()
            || field
                .chars()
                .any(|c| WHITESPACE.contains(&c) || matches!(c, '"' | '\'' | '\\'));
        if needs_quotes {
            f.write_char('"')?;
        }
        for c in field.chars() {
            match c {
                '"' | '\\' => f.write_char('\\')?,
                '%' => f.write_char('%')?,
                _ => {}
            }
            f.write_char(c)?;
        }
        if needs_quotes {
            f.write_char('"')?;
        }
        Ok(())
    }
}

impl fmt::Display for Entry {
    /// The entry as a sysusers.d line. Unset fields are written as `-`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User(user) => {
                write!(f, "{} {} {}", self.kind(), user.name, user.uid)?;
                if let Some(group) = &user.primary_group {
                    write!(f, ":{group}")?;
                }
                let optional = [
                    user.gecos.as_deref(),
                    user.home.as_deref().map(Utf8Path::as_str),
                    user.shell.as_deref().map(Utf8Path::as_str),
                ];
                for field in optional {
                    match field {
                        Some(field) => write!(f, " {}", Quoted(field))?,
                        None => f.write_str(" -")?,
                    }
                }
                Ok(())
            }
            Self::Group(group) => write!(f, "{} {} {}", self.kind(), group.name, group.gid),
            Self::Membership(membership) => {
                write!(
                    f,
                    "{} {} {}",
                    self.kind(),
                    membership.user,
                    membership.group
                )
            }
            Self::Range(range) => write!(f, "{} - {}-{}", self.kind(), range.start(), range.end()),
        }
    }
}

/// A sysusers.d file in the rootfs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFile {
    /// The path relative to the rootfs, e.g. `usr/lib/sysusers.d/basic.conf`.
    pub path: Utf8PathBuf,
    pub entries: Vec<Entry>,
}

/// Parse a UID or GID.
fn parse_id(field: &str) -> Result<u32> {
    let is_plain_decimal =
        field.bytes().all(|b| b.is_ascii_digit()) && (field == "0" || !field.starts_with('0'));
    let id: u32 = is_plain_decimal
        .then(|| field.parse().ok())
        .flatten()
        .with_context(|| format!("{field:?} is not a UID or GID"))?;
    ensure!(
        !PLACEHOLDER_IDS.contains(&id),
        "{id} is not a valid UID or GID"
    );
    Ok(id)
}

/// The non-empty lines of a file, trimmed and numbered from 1. Comments
/// start with `#`.
fn lines(content: &str) -> impl Iterator<Item = (usize, &str)> {
    content
        .lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line.trim_matches(WHITESPACE)))
        .filter(|(_, line)| !line.is_empty())
}

/// Parse the entries of a sysusers.d file. Empty lines and lines starting
/// with `#` are skipped.
///
/// # Errors
///
/// Fails on the first malformed line, naming its number.
pub(crate) fn parse(content: &str) -> Result<Vec<Entry>> {
    lines(content)
        .filter(|(_, line)| !line.starts_with('#'))
        .map(|(number, line)| line.parse().with_context(|| format!("line {number}")))
        .collect()
}

/// Whether `name` is a file systemd-sysusers reads from a sysusers.d
/// directory: a `.conf` file that is not hidden.
pub(crate) fn is_config_file_name(name: &str) -> bool {
    !name.starts_with('.') && name.strip_suffix(".conf").is_some()
}

/// Read every sysusers.d file in the rootfs, in the same order systemd-sysusers
/// applies them: sorted by file name across all of [`CONFIG_DIRS`]. A file overrides
/// files of the same name later directories in [`CONFIG_DIRS`], and a symlink to
/// `/dev/null` or an empty file hides them without contributing entries.
///
/// # Errors
///
/// Fails on a filesystem error, a file name that is not UTF-8, an entry
/// that is not a regular file, or a malformed file. The error names the
/// file.
pub(crate) fn read_all(root: &Dir) -> Result<Vec<ConfigFile>> {
    // File name to path, or `None` for a masked name.
    let mut files: BTreeMap<String, Option<Utf8PathBuf>> = BTreeMap::new();
    for dir in CONFIG_DIRS {
        let Some(confs) = root.open_dir_optional(dir)? else {
            continue;
        };
        for entry in confs.entries()? {
            let name = entry?
                .file_name()
                .with_context(|| format!("reading /{dir}"))?;
            if !is_config_file_name(&name) || files.contains_key(&name) {
                continue;
            }
            let path = Utf8Path::new(dir).join(&name);
            // Checked before following the link, which may not resolve in
            // the rootfs.
            let masks = confs
                .read_link_contents(&name)
                .is_ok_and(|target| target == "/dev/null");
            if masks {
                files.insert(name, None);
                continue;
            }
            let meta = confs
                .metadata(&name)
                .with_context(|| format!("reading /{path}"))?;
            ensure!(meta.is_file(), "/{path} is not a regular file");
            files.insert(name, (meta.len() > 0).then_some(path));
        }
    }
    files
        .into_values()
        .flatten()
        .map(|path| {
            let content = root
                .read_to_string(&path)
                .with_context(|| format!("reading /{path}"))?;
            let entries = parse(&content).with_context(|| format!("parsing /{path}"))?;
            Ok(ConfigFile { path, entries })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use indoc::indoc;

    use super::*;
    use crate::testutil::rootfs;

    fn name(name: &str) -> Name {
        Name(name.into())
    }

    #[test]
    fn validates_names() {
        for name in ["a", "_x", "avahi", "systemd-network", "a1", "Ab_c-1"] {
            assert!(name.parse::<Name>().is_ok(), "{name}");
        }
        for name in ["", "1a", "-a", "a b", "a:b", "a.b", "ä", "machine$"] {
            assert!(name.parse::<Name>().is_err(), "{name}");
        }
        assert!("a".repeat(31).parse::<Name>().is_ok());
        assert!("a".repeat(32).parse::<Name>().is_err());
    }

    #[test]
    fn displays_entries_and_round_trips() -> Result<()> {
        let content = indoc! {r#"
            u! httpd 404 "HTTP User" - -
            u _authd /usr/bin/authd "Authorization user" - -
            u spaced "/my path" "100%% sure" "/home/my dir" -
            u escaped - "it's \"quoted\" \\ ok" - -
            u postgres - "Postgresql Database" /var/lib/pgsql /usr/libexec/postgresdb
            u avahi 900:avahi - / /usr/bin/nologin
            u nogecos 10:20 - - -
            g input -
            g wheel 998
            m _authd input
            r - 500-900
            r - 42-42
        "#};
        let entries = parse(content)?;
        let displayed: String = entries
            .iter()
            .map(|entry| entry.to_string() + "\n")
            .collect();
        assert_eq!(displayed, content);
        assert_eq!(parse(&displayed)?, entries);
        Ok(())
    }

    #[test]
    fn reads_all_directories_in_file_name_order() -> Result<()> {
        let root = rootfs()?;
        for dir in CONFIG_DIRS {
            root.create_dir_all(dir)?;
        }
        root.write("usr/lib/sysusers.d/basic.conf", "g wheel 998\n")?;
        root.write("usr/lib/sysusers.d/zz.conf", "g zz -\n")?;
        root.write("usr/local/lib/sysusers.d/local.conf", "g local -\n")?;
        root.write("run/sysusers.d/aa.conf", "g aa -\n")?;
        root.write("etc/sysusers.d/README", "g not-a-conf -\n")?;
        root.write("etc/sysusers.d/.hidden.conf", "g hidden -\n")?;
        // A file in etc hides the one in usr/lib.
        root.write("usr/lib/sysusers.d/pkg.conf", "g vendor -\n")?;
        root.write("etc/sysusers.d/pkg.conf", "g admin -\n")?;
        // A symlink to /dev/null and an empty file mask files.
        root.write("usr/lib/sysusers.d/masked.conf", "g masked -\n")?;
        root.symlink_contents("/dev/null", "etc/sysusers.d/masked.conf")?;
        root.write("usr/lib/sysusers.d/empty.conf", "g empty -\n")?;
        root.write("run/sysusers.d/empty.conf", "")?;

        let files = read_all(&root)?;

        let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "run/sysusers.d/aa.conf",
                "usr/lib/sysusers.d/basic.conf",
                "usr/local/lib/sysusers.d/local.conf",
                "etc/sysusers.d/pkg.conf",
                "usr/lib/sysusers.d/zz.conf",
            ]
        );
        assert_eq!(
            files[3].entries,
            [Entry::Group(Group {
                name: name("admin"),
                gid: Id::Automatic,
            })],
            "the file in etc hides the one in usr/lib"
        );
        Ok(())
    }

    #[test]
    fn rejects_directories_and_dangling_symlinks() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("etc/sysusers.d")?;
        root.create_dir("etc/sysusers.d/dir.conf")?;
        let err = format!("{:#}", read_all(&root).unwrap_err());
        assert!(
            err.contains("/etc/sysusers.d/dir.conf is not a regular file"),
            "{err}"
        );

        root.remove_dir("etc/sysusers.d/dir.conf")?;
        root.symlink("nowhere", "etc/sysusers.d/dangling.conf")?;
        let err = format!("{:#}", read_all(&root).unwrap_err());
        assert!(
            err.contains("reading /etc/sysusers.d/dangling.conf"),
            "{err}"
        );
        Ok(())
    }

    #[test]
    fn names_malformed_file() -> Result<()> {
        let root = rootfs()?;
        assert_eq!(read_all(&root)?, [], "no sysusers.d directory yet");
        root.create_dir_all("usr/lib/sysusers.d")?;
        root.write("usr/lib/sysusers.d/bad.conf", "u\n")?;
        let err = format!("{:#}", read_all(&root).unwrap_err());
        assert!(err.contains("/usr/lib/sysusers.d/bad.conf"), "{err}");
        assert!(err.contains("line 1"), "{err}");
        Ok(())
    }
}
