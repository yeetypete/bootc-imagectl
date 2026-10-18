//! Kernel arguments required in all images.

use anyhow::{Context, Result};
use cap_std_ext::camino::Utf8Path;
use cap_std_ext::cap_std::fs_utf8::Dir;
use serde::{Deserialize, Serialize};

use super::GENERATED_HEADER;

/// The kargs.d directory, relative to the rootfs.
pub(super) const KARGS_DIR: &str = "usr/lib/bootc/kargs.d";

/// A kargs.d file, see bootc-kernel-arguments(7).
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct Config {
    pub(super) kargs: Vec<String>,
    /// If set, bootc applies the file only on these architectures.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) match_architectures: Option<Vec<String>>,
}

impl Config {
    /// Whether bootc reads the kargs.d file `name`.
    pub(super) fn filename_matches(name: &str) -> bool {
        Utf8Path::new(name).extension() == Some("toml")
    }

    /// Whether bootc applies the file on the architecture finalize runs on.
    pub(super) fn matches_architecture(&self) -> bool {
        self.match_architectures
            .as_ref()
            .is_none_or(|archs| archs.iter().any(|arch| arch == std::env::consts::ARCH))
    }
}

/// The kargs.d file.
const KARGS_FILE: &str = "10-bootc-imagectl.toml";

/// Mount the root read-write, so that /etc and /var are writable.
const KARGS: &[&str] = &["rw"];

/// Write the kargs.d file.
///
/// # Errors
///
/// Fails if the file cannot be written.
pub(super) fn finalize(root: &Dir) -> Result<()> {
    root.create_dir_all(KARGS_DIR)
        .with_context(|| format!("creating /{KARGS_DIR}"))?;
    let path = format!("{KARGS_DIR}/{KARGS_FILE}");
    let config = Config {
        kargs: KARGS.iter().map(|karg| (*karg).to_owned()).collect(),
        match_architectures: None,
    };
    root.write(
        &path,
        format!("{GENERATED_HEADER}\n{}", toml::to_string(&config)?),
    )
    .with_context(|| format!("writing /{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::rootfs;

    #[test]
    fn mounts_root_read_write() -> Result<()> {
        let root = rootfs()?;
        finalize(&root)?;
        let file = root.read_to_string(format!("{KARGS_DIR}/{KARGS_FILE}"))?;
        assert!(file.starts_with(GENERATED_HEADER), "{file}");
        let config: Config = toml::from_str(&file)?;
        assert_eq!(config.kargs, ["rw"]);
        Ok(())
    }
}
