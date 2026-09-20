//! Fix the UIDs and GIDs of the users and groups the build created with the
//! sysusers lock file, and move the users and their group memberships from
//! /etc into /usr/lib/userdb, where an upgrade replaces them.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

use anyhow::{Context, Result, bail};
use cap_std_ext::camino::{Utf8Path, Utf8PathBuf};
use cap_std_ext::cap_std::fs_utf8::Dir;
use tracing::{debug, info};

use crate::distro::Distro;
use crate::passwd::{self, Entry as _, Passwd};
use crate::sysusers::lockfile::{Block, LockFile, Package};
use crate::sysusers::{self, Entry, Id, Index, Membership, PrimaryGroup, User};

/// The UID and GID of root.
const ROOT_ID: u32 = 0;

/// The UID and GID of nobody, the kernel's overflow account.
const NOBODY_ID: u32 = 65534;

/// Whether the UID or GID is intrinsic, the category systemd assigns to
/// root and nobody which are always fixed on every system.
fn is_intrinsic(id: u32) -> bool {
    matches!(id, ROOT_ID | NOBODY_ID)
}

/// What the account checks look at.
struct Accounts<'a> {
    distro: &'a dyn Distro,
    /// The lock file's path, relative to the rootfs.
    lock_path: &'a Utf8Path,
    /// The lock file, if it exists.
    lock: Option<&'a LockFile>,
    users: &'a [Passwd],
    groups: &'a [passwd::Group],
    index: Index<'a>,
}

impl Accounts<'_> {
    /// The users the lock file must cover.
    fn users_to_lock(&self) -> impl Iterator<Item = &Passwd> {
        self.users.iter().filter(|user| !is_intrinsic(user.uid))
    }

    /// The groups the lock file must cover.
    fn groups_to_lock(&self) -> impl Iterator<Item = &passwd::Group> {
        self.groups.iter().filter(|group| !is_intrinsic(group.gid))
    }
}

/// The status of a user or group in /etc relative to the sysusers.d files.
enum LockStatus<'a> {
    /// An entry fixes the ID to the one in /etc.
    Locked,
    /// No entry, or an entry without a fixed ID.
    Missing,
    /// An entry fixes the ID to another one.
    Drifted { path: &'a Utf8Path, specified: u32 },
}

/// The status of an account whose ID in /etc is `id`, given the file and ID
/// that configure it, if any.
fn lock_status<'a>(configured: Option<(&'a Utf8Path, &Id)>, id: u32) -> LockStatus<'a> {
    match configured {
        Some((path, Id::Fixed(specified))) => {
            if *specified == id {
                LockStatus::Locked
            } else {
                LockStatus::Drifted {
                    path,
                    specified: *specified,
                }
            }
        }
        _ => LockStatus::Missing,
    }
}

/// Checks that fail the build, in the order they run. A check reports
/// what it found and a fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Check {
    /// Every user, group and membership is in the sysusers lock file.
    Lock,
    /// Every fixed UID and GID is the one the build allocated.
    Drift,
    /// Every package the lock file names is installed.
    Stale,
}

impl Check {
    const ALL: [Self; 3] = [Self::Lock, Self::Drift, Self::Stale];

    /// Issues the check found, one finding per line. Empty if it passed.
    fn run(self, accounts: &Accounts<'_>) -> Result<Vec<String>> {
        match self {
            Self::Lock => missing_lines(accounts),
            Self::Drift => Ok(drifted_ids(accounts)),
            Self::Stale => stale_packages(accounts),
        }
    }

    /// What the developer does about the findings.
    fn help(self, accounts: &Accounts<'_>) -> String {
        let lock = accounts.lock_path;
        match self {
            Self::Lock if accounts.lock.is_some() => format!(
                "add these lines to /{lock}, fill in each blank '# package:' header, and rebuild"
            ),
            Self::Lock => format!(
                "create /{lock} with these lines, fill in each blank '# package:' header, and copy it into the image before installing packages"
            ),
            Self::Drift => format!(
                "systemd-sysusers keeps the ID of an existing account, so apply /{lock} before any package creates these accounts, or correct their IDs in it"
            ),
            Self::Stale => format!(
                "remove the package's block from /{lock}, or change its header to '# package: -' if its accounts are kept on purpose"
            ),
        }
    }
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Lock => "lock",
            Self::Drift => "drift",
            Self::Stale => "stale",
        })
    }
}

