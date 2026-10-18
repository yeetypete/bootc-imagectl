//! Checks shared by the container, the install and the VM tests, built only
//! with their features.

use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result};
use cap_std_ext::cap_std::fs_utf8::Dir;
use rustix::time::{ClockId, clock_gettime};

use crate::id::{Gid, Uid};
use crate::passwd::{Entry as _, Group, Gshadow, Passwd, Shadow};

/// The account files in /etc contain only root and nobody.
///
/// # Errors
///
/// Fails if a file cannot be read.
///
/// # Panics
///
/// Panics if a file contains other accounts or a backup exists.
pub fn etc_contains_only_root_and_nobody(root: &Dir) -> Result<()> {
    let users = Passwd::read_all(root)?;
    let uids: Vec<Uid> = users.iter().map(|user| user.uid).collect();
    assert_eq!(uids, [Uid::ROOT, Uid::NOBODY]);
    let names: Vec<&str> = users.iter().map(|user| user.name.as_str()).collect();
    let shadow = Shadow::read_all(root)?;
    let shadow: Vec<&str> = shadow.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(shadow, names);

    let groups = Group::read_all(root)?;
    let gids: Vec<Gid> = groups.iter().map(|group| group.gid).collect();
    assert_eq!(gids, [Gid::ROOT, Gid::NOBODY]);
    let names: Vec<&str> = groups.iter().map(|group| group.name.as_str()).collect();
    let gshadow = Gshadow::read_all(root)?;
    let gshadow: Vec<&str> = gshadow.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(gshadow, names);

    for name in ["passwd", "shadow", "group", "gshadow"] {
        assert!(!root.exists(format!("etc/{name}-")), "/etc/{name}- exists");
    }
    Ok(())
}

/// The time the system booted.
///
/// # Panics
///
/// Panics if `CLOCK_BOOTTIME` is negative, which the kernel never reports.
#[must_use]
pub fn boot_time() -> SystemTime {
    let since_boot = Duration::try_from(clock_gettime(ClockId::Boottime))
        .expect("CLOCK_BOOTTIME is never negative");
    SystemTime::now() - since_boot
}

/// Whether `path`, or the symlink at `path`, was last written before the
/// system booted.
///
/// # Errors
///
/// Fails if the metadata of `path` cannot be read.
pub fn written_before_boot(path: &Path) -> Result<bool> {
    let modified = path
        .symlink_metadata()
        .with_context(|| format!("reading {}", path.display()))?
        .modified()?;
    Ok(modified < boot_time())
}
