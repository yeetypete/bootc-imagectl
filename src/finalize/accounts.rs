//! Fix the UIDs and GIDs of the users and groups the build created with the
//! sysusers lock file, and move the users and their group memberships from
//! /etc into /usr/lib/userdb, where an upgrade replaces them.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};
use std::iter;
use std::ops::ControlFlow;

use anyhow::{Context, Result, bail};
use cap_std_ext::camino::{Utf8Path, Utf8PathBuf};
use cap_std_ext::cap_std::fs::MetadataExt;
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::{CapStdExtDirExt, WalkConfiguration};
use tracing::{debug, info};

use crate::distro::Distro;
use crate::login_defs::LoginDefs;
use crate::passwd::{self, Entry as _, Passwd, Shadow, is_intrinsic, non_empty};
use crate::sysusers::lockfile::{Block, LockFile, Package};
use crate::sysusers::{self, Entry, Id, Index, Membership, Name, PrimaryGroup, User};
use crate::userdb::{self, UserRecord};

/// The directories whose paths must be owned by accounts in the image,
/// relative to the rootfs.
const OWNED_DIRS: [&str; 2] = ["usr", "etc"];

/// What the account checks look at.
struct Accounts<'a> {
    root: &'a Dir,
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

    /// Whether a user or group in /etc has the ID.
    fn resolves(&self, owner: Owner) -> bool {
        match owner {
            Owner::Uid(uid) => self.users.iter().any(|user| user.uid == uid),
            Owner::Gid(gid) => self.groups.iter().any(|group| group.gid == gid),
        }
    }
}

/// The ID a path is owned by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Owner {
    Uid(u32),
    Gid(u32),
}

impl Owner {
    /// The kind of account that has the ID.
    fn account(self) -> &'static str {
        match self {
            Self::Uid(_) => "user",
            Self::Gid(_) => "group",
        }
    }
}

impl fmt::Display for Owner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Uid(uid) => write!(f, "UID {uid}"),
            Self::Gid(gid) => write!(f, "GID {gid}"),
        }
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
    /// Every path under /usr and /etc is owned by a user and group in /etc.
    Ownership,
    /// Every package the lock file names is installed.
    Stale,
}

impl Check {
    const ALL: [Self; 4] = [Self::Lock, Self::Drift, Self::Ownership, Self::Stale];

    /// Issues the check found, one finding per line. Empty if it passed.
    fn run(self, accounts: &Accounts<'_>) -> Result<Vec<String>> {
        match self {
            Self::Lock => missing_lines(accounts),
            Self::Drift => Ok(drifted_ids(accounts)),
            Self::Ownership => unresolved_owners(accounts),
            Self::Stale => stale_packages(accounts),
        }
    }

    /// What the developer does about the findings, if there is one thing
    /// to do.
    fn help(self, accounts: &Accounts<'_>) -> Option<String> {
        let lock = accounts.lock_path;
        let help = match self {
            Self::Lock if accounts.lock.is_some() => format!(
                "add these lines to /{lock}, fill in each blank '# package:' header, and rebuild"
            ),
            Self::Lock => format!(
                "create /{lock} with these lines, fill in each blank '# package:' header, and copy it into the image before installing packages"
            ),
            Self::Drift => format!(
                "systemd-sysusers keeps the ID of an existing account, so apply /{lock} before any package creates these accounts, or correct their IDs in it"
            ),
            Self::Ownership => return None,
            Self::Stale => format!(
                "remove the package's block from /{lock}, or change its header to '# package: -' if its accounts are kept on purpose"
            ),
        };
        Some(help)
    }
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Lock => "lock",
            Self::Drift => "drift",
            Self::Ownership => "ownership",
            Self::Stale => "stale",
        })
    }
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

/// Lock check: the lines the lock file lacks.
fn missing_lines(accounts: &Accounts<'_>) -> Result<Vec<String>> {
    let missing = missing(accounts);
    if missing.is_empty() {
        return Ok(Vec::new());
    }
    let lines = group_by_package(missing, accounts)?;
    Ok(vec![lines.to_string()])
}

/// Drift check: the accounts whose fixed ID is not the one in /etc.
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

