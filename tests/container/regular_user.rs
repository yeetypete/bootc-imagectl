//! Check the accounts of the images with a regular user built in.

use anyhow::Result;
use bootc_imagectl::testing::{self, Feature, Requirement, Test};
use bootc_imagectl::trial;
use cap_std_ext::cap_std::fs::MetadataExt;
use cap_std_ext::cap_std::fs_utf8::Dir;

use crate::finalize::{ROOT, initramfs_paths};

fn locks_uids_of_package_users() -> Result<()> {
    let accounts = testing::image_accounts()?;
    assert!(ROOT.exists("usr/lib/sysusers.d/00-bootc-imagectl.lock.conf"));
    for &(name, uid) in accounts.system_users {
        let found = uzers::get_user_by_name(name).map(|user| user.uid());
        assert_eq!(found, Some(uid), "{name}");
    }
    Ok(())
}

/// The account files in /etc contain only root and nobody, while the moved
/// users still resolve through NSS with their groups.
fn moves_accounts_out_of_etc_and_resolves_them_through_nss() -> Result<()> {
    let accounts = testing::image_accounts()?;
    testing::etc_contains_only_root_and_nobody(&Dir::from_cap_std(ROOT.try_clone()?))?;

    let (name, uid) = accounts.user;
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

fn writes_user_records_to_userdb() -> Result<()> {
    let accounts = testing::image_accounts()?;
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

fn writes_group_records_to_userdb() -> Result<()> {
    let accounts = testing::image_accounts()?;
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
fn writes_privileged_records_for_users_with_passwords() -> Result<()> {
    let accounts = testing::image_accounts()?;
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
/// [`ImageAccounts::group`], and root for its own group.
fn writes_membership_files_for_primary_and_auxiliary_groups() -> Result<()> {
    let accounts = testing::image_accounts()?;
    let (name, _) = accounts.user;
    assert_eq!(
        ROOT.read_to_string(format!("usr/lib/userdb/{name}:{name}.membership"))?,
        "{}\n"
    );
    assert!(ROOT.exists(format!(
        "usr/lib/userdb/{name}:{}.membership",
        accounts.group
    )));
    assert!(ROOT.exists("usr/lib/userdb/root:root.membership"));
    Ok(())
}

fn initramfs_contains_user_records() -> Result<()> {
    let accounts = testing::image_accounts()?;
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

const REQUIRES: &[Requirement] = &[Requirement::Without(Feature::Homed)];

pub(crate) const TESTS: &[Test] = &[
    trial!(locks_uids_of_package_users, REQUIRES),
    trial!(
        moves_accounts_out_of_etc_and_resolves_them_through_nss,
        REQUIRES
    ),
    trial!(writes_user_records_to_userdb, REQUIRES),
    trial!(writes_group_records_to_userdb, REQUIRES),
    trial!(writes_privileged_records_for_users_with_passwords, REQUIRES),
    trial!(
        writes_membership_files_for_primary_and_auxiliary_groups,
        REQUIRES
    ),
    trial!(initramfs_contains_user_records, REQUIRES),
];
