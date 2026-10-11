//! Check the enabled units in the booted image.

use std::path::Path;
use std::time::SystemTime;

use anyhow::{Context as _, Result};

use crate::boot_time;

/// The first boot, which applied the systemd presets, changed no symlink in
/// /etc/systemd/system.
pub(crate) fn unchanged_after_first_boot() -> Result<()> {
    // Enabling a unit creates a symlink and disabling one removes it. Either
    // updates the mtime of the symlink's directory.
    assert_written_before(Path::new("/etc/systemd/system"), boot_time())
}

/// Every entry under `path`, including `path`, was written before `time`.
fn assert_written_before(path: &Path, time: SystemTime) -> Result<()> {
    let metadata = path
        .symlink_metadata()
        .with_context(|| format!("reading {}", path.display()))?;
    assert!(
        metadata.modified()? < time,
        "{} was written at boot",
        path.display()
    );
    if metadata.is_dir() {
        for entry in path.read_dir()? {
            assert_written_before(&entry?.path(), time)?;
        }
    }
    Ok(())
}
