//! Check dpkg in finalized Debian and Ubuntu images.

use std::fs;
use std::process::Command;

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;

/// dpkg installs and removes a package. It refuses to while a stat override
/// names an account missing from /etc/passwd or /etc/group.
pub(crate) fn installs_and_removes_package() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let package = dir.path().join("package");
    fs::create_dir_all(package.join("DEBIAN"))?;
    fs::write(
        package.join("DEBIAN/control"),
        "Package: bootc-imagectl-test\nVersion: 1\nArchitecture: all\n\
         Maintainer: bootc-imagectl\nDescription: test\n",
    )?;
    let deb = dir.path().join("package.deb");
    Command::new("dpkg-deb")
        .args(["--root-owner-group", "--build"])
        .arg(&package)
        .arg(&deb)
        .run()?;
    Command::new("dpkg").arg("--install").arg(&deb).run()?;
    Command::new("dpkg")
        .args(["--remove", "bootc-imagectl-test"])
        .run()
}
