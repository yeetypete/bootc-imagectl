//! Check the account files in the booted image.

use std::path::Path;
use std::process::Command;

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;
use bootc_imagectl::testing::{self, IntegrationTest, integration_test, written_before_boot};
use cap_std_ext::cap_std::ambient_authority;
use cap_std_ext::cap_std::fs_utf8::Dir;
use rustix::fs::{StatVfsMountFlags, statvfs};

/// The first boot, including systemd-sysusers, left the account files in
/// /etc unchanged.
fn keeps_account_files_unchanged_after_first_boot() -> Result<()> {
    let result = Command::new("systemctl")
        .args(["show", "-P", "Result", "systemd-sysusers.service"])
        .run_get_string()?;
    assert_eq!(result.trim(), "success");

    // Check that the boot rewrote no account file. /etc is an overlay, so a
    // rewrite copies the file with a new mtime.
    for file in ["/etc/passwd", "/etc/shadow", "/etc/group", "/etc/gshadow"] {
        assert!(
            written_before_boot(Path::new(file))?,
            "{file} was written at boot"
        );
    }

    let root = Dir::open_ambient_dir("/", ambient_authority())?;
    testing::etc_contains_only_root_and_nobody(&root)
}

/// The account files are mounted read-only within a writable /etc, so adding
/// a group fails.
fn mounts_account_files_read_only() -> Result<()> {
    assert!(!statvfs("/etc")?.f_flag.contains(StatVfsMountFlags::RDONLY));
    for file in ["/etc/passwd", "/etc/shadow", "/etc/group", "/etc/gshadow"] {
        let flags = statvfs(file)?.f_flag;
        assert!(flags.contains(StatVfsMountFlags::RDONLY), "{file}");
    }
    let added = Command::new("groupadd")
        .arg("bootc-imagectl-test")
        .status()?;
    assert!(!added.success());
    Ok(())
}

pub(crate) const TESTS: &[IntegrationTest] = &[
    integration_test!(keeps_account_files_unchanged_after_first_boot),
    integration_test!(mounts_account_files_read_only),
];
