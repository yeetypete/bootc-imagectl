//! Read and write the account files in /etc: passwd(5), group(5), shadow(5)
//! and gshadow(5).
//!
//! Each file holds one entry per line, with colon-separated fields:
//!
//! ```text
//! root:x:0:0:root:/root:/bin/bash
//! adm:x:4:syslog,alice
//! alice:$6$salt$hash:20000:0:99999:7:::
//! wheel:!*:alice:alice,bob
//! ```

use std::fmt;
use std::io::Write as _;
use std::os::unix::fs::fchown;
use std::str::FromStr;

use anyhow::{Context, Result, anyhow, ensure};
use cap_std_ext::camino::Utf8PathBuf;
use cap_std_ext::cap_std::fs::MetadataExt;
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::CapStdExtDirExtUtf8;

use crate::id::{Gid, Uid};
use crate::sysusers::Name;

/// An entry of one of the account files.
pub trait Entry: FromStr<Err = anyhow::Error> + fmt::Display + fmt::Debug {
    /// The file the entries live in, relative to the rootfs.
    const PATH: &str;

    /// Read the entries of the file in the rootfs.
    ///
    /// # Errors
    ///
    /// Fails if the file cannot be read or a line does not parse. The error
    /// names the file and the line.
    fn read_all(root: &Dir) -> Result<Vec<Self>> {
        let content = root
            .read_to_string(Self::PATH)
            .with_context(|| format!("reading /{}", Self::PATH))?;
        content
            .lines()
            .zip(1..)
            .map(|(line, number)| line.parse().with_context(|| format!("line {number}")))
            .collect::<Result<_>>()
            .with_context(|| format!("parsing /{}", Self::PATH))
    }

    /// Replace the file in the rootfs with the entries, keeping its owner
    /// and mode.
    ///
    /// # Errors
    ///
    /// Fails if the file cannot be read or written.
    fn write_all(root: &Dir, entries: &[Self]) -> Result<()> {
        let meta = root
            .metadata(Self::PATH)
            .with_context(|| format!("reading /{}", Self::PATH))?;
        root.atomic_replace_with(Self::PATH, |file| -> Result<()> {
            for entry in entries {
                writeln!(file, "{entry}")?;
            }
            let file = file.get_ref().as_file();
            file.set_permissions(meta.permissions())?;
            fchown(file, Some(meta.uid()), Some(meta.gid()))?;
            Ok(())
        })
        .with_context(|| format!("writing /{}", Self::PATH))
    }
}

/// A field that may be empty, as `None`.
pub(crate) fn non_empty(field: &str) -> Option<String> {
    (!field.is_empty()).then(|| field.to_owned())
}

/// Split a line into its `N` colon-separated fields.
fn fields<const N: usize>(line: &str) -> Result<[&str; N]> {
    line.split(':')
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|fields: Vec<_>| anyhow!("expected {N} fields, got {}", fields.len()))
}

/// Parse a comma-separated list of names. An empty field is an empty list.
fn parse_names(field: &str) -> Result<Vec<Name>> {
    if field.is_empty() {
        return Ok(Vec::new());
    }
    field.split(',').map(str::parse).collect()
}

/// A comma-separated list of names.
pub(crate) struct Names<'a>(pub(crate) &'a [Name]);

impl fmt::Display for Names<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, name) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(",")?;
            }
            write!(f, "{name}")?;
        }
        Ok(())
    }
}

/// Parse a field that may be empty, which means unset.
fn optional<T: FromStr<Err = anyhow::Error>>(field: &str) -> Result<Option<T>> {
    (!field.is_empty()).then(|| field.parse()).transpose()
}

/// A field that is empty if unset.
struct Optional<T>(Option<T>);

impl<T: fmt::Display> fmt::Display for Optional<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(value) => write!(f, "{value}"),
            None => Ok(()),
        }
    }
}

/// A number of days, as in the password ages of shadow(5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Days(pub u64);