/// The paths under /usr and /etc owned by an ID that has no account, by ID.
fn paths_without_account(
    accounts: &Accounts<'_>,
) -> Result<BTreeMap<Owner, BTreeSet<Utf8PathBuf>>> {
    let mut paths: BTreeMap<Owner, BTreeSet<Utf8PathBuf>> = BTreeMap::new();
    for dir in OWNED_DIRS {
        let base = Utf8Path::new("/").join(dir);
        let config = WalkConfiguration::default().path_base(base.as_std_path());
        accounts
            .root
            .open_dir(dir)?
            .as_cap_std()
            .walk(&config, |e| -> Result<ControlFlow<()>> {
                let meta = e.entry.metadata()?;
                for owner in [Owner::Uid(meta.uid()), Owner::Gid(meta.gid())] {
                    if accounts.resolves(owner) {
                        continue;
                    }
                    let path = Utf8Path::from_path(e.path)
                        .with_context(|| format!("{} is not UTF-8", e.path.display()))?;
                    paths.entry(owner).or_default().insert(path.to_owned());
                }
                Ok(ControlFlow::Continue(()))
            })
            .with_context(|| format!("scanning {base}"))?;
    }
    Ok(paths)
}

/// Ownership check: the IDs that own a path under /usr and /etc but have
/// no account. They are left over from a build stage or a removed package.
fn unresolved_owners(accounts: &Accounts<'_>) -> Result<Vec<String>> {
    let mut findings = Vec::new();
    for (owner, paths) in paths_without_account(accounts)? {
        let Some(path) = paths.first() else {
            continue;
        };
        let account = owner.account();
        findings.push(match paths.len() - 1 {
            0 => format!("{owner} owns {path} but matches no {account}"),
            more => format!("{owner} owns {path} and {more} more paths but matches no {account}"),
        });
    }
    Ok(findings)
}

/// Stale check: The packages the lock file names that are not installed. The
/// lock file recreates their accounts on every build, so a removed package's
/// accounts would otherwise stay in the image.
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
        if let Some(help) = check.help(accounts) {
            writeln!(report, "help: {help}")?;
        }
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
        root,
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

/// Write a user record for every user in /etc/passwd other than root and
/// nobody, which nss-systemd synthesizes itself, to the drop-in directory.
/// A user with a password hash in /etc/shadow gets a privileged record with
/// the hash, every other user is locked.
///
/// # Errors
///
/// Fails if the files cannot be read or written, or a UID is in no range of
/// login.defs.
pub(super) fn write_user_records(root: &Dir) -> Result<()> {
    let defs = LoginDefs::read(root)?;
    let users = Passwd::read_all(root)?;
    let shadows = Shadow::read_all(root)?;
    let shadow_of: BTreeMap<_, _> = shadows
        .iter()
        .map(|shadow| (&shadow.name, shadow))
        .collect();
    root.create_dir_all(userdb::DROPIN_DIR)
        .with_context(|| format!("creating /{}", userdb::DROPIN_DIR))?;
    let dir = root.open_dir(userdb::DROPIN_DIR)?;
    let mut written = 0;
    for user in users.iter().filter(|user| !is_intrinsic(user.uid)) {
        let shadow = shadow_of.get(&user.name).copied();
        let record = UserRecord::from_passwd(user, shadow, &defs)
            .with_context(|| format!("the user {}", user.name))?;
        record
            .write(&dir)
            .with_context(|| format!("writing the record of {}", user.name))?;
        debug!("wrote the user record of {}", user.name);
        written += 1;
    }
    info!("wrote {written} user records to /{}", userdb::DROPIN_DIR);
    Ok(())
}

