//! Check the installed system booted from its encrypted root.

use std::fs;
use std::process::Command;

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;
use rustix::fs::{StatVfsMountFlags, statvfs};
use serde::Deserialize;

/// The block devices, as reported by `lsblk --json --list`.
#[derive(Debug, Deserialize)]
struct Devices {
    blockdevices: Vec<Device>,
}

#[derive(Debug, Deserialize)]
struct Device {
    #[serde(rename = "type")]
    kind: String,
    mountpoints: Vec<Option<String>>,
}

#[test]
fn boots_to_running() -> Result<()> {
    let state = Command::new("systemctl")
        .arg("is-system-running")
        .output_string()?;
    assert_eq!(state.trim(), "running");
    Ok(())
}

#[test]
fn finds_root_by_partition_type() -> Result<()> {
    let cmdline = fs::read_to_string("/proc/cmdline")?;
    assert!(
        !cmdline
            .split_whitespace()
            .any(|arg| arg.starts_with("root=")),
        "{cmdline}"
    );
    Ok(())
}

#[test]
fn unlocks_encrypted_root() -> Result<()> {
    let output = Command::new("lsblk")
        .args(["--json", "--list", "--output", "TYPE,MOUNTPOINTS"])
        .output_string()?;
    let devices: Devices = serde_json::from_str(&output)?;
    let crypt = devices
        .blockdevices
        .iter()
        .find(|device| device.kind == "crypt")
        .expect("an unlocked LUKS device");
    assert!(
        crypt.mountpoints.iter().flatten().any(|m| m == "/sysroot"),
        "{crypt:?}"
    );
    Ok(())
}

#[test]
fn formats_root_with_requested_filesystem() -> Result<()> {
    let expected =
        std::env::var("BOOTC_IMAGECTL_TEST_FILESYSTEM").unwrap_or_else(|_| "ext4".into());
    let fstype = Command::new("findmnt")
        .args(["--noheadings", "--output", "FSTYPE", "/sysroot"])
        .output_string()?;
    assert_eq!(fstype.trim(), expected);
    Ok(())
}

#[test]
fn mounts_etc_and_var_writable() -> Result<()> {
    for path in ["/etc", "/var"] {
        let flags = statvfs(path)?.f_flag;
        assert!(!flags.contains(StatVfsMountFlags::RDONLY), "{path}");
    }
    Ok(())
}