impl FromStr for Days {
    type Err = anyhow::Error;

    fn from_str(field: &str) -> Result<Self> {
        field
            .parse()
            .map(Self)
            .with_context(|| format!("{field:?} is not a number of days"))
    }
}

impl fmt::Display for Days {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A day counted from 1970-01-01, as in the dates of shadow(5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date(pub u64);

impl FromStr for Date {
    type Err = anyhow::Error;

    fn from_str(field: &str) -> Result<Self> {
        field
            .parse()
            .map(Self)
            .with_context(|| format!("{field:?} is not a number of days since 1970-01-01"))
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A passwd(5) entry: a user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Passwd {
    pub name: Name,
    /// `x` when the password is in shadow(5), otherwise the password hash or
    /// a value such as `*` or `!` that matches no password.
    pub password: String,
    pub uid: Uid,
    /// The primary group.
    pub gid: Gid,
    /// The GECOS field, a short description of the account.
    pub gecos: String,
    /// The home directory. Login uses `/` if empty.
    pub home: Utf8PathBuf,
    /// The login shell. Login uses `/bin/sh` if empty.
    pub shell: Utf8PathBuf,
}

impl Entry for Passwd {
    const PATH: &str = "etc/passwd";
}

impl FromStr for Passwd {
    type Err = anyhow::Error;

    fn from_str(line: &str) -> Result<Self> {
        let [name, password, uid, gid, gecos, home, shell] = fields(line)?;
        Ok(Self {
            name: name.parse()?,
            password: password.to_owned(),
            uid: uid.parse()?,
            gid: gid.parse()?,
            gecos: gecos.to_owned(),
            home: home.into(),
            shell: shell.into(),
        })
    }
}

impl fmt::Display for Passwd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}:{}:{}:{}:{}",
            self.name, self.password, self.uid, self.gid, self.gecos, self.home, self.shell
        )
    }
}

/// A group(5) entry: a group and its members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub name: Name,
    /// `x` when the password is in gshadow(5), otherwise the password hash
    /// or a value such as `*` or `!` that matches no password.
    pub password: String,
    pub gid: Gid,
    /// The users that are members of the group, other than through their
    /// primary group.
    pub members: Vec<Name>,
}

impl Entry for Group {
    const PATH: &str = "etc/group";
}

impl FromStr for Group {
    type Err = anyhow::Error;

    fn from_str(line: &str) -> Result<Self> {
        let [name, password, gid, members] = fields(line)?;
        Ok(Self {
            name: name.parse()?,
            password: password.to_owned(),
            gid: gid.parse()?,
            members: parse_names(members)?,
        })
    }
}

impl fmt::Display for Group {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}:{}",
            self.name,
            self.password,
            self.gid,
            Names(&self.members)
        )
    }
}

/// The password field of shadow(5) and gshadow(5): a crypt(3) hash, or a
/// value that matches no password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Password(String);

impl Password {
    /// The hash, unless the field is empty or starts with `!` or `*`, which
    /// disable the password.
    #[must_use]
    pub fn hash(&self) -> Option<&str> {
        let field = self.0.as_str();
        (!field.is_empty() && !field.starts_with(['!', '*'])).then_some(field)
    }
}

impl From<&str> for Password {
    fn from(field: &str) -> Self {
        Self(field.to_owned())
    }
}

impl fmt::Display for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A shadow(5) entry: a user's password and its aging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shadow {
    pub name: Name,
    /// The password. Empty if no password is needed.
    pub password: Password,
    /// The day of the last password change. Day 0 forces a change at the
    /// next login.
    pub last_change: Option<Date>,
    /// How long the user must wait between password changes.
    pub min_age: Option<Days>,
    /// How long a password stays valid.
    pub max_age: Option<Days>,
    /// How long before the password expires the user is warned.
    pub warn_days: Option<Days>,
    /// How long after the password expires the account stays usable.
    pub inactive_days: Option<Days>,
    /// The day the account expires.
    pub expire_date: Option<Date>,
}

