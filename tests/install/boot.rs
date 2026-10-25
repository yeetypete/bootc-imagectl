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
    /// The parent device's name, if requested.
    pkname: Option<String>,
    mountpoints: Vec<Option<String>>,
}

/// The LUKS2 metadata, as reported by `cryptsetup luksDump
/// --dump-json-metadata`.
#[derive(Debug, Deserialize)]
struct LuksMetadata {
    config: LuksConfig,
}

#[derive(Debug, Deserialize)]
struct LuksConfig {
    #[serde(default)]
    flags: Vec<String>,
}

#[test]
fn boots_to_running() -> Result<()> {
    // sshd accepts root before the boot finishes.
    let state = Command::new("systemctl")
        .args(["is-system-running", "--wait"])
        .run_get_string()?;
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
    let devices: Devices = Command::new("lsblk")
        .args(["--json", "--list", "--output", "TYPE,MOUNTPOINTS"])
        .run_and_parse_json()?;
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
fn allows_discards_on_encrypted_root() -> Result<()> {
    let devices: Devices = Command::new("lsblk")
        .args(["--json", "--list", "--output", "TYPE,PKNAME,MOUNTPOINTS"])
        .run_and_parse_json()?;
    let partition = devices
        .blockdevices
        .iter()
        .find(|device| device.kind == "crypt")
        .and_then(|crypt| crypt.pkname.as_deref())
        .expect("the partition of an unlocked LUKS device");
    let metadata: LuksMetadata = Command::new("cryptsetup")
        .args(["luksDump", "--dump-json-metadata"])
        .arg(format!("/dev/{partition}"))
        .run_and_parse_json()?;
    assert!(
        metadata
            .config
            .flags
            .iter()
            .any(|flag| flag == "allow-discards"),
        "{:?}",
        metadata.config.flags
    );
    Ok(())
}

#[test]
fn formats_root_with_requested_filesystem() -> Result<()> {
    let expected =
        std::env::var("BOOTC_IMAGECTL_TEST_FILESYSTEM").unwrap_or_else(|_| "ext4".into());
    let fstype = Command::new("findmnt")
        .args(["--noheadings", "--output", "FSTYPE", "/sysroot"])
        .run_get_string()?;
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