fn non_empty(field: &str) -> Option<String> {
    (!field.is_empty()).then(|| field.to_owned())
}

/// The lines for the accounts and memberships no entry fixes.
fn missing(accounts: &Accounts<'_>) -> Vec<Entry> {
    let index = &accounts.index;
    let mut missing = Vec::new();
    for group in accounts.groups_to_lock() {
        if let LockStatus::Missing = lock_status(index.gid(&group.name), group.gid) {
            missing.push(Entry::Group(sysusers::Group {
                name: group.name.clone(),
                gid: Id::Fixed(group.gid),
            }));
        }
    }
    for user in accounts.users_to_lock() {
        let LockStatus::Missing = lock_status(index.uid(&user.name), user.uid) else {
            continue;
        };
        // A `u` line without a primary group creates a group with the user's
        // name and UID.
        let own_group = user.gid == user.uid
            && accounts
                .groups
                .iter()
                .any(|group| group.name == user.name && group.gid == user.gid);
        missing.push(Entry::User(User {
            name: user.name.clone(),
            uid: Id::Fixed(user.uid),
            primary_group: (!own_group).then_some(PrimaryGroup::Gid(user.gid)),
            gecos: non_empty(&user.gecos),
            home: non_empty(user.home.as_str()).map(Utf8PathBuf::from),
            shell: non_empty(user.shell.as_str()).map(Utf8PathBuf::from),
            locked: index
                .users
                .get(&user.name)
                .is_some_and(|user| user.entry.locked),
        }));
    }
    // Memberships in every group count, root and nobody included. Only
    // intrinsic members are skipped.
    for group in accounts.groups {
        for member in &group.members {
            let intrinsic = accounts
                .users
                .iter()
                .any(|user| user.name == *member && is_intrinsic(user.uid));
            if intrinsic || index.memberships.contains(&(member, &group.name)) {
                continue;
            }
            missing.push(Entry::Membership(Membership {
                user: member.clone(),
                group: group.name.clone(),
            }));
        }
    }
    missing
}

/// The package that owns the sysusers.d file at `path`, relative to the
/// rootfs, or [`Package::Unknown`] if none does.
fn owner(distro: &dyn Distro, path: &Utf8Path) -> Result<Package> {
    let absolute = Utf8Path::new("/").join(path);
    let package = distro
        .package_owning(&absolute)
        .with_context(|| format!("finding the package that owns {absolute}"))?;
    Ok(package.map_or(Package::Unknown, Package::Named))
}

/// Group the missing lines into lock file blocks by the package that owns
/// the sysusers.d file configuring each account. An account no package
/// configures goes into a block with a blank header, for the author to fill
/// in.
fn group_by_package(missing: Vec<Entry>, accounts: &Accounts<'_>) -> Result<LockFile> {
    let index = &accounts.index;
    let paths: BTreeSet<&Utf8Path> = missing
        .iter()
        .filter_map(|entry| index.file_of(entry))
        .collect();
    let owners: BTreeMap<&Utf8Path, Package> = paths
        .into_iter()
        .map(|path| Ok((path, owner(accounts.distro, path)?)))
        .collect::<Result<_>>()?;
    let mut blocks: BTreeMap<Package, Vec<Entry>> = BTreeMap::new();
    for entry in missing {
        let package = index
            .file_of(&entry)
            .map_or(Package::Unknown, |path| owners[path].clone());
        blocks.entry(package).or_default().push(entry);
    }
    Ok(LockFile {
        blocks: blocks
            .into_iter()
            .map(|(package, entries)| Block { package, entries })
            .collect(),
    })
}

/// The lock check: the lines the lock file lacks, as one finding.
fn missing_lines(accounts: &Accounts<'_>) -> Result<Vec<String>> {
    let missing = missing(accounts);
    if missing.is_empty() {
        return Ok(Vec::new());
    }
    let lines = group_by_package(missing, accounts)?;
    Ok(vec![lines.to_string()])
}