/// Write a membership file for every user with a record and each group it
/// belongs to: its primary group, and every group in /etc/group that lists
/// it.
///
/// # Errors
///
/// Fails if the files cannot be read or written, or a user's primary GID
/// has no group.
pub(super) fn write_memberships(root: &Dir) -> Result<()> {
    let users = Passwd::read_all(root)?;
    let groups = passwd::Group::read_all(root)?;
    let dir = root.open_dir(userdb::DROPIN_DIR)?;
    let mut written = 0;
    for user in users.iter().filter(|user| !is_intrinsic(user.uid)) {
        let primary = groups
            .iter()
            .find(|group| group.gid == user.gid)
            .with_context(|| {
                format!(
                    "the primary GID {} of {} matches no group",
                    user.gid, user.name
                )
            })?;
        let auxiliary = groups
            .iter()
            .filter(|group| group.members.contains(&user.name));
        let names: BTreeSet<&Name> = iter::once(primary)
            .chain(auxiliary)
            .map(|group| &group.name)
            .collect();
        for group in names {
            userdb::write_membership(&dir, &user.name, group)?;
            written += 1;
        }
    }
    info!(
        "wrote {written} membership files to /{}",
        userdb::DROPIN_DIR
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use cap_std_ext::cap_tempfile::utf8::TempDir;
    use cap_std_ext::dirext::CapStdExtDirExtUtf8;
    use indoc::indoc;

    use super::*;
    use crate::sysusers::ConfigFile;
    use crate::testutil::{TestDistro, rootfs};

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

    /// An empty rootfs with /usr and /etc, the accounts of the test /etc,
    /// the lock file parsed from `lock`, and one more sysusers.d file with
    /// `entries`.
    struct Fixture {
        root: TempDir,
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
            let root = rootfs()?;
            for dir in OWNED_DIRS {
                root.create_dir(dir)?;
            }
            Ok(Self {
                root,
                distro,
                users: USERS.lines().map(str::parse).collect::<Result<_>>()?,
                groups: GROUPS.lines().map(str::parse).collect::<Result<_>>()?,
                lock,
                files,
            })
        }

        fn accounts(&self) -> Accounts<'_> {
            Accounts {
                root: &self.root,
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

    #[test]
    fn ownership_check_names_the_ids_without_an_account() -> Result<()> {
        let fixture = Fixture::new(TestDistro::default(), None, "")?;
        let root = &fixture.root;
        root.create_dir_all("usr/lib/foo")?;
        root.write("usr/lib/foo/bar", "")?;
        root.write("etc/baz", "")?;
        root.symlink("baz", "etc/link")?;

        // The test cannot chown, so the paths are owned by whoever runs it.
        let meta = root.metadata("etc/baz")?;
        let mut expected = Vec::new();
        if !USERS.contains(&format!(":x:{}:", meta.uid())) {
            expected.push(format!(
                "UID {} owns /etc/baz and 4 more paths but matches no user",
                meta.uid()
            ));
        }
        if !GROUPS.contains(&format!(":x:{}:", meta.gid())) {
            expected.push(format!(
                "GID {} owns /etc/baz and 4 more paths but matches no group",
                meta.gid()
            ));
        }
        assert_eq!(Check::Ownership.run(&fixture.accounts())?, expected);
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

    const SHADOW: &str = indoc! {"
        root:!*:20702::::::
        avahi:!*:20702:::::1:
        http:$6$salt$hash:20702::::::
    "};

    #[test]
    fn writes_records_for_every_user_but_root_and_nobody() -> Result<()> {
        let root = rootfs()?;
        root.create_dir("etc")?;
        root.write("etc/passwd", USERS)?;
        root.write("etc/shadow", SHADOW)?;
        write_user_records(&root)?;
        let names = root.open_dir(userdb::DROPIN_DIR)?.filenames_sorted()?;
        assert_eq!(
            names,
            [
                "33.user",
                "33.user-privileged",
                "900.user",
                "971.user",
                "avahi.user",
                "http.user",
                "http.user-privileged",
                "tss.user",
            ]
        );
        let tss = root.read_to_string("usr/lib/userdb/tss.user")?;
        assert!(tss.contains("\"locked\": true"), "{tss}");

        root.write("etc/group", GROUPS)?;
        write_memberships(&root)?;
        let names = root.open_dir(userdb::DROPIN_DIR)?.filenames_sorted()?;
        let memberships: Vec<_> = names
            .iter()
            .filter(|name| name.ends_with(".membership"))
            .collect();
        assert_eq!(
            memberships,
            [
                "avahi:adm.membership",
                "avahi:avahi.membership",
                "http:http.membership",
                "tss:adm.membership",
                "tss:tss.membership",
            ]
        );
        assert!(
            root.read_to_string("usr/lib/userdb/http.user")?
                .contains("\"disposition\": \"system\"")
        );
        Ok(())
    }
}
