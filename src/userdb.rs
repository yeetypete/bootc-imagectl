//! Write the user records nss-systemd reads from /usr/lib/userdb.
//!
//! The files of a user are named after the user and its UID:
//!
//! ```text
//! alice.user                                 the record, mode 0644
//! 1000.user -> alice.user                    for lookups by UID
//! alice.user-privileged                      the password hash, mode 0600
//! 1000.user-privileged -> alice.user-privileged
//! alice:wheel.membership                     membership in a group
//! ```
//!
//! The record holds the passwd(5) fields. systemd derives its disposition,
//! system or regular, from the UID. The privileged file holds the
//! password hash from shadow(5), which nss-systemd merges into the record
//! when it may read the file. A user without a hash has no privileged file
//! and is locked. A membership file exists for every group the user belongs
//! to.

use std::ops::Not;

use anyhow::{Context, Result};
use cap_std_ext::cap_std::fs::{Permissions, PermissionsExt};
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::CapStdExtDirExtUtf8;
use serde::Serialize;

use crate::id::{Gid, Uid};
use crate::passwd::{Passwd, Shadow, non_empty};
use crate::sysusers::Name;

/// The drop-in directory under /usr, where the image ships its user
/// records, relative to the rootfs.
pub const DROPIN_DIR: &str = "usr/lib/userdb";

/// The privileged section of a user record: the fields only root may read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Privileged {
    /// The crypt(3) hashes of the user's password.
    pub hashed_password: Vec<String>,
}

/// The content of a privileged file.
#[derive(Serialize)]
struct PrivilegedRecord<'a> {
    privileged: &'a Privileged,
}

/// A user record, with the fields a passwd(5) and a shadow(5) entry
/// provide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserRecord {
    pub user_name: String,
    pub uid: Uid,
    pub gid: Gid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub real_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// Whether the user cannot log in. Set when the user has no password.
    #[serde(skip_serializing_if = "Not::not")]
    pub locked: bool,
    /// The privileged section, written to its own file.
    #[serde(skip)]
    pub privileged: Option<Privileged>,
}

impl UserRecord {
    /// The record of a passwd entry and its shadow entry, if it has one.
    #[must_use]
    pub fn from_passwd(user: &Passwd, shadow: Option<&Shadow>) -> Self {
        let privileged = shadow
            .and_then(|shadow| shadow.password.hash())
            .map(|hash| Privileged {
                hashed_password: vec![hash.to_owned()],
            });
        Self {
            user_name: user.name.to_string(),
            uid: user.uid,
            gid: user.gid,
            real_name: non_empty(&user.gecos),
            home_directory: non_empty(user.home.as_str()),
            shell: non_empty(user.shell.as_str()),
            locked: privileged.is_none(),
            privileged,
        }
    }

    /// Write the record and, if there is one, the privileged section to the
    /// drop-in directory, each with its symlink by UID. The files replace
    /// those of an earlier build.
    ///
    /// # Errors
    ///
    /// Fails if a file cannot be written.
    pub fn write(&self, dir: &Dir) -> Result<()> {
        let name = &self.user_name;
        let uid = self.uid;
        write_linked(
            dir,
            &format!("{name}.user"),
            &format!("{uid}.user"),
            self,
            0o644,
        )?;
        let (primary, by_uid) = (
            format!("{name}.user-privileged"),
            format!("{uid}.user-privileged"),
        );
        if let Some(privileged) = &self.privileged {
            let record = PrivilegedRecord { privileged };
            write_linked(dir, &primary, &by_uid, &record, 0o600)?;
        } else {
            dir.remove_file_optional(&primary)?;
            dir.remove_file_optional(&by_uid)?;
        }
        Ok(())
    }
}

/// Write the membership file that makes `user` a member of `group`.
///
/// # Errors
///
/// Fails if the file cannot be written.
pub fn write_membership(dir: &Dir, user: &Name, group: &Name) -> Result<()> {
    let path = format!("{user}:{group}.membership");
    dir.atomic_write_with_perms(&path, "{}\n", Permissions::from_mode(0o644))
        .with_context(|| format!("writing {path}"))
}

