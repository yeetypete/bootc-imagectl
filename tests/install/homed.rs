//! Check the user systemd-homed's first boot wizard created.

use std::collections::BTreeMap;
use std::process::Command;

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;
use serde::Deserialize;

/// The user the xtask passes a credential for.
const USER: &str = "alice";

/// systemd-homed manages the user, which is a member of `groups`.
pub(crate) fn adds_wizard_user_to(groups: &[&str]) -> Result<()> {
    Command::new("homectl")
        .args(["inspect", USER])
        .output_string()?;
    let user = uzers::get_user_by_name(USER).expect("the user resolves through NSS");
    let found: Vec<String> = uzers::get_user_groups(USER, user.primary_group_id())
        .expect("the groups of the user")
        .iter()
        .map(|group| group.name().to_string_lossy().into_owned())
        .collect();
    for group in groups {
        assert!(
            found.iter().any(|found| found == group),
            "{group}: {found:?}"
        );
    }
    Ok(())
}

/// A user record.
#[derive(Debug, Deserialize)]
struct UserRecord {
    /// The fields bound to each machine the record was used on, by machine
    /// ID.
    binding: BTreeMap<String, Binding>,
}

/// The `binding` section of a user record for one machine.
#[derive(Debug, Deserialize)]
struct Binding {
    storage: UserStorage,
}

/// How systemd-homed stores a home, as in systemd's `UserStorage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum UserStorage {
    Classic,
    Luks,
    Directory,
    Subvolume,
    Fscrypt,
    Cifs,
}

/// systemd-homed stored the user's home without encrypting it again.
pub(crate) fn stores_wizard_user_unencrypted() -> Result<()> {
    let output = Command::new("homectl")
        .args(["inspect", "--json=short", USER])
        .output_string()?;
    let record: UserRecord = serde_json::from_str(&output)?;
    let storages: Vec<UserStorage> = record
        .binding
        .into_values()
        .map(|binding| binding.storage)
        .collect();
    // The record reports subvolume storage even where homed falls back to a
    // plain directory, on filesystems other than btrfs.
    assert_eq!(storages, [UserStorage::Subvolume]);
    Ok(())
}
