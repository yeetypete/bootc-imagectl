//! Read and write the image's sysusers lock file.
//!
//! The lock file is a sysusers.d file that specifies every user and group
//! the build creates with a fixed UID and GID. It is a sequence of blocks. A
//! `# package: <name>` comment starts a block and names the package that
//! created the accounts on the lines that follow. `# package: -` marks
//! accounts no package created that are kept on purpose. A
//! `# removed: <name>` comment starts the block of a removed package, whose
//! accounts stay so that no other account takes their IDs.
//!
//! ```text
//! # package: avahi-daemon
//! g avahi 900
//! u avahi 900 "Avahi mDNS/DNS-SD daemon" / /usr/bin/nologin
//! # package: systemd
//! g systemd-journal 981
//! u systemd-network 976 "systemd Network Management" / /usr/bin/nologin
//! m daemon adm
//! # removed: geoclue-2.0
//! u geoclue 107 - /var/lib/geoclue /usr/sbin/nologin
//! ```
//!
//! systemd-sysusers reads the lock file like any other sysusers.d file.

use std::fmt;
use std::io::Read;
use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};
use cap_std_ext::camino::Utf8Path;
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::CapStdExtDirExtUtf8;

use super::word::WHITESPACE;
use super::{Entry, IdSource, Name, is_config_file_name, lines};
use crate::distro::PackageName;

/// The comment that starts a block, up to the package name.
const PACKAGE_HEADER: &str = "package:";

/// The comment that starts the block of a removed package, up to its name.
const REMOVED_HEADER: &str = "removed:";

/// The sysusers.d directory the lock file must live in. This
/// guarantees it is tracked in the image, unlike /etc.
const LOCK_DIR: &str = "usr/lib/sysusers.d";

/// The package a block attributes its accounts to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Package {
    /// `# package: <name>`: the package that created the accounts.
    Named(PackageName),
    /// `# package: -`: no package created the accounts, and they are kept
    /// on purpose.
    Unowned,
    /// `# package:` with no name. `finalize` prints this for accounts it
    /// cannot attribute, and the author fills the name in. A lock file
    /// with such a block does not parse.
    Unknown,
}

/// The accounts one package created.
impl FromStr for Package {
    type Err = anyhow::Error;

