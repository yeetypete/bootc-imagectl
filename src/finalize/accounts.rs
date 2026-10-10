//! Fix the UIDs and GIDs of the users and groups the build created with the
//! sysusers lock file, and move the users from /etc into /usr/lib/userdb,
//! where an upgrade replaces them.

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
use uzers::cache::UsersCache;
use uzers::{Groups, Users};

use crate::distro::Distro;
use crate::id::{Gid, Uid};
use crate::passwd::{self, Entry as _, Passwd, Shadow, non_empty};
use crate::sysusers::lockfile::{Block, LockFile, Package};
use crate::sysusers::{self, Entry, IdSource, Index, Membership, Name, PrimaryGroup, User};
use crate::userdb::{self, GroupRecord, UserRecord};

/// The directories whose paths must be owned by accounts in the image,
/// relative to the rootfs.
const OWNED_DIRS: [&str; 2] = ["usr", "etc"];

/// Account lookups through NSS.
trait Nss {
    /// The UID of the user with the name.
    fn uid_of(&self, name: &Name) -> Option<Uid>;
    /// The name of the user with the UID.
    fn name_of(&self, uid: Uid) -> Option<String>;
    /// The GID of the group with the name.
    fn gid_of(&self, group: &Name) -> Option<Gid>;
    /// The name of the group with the GID.
    fn group_name_of(&self, gid: Gid) -> Option<String>;
    /// The groups the user belongs to, as `getgrouplist` merges them from
    /// every NSS source. `gid` is the user's primary group.
    fn groups_of(&self, user: &Name, gid: Gid) -> Option<Vec<String>>;
}

impl Nss for UsersCache {
    fn uid_of(&self, name: &Name) -> Option<Uid> {
        self.get_user_by_name(name.as_str())
            .and_then(|user| Uid::new(user.uid()))
    }

    fn name_of(&self, uid: Uid) -> Option<String> {
        self.get_user_by_uid(uid.as_raw())
            .map(|user| user.name().to_string_lossy().into_owned())
    }

    fn gid_of(&self, group: &Name) -> Option<Gid> {
        self.get_group_by_name(group.as_str())
            .and_then(|group| Gid::new(group.gid()))
    }

    fn group_name_of(&self, gid: Gid) -> Option<String> {
        self.get_group_by_gid(gid.as_raw())
            .map(|group| group.name().to_string_lossy().into_owned())
    }

    fn groups_of(&self, user: &Name, gid: Gid) -> Option<Vec<String>> {
        let groups = uzers::get_user_groups(user.as_str(), gid.as_raw())?;
        Some(
            groups
                .iter()
                .map(|group| group.name().to_string_lossy().into_owned())
                .collect(),
        )
    }
}

/// The accounts of the image, what configures them, and how the system
/// resolves them.
struct Accounts<'a> {
    root: &'a Dir,
    distro: &'a dyn Distro,
    nss: &'a dyn Nss,
    /// The lock file's path, relative to the rootfs.
    lock_path: &'a Utf8Path,
    /// The lock file, if it exists.
    lock: Option<&'a LockFile>,
    users: &'a [Passwd],
    groups: &'a [passwd::Group],
    shadows: &'a [Shadow],
    index: Index<'a>,
}

