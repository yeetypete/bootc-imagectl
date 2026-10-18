//! Check the enabled units in the booted image.

use std::path::Path;

use anyhow::Result;
use bootc_imagectl::testing::written_before_boot;

/// The first boot, which applied the systemd presets, changed no symlink in
/// /etc/systemd/system.
#[test]
fn keeps_units_unchanged_after_first_boot() -> Result<()> {
    // Enabling a unit creates a symlink and disabling one removes it. Either
    // updates the mtime of the symlink's directory.
    assert_written_before_boot(Path::new("/etc/systemd/system"))
}

/// Every entry under `path`, including `path`, was written before the boot.
fn assert_written_before_boot(path: &Path) -> Result<()> {
    assert!(
        written_before_boot(path)?,
        "{} was written at boot",
        path.display()
    );
    if path.symlink_metadata()?.is_dir() {
        for entry in path.read_dir()? {
            assert_written_before_boot(&entry?.path())?;
        }
    }
    Ok(())
}
