//! Mount a tmpfs on /tmp when missing.
//!
//! bootc requires a writable /tmp, e.g. `bootc status` mounts the ESP below it,
//! but the root is read-only. Most distributions mount a tmpfs there with
//! systemd's `tmp.mount`. Debian and Ubuntu before 25.04 only ship it as an
//! example in /usr/share/systemd.

use anyhow::{Result, ensure};
use cap_std_ext::cap_std::fs_utf8::Dir;
use tracing::info;

use super::units::{self, UnitFile};

/// systemd's unit mounting a tmpfs on /tmp.
const UNIT: &str = "tmp.mount";

/// [`UNIT`] from systemd 255, the oldest systemd bootc-imagectl supports.
const UNIT_CONTENT: &str = include_str!("../units/tmp.mount");

/// Install and enable [`UNIT`] if the image has none.
///
/// # Errors
///
/// Fails if the image's [`UNIT`] is masked or not enabled, or if the unit
/// files cannot be listed or the unit cannot be written.
pub(super) fn finalize(root: &Dir) -> Result<()> {
    install(root, &units::unit_files()?)
}

fn install(root: &Dir, unit_files: &[UnitFile]) -> Result<()> {
    if unit_files.iter().any(|unit| unit.name.as_str() == UNIT) {
        ensure!(
            units::is_enabled(root, unit_files, UNIT)?,
            "{UNIT} is masked or not enabled, but bootc needs a writable /tmp. \
             help: unmask or enable {UNIT} in the image"
        );
        return Ok(());
    }
    units::write_enabled_unit(root, UNIT, UNIT_CONTENT, "local-fs.target")?;
    info!("installed {UNIT}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finalize::units::{EnablementState, VENDOR_UNIT_DIR};
    use crate::testutil::rootfs;

    fn unit_file(name: &str, state: EnablementState) -> Result<UnitFile> {
        Ok(UnitFile {
            name: name.parse()?,
            state,
        })
    }

    #[test]
    fn installs_missing_unit() -> Result<()> {
        let root = rootfs()?;
        install(&root, &[])?;
        assert_eq!(
            root.read_to_string(format!("{VENDOR_UNIT_DIR}/{UNIT}"))?,
            UNIT_CONTENT
        );
        assert_eq!(
            root.read_link_contents(format!("{VENDOR_UNIT_DIR}/local-fs.target.wants/{UNIT}"))?,
            format!("../{UNIT}")
        );
        Ok(())
    }

    #[test]
    fn keeps_enabled_unit() -> Result<()> {
        let root = rootfs()?;
        install(&root, &[unit_file(UNIT, EnablementState::Enabled)?])?;
        assert!(!root.exists(format!("{VENDOR_UNIT_DIR}/{UNIT}")));

        root.create_dir_all(format!("{VENDOR_UNIT_DIR}/local-fs.target.wants"))?;
        root.symlink(
            format!("../{UNIT}"),
            format!("{VENDOR_UNIT_DIR}/local-fs.target.wants/{UNIT}"),
        )?;
        install(&root, &[unit_file(UNIT, EnablementState::Static)?])?;
        assert!(!root.exists(format!("{VENDOR_UNIT_DIR}/{UNIT}")));
        Ok(())
    }

    #[test]
    fn rejects_masked_or_disabled_unit() -> Result<()> {
        let root = rootfs()?;
        for state in [
            EnablementState::Masked,
            EnablementState::Disabled,
            EnablementState::Static,
        ] {
            assert!(install(&root, &[unit_file(UNIT, state)?]).is_err());
        }
        Ok(())
    }
}
