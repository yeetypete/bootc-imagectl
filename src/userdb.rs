//! Write the user records systemd-userdbd reads from /usr/lib/userdb.

use anyhow::{Context, Result, bail};
use cap_std_ext::cap_std::fs::{Permissions, PermissionsExt};
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::CapStdExtDirExtUtf8;
use serde::Serialize;

use crate::login_defs::LoginDefs;
use crate::passwd::{Passwd, non_empty};

/// The drop-in directory under /usr, where the image ships its user
/// records, relative to the rootfs.
pub const DROPIN_DIR: &str = "usr/lib/userdb";

/// The context a user is defined in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Disposition {
    /// A system user, in the system range of login.defs(5).
    System,
    /// A regular user, in the regular range of login.defs(5).
    Regular,
}

impl Disposition {
    /// The disposition of a UID by the login.defs ranges.
    fn of(uid: u32, defs: &LoginDefs) -> Result<Self> {
        if defs.is_system(uid) {
            Ok(Self::System)
        } else if defs.is_regular(uid) {
            Ok(Self::Regular)
        } else {
            bail!("UID {uid} is in neither the system nor the regular range of login.defs")
        }
    }
}

/// A user record, with the fields a passwd(5) entry provides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserRecord {
    pub user_name: String,
    pub disposition: Disposition,
    pub uid: u32,
    pub gid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub real_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
}

impl UserRecord {
    /// The record of a passwd entry.
    ///
    /// # Errors
    ///
    /// Fails if the UID is in no range of login.defs.
    pub fn from_passwd(user: &Passwd, defs: &LoginDefs) -> Result<Self> {
        Ok(Self {
            user_name: user.name.to_string(),
            disposition: Disposition::of(user.uid, defs)?,
            uid: user.uid,
            gid: user.gid,
            real_name: non_empty(&user.gecos),
            home_directory: non_empty(user.home.as_str()),
            shell: non_empty(user.shell.as_str()),
        })
    }

    /// Write the record to `<name>.user` in the drop-in directory, and
    /// symlink `<UID>.user` to it for lookups by UID. Both files are
    /// world-readable and replace those of an earlier build.
    ///
    /// # Errors
    ///
    /// Fails if a file cannot be written.
    pub fn write(&self, dir: &Dir) -> Result<()> {
        let primary = format!("{}.user", self.user_name);
        let by_uid = format!("{}.user", self.uid);
        let mut json = serde_json::to_string_pretty(self)?;
        json.push('\n');
        dir.atomic_write_with_perms(&primary, json, Permissions::from_mode(0o644))
            .with_context(|| format!("writing {primary}"))?;
        dir.remove_file_optional(&by_uid)?;
        dir.symlink(&primary, &by_uid)
            .with_context(|| format!("linking {by_uid} to {primary}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use cap_std_ext::cap_std::fs::MetadataExt;
    use indoc::indoc;

    use super::*;
    use crate::testutil::rootfs;

    #[test]
    fn takes_the_fields_from_passwd() -> Result<()> {
        let defs = LoginDefs::default();
        let user: Passwd = "avahi:x:969:969:Avahi mDNS/DNS-SD daemon:/:/usr/bin/nologin".parse()?;
        let record = UserRecord::from_passwd(&user, &defs)?;
        assert_eq!(
            record,
            UserRecord {
                user_name: "avahi".into(),
                disposition: Disposition::System,
                uid: 969,
                gid: 969,
                real_name: Some("Avahi mDNS/DNS-SD daemon".into()),
                home_directory: Some("/".into()),
                shell: Some("/usr/bin/nologin".into()),
            }
        );

        let user: Passwd = "alice:x:1000:1000:::".parse()?;
        let record = UserRecord::from_passwd(&user, &defs)?;
        assert_eq!(record.disposition, Disposition::Regular);
        assert_eq!(
            (record.real_name, record.home_directory, record.shell),
            (None, None, None)
        );

        let user: Passwd = "x:x:65000:65000:::".parse()?;
        let err = format!("{:#}", UserRecord::from_passwd(&user, &defs).unwrap_err());
        assert_eq!(
            err,
            "UID 65000 is in neither the system nor the regular range of login.defs"
        );
        Ok(())
    }

    #[test]
    fn writes_the_record_and_links_it_by_uid() -> Result<()> {
        let root = rootfs()?;
        let user: Passwd = "alice:x:1000:1000:Alice:/home/alice:/bin/sh".parse()?;
        let record = UserRecord::from_passwd(&user, &LoginDefs::default())?;
        record.write(&root)?;
        assert_eq!(
            root.read_to_string("alice.user")?,
            indoc! {r#"
                {
                  "userName": "alice",
                  "disposition": "regular",
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

        // A second write replaces both files.
        let user: Passwd = "alice:x:1000:1000:Alice Smith:/home/alice:/bin/sh".parse()?;
        UserRecord::from_passwd(&user, &LoginDefs::default())?.write(&root)?;
        assert!(root.read_to_string("1000.user")?.contains("Alice Smith"));
        assert_eq!(root.read_link("1000.user")?, "alice.user");
        Ok(())
    }
}
