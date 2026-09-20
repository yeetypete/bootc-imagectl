//! Parse the lines of a sysusers.d file into entries.

use std::ops::RangeInclusive;
use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};
use cap_std_ext::camino::{Utf8Path, Utf8PathBuf};

use super::{Entry, Group, Id, Kind, Membership, Name, PrimaryGroup, User, parse_id, word};

/// type, name, ID, GECOS, home directory, shell.
const MAX_FIELDS: usize = 6;

/// Parse the ID field of a `u` line: `UID`, `UID:GID`, `UID:groupname` or a
/// path, with `-` for an automatic UID.
fn parse_user_id(field: Option<&str>) -> Result<(Id, Option<PrimaryGroup>)> {
    let Some(field) = field else {
        return Ok((Id::Automatic, None));
    };
    if field.starts_with('/') {
        return Ok((field.parse()?, None));
    }
    Ok(match field.split_once(':') {
        Some((uid, group)) => (uid.parse()?, Some(group.parse()?)),
        None => (field.parse()?, None),
    })
}

/// Parse a home directory or shell field, which must be an absolute path.
fn parse_path(field: &str) -> Result<Utf8PathBuf> {
    let path = Utf8Path::new(field);
    ensure!(path.is_absolute(), "{field:?} is not an absolute path");
    Ok(path.to_owned())
}

/// Expand `%%` to `%`, the only specifier supported. A `%` followed by a
/// letter or digit is another specifier and fails. Like systemd, a `%`
/// followed by anything else, or at the end of the field, is kept as a
/// literal `%`.
fn expand_specifiers(field: &str) -> Result<String> {
    let mut expanded = String::with_capacity(field.len());
    let mut chars = field.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.peek() {
                Some('%') => {
                    chars.next();
                }
                Some(&next) if next.is_ascii_alphanumeric() => {
                    bail!("{field:?} uses the specifier %{next}, only %% is supported")
                }
                _ => {}
            }
        }
        expanded.push(c);
    }
    Ok(expanded)
}

/// The value of a field, or `None` if it is unset, which systemd spells `-`
/// or leaves empty.
fn value(word: &str) -> Option<&str> {
    (!word.is_empty() && word != "-").then_some(word)
}

/// The columns of one line.
struct Fields<'a> {
    kind: Kind,
    name: Option<&'a str>,
    id: Option<&'a str>,
    gecos: Option<&'a str>,
    home: Option<&'a str>,
    shell: Option<&'a str>,
}

impl<'a> Fields<'a> {
    fn new(words: &'a [String]) -> Result<Self> {
        let [kind, rest @ ..] = words else {
            bail!("empty line");
        };
        ensure!(!rest.is_empty(), "missing name field");
        ensure!(
            words.len() <= MAX_FIELDS,
            "a line has at most {MAX_FIELDS} fields, got {}",
            words.len()
        );
        let mut columns = [None; MAX_FIELDS - 1];
        for (column, word) in columns.iter_mut().zip(rest) {
            *column = value(word);
        }
        let [name, id, gecos, home, shell] = columns;
        Ok(Self {
            kind: kind.parse()?,
            name,
            id,
            gecos,
            home,
            shell,
        })
    }

    /// The name field: a group for `g` lines, a user otherwise.
    fn name(&self) -> Result<Name> {
        let what = match self.kind {
            Kind::Group => "group",
            _ => "user",
        };
        self.name
            .with_context(|| format!("missing {what} name"))?
            .parse()
    }

    /// Fail if a field only `u` lines take is set.
    fn ensure_user_fields_unset(&self) -> Result<()> {
        if let Some(field) = self.gecos.or(self.home).or(self.shell) {
            bail!("only u lines take a GECOS, home directory or shell, got {field:?}");
        }
        Ok(())
    }
}

