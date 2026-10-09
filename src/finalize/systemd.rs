//! Check that the image's systemd is new enough for bootc-imagectl.

use std::process::Command;

use anyhow::{Context, Result, ensure};

use crate::command::CommandRunExt;

/// The oldest systemd bootc-imagectl works with. finalize needs
/// `systemd-sysusers --tldr` from systemd 255.
const MIN_VERSION: u32 = 255;

/// The major version of the image's systemd.
pub(super) fn version() -> Result<u32> {
    let output = Command::new("systemctl").arg("--version").output_string()?;
    parse_version(&output)
}

/// Fail if the image's systemd is older than [`MIN_VERSION`].
pub(super) fn check() -> Result<()> {
    let version = version()?;
    ensure!(
        version >= MIN_VERSION,
        "the image has systemd {version} but bootc-imagectl needs systemd {MIN_VERSION} or newer"
    );
    Ok(())
}

/// The major version in `systemctl --version` output, e.g. 259 in
/// `systemd 259 (259.5-0ubuntu3.4)`.
fn parse_version(output: &str) -> Result<u32> {
    output
        .split_whitespace()
        .nth(1)
        .and_then(|version| version.parse().ok())
        .with_context(|| format!("parsing the systemd version in {output:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_version() {
        assert_eq!(
            parse_version("systemd 259 (259.5-0ubuntu3.4)\n+PAM +AUDIT\n").unwrap(),
            259
        );
        assert_eq!(parse_version("systemd 261 (261-1.fc45)\n").unwrap(), 261);
    }

    #[test]
    fn fails_to_parse_unexpected_version() {
        assert!(parse_version("systemd unknown\n").is_err());
        assert!(parse_version("").is_err());
    }
}
