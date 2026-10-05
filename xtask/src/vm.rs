//! Run tests in a VM booted from a bootc image.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, ensure};
use xshell::{Shell, cmd};

use crate::{bound_binary, target_dir};

/// Where the VM sees the target directory. bcvk mounts a bind named `target`
/// there.
const TARGET: &str = "/run/virtiofs-mnt-target";

/// Boot the image in a VM with the target directory bound in and run the
/// test binary in it over ssh.
pub(crate) fn run(sh: &Shell, image: &str, binary: &Path, args: &[OsString]) -> Result<()> {
    let bind = format!("{}:target", target_dir(binary)?.display());
    let test = bound_binary(binary, TARGET)?;
    cmd!(
        sh,
        "bcvk ephemeral run-ssh --rm --bind {bind} --karg systemd.firstboot=no {image} {test} {args...}"
    )
    .run()?;
    Ok(())
}

/// Boot the image in a VM with its console on this terminal. The VM is
/// removed when it powers off.
pub(crate) fn boot(image: &str) -> Result<()> {
    // xshell would connect stdin to /dev/null.
    let status = Command::new("bcvk")
        .args([
            "ephemeral",
            "run",
            "--rm",
            "--interactive",
            "--tty",
            "--console",
        ])
        .arg(image)
        .status()
        .context("running bcvk")?;
    ensure!(status.success(), "bcvk failed: {status}");
    Ok(())
}
