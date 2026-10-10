//! Patch the tmpfiles.d(5) files systemd ships to match the toplevel symlinks
//! (see layout.rs). Also read which paths the tmpfiles.d files in the image
//! declare, so the generated /var entries (see var.rs) do not repeat them.

use std::collections::HashSet;
use std::fmt::{self, Write};
use std::process::Command;

use anyhow::{Context, Result};
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::{CapStdExtDirExt, CapStdExtDirExtUtf8};
use tracing::debug;

use crate::command::CommandRunExt;

/// Where packages install their tmpfiles.d files. Generated files go here too.
pub(super) const USR_TMPFILES_DIR: &str = "usr/lib/tmpfiles.d";

/// Specifiers systemd expands to a directory at the start of a tmpfiles.d
/// path.
const PATH_SPECIFIERS: &[(&str, &str)] = &[
    ("%C", "/var/cache"),
    ("%E", "/etc"),
    ("%L", "/var/log"),
    ("%S", "/var/lib"),
    ("%T", "/tmp"),
    ("%t", "/run"),
    ("%V", "/var/tmp"),
];

/// Escape a path for a tmpfiles.d line.
fn escape_path(path: &str) -> String {
    let mut escaped = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r' | '\\' | '\'' | '"' => {
                let _ = write!(escaped, "\\x{:02x}", u32::from(c));
            }
            '%' => escaped.push_str("%%"),
            c => escaped.push(c),
        }
    }
    escaped
}

/// This type is modeled after a tmpfiles.d(5) line: type, path, mode, user,
/// group, age and argument, separated by whitespace, with `-` for a field
/// that does not apply. Only the two types bootc's own generator writes are
/// modeled.
pub(super) enum Entry<'a> {
    /// A `d` line. systemd creates the directory with this mode and owner.
    Directory {
        path: &'a str,
        mode: u32,
        user: &'a str,
        group: &'a str,
    },
    /// An `L` line. systemd creates the symlink, with the target in the
    /// argument field. Mode and owner are ignored for symlinks.
    Symlink { path: &'a str, target: &'a str },
}

impl Entry<'_> {
    /// The path as written in tmpfiles.d, escaped.
    pub(super) fn path(&self) -> String {
        match self {
            Self::Directory { path, .. } | Self::Symlink { path, .. } => escape_path(path),
        }
    }
}

impl fmt::Display for Entry<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Directory {
                mode, user, group, ..
            } => {
                write!(f, "d {} {mode:04o} {user} {group} -", self.path())
            }
            Self::Symlink { target, .. } => {
                write!(f, "L {} - - - - {}", self.path(), escape_path(target))
            }
        }
    }
}

/// The path field of a tmpfiles.d line, or `None` for a comment or blank line.
pub(super) fn entry_path(line: &str) -> Option<&str> {
    let mut fields = line.split_whitespace();
    if fields.next()?.starts_with('#') {
        return None;
    }
    fields.next()
}

/// Expand the systemd specifier a tmpfiles.d path starts with, if any.
fn expand_specifier(path: &str) -> String {
    PATH_SPECIFIERS
        .iter()
        .find_map(|(specifier, target)| {
            let rest = path.strip_prefix(specifier)?;
            (rest.is_empty() || rest.starts_with('/')).then(|| format!("{target}{rest}"))
        })
        .unwrap_or_else(|| path.to_owned())
}

/// The tmpfiles.d files of the rootfs at `/`, merged as systemd-tmpfiles
/// reads them.
///
/// # Errors
///
/// Fails if systemd-tmpfiles fails.
pub(super) fn cat_config() -> Result<String> {
    Command::new("systemd-tmpfiles")
        .args(["--root=/", "--cat-config"])
        .output_string()
}

/// Every path the tmpfiles.d `config` declares.
pub(super) fn declared_paths(config: &str) -> HashSet<String> {
    config
        .lines()
        .filter_map(entry_path)
        .map(expand_specifier)
        .collect()
}

