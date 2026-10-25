//! Mount a tmpfs on /tmp when missing.
//!
//! bootc requires a writable /tmp, e.g. `bootc status` mounts the ESP below it,
//! but the root is read-only. Most distributions mount a tmpfs there with
//! systemd's `tmp.mount`. Debian and Ubuntu before 25.04 only ship it as an
//! example in /usr/share/systemd.

use anyhow::Result;
use cap_std_ext::cap_std::fs_utf8::Dir;
use tracing::info;

use super::units::{self, UnitFile};

/// systemd's unit mounting a tmpfs on /tmp.
const UNIT: &str = "tmp.mount";

/// [`UNIT`] from systemd 255, the oldest systemd bootc-imagectl supports.
const UNIT_CONTENT: &str = include_str!("../units/tmp.mount");

/// Install and enable [`UNIT`] if the image has none. A masked or disabled
/// one is left as the image has it.
///
/// # Errors
///
/// Fails if the unit files cannot be listed or the unit cannot be written.
pub(super) fn finalize(root: &Dir) -> Result<()> {
    install(root, &units::unit_files()?)
}

fn install(root: &Dir, unit_files: &[UnitFile]) -> Result<()> {
    if unit_files.iter().any(|unit| unit.name.as_str() == UNIT) {
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
    fn keeps_existing_unit() -> Result<()> {
        let root = rootfs()?;
        for state in [EnablementState::Static, EnablementState::Masked] {
            install(&root, &[unit_file(UNIT, state)?])?;
            assert!(!root.exists(format!("{VENDOR_UNIT_DIR}/{UNIT}")));
        }
        Ok(())
    }
}
