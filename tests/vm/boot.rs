//! Check the image booted cleanly.

use std::process::Command;

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;
use bootc_imagectl::distro::{self, PackageName};
use cap_std_ext::camino::Utf8Path;
use cap_std_ext::cap_std::{ambient_authority, fs_utf8};

#[test]
fn boots_to_running() -> Result<()> {
    let state = Command::new("systemctl")
        .args(["is-system-running", "--wait"])
        .output_string()?;
    assert_eq!(state.trim(), "running");
    Ok(())
}

#[test]
fn queries_packages_from_moved_database() -> Result<()> {
    let distro = distro::detect(&fs_utf8::Dir::open_ambient_dir("/", ambient_authority())?)?;
    let bash: PackageName = "bash".parse()?;
    assert!(distro.is_installed(&bash)?);
    assert!(!distro.is_installed(&"no-such-package".parse()?)?);
    assert_eq!(
        distro.package_owning(Utf8Path::new("/usr/bin/bash"))?,
        Some(bash)
    );
    Ok(())
}
