//! Check the user systemd-homed's first boot wizard created.

use std::collections::BTreeMap;
use std::process::Command;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;
use serde::{Deserialize, Deserializer};

/// The user the xtask passes a credential for.
const USER: &str = "alice";

/// systemd-homed manages the user, which is a member of `groups`.
pub(crate) fn adds_wizard_user_to(groups: &[&str]) -> Result<()> {
    Command::new("homectl")
        .args(["inspect", USER])
        .run_get_string()?;
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
pub(crate) struct UserRecord {
    /// When the record was last changed.
    #[serde(rename = "lastChangeUSec", deserialize_with = "from_usec")]
    pub(crate) last_change: SystemTime,
    /// The fields bound to each machine the record was used on, by machine
    /// ID.
    pub(crate) binding: BTreeMap<String, Binding>,
}

/// Deserialize a time in microseconds since the epoch.
fn from_usec<'de, D: Deserializer<'de>>(deserializer: D) -> Result<SystemTime, D::Error> {
    let usec = u64::deserialize(deserializer)?;
    Ok(SystemTime::UNIX_EPOCH + Duration::from_micros(usec))
}

/// The user record of the wizard's user.
pub(crate) fn user_record() -> Result<UserRecord> {
    Command::new("homectl")
        .args(["inspect", "--json=short", USER])
        .run_and_parse_json()
}

/// The `binding` section of a user record for one machine.
#[derive(Debug, Deserialize)]
pub(crate) struct Binding {
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
    let storages: Vec<UserStorage> = user_record()?
        .binding
        .into_values()
        .map(|binding| binding.storage)
        .collect();
    // The record reports subvolume storage even where homed falls back to a
    // plain directory, on filesystems other than btrfs.
    assert_eq!(storages, [UserStorage::Subvolume]);
    Ok(())
}