    /// Parse the text after `# package:`.
    fn from_str(package: &str) -> Result<Self> {
        Ok(match package.trim_matches(WHITESPACE) {
            "" => bail!(
                "the '# {PACKAGE_HEADER}' header names no package. Name the package that created the accounts, or - if none did"
            ),
            "-" => Self::Unowned,
            name => Self::Named(name.parse()?),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub package: Package,
    /// Whether the package was removed. Its accounts stay, locked, and keep
    /// their IDs, but belong to no group but their own.
    pub removed: bool,
    pub entries: Vec<Entry>,
}

impl Block {
    /// The block that keeps the accounts of this one once its package is
    /// removed: the same users and groups, without memberships.
    #[must_use]
    pub fn to_removed(&self) -> Self {
        Self {
            package: self.package.clone(),
            removed: true,
            entries: self
                .entries
                .iter()
                .filter(|entry| !matches!(entry, Entry::Membership(_)))
                .cloned()
                .collect(),
        }
    }
}

/// A parsed sysusers lock file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LockFile {
    pub blocks: Vec<Block>,
}

/// Parse an entry line. Every lock file entry must fix its ID.
fn parse_entry(line: &str) -> Result<Entry> {
    let entry: Entry = line.parse()?;
    match &entry {
        Entry::User(user) => ensure_fixed(&user.name, &user.uid)?,
        Entry::Group(group) => ensure_fixed(&group.name, &group.gid)?,
        Entry::Membership(_) => {}
        Entry::Range(_) => bail!("the lock file takes no r lines"),
    }
    Ok(entry)
}

/// Fail unless the lock file gives the account `name` a fixed ID.
fn ensure_fixed<T: fmt::Display>(name: &Name, id: &IdSource<T>) -> Result<()> {
    ensure!(
        matches!(id, IdSource::Fixed(_)),
        "the lock file must give {name} a fixed UID or GID, got {id}"
    );
    Ok(())
}

/// Parse one non-empty line into `blocks`.
fn parse_line(blocks: &mut Vec<Block>, line: &str) -> Result<()> {
    if let Some(comment) = line.strip_prefix('#') {
        let header = comment.trim_matches(WHITESPACE);
        for (prefix, removed) in [(PACKAGE_HEADER, false), (REMOVED_HEADER, true)] {
            if let Some(package) = header.strip_prefix(prefix) {
                blocks.push(Block {
                    package: package.parse()?,
                    removed,
                    entries: Vec::new(),
                });
            }
        }
        return Ok(());
    }
    let entry = parse_entry(line)?;
    let Some(block) = blocks.last_mut() else {
        bail!("entry before the first '# {PACKAGE_HEADER}' header");
    };
    ensure!(
        !(block.removed && matches!(entry, Entry::Membership(_))),
        "the block of a removed package takes no m lines"
    );
    block.entries.push(entry);
    Ok(())
}

impl FromStr for LockFile {
    type Err = anyhow::Error;

    /// Parse a lock file. Fails on a malformed line, a `# package:` header
    /// with no name, an entry before the first header, or an entry without
    /// a fixed ID. The error names the line.
    fn from_str(content: &str) -> Result<Self> {
        let mut blocks = Vec::new();
        for (number, line) in lines(content) {
            parse_line(&mut blocks, line).with_context(|| format!("line {number}"))?;
        }
        Ok(Self { blocks })
    }
}

impl LockFile {
    /// The users of removed packages.
    pub fn removed_users(&self) -> impl Iterator<Item = &Name> {
        self.blocks
            .iter()
            .filter(|block| block.removed)
            .flat_map(|block| &block.entries)
            .filter_map(|entry| match entry {
                Entry::User(user) => Some(&user.name),
                _ => None,
            })
    }

    /// Read the lock file at `path`, relative to the rootfs. `None` if it
    /// does not exist yet, e.g. on a first image build.
    ///
    /// # Errors
    ///
    /// Fails if the path is not a .conf file in /usr/lib/sysusers.d, or if
    /// the file cannot be read or parsed. The error names the file.
    pub fn read(root: &Dir, path: &Utf8Path) -> Result<Option<Self>> {
        ensure!(
            path.parent() == Some(Utf8Path::new(LOCK_DIR))
                && path.file_name().is_some_and(is_config_file_name),
            "the lock file must be a .conf file in /{LOCK_DIR}, got /{path}"
        );
        let Some(mut file) = root
            .open_optional(path)
            .with_context(|| format!("reading /{path}"))?
        else {
            return Ok(None);
        };
        let mut content = String::new();
        file.read_to_string(&mut content)
            .with_context(|| format!("reading /{path}"))?;
        content
            .parse()
            .map(Some)
            .with_context(|| format!("parsing /{path}"))
    }
}

impl fmt::Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let header = if self.removed {
            REMOVED_HEADER
        } else {
            PACKAGE_HEADER
        };
        match &self.package {
            Package::Named(name) => writeln!(f, "# {header} {name}")?,
            Package::Unowned => writeln!(f, "# {header} -")?,
            Package::Unknown => writeln!(f, "# {header}")?,
        }
        for entry in &self.entries {
            writeln!(f, "{entry}")?;
        }
        Ok(())
    }
}