impl Entry for Shadow {
    const PATH: &str = "etc/shadow";
}

impl FromStr for Shadow {
    type Err = anyhow::Error;

    fn from_str(line: &str) -> Result<Self> {
        let [
            name,
            password,
            last_change,
            min_age,
            max_age,
            warn_days,
            inactive_days,
            expire_date,
            reserved,
        ] = fields(line)?;
        ensure!(
            reserved.is_empty(),
            "the reserved field is not empty, got {reserved:?}"
        );
        Ok(Self {
            name: name.parse()?,
            password: password.into(),
            last_change: optional(last_change).context("last password change")?,
            min_age: optional(min_age).context("minimum password age")?,
            max_age: optional(max_age).context("maximum password age")?,
            warn_days: optional(warn_days).context("password warning period")?,
            inactive_days: optional(inactive_days).context("password inactivity period")?,
            expire_date: optional(expire_date).context("account expiration date")?,
        })
    }
}

impl fmt::Display for Shadow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}:{}:{}:{}:{}:{}:",
            self.name,
            self.password,
            Optional(self.last_change),
            Optional(self.min_age),
            Optional(self.max_age),
            Optional(self.warn_days),
            Optional(self.inactive_days),
            Optional(self.expire_date),
        )
    }
}

/// A gshadow(5) entry: a group's password, administrators and members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gshadow {
    pub name: Name,
    /// The password. Empty if only members may use `newgrp`.
    pub password: Password,
    /// The users that may change the group's password and members.
    pub administrators: Vec<Name>,
    /// The same members as in group(5).
    pub members: Vec<Name>,
}

impl Entry for Gshadow {
    const PATH: &str = "etc/gshadow";
}

impl FromStr for Gshadow {
    type Err = anyhow::Error;

    fn from_str(line: &str) -> Result<Self> {
        let [name, password, administrators, members] = fields(line)?;
        Ok(Self {
            name: name.parse()?,
            password: password.into(),
            administrators: parse_names(administrators).context("administrators")?,
            members: parse_names(members).context("members")?,
        })
    }
}

impl fmt::Display for Gshadow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}:{}",
            self.name,
            self.password,
            Names(&self.administrators),
            Names(&self.members)
        )
    }
}

#[cfg(test)]
mod tests {
    use indoc::indoc;

    use super::*;
    use crate::testutil::{gid, rootfs, uid};

    fn name(name: &str) -> Name {
        name.parse().expect("a valid name")
    }

    #[test]
    fn password_hash_is_absent_when_disabled() {
        assert_eq!(Password::from("$6$salt$hash").hash(), Some("$6$salt$hash"));
        for disabled in ["", "!", "*", "!*", "!!", "!$6$salt$hash"] {
            assert_eq!(Password::from(disabled).hash(), None, "{disabled:?}");
        }
    }

    #[test]
    fn parses() -> Result<()> {
        assert_eq!(
            "alice:x:1000:1000:Alice,,,:/home/alice:".parse::<Passwd>()?,
            Passwd {
                name: name("alice"),
                password: "x".into(),
                uid: uid(1000),
                gid: gid(1000),
                gecos: "Alice,,,".into(),
                home: "/home/alice".into(),
                shell: "".into(),
            }
        );
        assert_eq!(
            "adm:x:4:syslog,alice".parse::<Group>()?,
            Group {
                name: name("adm"),
                password: "x".into(),
                gid: gid(4),
                members: vec![name("syslog"), name("alice")],
            }
        );
        assert_eq!(
            "alpm:!*:20702:::::1:".parse::<Shadow>()?,
            Shadow {
                name: name("alpm"),
                password: "!*".into(),
                last_change: Some(Date(20702)),
                min_age: None,
                max_age: None,
                warn_days: None,
                inactive_days: None,
                expire_date: Some(Date(1)),
            }
        );
        assert_eq!(
            "wheel:!*:alice:alice,bob".parse::<Gshadow>()?,
            Gshadow {
                name: name("wheel"),
                password: "!*".into(),
                administrators: vec![name("alice")],
                members: vec![name("alice"), name("bob")],
            }
        );
        Ok(())
    }