/// Write `record` as JSON to `primary` with `mode`, and symlink `by_uid` to
/// it, replacing both.
fn write_linked(
    dir: &Dir,
    primary: &str,
    by_uid: &str,
    record: &impl Serialize,
    mode: u32,
) -> Result<()> {
    let mut json = serde_json::to_string_pretty(record)?;
    json.push('\n');
    dir.atomic_write_with_perms(primary, json, Permissions::from_mode(mode))
        .with_context(|| format!("writing {primary}"))?;
    dir.remove_file_optional(by_uid)?;
    dir.symlink(primary, by_uid)
        .with_context(|| format!("linking {by_uid} to {primary}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use cap_std_ext::cap_std::fs::MetadataExt;
    use indoc::indoc;

    use super::*;
    use crate::testutil::{gid, rootfs, uid};

    #[test]
    fn takes_fields_from_passwd_and_shadow() -> Result<()> {
        let user: Passwd = "avahi:x:969:969:Avahi mDNS/DNS-SD daemon:/:/usr/bin/nologin".parse()?;
        let shadow: Shadow = "avahi:!*:20702:::::1:".parse()?;
        let record = UserRecord::from_passwd(&user, Some(&shadow));
        assert_eq!(
            record,
            UserRecord {
                user_name: "avahi".into(),
                uid: uid(969),
                gid: gid(969),
                real_name: Some("Avahi mDNS/DNS-SD daemon".into()),
                home_directory: Some("/".into()),
                shell: Some("/usr/bin/nologin".into()),
                locked: true,
                privileged: None,
            }
        );

        let user: Passwd = "alice:x:1000:1000:::".parse()?;
        let shadow: Shadow = "alice:$6$salt$hash:20702::::::".parse()?;
        let record = UserRecord::from_passwd(&user, Some(&shadow));
        assert_eq!(
            (record.real_name, record.home_directory, record.shell),
            (None, None, None)
        );
        assert!(!record.locked);
        assert_eq!(
            record.privileged,
            Some(Privileged {
                hashed_password: vec!["$6$salt$hash".into()],
            })
        );

        // A user without a shadow entry has no password.
        assert!(UserRecord::from_passwd(&user, None).locked);
        Ok(())
    }

    #[test]
    fn writes_record_and_links_it_by_uid() -> Result<()> {
        let root = rootfs()?;
        let user: Passwd = "alice:x:1000:1000:Alice:/home/alice:/bin/sh".parse()?;
        let shadow: Shadow = "alice:$6$salt$hash:20702::::::".parse()?;
        UserRecord::from_passwd(&user, Some(&shadow)).write(&root)?;
        assert_eq!(
            root.read_to_string("alice.user")?,
            indoc! {r#"
                {
                  "userName": "alice",
                  "uid": 1000,
                  "gid": 1000,
                  "realName": "Alice",
                  "homeDirectory": "/home/alice",
                  "shell": "/bin/sh"
                }
            "#}
        );
        assert_eq!(root.metadata("alice.user")?.mode() & 0o777, 0o644);
        assert_eq!(root.read_link("1000.user")?, "alice.user");
        assert_eq!(
            root.read_to_string("alice.user-privileged")?,
            indoc! {r#"
                {
                  "privileged": {
                    "hashedPassword": [
                      "$6$salt$hash"
                    ]
                  }
                }
            "#}
        );
        assert_eq!(
            root.metadata("alice.user-privileged")?.mode() & 0o777,
            0o600
        );
        assert_eq!(
            root.read_link("1000.user-privileged")?,
            "alice.user-privileged"
        );

        // A second write replaces the files, and removes the privileged ones
        // of a user who lost the password.
        let user: Passwd = "alice:x:1000:1000:Alice Smith:/home/alice:/bin/sh".parse()?;
        UserRecord::from_passwd(&user, None).write(&root)?;
        let record = root.read_to_string("1000.user")?;
        assert!(record.contains("Alice Smith"), "{record}");
        assert!(record.contains("\"locked\": true"), "{record}");
        assert_eq!(root.read_link("1000.user")?, "alice.user");
        assert!(!root.exists("alice.user-privileged"));
        assert!(!root.exists("1000.user-privileged"));
        Ok(())
    }

    #[test]
    fn writes_membership_files() -> Result<()> {
        let root = rootfs()?;
        let name = |name: &str| name.parse::<Name>().expect("a valid name");
        write_membership(&root, &name("alice"), &name("wheel"))?;
        assert_eq!(root.read_to_string("alice:wheel.membership")?, "{}\n");
        assert_eq!(
            root.metadata("alice:wheel.membership")?.mode() & 0o777,
            0o644
        );
        Ok(())
    }
}