impl fmt::Display for LockFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for block in &self.blocks {
            write!(f, "{block}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use indoc::indoc;

    use super::*;
    use crate::sysusers::{Group, Membership};
    use crate::testutil::{gid, rootfs};

    const EXAMPLE: &str = indoc! {r#"
        # package: avahi-daemon
        g avahi 900
        u avahi 900 "Avahi mDNS/DNS-SD daemon" / /usr/bin/nologin
        # package: systemd
        g systemd-journal 981
        u systemd-network 976 "systemd Network Management" / /usr/bin/nologin
        m daemon adm
        # package: -
        g utmp 5
    "#};

    const REMOVED: &str = indoc! {r#"
        # package: avahi-daemon
        g avahi 900
        u avahi 900 "Avahi mDNS/DNS-SD daemon" / /usr/bin/nologin
        # removed: geoclue-2.0
        g geoclue-extra 110
        u geoclue 107 - /var/lib/geoclue /usr/sbin/nologin
    "#};

    const LOCK_PATH: &str = "usr/lib/sysusers.d/00-bootc-imagectl.lock.conf";

    fn lock_path() -> &'static Utf8Path {
        Utf8Path::new(LOCK_PATH)
    }

    fn packages(lock: &LockFile) -> Vec<(&Package, usize)> {
        lock.blocks
            .iter()
            .map(|block| (&block.package, block.entries.len()))
            .collect()
    }

    #[test]
    fn parses_blocks() -> Result<()> {
        let lock: LockFile = EXAMPLE.parse()?;
        assert_eq!(
            packages(&lock),
            [
                (&Package::Named("avahi-daemon".parse()?), 2),
                (&Package::Named("systemd".parse()?), 3),
                (&Package::Unowned, 1),
            ]
        );
        assert_eq!(
            lock.blocks[1].entries[2],
            Entry::Membership(Membership {
                user: "daemon".parse()?,
                group: "adm".parse()?,
            })
        );
        assert_eq!(
            lock.blocks
                .iter()
                .map(|block| block.entries.len())
                .sum::<usize>(),
            6
        );
        Ok(())
    }

    #[test]
    fn round_trips() -> Result<()> {
        let lock: LockFile = EXAMPLE.parse()?;
        assert_eq!(lock.to_string(), EXAMPLE);
        assert_eq!(lock.to_string().parse::<LockFile>()?, lock);
        assert_eq!("".parse::<LockFile>()?, LockFile::default());
        assert_eq!(LockFile::default().to_string(), "");
        Ok(())
    }

    #[test]
    fn parses_and_round_trips_removed_blocks() -> Result<()> {
        let lock: LockFile = REMOVED.parse()?;
        assert!(!lock.blocks[0].removed);
        assert!(lock.blocks[1].removed);
        assert_eq!(lock.to_string(), REMOVED);
        let removed: Vec<&str> = lock.removed_users().map(Name::as_str).collect();
        assert_eq!(removed, ["geoclue"]);
        Ok(())
    }

    #[test]
    fn rejects_memberships_in_removed_blocks() {
        let err = format!(
            "{:#}",
            "# removed: x\ng x 1\nm y x\n"
                .parse::<LockFile>()
                .unwrap_err()
        );
        assert!(err.starts_with("line 3: "), "{err}");
        assert!(err.contains("takes no m lines"), "{err}");
    }

    #[test]
    fn removed_block_drops_memberships() -> Result<()> {
        let lock: LockFile = EXAMPLE.parse()?;
        assert_eq!(
            lock.blocks[1].to_removed().to_string(),
            indoc! {r#"
                # removed: systemd
                g systemd-journal 981
                u systemd-network 976 "systemd Network Management" / /usr/bin/nologin
            "#}
        );
        Ok(())
    }

    #[test]
    fn displays_unknown_package() -> Result<()> {
        let block = Block {
            package: Package::Unknown,
            removed: false,
            entries: vec![Entry::Group(Group {
                name: "x".parse()?,
                gid: IdSource::Fixed(gid(1)),
            })],
        };
        assert_eq!(block.to_string(), "# package:\ng x 1\n");
        let err = format!("{:#}", block.to_string().parse::<LockFile>().unwrap_err());
        assert!(err.contains("line 1"), "{err}");
        assert!(err.contains("names no package"), "{err}");
        Ok(())
    }

    #[test]
    fn ignores_other_comments_and_tolerates_loose_headers() -> Result<()> {
        let lock: LockFile = indoc! {"
            #Type Name ID
            #package:x

            g x 1
            # a comment in the block
            #  package:   y
        "}
        .parse()?;
        assert_eq!(
            packages(&lock),
            [
                (&Package::Named("x".parse()?), 1),
                (&Package::Named("y".parse()?), 0),
            ]
        );
        Ok(())
    }

    #[test]
    fn rejects_malformed_files_with_their_line() {
        let cases = [
            ("g x 1\n", 1, "before the first"),
            ("# package: a b\ng x 1\n", 1, "contains whitespace"),
            ("# package: x\ng x 1\nbogus\n", 3, "missing name field"),
            ("# package: x\nu foo -\n", 2, "fixed UID or GID, got -"),
            (
                "# package: x\ng foo /usr/bin/x\n",
                2,
                "fixed UID or GID, got /usr/bin/x",
            ),
            ("# package: x\nr - 500-900\n", 2, "takes no r lines"),
        ];
        for (content, line, expected) in cases {
            let err = format!("{:#}", content.parse::<LockFile>().unwrap_err());
            assert!(
                err.starts_with(&format!("line {line}: ")),
                "{content:?}: {err}"
            );
            assert!(err.contains(expected), "{content:?}: {err}");
        }
    }

    #[test]
    fn reads_lock_file_if_present() -> Result<()> {
        let root = rootfs()?;
        assert_eq!(LockFile::read(&root, lock_path())?, None);

        root.create_dir_all("usr/lib/sysusers.d")?;
        root.write(LOCK_PATH, "# package: x\ng x 1\n")?;
        let lock = LockFile::read(&root, lock_path())?.expect("the lock file exists");
        assert_eq!(
            lock.blocks[0].entries,
            [Entry::Group(Group {
                name: "x".parse()?,
                gid: IdSource::Fixed(gid(1)),
            })]
        );

        root.write(LOCK_PATH, "g x 1\n")?;
        let err = format!("{:#}", LockFile::read(&root, lock_path()).unwrap_err());
        assert!(err.contains(LOCK_PATH), "{err}");
        Ok(())
    }

    #[test]
    fn rejects_path_outside_usr_lib_sysusers_d() -> Result<()> {
        let root = rootfs()?;
        for path in ["etc/sysusers.d/lock.conf", "usr/lib/sysusers.d/lock"] {
            let err = format!(
                "{:#}",
                LockFile::read(&root, Utf8Path::new(path)).unwrap_err()
            );
            assert!(err.contains("must be a .conf file in"), "{path}: {err}");
        }
        Ok(())
    }
}