impl User {
    fn from_fields(fields: &Fields<'_>, locked: bool) -> Result<Self> {
        let (uid, primary_group) = parse_user_id(fields.id)?;
        if let Some(gecos) = fields.gecos {
            ensure!(
                !gecos.contains(|c: char| c == ':' || c.is_ascii_control()),
                "the GECOS field {gecos:?} contains a colon or a control character"
            );
        }
        Ok(Self {
            name: fields.name()?,
            uid,
            primary_group,
            gecos: fields.gecos.map(str::to_owned),
            home: fields
                .home
                .map(parse_path)
                .transpose()
                .context("home directory")?,
            shell: fields.shell.map(parse_path).transpose().context("shell")?,
            locked,
        })
    }
}

impl Group {
    fn from_fields(fields: &Fields<'_>) -> Result<Self> {
        fields.ensure_user_fields_unset()?;
        Ok(Self {
            name: fields.name()?,
            gid: fields.id.map_or(Ok(Id::Automatic), str::parse)?,
        })
    }
}

impl Membership {
    fn from_fields(fields: &Fields<'_>) -> Result<Self> {
        fields.ensure_user_fields_unset()?;
        Ok(Self {
            user: fields.name()?,
            group: fields.id.context("missing group name")?.parse()?,
        })
    }
}

/// Parse the range of an `r` line: `FROM-TO`, or a single ID.
fn parse_range(fields: &Fields<'_>) -> Result<RangeInclusive<u32>> {
    fields.ensure_user_fields_unset()?;
    if let Some(name) = fields.name {
        bail!("r lines take no name, got {name:?}");
    }
    let field = fields.id.context("missing range")?;
    let (first, last) = field.split_once('-').unwrap_or((field, field));
    let (first, last) = (parse_id(first)?, parse_id(last)?);
    ensure!(first <= last, "{field:?} is not a range");
    Ok(first..=last)
}

impl FromStr for Entry {
    type Err = anyhow::Error;

    /// Parse one entry line, which is neither empty nor a comment.
    fn from_str(line: &str) -> Result<Self> {
        let mut words = word::split(line)?;
        for word in words.iter_mut().skip(1) {
            *word = expand_specifiers(word)?;
        }
        let fields = Fields::new(&words)?;
        Ok(match fields.kind {
            Kind::User { locked } => Self::User(User::from_fields(&fields, locked)?),
            Kind::Group => Self::Group(Group::from_fields(&fields)?),
            Kind::Membership => Self::Membership(Membership::from_fields(&fields)?),
            Kind::Range => Self::Range(parse_range(&fields)?),
        })
    }
}

#[cfg(test)]
mod tests {
    use indoc::indoc;

    use super::*;
    use crate::sysusers::parse;

    fn name(name: &str) -> Name {
        Name(name.into())
    }

    fn user(name: &str) -> User {
        User {
            name: Name(name.into()),
            uid: Id::Automatic,
            primary_group: None,
            gecos: None,
            home: None,
            shell: None,
            locked: false,
        }
    }

    /// The single user entry in `line`.
    fn parse_user(line: &str) -> Result<User> {
        match line.parse()? {
            Entry::User(user) => Ok(user),
            other => bail!("{other:?} is not a user"),
        }
    }