/// The drift check: the accounts whose fixed ID is not the one in /etc.
fn drifted_ids(accounts: &Accounts<'_>) -> Vec<String> {
    let index = &accounts.index;
    let mut findings = Vec::new();
    for group in accounts.groups_to_lock() {
        if let LockStatus::Drifted { path, specified } =
            lock_status(index.gid(&group.name), group.gid)
        {
            findings.push(format!(
                "group {} has GID {} but /{path} specifies {specified}",
                group.name, group.gid
            ));
        }
    }
    for user in accounts.users_to_lock() {
        if let LockStatus::Drifted { path, specified } =
            lock_status(index.uid(&user.name), user.uid)
        {
            findings.push(format!(
                "user {} has UID {} but /{path} specifies {specified}",
                user.name, user.uid
            ));
        }
    }
    findings
}

/// The stale check: the packages the lock file names that are not
/// installed. The lock file recreates their accounts on every build, so a
/// removed package's accounts would otherwise stay in the image.
fn stale_packages(accounts: &Accounts<'_>) -> Result<Vec<String>> {
    let mut findings = Vec::new();
    for block in accounts.lock.iter().flat_map(|lock| &lock.blocks) {
        if let Package::Named(name) = &block.package
            && !accounts.distro.is_installed(name)?
        {
            findings.push(format!(
                "the package {name} is not installed but has a block in the lock file"
            ));
        }
    }
    Ok(findings)
}

/// Run `checks` and fail if any has findings. Every failed check is
/// reported, with its findings and help.
fn run(checks: &[Check], accounts: &Accounts<'_>) -> Result<()> {
    let mut report = String::new();
    let mut failed = 0;
    for check in checks {
        let findings = check
            .run(accounts)
            .with_context(|| format!("running the {check} check"))?;
        if findings.is_empty() {
            debug!("the {check} check passed");
            continue;
        }
        failed += 1;
        writeln!(report, "\nThe {check} check failed:")?;
        for finding in findings {
            writeln!(report, "{}", finding.trim_end())?;
        }
        writeln!(report, "help: {}", check.help(accounts))?;
    }
    if failed > 0 {
        bail!("{failed} account checks failed\n{report}");
    }
    Ok(())
}

