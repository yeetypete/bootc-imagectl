//! Look up the entry that configures a user, group or membership.

use std::collections::{BTreeMap, BTreeSet};

use cap_std_ext::camino::Utf8Path;

use super::{ConfigFile, Entry, Group, IdSource, Name, User};
use crate::id::{Gid, Uid};

/// The entry that configures an account, and the configuration file it is
/// in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Configuration<'a, T> {
    /// The sysusers.d file, relative to the rootfs.
    pub path: &'a Utf8Path,
    pub entry: T,
}

/// The accounts the sysusers.d files configure, by name. When two files
/// configure the same name, the first in the order systemd-sysusers applies
/// them wins.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Index<'a> {
    pub users: BTreeMap<&'a Name, Configuration<'a, User>>,
    /// The groups, including the one a `u` line without a primary group
    /// implicitly creates with the user's name and UID.
    pub groups: BTreeMap<&'a Name, Configuration<'a, Group>>,
    /// The `(user, group)` pairs of the `m` lines.
    pub memberships: BTreeSet<(&'a Name, &'a Name)>,
}

impl<'a> Index<'a> {
    /// Add an entry from the file at `path`, unless an earlier entry
    /// configures the same name.
    fn insert(&mut self, path: &'a Utf8Path, entry: &'a Entry) {
        match entry {
            Entry::User(user) => {
                if let Some(group) = user.implicit_group() {
                    self.groups
                        .entry(&user.name)
                        .or_insert_with(|| Configuration { path, entry: group });
                }
                self.users
                    .entry(&user.name)
                    .or_insert_with(|| Configuration {
                        path,
                        entry: user.clone(),
                    });
            }
            Entry::Group(group) => {
                self.groups
                    .entry(&group.name)
                    .or_insert_with(|| Configuration {
                        path,
                        entry: group.clone(),
                    });
            }
            Entry::Membership(membership) => {
                self.memberships
                    .insert((&membership.user, &membership.group));
            }
            Entry::Range(_) => {}
        }
    }

    /// The file and UID that configure the user `name`.
    #[must_use]
    pub fn uid(&self, name: &Name) -> Option<(&'a Utf8Path, &IdSource<Uid>)> {
        self.users
            .get(name)
            .map(|user| (user.path, &user.entry.uid))
    }

    /// The file that configures the user or group named by `entry`, if any.
    #[must_use]
    pub fn file_of(&self, entry: &Entry) -> Option<&'a Utf8Path> {
        match entry {
            Entry::User(user) => self.users.get(&user.name).map(|user| user.path),
            Entry::Group(group) => self.groups.get(&group.name).map(|group| group.path),
            Entry::Membership(membership) => self.users.get(&membership.user).map(|user| user.path),
            Entry::Range(_) => None,
        }
    }

    /// The file and GID that configure the group `name`.
    #[must_use]
    pub fn gid(&self, name: &Name) -> Option<(&'a Utf8Path, &IdSource<Gid>)> {
        self.groups
            .get(name)
            .map(|group| (group.path, &group.entry.gid))
    }
}

/// Index the entries of the files, which are in the order systemd-sysusers
/// applies them.
impl<'a> FromIterator<&'a ConfigFile> for Index<'a> {
    fn from_iter<I: IntoIterator<Item = &'a ConfigFile>>(files: I) -> Self {
        let mut index = Self::default();
        for file in files {
            for entry in &file.entries {
                index.insert(&file.path, entry);
            }
        }
        index
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::*;
    use crate::sysusers::parse;
    use crate::testutil::{gid, uid};

    fn file(path: &str, content: &str) -> Result<ConfigFile> {
        Ok(ConfigFile {
            path: path.into(),
            entries: parse(content)?,
        })
    }

    #[test]
    fn first_file_wins_and_u_lines_imply_groups() -> Result<()> {
        let files = [
            file(
                "usr/lib/sysusers.d/00-lock.conf",
                "g avahi 900\nu avahi 900\nm avahi audio\n",
            )?,
            file(
                "usr/lib/sysusers.d/avahi.conf",
                "u avahi -\nu! http 33:http\ng http 33\nr - 500-900\n",
            )?,
        ];
        let index: Index = files.iter().collect();
        let name = |name: &str| name.parse::<Name>().expect("a valid name");
        let lock = Utf8Path::new("usr/lib/sysusers.d/00-lock.conf");
        let avahi_conf = Utf8Path::new("usr/lib/sysusers.d/avahi.conf");

        assert_eq!(
            index.uid(&name("avahi")),
            Some((lock, &IdSource::Fixed(uid(900))))
        );
        assert_eq!(
            index.gid(&name("avahi")),
            Some((lock, &IdSource::Fixed(gid(900))))
        );
        assert!(!index.users[&name("avahi")].entry.locked);

        let http = &index.users[&name("http")];
        assert_eq!((http.path, http.entry.locked), (avahi_conf, true));
        // `u http 33:http` names its primary group, so it implies no group.
        assert_eq!(
            index.gid(&name("http")),
            Some((avahi_conf, &IdSource::Fixed(gid(33))))
        );

        assert!(
            index
                .memberships
                .contains(&(&name("avahi"), &name("audio")))
        );
        Ok(())
    }
}