    #[test]
    fn parses() -> Result<()> {
        let entries = parse(indoc! {r#"
            #Type Name     ID             GECOS                 Home directory Shell
            u!    httpd    404            "HTTP User"

            u     _authd   /usr/bin/authd "Authorization user"

            u     postgres -              "Postgresql Database" /var/lib/pgsql /usr/libexec/postgresdb
            g     input    -              -
            m     _authd   input
            u     root     0              "Superuser"           /root          /bin/zsh
            r     -        500-900
        "#})?;
        assert_eq!(
            entries,
            [
                Entry::User(User {
                    uid: Id::Fixed(404),
                    gecos: Some("HTTP User".into()),
                    locked: true,
                    ..user("httpd")
                }),
                Entry::User(User {
                    uid: Id::FromPath("/usr/bin/authd".into()),
                    gecos: Some("Authorization user".into()),
                    ..user("_authd")
                }),
                Entry::User(User {
                    gecos: Some("Postgresql Database".into()),
                    home: Some("/var/lib/pgsql".into()),
                    shell: Some("/usr/libexec/postgresdb".into()),
                    ..user("postgres")
                }),
                Entry::Group(Group {
                    name: name("input"),
                    gid: Id::Automatic,
                }),
                Entry::Membership(Membership {
                    user: name("_authd"),
                    group: name("input"),
                }),
                Entry::User(User {
                    uid: Id::Fixed(0),
                    gecos: Some("Superuser".into()),
                    home: Some("/root".into()),
                    shell: Some("/bin/zsh".into()),
                    ..user("root")
                }),
                Entry::Range(500..=900),
            ]
        );
        Ok(())
    }

    #[test]
    fn parses_id_forms() -> Result<()> {
        let cases = [
            (
                "u a 10:20",
                Entry::User(User {
                    uid: Id::Fixed(10),
                    primary_group: Some(PrimaryGroup::Gid(20)),
                    ..user("a")
                }),
            ),
            (
                "u b 10:wheel",
                Entry::User(User {
                    uid: Id::Fixed(10),
                    primary_group: Some(PrimaryGroup::Name(name("wheel"))),
                    ..user("b")
                }),
            ),
            (
                "u c -:wheel",
                Entry::User(User {
                    primary_group: Some(PrimaryGroup::Name(name("wheel"))),
                    ..user("c")
                }),
            ),
            (
                "g x 5",
                Entry::Group(Group {
                    name: name("x"),
                    gid: Id::Fixed(5),
                }),
            ),
            (
                "g y /usr/bin/x",
                Entry::Group(Group {
                    name: name("y"),
                    gid: Id::FromPath("/usr/bin/x".into()),
                }),
            ),
            ("r - 700", Entry::Range(700..=700)),
        ];
        for (line, expected) in cases {
            assert_eq!(line.parse::<Entry>()?, expected, "{line}");
        }
        Ok(())
    }

    #[test]
    fn keeps_literal_percent() -> Result<()> {
        for (line, gecos) in [
            (r#"u x - "100%% sure""#, "100% sure"),
            (r#"u x - "100%""#, "100%"),
            (r#"u x - "50%-off %""#, "50%-off %"),
        ] {
            assert_eq!(parse_user(line)?.gecos.as_deref(), Some(gecos), "{line}");
        }
        Ok(())
    }

    #[test]
    fn rejects_malformed_lines_with_their_number() {
        let cases = [
            ("u\n", "missing name field"),
            ("u %o-user -\n", "specifier %o"),
            ("x foo\n", "unknown type"),
            ("g! foo\n", "! modifier"),
            ("g foo - \"gecos\"\n", "only u lines take a GECOS"),
            ("m foo\n", "missing group name"),
            ("r foo 500-600\n", "take no name"),
            ("r -\n", "missing range"),
            ("r - 900-500\n", "not a range"),
            ("u 1foo -\n", "does not start with a letter"),
            ("u foo 65535\n", "65535 is not a valid UID or GID"),
            ("u foo abc\n", "not a UID"),
            ("u foo +5\n", "not a UID"),
            ("u foo 007\n", "not a UID"),
            ("u foo 10:1bad\n", "neither a GID nor a group name"),
            ("u foo - \"a:b\"\n", "colon or a control"),
            ("u foo - \"a\tb\"\n", "colon or a control"),
            (
                "u foo - - relative\n",
                "home directory: \"relative\" is not an absolute path",
            ),
            ("u foo - - - - extra\n", "at most 6 fields, got 7"),
            ("u foo - \"open\n", "unbalanced"),
        ];
        for (content, expected) in cases {
            let err = format!("{:#}", parse(&format!("\n{content}")).unwrap_err());
            assert!(err.starts_with("line 2: "), "{content:?}: {err}");
            assert!(err.contains(expected), "{content:?}: {err}");
        }
    }
}