/// Check the users and groups in /etc against the sysusers.d files and the
/// lock file at `lock`, an absolute path in the image.
///
/// # Errors
///
/// Fails if a check fails, reporting every failed check, or if the files
/// cannot be read.
pub(super) fn check_accounts(root: &Dir, distro: &dyn Distro, lock: &Utf8Path) -> Result<()> {
    let lock_path = lock.strip_prefix("/").unwrap_or(lock);
    let users = Passwd::read_all(root)?;
    let groups = passwd::Group::read_all(root)?;
    let files = sysusers::read_all(root)?;
    let lock = LockFile::read(root, lock_path)?;
    let accounts = Accounts {
        distro,
        lock_path,
        lock: lock.as_ref(),
        users: &users,
        groups: &groups,
        index: files.iter().collect(),
    };
    run(&Check::ALL, &accounts)?;
    info!("every user and group is in the sysusers lock file /{lock_path}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use indoc::indoc;

    use super::*;
    use crate::sysusers::ConfigFile;
    use crate::testutil::TestDistro;

    const USERS: &str = indoc! {"
        root:x:0:0:root:/root:/bin/bash
        nobody:x:65534:65534::/:/usr/bin/nologin
        avahi:x:900:900:Avahi mDNS/DNS-SD daemon:/:/usr/bin/nologin
        http:x:33:34::/srv/http:/usr/bin/nologin
        tss:x:971:971:tss user for tpm2:/:
    "};

    const GROUPS: &str = indoc! {"
        root:x:0:root
        nobody:x:65534:
        avahi:x:900:
        http:x:34:
        tss:x:971:
        adm:x:4:tss,avahi,root
    "};

    /// The test rootfs's /etc, the lock file parsed from `lock`, and one
    /// more sysusers.d file with `entries`.
    struct Fixture {
        distro: TestDistro,
        users: Vec<Passwd>,
        groups: Vec<passwd::Group>,
        lock: Option<LockFile>,
        files: Vec<ConfigFile>,
    }

    impl Fixture {
        fn new(distro: TestDistro, lock: Option<&str>, entries: &str) -> Result<Self> {
            let lock: Option<LockFile> = lock.map(str::parse).transpose()?;
            let mut files = Vec::new();
            if let Some(lock) = &lock {
                files.push(ConfigFile {
                    path: "usr/lib/sysusers.d/00-bootc-imagectl.conf".into(),
                    entries: sysusers::parse(&lock.to_string())?,
                });
            }
            files.push(ConfigFile {
                path: "usr/lib/sysusers.d/pkg.conf".into(),
                entries: sysusers::parse(entries)?,
            });
            Ok(Self {
                distro,
                users: USERS.lines().map(str::parse).collect::<Result<_>>()?,
                groups: GROUPS.lines().map(str::parse).collect::<Result<_>>()?,
                lock,
                files,
            })
        }

        fn accounts(&self) -> Accounts<'_> {
            Accounts {
                distro: &self.distro,
                lock_path: Utf8Path::new("usr/lib/sysusers.d/00-bootc-imagectl.conf"),
                lock: self.lock.as_ref(),
                users: &self.users,
                groups: &self.groups,
                index: self.files.iter().collect(),
            }
        }
    }

    #[test]
    fn lock_check_prints_missing_lines_in_lock_file() -> Result<()> {
        let distro = TestDistro {
            owner: Some("pkg"),
            ..TestDistro::default()
        };
        let fixture = Fixture::new(
            distro,
            None,
            indoc! {"
                g avahi 900
                u avahi 900
                u! http -:34
                m avahi adm
            "},
        )?;
        let findings = Check::Lock.run(&fixture.accounts())?;
        assert_eq!(
            findings,
            [indoc! {"
                # package: pkg
                u! http 33:34 - /srv/http /usr/bin/nologin
                # package:
                g http 34
                g tss 971
                g adm 4
                u tss 971 \"tss user for tpm2\" / -
                m tss adm
            "}]
        );
        Ok(())
    }

    #[test]
    fn accounts_without_a_sysusers_file_get_a_blank_header() -> Result<()> {
        let fixture = Fixture::new(TestDistro::default(), None, "u avahi -\n")?;
        let findings = Check::Lock.run(&fixture.accounts())?;
        assert!(
            findings[0].starts_with("# package:\ng avahi 900\n"),
            "{findings:?}"
        );
        Ok(())
    }

    #[test]
    fn drift_check_lists_every_changed_id() -> Result<()> {
        let fixture = Fixture::new(TestDistro::default(), None, "u avahi 901\ng adm 5\n")?;
        assert_eq!(
            Check::Drift.run(&fixture.accounts())?,
            [
                "group avahi has GID 900 but /usr/lib/sysusers.d/pkg.conf specifies 901",
                "group adm has GID 4 but /usr/lib/sysusers.d/pkg.conf specifies 5",
                "user avahi has UID 900 but /usr/lib/sysusers.d/pkg.conf specifies 901",
            ]
        );
        Ok(())
    }

    const LOCK: &str = indoc! {"
        # package: avahi
        g avahi 900
        u avahi 900
        m avahi adm
        # package: apache
        g http 34
        u http 33:34
        # package: -
        g tss 971
        u tss 971
        g adm 4
        m tss adm
    "};

    #[test]
    fn stale_check_names_uninstalled_packages() -> Result<()> {
        let distro = TestDistro {
            not_installed: &["apache"],
            ..TestDistro::default()
        };
        let fixture = Fixture::new(distro, Some(LOCK), "")?;
        assert_eq!(
            Check::Stale.run(&fixture.accounts())?,
            ["the package apache is not installed but has a block in the lock file"]
        );
        Ok(())
    }

    #[test]
    fn reports_every_failed_check() -> Result<()> {
        let distro = TestDistro {
            not_installed: &["apache"],
            ..TestDistro::default()
        };
        let fixture = Fixture::new(distro, Some(LOCK), "")?;
        let err = format!("{:#}", run(&Check::ALL, &fixture.accounts()).unwrap_err());
        assert!(
            err.starts_with("1 account checks failed\n\nThe stale check failed:\n"),
            "{err}"
        );
        assert!(err.contains("help: remove the package's block"), "{err}");
        assert!(!err.contains("lock check"), "{err}");
        Ok(())
    }

    #[test]
    fn passes_a_complete_lock_file() -> Result<()> {
        let fixture = Fixture::new(TestDistro::default(), Some(LOCK), "")?;
        run(&Check::ALL, &fixture.accounts())
    }
}
