//! Run tests in a VM booted from a bootc image.

use std::ffi::OsString;
use std::path::Path;

use anyhow::Result;
use xshell::{Shell, cmd};

use crate::{bound_binary, target_dir};

/// Where the VM sees the target directory, as bcvk names its binds.
const TARGET: &str = "/run/virtiofs-mnt-target";

/// Boot the image in a VM with the target directory bound in and run the
/// test binary in it over ssh.
pub(crate) fn run(sh: &Shell, image: &str, binary: &Path, args: &[OsString]) -> Result<()> {
    let bind = format!("{}:target", target_dir(binary)?.display());
    let test = bound_binary(binary, TARGET)?;
    cmd!(
        sh,
        "bcvk ephemeral run-ssh --rm --bind {bind} {image} {test} {args...}"
    )
    .run()?;
    Ok(())
}
