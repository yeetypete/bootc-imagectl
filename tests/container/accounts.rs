//! Check the accounts finalize moved to /usr/lib/userdb, for every image.

use anyhow::Result;
use bootc_imagectl::passwd::{Entry, Group, Passwd, Shadow};
use cap_std_ext::cap_std::fs::MetadataExt;
use cap_std_ext::cap_std::fs_utf8::Dir;

use crate::finalize::{ROOT, initramfs_paths};

pub(crate) struct MovedAccounts {
    /// Users packages created, by name and UID. They have no password.
    pub(crate) system_users: &'static [(&'static str, u32)],
    /// A regular user with a password, by name and UID.
    pub(crate) user: (&'static str, u32),
    /// A group the regular user is a member of besides its own.
    pub(crate) group: &'static str,
}

pub(crate) fn locks_uids(accounts: &MovedAccounts) {
    assert!(ROOT.exists("usr/lib/sysusers.d/00-bootc-imagectl.lock.conf"));
    for &(name, uid) in accounts.system_users {
        let found = uzers::get_user_by_name(name).map(|user| user.uid());
        assert_eq!(found, Some(uid), "{name}");
    }
}

/// The user of a removed package keeps its UID and the GID of its group,
/// locked.
pub(crate) fn keeps_removed_accounts_locked() -> Result<()> {
    let user = uzers::get_user_by_name("removed").expect("the user resolves through NSS");
    assert_eq!(user.uid(), 850);
    let group = uzers::get_group_by_gid(850).expect("the group resolves through NSS");
    assert_eq!(group.name(), "removed");
    let record = ROOT.read_to_string("usr/lib/userdb/removed.user")?;
    assert!(record.contains("\"locked\": true"), "{record}");
    Ok(())
}

/// /etc/passwd and /etc/shadow keep only root and nobody, while the moved
/// users still resolve through NSS with their groups.
pub(crate) fn moves_users_out_of_etc(accounts: &MovedAccounts) -> Result<()> {
    let root = Dir::from_cap_std(ROOT.try_clone()?);
    let users: Vec<String> = Passwd::read_all(&root)?
        .iter()
        .map(|user| user.name.to_string())
        .collect();
    assert_eq!(users, ["root", "nobody"]);
    let shadow: Vec<String> = Shadow::read_all(&root)?
        .iter()
        .map(|entry| entry.name.to_string())
        .collect();
    assert_eq!(shadow, ["root", "nobody"]);

    // Members stay in /etc/group, where systemd-sysusers maintains them.
    let (name, uid) = accounts.user;
    let group = Group::read_all(&root)?
        .into_iter()
        .find(|group| group.name.as_str() == accounts.group)
        .expect("the group exists");
    assert!(
        group.members.iter().any(|member| member.as_str() == name),
        "{group}"
    );

    let user = uzers::get_user_by_name(name).expect("the user resolves through NSS");
    assert_eq!(user.uid(), uid);
    let groups: Vec<String> = uzers::get_user_groups(name, user.primary_group_id())
        .expect("the groups of the user")
        .iter()
        .map(|group| group.name().to_string_lossy().into_owned())
        .collect();
    assert!(groups.contains(&accounts.group.to_owned()), "{groups:?}");
    Ok(())
}

pub(crate) fn writes_user_records(accounts: &MovedAccounts) -> Result<()> {
    for &(name, uid) in accounts.system_users {
        let record = ROOT.read_to_string(format!("usr/lib/userdb/{name}.user"))?;
        assert!(record.contains(&format!("\"uid\": {uid}")), "{record}");
        assert_eq!(
            ROOT.read_link(format!("usr/lib/userdb/{uid}.user"))?,
            std::path::Path::new(&format!("{name}.user"))
        );
    }
    assert!(!ROOT.exists("usr/lib/userdb/root.user"));
    Ok(())
}

pub(crate) fn writes_group_records(accounts: &MovedAccounts) -> Result<()> {
    let group = uzers::get_group_by_name(accounts.group).expect("the group resolves through NSS");
    let gid = group.gid();
    let record = ROOT.read_to_string(format!("usr/lib/userdb/{}.group", accounts.group))?;
    assert!(record.contains(&format!("\"gid\": {gid}")), "{record}");
    assert_eq!(
        ROOT.read_link(format!("usr/lib/userdb/{gid}.group"))?,
        std::path::Path::new(&format!("{}.group", accounts.group))
    );
    assert!(!ROOT.exists("usr/lib/userdb/root.group"));
    Ok(())
}

/// The regular user's password hash is in a privileged record only root may
/// read. Users without a password are locked instead.
pub(crate) fn writes_privileged_records(accounts: &MovedAccounts) -> Result<()> {
    let (name, uid) = accounts.user;
    let privileged = format!("usr/lib/userdb/{name}.user-privileged");
    let record = ROOT.read_to_string(&privileged)?;
    assert!(
        record.contains("\"hashedPassword\": [\n      \"$"),
        "{record}"
    );
    assert_eq!(ROOT.metadata(&privileged)?.mode() & 0o777, 0o600);
    assert_eq!(
        ROOT.read_link(format!("usr/lib/userdb/{uid}.user-privileged"))?,
        std::path::Path::new(&format!("{name}.user-privileged"))
    );
    assert!(
        !ROOT
            .read_to_string(format!("usr/lib/userdb/{name}.user"))?
            .contains("locked")
    );

    for &(name, _) in accounts.system_users {
        let record = ROOT.read_to_string(format!("usr/lib/userdb/{name}.user"))?;
        assert!(record.contains("\"locked\": true"), "{record}");
        assert!(!ROOT.exists(format!("usr/lib/userdb/{name}.user-privileged")));
    }
    Ok(())
}

/// The regular user has a membership file for its primary group and for
/// [`MovedAccounts::group`]. root has none.
pub(crate) fn writes_membership_files(accounts: &MovedAccounts) -> Result<()> {
    let (name, _) = accounts.user;
    assert_eq!(
        ROOT.read_to_string(format!("usr/lib/userdb/{name}:{name}.membership"))?,
        "{}\n"
    );
    assert!(ROOT.exists(format!(
        "usr/lib/userdb/{name}:{}.membership",
        accounts.group
    )));
    assert!(!ROOT.exists("usr/lib/userdb/root:root.membership"));
    Ok(())
}

pub(crate) fn initramfs_contains_user_records(accounts: &MovedAccounts) -> Result<()> {
    let paths = initramfs_paths()?;
    for &(name, uid) in accounts.system_users.iter().chain([&accounts.user]) {
        for path in [
            format!("usr/lib/userdb/{name}.user"),
            format!("usr/lib/userdb/{uid}.user"),
        ] {
            assert!(paths.contains(&path), "{path}");
        }
    }
    Ok(())
}