impl Accounts<'_> {
    /// The users the lock file must cover.
    fn users_to_lock(&self) -> impl Iterator<Item = &Passwd> {
        self.users.iter().filter(|user| !user.uid.is_intrinsic())
    }

    /// The groups the lock file must cover.
    fn groups_to_lock(&self) -> impl Iterator<Item = &passwd::Group> {
        self.groups.iter().filter(|group| !group.gid.is_intrinsic())
    }

    /// The groups each user belongs to, through its primary group or a
    /// member list in /etc/group. root and nobody in root's and nobody's
    /// groups are left out.
    fn memberships(&self) -> Result<BTreeMap<&Name, BTreeSet<&Name>>> {
        let mut memberships: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
        for user in self.users {
            let primary = self
                .groups
                .iter()
                .find(|group| group.gid == user.gid)
                .with_context(|| {
                    format!(
                        "the primary GID {} of {} matches no group",
                        user.gid, user.name
                    )
                })?;
            let auxiliary = self
                .groups
                .iter()
                .filter(|group| group.members.contains(&user.name));
            for group in iter::once(primary).chain(auxiliary) {
                if user.uid.is_intrinsic() && group.gid.is_intrinsic() {
                    continue;
                }
                memberships
                    .entry(&user.name)
                    .or_default()
                    .insert(&group.name);
            }
        }
        Ok(memberships)
    }

    /// The shadow entry of the user, if it has one.
    fn shadow_of(&self, name: &Name) -> Option<&Shadow> {
        self.shadows.iter().find(|shadow| shadow.name == *name)
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
    Uid(Uid),
    Gid(Gid),
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
enum LockStatus<'a, T> {
    /// An entry fixes the ID to the one in /etc.
    Locked,
    /// No entry, or an entry without a fixed ID.
    Missing,
    /// An entry fixes the ID to another one.
    Drifted { path: &'a Utf8Path, specified: T },
}

/// The status of an account whose ID in /etc is `id`, given the file and ID
/// that configure it, if any.
fn lock_status<'a, T: Copy + PartialEq>(
    configured: Option<(&'a Utf8Path, &IdSource<T>)>,
    id: T,
) -> LockStatus<'a, T> {
    match configured {
        Some((path, IdSource::Fixed(specified))) => {
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
    /// Every moved user resolves through NSS by name and by UID, every moved
    /// group by GID, and every user with its memberships.
    Resolution,
    /// Every user, group and membership the sysusers.d files configure
    /// exists, so systemd-sysusers changes nothing at boot.
    Sysusers,
}

impl Check {
    /// The checks on the accounts in /etc, before the users move.
    const BEFORE_MOVE: [Self; 4] = [Self::Lock, Self::Drift, Self::Ownership, Self::Stale];

    /// The checks that the moved users still resolve.
    const AFTER_MOVE: [Self; 2] = [Self::Resolution, Self::Sysusers];

    /// Issues the check found, one finding per line. Empty if it passed.
    fn run(self, accounts: &Accounts<'_>) -> Result<Vec<String>> {
        match self {
            Self::Lock => missing_lines(accounts),
            Self::Drift => Ok(drifted_ids(accounts)),
            Self::Ownership => unresolved_owners(accounts),
            Self::Stale => stale_packages(accounts),
            Self::Resolution => unresolved_accounts(accounts),
            Self::Sysusers => Ok(unapplied_entries(accounts)),
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
            Self::Stale => format!("update the blocks in /{lock} as shown"),
            Self::Resolution => {
                "install nss-systemd and list systemd in the passwd, group and shadow databases of /etc/nsswitch.conf".to_owned()
            }
            Self::Sysusers => {
                "run systemd-sysusers in the build after installing the packages".to_owned()
            }
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
            Self::Resolution => "resolution",
            Self::Sysusers => "sysusers",
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
                gid: IdSource::Fixed(group.gid),
            }));
        }
    }
    for user in accounts.users_to_lock() {
        let LockStatus::Missing = lock_status(index.uid(&user.name), user.uid) else {
            continue;
        };
        // A `u` line without a primary group creates a group with the user's
        // name and UID.
        let own_group = user.gid == user.uid.matching_gid()
            && accounts
                .groups
                .iter()
                .any(|group| group.name == user.name && group.gid == user.gid);
        missing.push(Entry::User(User {
            name: user.name.clone(),
            uid: IdSource::Fixed(user.uid),
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
                .any(|user| user.name == *member && user.uid.is_intrinsic());
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
            .map(|(package, entries)| Block {
                package,
                removed: false,
                entries,
            })
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
                let owners = [
                    Owner::Uid(meta.uid().try_into()?),
                    Owner::Gid(meta.gid().try_into()?),
                ];
                for owner in owners {
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

/// Stale check: the `# package:` blocks of packages that are not installed,
/// and the `# removed:` blocks of packages that are. A removed package's
/// accounts would otherwise keep their memberships, and a returned package's
/// accounts would stay locked.
fn stale_packages(accounts: &Accounts<'_>) -> Result<Vec<String>> {
    let mut findings = Vec::new();
    for block in accounts.lock.iter().flat_map(|lock| &lock.blocks) {
        let Package::Named(name) = &block.package else {
            continue;
        };
        let installed = accounts.distro.is_installed(name)?;
        if !block.removed && !installed {
            findings.push(format!(
                "the package {name} is not installed. Replace its block with:\n{}",
                block.to_removed()
            ));
        } else if block.removed && installed {
            findings.push(format!(
                "the package {name} is installed again. Change the header of its block to '# package: {name}'"
            ));
        }
    }
    Ok(findings)
}

/// Resolution check: the moved users, groups and memberships NSS does not
/// resolve as /etc had them.
fn unresolved_accounts(accounts: &Accounts<'_>) -> Result<Vec<String>> {
    let nss = accounts.nss;
    let memberships = accounts.memberships()?;
    let mut findings = Vec::new();
    for user in accounts.users_to_lock() {
        let (name, uid) = (&user.name, user.uid);
        match nss.uid_of(name) {
            Some(found) if found == uid => {}
            Some(found) => findings.push(format!("user {name} resolves to UID {found}, not {uid}")),
            None => findings.push(format!("user {name} does not resolve")),
        }
        match nss.name_of(uid) {
            Some(found) if found == name.as_str() => {}
            Some(found) => findings.push(format!("UID {uid} resolves to {found}, not {name}")),
            None => findings.push(format!("UID {uid} does not resolve")),
        }
    }
    // The memberships below resolve the groups by name.
    for group in accounts.groups_to_lock() {
        let (name, gid) = (&group.name, group.gid);
        match nss.group_name_of(gid) {
            Some(found) if found == name.as_str() => {}
            Some(found) => findings.push(format!("GID {gid} resolves to {found}, not {name}")),
            None => findings.push(format!("GID {gid} does not resolve")),
        }
    }
    for user in accounts.users {
        let name = &user.name;
        let Some(groups) = memberships.get(name) else {
            continue;
        };
        let Some(resolved) = nss.groups_of(name, user.gid) else {
            findings.push(format!("the groups of {name} do not resolve"));
            continue;
        };
        for group in groups {
            if !resolved.iter().any(|found| found == group.as_str()) {
                findings.push(format!("{name} does not resolve as a member of {group}"));
            }
        }
    }
    Ok(findings)
}

/// Sysusers check: the entries systemd-sysusers would still apply at boot.
/// It creates a user or group it cannot find through NSS, and adds an `m`
/// line's user to the group's member list in /etc/group.
fn unapplied_entries(accounts: &Accounts<'_>) -> Vec<String> {
    let (nss, index) = (accounts.nss, &accounts.index);
    let mut findings = Vec::new();
    for name in index.users.keys() {
        if nss.uid_of(name).is_none() {
            findings.push(format!("systemd-sysusers would create the user {name}"));
        }
    }
    for name in index.groups.keys() {
        if nss.gid_of(name).is_none() {
            findings.push(format!("systemd-sysusers would create the group {name}"));
        }
    }
    for &(user, group) in &index.memberships {
        let listed = accounts
            .groups
            .iter()
            .any(|entry| entry.name == *group && entry.members.contains(user));
        if !listed {
            findings.push(format!(
                "systemd-sysusers would add {user} to the group {group}"
            ));
        }
    }
    findings
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

/// Check the accounts, write their user and group records and membership
/// files, and move the users out of /etc. `lock` is the lock file's absolute
/// path in the image.
///
/// # Errors
///
/// Fails if a check fails, reporting every failed check, or if a file
/// cannot be read or written.
pub(super) fn finalize(root: &Dir, distro: &dyn Distro, lock: &Utf8Path) -> Result<()> {
    let lock_path = lock.strip_prefix("/").unwrap_or(lock);
    let users = Passwd::read_all(root)?;
    let groups = passwd::Group::read_all(root)?;
    let shadows = Shadow::read_all(root)?;
    let files = sysusers::read_all(root)?;
    let lock = LockFile::read(root, lock_path)?;
    let nss = UsersCache::new();
    let accounts = Accounts {
        root,
        distro,
        nss: &nss,
        lock_path,
        lock: lock.as_ref(),
        users: &users,
        groups: &groups,
        shadows: &shadows,
        index: files.iter().collect(),
    };
    run(&Check::BEFORE_MOVE, &accounts).context("checking the accounts")?;
    write_user_records(&accounts).context("writing the user records")?;
    write_group_records(&accounts).context("writing the group records")?;
    write_memberships(&accounts).context("writing the membership files")?;
    move_users(&accounts).context("moving the users out of /etc")?;
    run(&Check::AFTER_MOVE, &accounts).context("checking that the moved users resolve")
}

/// Write a user record for every user in /etc/passwd other than root and
/// nobody, which nss-systemd synthesizes itself, to the drop-in directory.
/// A user with a password hash in /etc/shadow gets a privileged record with
/// the hash, every other user is locked, as is every user of a removed
/// package.
fn write_user_records(accounts: &Accounts<'_>) -> Result<()> {
    let root = accounts.root;
    root.create_dir_all(userdb::DROPIN_DIR)
        .with_context(|| format!("creating /{}", userdb::DROPIN_DIR))?;
    let dir = root.open_dir(userdb::DROPIN_DIR)?;
    let removed: BTreeSet<&Name> = accounts
        .lock
        .iter()
        .flat_map(|lock| lock.removed_users())
        .collect();
    let mut written = 0;
    for user in accounts.users_to_lock() {
        let shadow = accounts
            .shadow_of(&user.name)
            .filter(|_| !removed.contains(&user.name));
        UserRecord::from_passwd(user, shadow)
            .write(&dir)
            .with_context(|| format!("writing the record of {}", user.name))?;
        debug!("wrote the user record of {}", user.name);
        written += 1;
    }
    info!("wrote {written} user records to /{}", userdb::DROPIN_DIR);
    Ok(())
}

/// Write a group record for every group in /etc/group other than root and
/// nobody, which nss-systemd synthesizes itself, to the drop-in directory.
fn write_group_records(accounts: &Accounts<'_>) -> Result<()> {
    let dir = accounts.root.open_dir(userdb::DROPIN_DIR)?;
    let mut written = 0;
    for group in accounts.groups_to_lock() {
        GroupRecord::from_group(group)
            .write(&dir)
            .with_context(|| format!("writing the record of {}", group.name))?;
        debug!("wrote the group record of {}", group.name);
        written += 1;
    }
    info!("wrote {written} group records to /{}", userdb::DROPIN_DIR);
    Ok(())
}

/// Write a membership file for every user and each group it belongs to,
/// see [`Accounts::memberships`].
fn write_memberships(accounts: &Accounts<'_>) -> Result<()> {
    let dir = accounts.root.open_dir(userdb::DROPIN_DIR)?;
    let memberships = accounts.memberships()?;
    let mut written = 0;
    for (user, groups) in &memberships {
        for group in groups {
            userdb::write_membership(&dir, user, group)?;
            written += 1;
        }
    }
    info!(
        "wrote {written} membership files to /{}",
        userdb::DROPIN_DIR
    );
    Ok(())
}

/// Remove the users with records from /etc/passwd and /etc/shadow. Their
/// names stay in the member lists of /etc/group, which systemd-sysusers
/// maintains at boot.
fn move_users(accounts: &Accounts<'_>) -> Result<()> {
    let root = accounts.root;
    let moved: BTreeSet<&Name> = accounts.users_to_lock().map(|user| &user.name).collect();
    let users: Vec<Passwd> = accounts
        .users
        .iter()
        .filter(|user| !moved.contains(&user.name))
        .cloned()
        .collect();
    Passwd::write_all(root, &users)?;
    let shadows: Vec<Shadow> = accounts
        .shadows
        .iter()
        .filter(|shadow| !moved.contains(&shadow.name))
        .cloned()
        .collect();
    Shadow::write_all(root, &shadows)?;
    info!("moved {} users out of /etc", moved.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use cap_std_ext::cap_tempfile::utf8::TempDir;
    use cap_std_ext::dirext::CapStdExtDirExtUtf8;
    use indoc::indoc;

    use uzers::mock::MockUsers;
    use uzers::os::unix::GroupExt;
    use uzers::{AllGroups, Group, User};

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

    const SHADOW: &str = indoc! {"
        root:!*:20702::::::
        avahi:!*:20702:::::1:
        http:$6$salt$hash:20702::::::
    "};

    /// NSS as it resolves the test /etc once the users have moved: every
    /// user by name and UID, and every group with its members, the primary
    /// ones included.
    fn nss(users: &[Passwd], groups: &[passwd::Group]) -> MockUsers {
        let mut nss = MockUsers::with_current_uid(0);
        for user in users {
            nss.add_user(User::new(
                user.uid.as_raw(),
                user.name.as_str(),
                user.gid.as_raw(),
            ));
        }
        for group in groups {
            let mut mock = Group::new(group.gid.as_raw(), group.name.as_str());
            for member in &group.members {
                mock = mock.add_member(member.as_str());
            }
            nss.add_group(mock);
        }
        nss
    }

    impl Nss for MockUsers {
        fn uid_of(&self, name: &Name) -> Option<Uid> {
            self.get_user_by_name(name.as_str())
                .and_then(|user| Uid::new(user.uid()))
        }

        fn name_of(&self, uid: Uid) -> Option<String> {
            self.get_user_by_uid(uid.as_raw())
                .map(|user| user.name().to_string_lossy().into_owned())
        }

        fn gid_of(&self, group: &Name) -> Option<Gid> {
            self.get_group_by_name(group.as_str())
                .and_then(|group| Gid::new(group.gid()))
        }

        fn group_name_of(&self, gid: Gid) -> Option<String> {
            self.get_group_by_gid(gid.as_raw())
                .map(|group| group.name().to_string_lossy().into_owned())
        }

        /// The primary group and every group that lists the user, as
        /// `getgrouplist` would collect them.
        fn groups_of(&self, user: &Name, gid: Gid) -> Option<Vec<String>> {
            Some(
                self.get_all_groups()
                    .filter(|group| {
                        group.gid() == gid.as_raw()
                            || group.members().iter().any(|member| member == user.as_str())
                    })
                    .map(|group| group.name().to_string_lossy().into_owned())
                    .collect(),
            )
        }
    }

    /// An empty rootfs with /usr and /etc, the accounts of the test /etc,
    /// the lock file parsed from `lock`, and one more sysusers.d file with
    /// `entries`.
    struct Fixture {
        root: TempDir,
        distro: TestDistro,
        nss: MockUsers,
        users: Vec<Passwd>,
        groups: Vec<passwd::Group>,
        shadows: Vec<Shadow>,
        lock: Option<LockFile>,
        files: Vec<ConfigFile>,
    }

    impl Fixture {
        fn new(distro: TestDistro, lock: Option<&str>, entries: &str) -> Result<Self> {
            let lock: Option<LockFile> = lock.map(str::parse).transpose()?;
            let mut files = Vec::new();
            if let Some(lock) = &lock {
                files.push(ConfigFile {
                    path: "usr/lib/sysusers.d/00-bootc-imagectl.lock.conf".into(),
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
            let users: Vec<Passwd> = USERS.lines().map(str::parse).collect::<Result<_>>()?;
            let groups: Vec<passwd::Group> =
                GROUPS.lines().map(str::parse).collect::<Result<_>>()?;
            Ok(Self {
                root,
                distro,
                nss: nss(&users, &groups),
                users,
                groups,
                shadows: SHADOW.lines().map(str::parse).collect::<Result<_>>()?,
                lock,
                files,
            })
        }

        fn accounts(&self) -> Accounts<'_> {
            Accounts {
                root: &self.root,
                distro: &self.distro,
                nss: &self.nss,
                lock_path: Utf8Path::new("usr/lib/sysusers.d/00-bootc-imagectl.lock.conf"),
                lock: self.lock.as_ref(),
                users: &self.users,
                groups: &self.groups,
                shadows: &self.shadows,
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
    fn accounts_without_sysusers_file_get_blank_header() -> Result<()> {
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
    fn ownership_check_names_ids_without_account() -> Result<()> {
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
            [indoc! {"
                the package apache is not installed. Replace its block with:
                # removed: apache
                g http 34
                u http 33:34 - - -
            "}]
        );
        Ok(())
    }

    #[test]
    fn stale_check_names_removed_packages_installed_again() -> Result<()> {
        let lock = LOCK.replace("# package: apache", "# removed: apache");
        let fixture = Fixture::new(TestDistro::default(), Some(&lock), "")?;
        assert_eq!(
            Check::Stale.run(&fixture.accounts())?,
            [
                "the package apache is installed again. Change the header of its block to '# package: apache'"
            ]
        );

        let distro = TestDistro {
            not_installed: &["apache"],
            ..TestDistro::default()
        };
        let fixture = Fixture::new(distro, Some(&lock), "")?;
        assert_eq!(Check::Stale.run(&fixture.accounts())?, Vec::<String>::new());
        Ok(())
    }

    #[test]
    fn reports_every_failed_check() -> Result<()> {
        let distro = TestDistro {
            not_installed: &["apache"],
            ..TestDistro::default()
        };
        let fixture = Fixture::new(distro, Some(LOCK), "")?;
        let err = format!(
            "{:#}",
            run(&Check::BEFORE_MOVE, &fixture.accounts()).unwrap_err()
        );
        assert!(
            err.starts_with("1 account checks failed\n\nThe stale check failed:\n"),
            "{err}"
        );
        assert!(
            err.contains("help: update the blocks in /usr/lib/sysusers.d/"),
            "{err}"
        );
        assert!(!err.contains("lock check"), "{err}");
        Ok(())
    }

    #[test]
    fn passes_complete_lock_file() -> Result<()> {
        let fixture = Fixture::new(TestDistro::default(), Some(LOCK), "")?;
        run(&Check::BEFORE_MOVE, &fixture.accounts())
    }

    #[test]
    fn resolution_check_names_what_nss_lacks() -> Result<()> {
        let mut fixture = Fixture::new(TestDistro::default(), Some(LOCK), "")?;
        assert_eq!(
            Check::Resolution.run(&fixture.accounts())?,
            Vec::<String>::new()
        );

        // NSS without tss, with http at another UID and GID, and avahi and
        // root missing from adm.
        let users: Vec<Passwd> = [
            "root:x:0:0::/root:",
            "http:x:35:34::/srv/http:/usr/bin/nologin",
            "avahi:x:900:900::/:",
        ]
        .into_iter()
        .map(str::parse)
        .collect::<Result<_>>()?;
        let groups: Vec<passwd::Group> = ["root:x:0:", "adm:x:4:tss", "avahi:x:900:", "http:x:36:"]
            .into_iter()
            .map(str::parse)
            .collect::<Result<_>>()?;
        fixture.nss = nss(&users, &groups);
        assert_eq!(
            Check::Resolution.run(&fixture.accounts())?,
            [
                "user http resolves to UID 35, not 33",
                "UID 33 does not resolve",
                "user tss does not resolve",
                "UID 971 does not resolve",
                "GID 34 does not resolve",
                "GID 971 does not resolve",
                "root does not resolve as a member of adm",
                "avahi does not resolve as a member of adm",
                "http does not resolve as a member of http",
                "tss does not resolve as a member of tss",
            ]
        );
        Ok(())
    }

    #[test]
    fn sysusers_check_names_unapplied_entries() -> Result<()> {
        let fixture = Fixture::new(TestDistro::default(), Some(LOCK), "")?;
        assert_eq!(
            Check::Sysusers.run(&fixture.accounts())?,
            Vec::<String>::new()
        );

        let fixture = Fixture::new(
            TestDistro::default(),
            Some(LOCK),
            "u foo 500\ng bar 501\nm tss avahi\n",
        )?;
        assert_eq!(
            Check::Sysusers.run(&fixture.accounts())?,
            [
                "systemd-sysusers would create the user foo",
                "systemd-sysusers would create the group bar",
                "systemd-sysusers would create the group foo",
                "systemd-sysusers would add tss to the group avahi",
            ]
        );
        Ok(())
    }

    #[test]
    fn moves_users_out_of_etc() -> Result<()> {
        let fixture = Fixture::new(TestDistro::default(), Some(LOCK), "")?;
        let root = &fixture.root;
        root.write("etc/passwd", USERS)?;
        root.write("etc/shadow", SHADOW)?;
        root.write("etc/group", GROUPS)?;
        move_users(&fixture.accounts())?;
        assert_eq!(
            root.read_to_string("etc/passwd")?,
            "root:x:0:0:root:/root:/bin/bash\nnobody:x:65534:65534::/:/usr/bin/nologin\n"
        );
        assert_eq!(root.read_to_string("etc/shadow")?, "root:!*:20702::::::\n");
        assert_eq!(root.read_to_string("etc/group")?, GROUPS);
        Ok(())
    }

    #[test]
    fn locks_users_of_removed_packages() -> Result<()> {
        // http has a password hash, but apache was removed.
        let lock = LOCK.replace("# package: apache", "# removed: apache");
        let fixture = Fixture::new(TestDistro::default(), Some(&lock), "")?;
        let root = &fixture.root;
        write_user_records(&fixture.accounts())?;
        let http = root.read_to_string("usr/lib/userdb/http.user")?;
        assert!(http.contains("\"locked\": true"), "{http}");
        assert!(!root.exists("usr/lib/userdb/http.user-privileged"));
        Ok(())
    }

    #[test]
    fn writes_records_for_every_user_but_root_and_nobody() -> Result<()> {
        let fixture = Fixture::new(TestDistro::default(), Some(LOCK), "")?;
        let root = &fixture.root;
        write_user_records(&fixture.accounts())?;
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

        write_memberships(&fixture.accounts())?;
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
                "root:adm.membership",
                "tss:adm.membership",
                "tss:tss.membership",
            ]
        );
        Ok(())
    }

    #[test]
    fn writes_records_for_every_group_but_root_and_nobody() -> Result<()> {
        let fixture = Fixture::new(TestDistro::default(), Some(LOCK), "")?;
        let root = &fixture.root;
        root.create_dir_all(userdb::DROPIN_DIR)?;
        write_group_records(&fixture.accounts())?;
        let names = root.open_dir(userdb::DROPIN_DIR)?.filenames_sorted()?;
        assert_eq!(
            names,
            [
                "34.group",
                "4.group",
                "900.group",
                "971.group",
                "adm.group",
                "avahi.group",
                "http.group",
                "tss.group",
            ]
        );
        Ok(())
    }
}
