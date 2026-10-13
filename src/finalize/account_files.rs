//! Mount the account files in /etc (passwd, shadow, group and gshadow)
//! read-only at boot, once systemd-sysusers and systemd-firstboot have
//! written them.
//!
//! This ensures that a booted system cannot have accounts added outside
//! of /usr/lib/userdb.

use anyhow::{Context, Result};
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::CapStdExtDirExtUtf8;
use tracing::debug;

use super::GENERATED_HEADER;
use super::units::VENDOR_UNIT_DIR;
use crate::passwd::{Entry, Group, Gshadow, Passwd, Shadow};

/// The target that pulls in the mount units, relative to
/// [`VENDOR_UNIT_DIR`].
const WANTS_DIR: &str = "sysinit.target.wants";

const FILES: [&str; 4] = [Passwd::PATH, Shadow::PATH, Group::PATH, Gshadow::PATH];

fn unit_name(file: &str) -> String {
    format!("{}.mount", file.replace('/', "-"))
}

fn mount_unit(file: &str) -> String {
    format!(
        "{GENERATED_HEADER}
[Unit]
Description=Read-only /{file}
DefaultDependencies=no
After=systemd-sysusers.service systemd-firstboot.service
Before=sysinit.target umount.target
Conflicts=umount.target

[Mount]
What=/{file}
Where=/{file}
Options=bind,ro
"
    )
}

/// Write a mount unit for every account file and enable it.
pub(super) fn finalize(root: &Dir) -> Result<()> {
    let wants = format!("{VENDOR_UNIT_DIR}/{WANTS_DIR}");
    root.create_dir_all(&wants)
        .with_context(|| format!("creating /{wants}"))?;
    for file in FILES {
        let name = unit_name(file);
        let unit = format!("{VENDOR_UNIT_DIR}/{name}");
        debug!("writing /{unit}");
        root.atomic_write(&unit, mount_unit(file))
            .with_context(|| format!("writing /{unit}"))?;
        let link = format!("{wants}/{name}");
        root.remove_file_optional(&link)?;
        root.symlink(format!("../{name}"), &link)
            .with_context(|| format!("enabling /{unit}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::rootfs;

    #[test]
    fn enables_mount_unit_for_every_account_file() -> Result<()> {
        let root = rootfs()?;
        finalize(&root)?;
        for name in [
            "etc-passwd.mount",
            "etc-shadow.mount",
            "etc-group.mount",
            "etc-gshadow.mount",
        ] {
            assert_eq!(
                root.read_link_contents(format!("{VENDOR_UNIT_DIR}/{WANTS_DIR}/{name}"))?,
                format!("../{name}")
            );
        }
        let unit = root.read_to_string(format!("{VENDOR_UNIT_DIR}/etc-shadow.mount"))?;
        assert!(
            unit.contains("What=/etc/shadow\nWhere=/etc/shadow\nOptions=bind,ro\n"),
            "{unit}"
        );
        Ok(())
    }
}