    /// Parse `line` as a `T` and check that it prints back the same.
    fn assert_round_trip<T: Entry>(line: &str) -> Result<()> {
        assert_eq!(line.parse::<T>()?.to_string(), line);
        Ok(())
    }

    #[test]
    fn round_trips() -> Result<()> {
        assert_round_trip::<Passwd>("bin:x:1:1::/:/usr/bin/nologin")?;
        assert_round_trip::<Group>("tty:x:5:")?;
        assert_round_trip::<Shadow>("root:*:::::::")?;
        assert_round_trip::<Shadow>("daemon:*:20714:0:99999:7:::")?;
        assert_round_trip::<Gshadow>("root:::root")?;
        assert_round_trip::<Gshadow>("adm:*::")?;
        Ok(())
    }

    /// The error of parsing `line` as a `T`.
    fn error<T: Entry>(line: &str) -> String {
        format!("{:#}", line.parse::<T>().unwrap_err())
    }

    #[test]
    fn rejects_malformed_lines() {
        let cases = [
            (
                error::<Passwd>("root:x:0:0:root:/root"),
                "expected 7 fields, got 6",
            ),
            (error::<Passwd>(""), "expected 7 fields, got 1"),
            (
                error::<Passwd>("root:x:a:0::/:"),
                "\"a\" is not a UID or GID",
            ),
            (
                error::<Group>("adm:x:4:syslog,,alice"),
                "empty user or group name",
            ),
            (
                error::<Shadow>("root:*:x::::::"),
                "last password change: \"x\" is not a number of days since 1970-01-01",
            ),
            (
                error::<Shadow>("root:*::::::1:x"),
                "the reserved field is not empty, got \"x\"",
            ),
            (
                error::<Gshadow>("wheel:!*:a b:"),
                "administrators: the name \"a b\" contains ' '",
            ),
        ];
        for (error, expected) in cases {
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn writes_file_and_keeps_its_mode() -> Result<()> {
        use cap_std_ext::cap_std::fs::{Permissions, PermissionsExt};

        let root = rootfs()?;
        root.create_dir("etc")?;
        root.write(
            Shadow::PATH,
            "root:!*:20702::::::\nalice:$6$salt$hash:20702::::::\n",
        )?;
        root.set_permissions(Shadow::PATH, Permissions::from_mode(0o600))?;
        let kept: Vec<Shadow> = Shadow::read_all(&root)?
            .into_iter()
            .filter(|entry| entry.name.as_str() == "root")
            .collect();
        Shadow::write_all(&root, &kept)?;
        assert_eq!(root.read_to_string(Shadow::PATH)?, "root:!*:20702::::::\n");
        assert_eq!(root.metadata(Shadow::PATH)?.mode() & 0o777, 0o600);
        Ok(())
    }

    #[test]
    fn reads_file_and_names_malformed_line() -> Result<()> {
        let root = rootfs()?;
        root.create_dir("etc")?;
        root.write(
            Group::PATH,
            indoc! {"
                root:x:0:
                adm:x:4:alice
            "},
        )?;
        let groups = Group::read_all(&root)?;
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[1].members, [name("alice")]);

        root.write(Group::PATH, "root:x:0:\nadm:x:4\n")?;
        let err = format!("{:#}", Group::read_all(&root).unwrap_err());
        assert!(
            err.starts_with("parsing /etc/group: line 2: expected 4 fields"),
            "{err}"
        );

        let err = format!("{:#}", Passwd::read_all(&root).unwrap_err());
        assert!(err.starts_with("reading /etc/passwd"), "{err}");
        Ok(())
    }
}
