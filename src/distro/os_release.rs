//! A subset of os-release(5) needed to identify a distribution.

use anyhow::{Context, Result, bail};
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::CapStdExtDirExt;

/// Identification fields from os-release.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct OsRelease {
    /// `ID`: the distribution, lowercase.
    pub id: String,
    /// `ID_LIKE`: distributions this one derives from, closest first.
    pub id_like: Vec<String>,
}

impl OsRelease {
    /// This distribution and those it derives from, closest first.
    pub fn lineage(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.id.as_str()).chain(self.id_like.iter().map(String::as_str))
    }
}

/// Read the rootfs's os-release. Per the spec `/etc/os-release` takes
/// precedence, and is normally a relative symlink to the `/usr/lib` copy.
pub(super) fn read(root: &Dir) -> Result<OsRelease> {
    for path in ["etc/os-release", "usr/lib/os-release"] {
        if let Some(content) = root
            .as_cap_std()
            .read_to_string_optional(path)
            .with_context(|| format!("reading /{path}"))?
        {
            return parse(&content).with_context(|| format!("parsing /{path}"));
        }
    }
    bail!("No os-release file in the rootfs.")
}

/// Parse os-release content. `ID` is required.
fn parse(content: &str) -> Result<OsRelease> {
    let mut id = None;
    let mut id_like = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = unquote(value.trim());
        match key.trim() {
            "ID" => id = Some(value.to_owned()),
            "ID_LIKE" => id_like = value.split_whitespace().map(str::to_owned).collect(),
            _ => {}
        }
    }
    let Some(id) = id else {
        bail!("os-release has no ID field")
    };
    Ok(OsRelease { id, id_like })
}

/// Strip one layer of matching quotes.
fn unquote(value: &str) -> &str {
    ['"', '\'']
        .into_iter()
        .find_map(|quote| value.strip_prefix(quote)?.strip_suffix(quote))
        .unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use indoc::indoc;

    use super::*;
    use crate::testutil::rootfs;

    /// A rootfs laid out like Debian, Ubuntu and Fedora images.
    /// `/etc/os-release` is a relative symlink to `/usr/lib/os-release`.
    fn with_os_release(content: &str) -> Result<cap_std_ext::cap_tempfile::utf8::TempDir> {
        let root = rootfs()?;
        root.create_dir_all("usr/lib")?;
        root.create_dir_all("etc")?;
        root.write("usr/lib/os-release", content)?;
        root.symlink("../usr/lib/os-release", "etc/os-release")?;
        Ok(root)
    }

    #[test]
    fn reads_through_etc_symlink() -> Result<()> {
        let root = with_os_release("ID=debian\n")?;
        assert_eq!(read(&root)?.id, "debian");
        Ok(())
    }

    #[test]
    fn etc_takes_precedence_over_usr_lib() -> Result<()> {
        let root = with_os_release("ID=fedora\n")?;
        root.remove_file("etc/os-release")?;
        root.write("etc/os-release", "ID=debian\n")?;
        assert_eq!(read(&root)?.id, "debian");
        Ok(())
    }

    #[test]
    fn falls_back_to_usr_lib() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("usr/lib")?;
        root.write("usr/lib/os-release", "ID=debian\n")?;
        assert_eq!(read(&root)?.id, "debian");
        Ok(())
    }

    #[test]
    fn fails_without_os_release() -> Result<()> {
        let root = rootfs()?;
        assert!(read(&root).is_err());
        Ok(())
    }

    #[test]
    fn parses_os_release_contents() -> Result<()> {
        let release = parse(indoc! {r#"
            # comment
            NAME="Ubuntu"
            ID=ubuntu
            ID_LIKE='debian'

            MALFORMED LINE
        "#})?;
        assert_eq!(
            release,
            OsRelease {
                id: "ubuntu".into(),
                id_like: vec!["debian".into()],
            }
        );
        Ok(())
    }

    #[test]
    fn splits_id_like_on_whitespace() -> Result<()> {
        let release = parse(indoc! {r#"
            ID=almalinux
            ID_LIKE="rhel centos fedora"
        "#})?;
        assert_eq!(release.id_like, ["rhel", "centos", "fedora"]);
        Ok(())
    }

    #[test]
    fn fails_without_id() {
        assert!(parse("ID_LIKE=debian\n").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn lineage_starts_with_id_then_follows_id_like() -> Result<()> {
        let release = parse(indoc! {r#"
            ID=linuxmint
            ID_LIKE="ubuntu debian"
        "#})?;
        let lineage: Vec<&str> = release.lineage().collect();
        assert_eq!(lineage, ["linuxmint", "ubuntu", "debian"]);

        let release = parse("ID=debian\n")?;
        let lineage: Vec<&str> = release.lineage().collect();
        assert_eq!(lineage, ["debian"], "no ID_LIKE means just the ID");
        Ok(())
    }
}