/// Patch the tmpfiles.d files systemd ships to match the toplevel symlinks.
pub(super) fn patch_tmpfiles(root: &Dir) -> Result<()> {
    debug!("patching tmpfiles.d entries");

    // home.conf turns /home and /srv into real directories, which conflicts
    // with the /home -> var/home and /srv -> var/srv symlinks. Nothing else in
    // the file applies.
    root.remove_file_optional(format!("{USR_TMPFILES_DIR}/home.conf"))
        .context("removing home.conf")?;

    // provision.conf writes the credential-provisioned root ssh key to /root,
    // now a symlink to /var/roothome. Point it there directly. Drop its
    // /var/roothome line, since the layout step declares that directory and
    // systemd warns about duplicates at boot.
    let provision = format!("{USR_TMPFILES_DIR}/provision.conf");
    if let Some(content) = root
        .as_cap_std()
        .read_to_string_optional(&provision)
        .context("reading provision.conf")?
    {
        let patched: String = content
            .lines()
            .map(|line| line.replace(" /root", " /var/roothome"))
            .filter(|line| !line.starts_with("d- /var/roothome "))
            .map(|line| line + "\n")
            .collect();
        root.write(&provision, patched)
            .context("writing provision.conf")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use indoc::indoc;

    use super::*;
    use crate::testutil::rootfs;

    #[test]
    fn removes_home_conf() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all(USR_TMPFILES_DIR)?;
        root.write(
            format!("{USR_TMPFILES_DIR}/home.conf"),
            "Q /home 0755 - - -\n",
        )?;

        patch_tmpfiles(&root)?;

        assert!(!root.exists(format!("{USR_TMPFILES_DIR}/home.conf")));
        Ok(())
    }

    #[test]
    fn points_provision_conf_at_var_roothome() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all(USR_TMPFILES_DIR)?;
        root.write(
            format!("{USR_TMPFILES_DIR}/provision.conf"),
            indoc! {"
                # Provision SSH key for root
                d- /root/.ssh :0700 root :root -
                f^ /root/.ssh/authorized_keys :0600 root :root - ssh.authorized_keys.root
                d- /var/roothome :0700 root :root -
            "},
        )?;

        patch_tmpfiles(&root)?;

        assert_eq!(
            root.read_to_string(format!("{USR_TMPFILES_DIR}/provision.conf"))?,
            indoc! {"
                # Provision SSH key for root
                d- /var/roothome/.ssh :0700 root :root -
                f^ /var/roothome/.ssh/authorized_keys :0600 root :root - ssh.authorized_keys.root
            "}
        );
        Ok(())
    }

    #[test]
    fn is_idempotent() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all(USR_TMPFILES_DIR)?;
        root.write(
            format!("{USR_TMPFILES_DIR}/provision.conf"),
            indoc! {"
                d- /root/.ssh :0700 root :root -
                d- /var/roothome :0700 root :root -
            "},
        )?;

        patch_tmpfiles(&root)?;
        patch_tmpfiles(&root)?;

        assert_eq!(
            root.read_to_string(format!("{USR_TMPFILES_DIR}/provision.conf"))?,
            "d- /var/roothome/.ssh :0700 root :root -\n"
        );
        Ok(())
    }

    #[test]
    fn skips_missing_files() -> Result<()> {
        let root = rootfs()?;
        patch_tmpfiles(&root)?;
        Ok(())
    }

    #[test]
    fn collects_declared_paths() {
        let declared = declared_paths(indoc! {"
            # /usr/lib/tmpfiles.d/var.conf
            # comment

            d /var/log 0755 - - -
            d %S/containers 0755 root root -
            L /var/lock - - - - ../run/lock
        "});
        assert_eq!(
            declared,
            ["/var/log", "/var/lib/containers", "/var/lock"]
                .map(String::from)
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn extracts_entry_paths() {
        assert_eq!(entry_path("d /var/log 0755 - - -"), Some("/var/log"));
        assert_eq!(
            entry_path("  L /var/lock - - - - ../run/lock"),
            Some("/var/lock")
        );
        assert_eq!(entry_path("# d /commented 0755 - - -"), None);
        assert_eq!(entry_path(""), None);
        assert_eq!(entry_path("d"), None);
    }

    #[test]
    fn displays_entries() {
        let dir = Entry::Directory {
            path: "/var/lib/with space",
            mode: 0o700,
            user: "foo",
            group: "bar",
        };
        assert_eq!(dir.to_string(), "d /var/lib/with\\x20space 0700 foo bar -");
        let link = Entry::Symlink {
            path: "/var/lock",
            target: "../run/lock",
        };
        assert_eq!(link.to_string(), "L /var/lock - - - - ../run/lock");
    }

    #[test]
    fn escapes_paths() {
        assert_eq!(escape_path("/var/lib/plain"), "/var/lib/plain");
        assert_eq!(
            escape_path("/var/lib/with space\tand 'quotes' \"too\" 100%\\"),
            "/var/lib/with\\x20space\\x09and\\x20\\x27quotes\\x27\\x20\\x22too\\x22\\x20100%%\\x5c"
        );
    }

    #[test]
    fn expands_leading_specifiers() {
        assert_eq!(expand_specifier("%S/containers"), "/var/lib/containers");
        assert_eq!(expand_specifier("%t"), "/run");
        assert_eq!(expand_specifier("/var/lib/%S"), "/var/lib/%S");
        assert_eq!(expand_specifier("%Z/unknown"), "%Z/unknown");
    }
}
