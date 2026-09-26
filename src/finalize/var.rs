//! Record what the image ships in /var as tmpfiles.d entries, then empty it.
//! bootc populates /var only at install time, so tmpfiles.d has to recreate
//! on every machine what a package created there at build time.

use std::collections::{BTreeMap, HashSet};
use std::io::ErrorKind;
use std::ops::ControlFlow;
use std::path::Path;

use anyhow::{Context, Result};
use cap_std_ext::camino::{Utf8Path, Utf8PathBuf};
use cap_std_ext::cap_std::fs_utf8::{Dir, MetadataExt, Permissions, PermissionsExt};
use cap_std_ext::dirext::{CapStdExtDirExt, CapStdExtDirExtUtf8, WalkConfiguration};
use tracing::{debug, info};
use uzers::{Groups, Users, UsersCache};

use super::tmpfiles::{self, Entry};
use crate::distro::Distro;

/// Each run writes a new `bootc-imagectl-var-N.conf` and leaves earlier
/// ones alone. Derived images which run `bootc-imagectl finalize` again would
/// create their own files.
const GENERATED_PREFIX: &str = "bootc-imagectl-var-";

/// Directories kept in /var after finalize, and their modes. E.g. dracut
/// refuses to run without a tmpdir.
const VAR_DIRS: &[(&str, u32)] = &[("var/tmp", 0o1777)];

/// Symlinks systemd recreates at boot, kept so a derived image has them
/// between finalize and its own package installs.
const VAR_LINKS: &[(&str, &str)] = &[("var/run", "../run"), ("var/lock", "../run/lock")];

/// The tmpfiles.d file this run writes, numbered after the highest one
/// present.
fn next_tmpfiles_path(root: &Dir) -> Result<String> {
    let mut last = 0;
    if let Some(dir) = root.open_dir_optional(tmpfiles::USR_TMPFILES_DIR)? {
        for entry in dir.entries()? {
            let name = entry?.file_name()?;
            let number = name
                .strip_prefix(GENERATED_PREFIX)
                .and_then(|name| name.strip_suffix(".conf"))
                .and_then(|number| number.parse::<u32>().ok());
            if let Some(number) = number {
                last = last.max(number);
            }
        }
    }
    Ok(format!(
        "{}/{GENERATED_PREFIX}{}.conf",
        tmpfiles::USR_TMPFILES_DIR,
        last + 1
    ))
}

/// Keep `entry` unless another tmpfiles.d file already declares its path.
fn record(entries: &mut BTreeMap<String, String>, declared: &HashSet<String>, entry: &Entry<'_>) {
    let path = entry.path();
    if !declared.contains(&path) {
        entries.insert(path, entry.to_string());
    }
}

/// Move the package state out of /var, record what /var must contain at
/// boot, and empty it.
pub(super) fn finalize(root: &Dir, distro: &dyn Distro) -> Result<()> {
    distro
        .relocate_package_state(root)
        .context("relocating the package state")?;
    write_var_tmpfiles(root, &UsersCache::new()).context("generating /var tmpfiles.d entries")?;
    empty_var(root).context("emptying /var")
}

/// Write a tmpfiles.d file for directories and symlinks in /var.
fn write_var_tmpfiles(root: &Dir, db: &(impl Users + Groups)) -> Result<()> {
    debug!("generating /var tmpfiles.d entries");
    let declared = tmpfiles::declared_paths(root)?;
    let mut entries = BTreeMap::new();

    // Files are skipped, tmpfiles.d cannot recreate them.
    if let Some(var) = root.open_dir_optional("var")? {
        let config = WalkConfiguration::default().path_base(Path::new("/var"));
        // The walk yields byte paths, which tmpfiles.d needs as UTF-8.
        let var = var.as_cap_std();
        var.walk(&config, |e| -> Result<ControlFlow<()>> {
            let path = e
                .path
                .to_str()
                .with_context(|| format!("{} is not UTF-8", e.path.display()))?;
            if e.file_type.is_symlink() {
                let target = e.dir.read_link_contents(e.filename)?;
                let target = target
                    .to_str()
                    .with_context(|| format!("the target of {path} is not UTF-8"))?;
                let entry = tmpfiles::Entry::Symlink { path, target };
                record(&mut entries, &declared, &entry);
            } else if e.file_type.is_dir() {
                let meta = e.entry.metadata()?;
                let (uid, gid) = (meta.uid(), meta.gid());
                let user = db
                    .get_user_by_uid(uid)
                    .with_context(|| format!("{path} is owned by uid {uid}, which no user has"))?;
                let group = db
                    .get_group_by_gid(gid)
                    .with_context(|| format!("{path} is owned by gid {gid}, which no group has"))?;
                let (user, group) = (
                    user.name().to_string_lossy(),
                    group.name().to_string_lossy(),
                );
                let entry = tmpfiles::Entry::Directory {
                    path,
                    mode: meta.mode() & 0o7777,
                    user: &user,
                    group: &group,
                };
                record(&mut entries, &declared, &entry);
            } else {
                debug!("dropping {path}, only directories and symlinks are recorded");
            }
            Ok(ControlFlow::Continue(()))
        })
        .context("scanning /var")?;
    }

    if entries.is_empty() {
        debug!("everything in /var is already declared");
        return Ok(());
    }
    let mut content = String::from("# Generated by bootc-imagectl. Do not edit.\n");
    for line in entries.values() {
        content.push_str(line);
        content.push('\n');
    }
    let tmpfiles_path = next_tmpfiles_path(root)?;
    root.create_dir_all(tmpfiles::USR_TMPFILES_DIR)?;
    root.write(&tmpfiles_path, content)
        .with_context(|| format!("writing /{tmpfiles_path}"))?;
    info!(
        "recorded {} /var entries in /{tmpfiles_path}",
        entries.len()
    );
    Ok(())
}

/// The symlinks in /var that point into /usr, relative to the rootfs, and
/// their targets.
fn links_into_usr(root: &Dir) -> Result<Vec<(Utf8PathBuf, Utf8PathBuf)>> {
    let mut links = Vec::new();
    let Some(var) = root.open_dir_optional("var")? else {
        return Ok(links);
    };
    let config = WalkConfiguration::default().path_base(Path::new("var"));
    var.as_cap_std()
        .walk(&config, |e| -> Result<ControlFlow<()>> {
            if e.file_type.is_symlink() {
                let path = Utf8Path::from_path(e.path)
                    .with_context(|| format!("{} is not UTF-8", e.path.display()))?;
                let into_usr = root
                    .canonicalize(path)
                    .is_ok_and(|resolved| resolved.starts_with("usr"));
                if into_usr {
                    let target = Utf8PathBuf::try_from(e.dir.read_link_contents(e.filename)?)?;
                    links.push((path.to_owned(), target));
                }
            }
            Ok(ControlFlow::Continue(()))
        })
        .context("scanning /var for links into /usr")?;
    Ok(links)
}

/// Empty /var, /run and /tmp. tmpfiles.d recreates their contents at boot.
/// Symlinks from /var into /usr are kept.
fn empty_var(root: &Dir) -> Result<()> {
    debug!("emptying /var, /run and /tmp");
    let links = links_into_usr(root)?;
    for dir in ["run", "tmp"] {
        let Some(entries) = root.open_dir_optional(dir)? else {
            continue;
        };
        for entry in entries.entries()? {
            let name = entry?.file_name()?;
            match entries.remove_all_optional(&name) {
                // The container runtime bind-mounts files such as
                // /run/.containerenv and /run/secrets into the build.
                Err(e) if e.kind() == ErrorKind::ResourceBusy => {}
                result => {
                    result.with_context(|| format!("removing /{dir}/{name}"))?;
                }
            }
        }
    }

    if let Some(var) = root.open_dir_optional("var")? {
        for entry in var.entries()? {
            let name = entry?.file_name()?;
            var.remove_all_optional(&name)
                .with_context(|| format!("removing /var/{name}"))?;
        }
    }

    for &(dir, mode) in VAR_DIRS {
        root.create_dir_all(dir)?;
        root.set_permissions(dir, Permissions::from_mode(mode))
            .with_context(|| format!("setting the mode of /{dir}"))?;
    }
    for (link, target) in VAR_LINKS {
        root.symlink(target, link)
            .with_context(|| format!("linking /{link} -> {target}"))?;
    }
    for (link, target) in &links {
        if let Some(parent) = link.parent() {
            root.create_dir_all(parent)?;
        }
        root.symlink(target, link)
            .with_context(|| format!("linking /{link} -> {target}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use uzers::mock::MockUsers;
    use uzers::{Group, User};

    use indoc::{formatdoc, indoc};

    use super::*;
    use crate::testutil::rootfs;

    /// An account database naming the account the tests run as.
    fn accounts(root: &Dir) -> Result<MockUsers> {
        let meta = root.dir_metadata()?;
        let mut db = MockUsers::with_current_uid(meta.uid());
        db.add_user(User::new(meta.uid(), "foo", meta.gid()));
        db.add_group(Group::new(meta.gid(), "bar"));
        Ok(db)
    }

    /// An account database with no accounts at all.
    fn no_accounts() -> MockUsers {
        MockUsers::with_current_uid(0)
    }

    fn generated_path(number: u32) -> String {
        format!(
            "{}/{GENERATED_PREFIX}{number}.conf",
            tmpfiles::USR_TMPFILES_DIR
        )
    }

    fn generated(root: &Dir, number: u32) -> Result<String> {
        Ok(root.read_to_string(generated_path(number))?)
    }

    #[test]
    fn records_undeclared_directories_and_symlinks() -> Result<()> {
        let root = rootfs()?;
        let db = accounts(&root)?;
        root.create_dir_all("var/lib/private")?;
        root.set_permissions("var/lib/private", Permissions::from_mode(0o700))?;
        root.create_dir_all("var/cache/with space")?;
        root.write("var/lib/file", b"files are not recorded")?;
        root.symlink("lib", "var/link")?;
        root.symlink_contents("/var/lib/private", "var/absolute")?;
        root.create_dir_all(tmpfiles::USR_TMPFILES_DIR)?;
        root.write(
            format!("{}/var.conf", tmpfiles::USR_TMPFILES_DIR),
            indoc! {"
                d /var/lib 0755 - - -
                d %C 0755 - - -
                L /var/run - - - - ../run
            "},
        )?;

        write_var_tmpfiles(&root, &db)?;

        let mode = root.metadata("var")?.mode() & 0o7777;
        assert_eq!(
            generated(&root, 1)?,
            formatdoc! {"
                # Generated by bootc-imagectl. Do not edit.
                L /var/absolute - - - - /var/lib/private
                d /var/cache/with\\x20space {mode:04o} foo bar -
                d /var/lib/private 0700 foo bar -
                L /var/link - - - - lib
            "}
        );
        Ok(())
    }

    #[test]
    fn fails_on_owner_without_account() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("var/lib/x")?;
        let err = format!(
            "{:#}",
            write_var_tmpfiles(&root, &no_accounts()).unwrap_err()
        );
        assert!(err.contains("which no user has"), "{err}");
        Ok(())
    }

    #[test]
    fn writes_only_new_entries_to_new_file() -> Result<()> {
        let root = rootfs()?;
        let db = accounts(&root)?;
        root.create_dir_all(tmpfiles::USR_TMPFILES_DIR)?;
        let earlier = "d /var/lib/earlier 0750 root root -\n";
        root.write(generated_path(1), earlier)?;
        root.create_dir_all("var/lib/earlier")?;
        root.create_dir_all("var/lib/later")?;

        write_var_tmpfiles(&root, &db)?;

        assert_eq!(
            generated(&root, 1)?,
            earlier,
            "the earlier file is left alone"
        );
        let content = generated(&root, 2)?;
        assert!(content.contains("d /var/lib/later "), "{content}");
        assert!(!content.contains("earlier"), "{content}");
        Ok(())
    }

    #[test]
    fn writes_no_file_when_everything_is_declared() -> Result<()> {
        let root = rootfs()?;
        let db = accounts(&root)?;
        root.create_dir_all("var/lib")?;
        root.create_dir_all(tmpfiles::USR_TMPFILES_DIR)?;
        root.write(
            format!("{}/var.conf", tmpfiles::USR_TMPFILES_DIR),
            "d /var/lib 0755 - - -\n",
        )?;

        write_var_tmpfiles(&root, &db)?;

        assert!(!root.exists(generated_path(1)));
        Ok(())
    }

    #[test]
    fn empties_var_and_recreates_var_tmp_and_run_links() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("var/lib/deep/er")?;
        root.write("var/lib/deep/er/file", b"x")?;
        root.symlink("lib", "var/link")?;
        root.create_dir_all("run/systemd")?;
        root.write("run/file", b"x")?;
        root.create_dir_all("tmp/build")?;

        empty_var(&root)?;

        assert_eq!(root.read_dir("run")?.count(), 0);
        assert_eq!(root.read_dir("tmp")?.count(), 0);
        let mut kept: Vec<_> = root
            .read_dir("var")?
            .map(|e| e?.file_name())
            .collect::<Result<_, _>>()?;
        kept.sort();
        assert_eq!(kept, ["lock", "run", "tmp"]);
        assert_eq!(root.metadata("var/tmp")?.mode() & 0o7777, 0o1777);
        assert_eq!(root.read_link("var/run")?, "../run");
        assert_eq!(root.read_link("var/lock")?, "../run/lock");
        Ok(())
    }

    #[test]
    fn keeps_links_into_usr() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("usr/lib/sysimage/dpkg")?;
        root.create_dir_all("var/lib")?;
        root.create_dir_all("var/spool/mail")?;
        root.symlink("../../usr/lib/sysimage/dpkg", "var/lib/dpkg")?;
        root.symlink("spool/mail", "var/mail")?;

        empty_var(&root)?;

        assert_eq!(
            root.read_link_contents("var/lib/dpkg")?,
            "../../usr/lib/sysimage/dpkg"
        );
        assert!(!root.exists("var/mail"), "a link within /var is dropped");
        assert!(!root.exists("var/spool"));
        Ok(())
    }

    #[test]
    fn creates_var_tmp_on_empty_rootfs() -> Result<()> {
        let root = rootfs()?;
        write_var_tmpfiles(&root, &no_accounts())?;
        empty_var(&root)?;
        assert!(root.is_dir("var/tmp"));
        Ok(())
    }
}
