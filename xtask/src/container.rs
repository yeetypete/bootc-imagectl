//! Run tests in a container derived from a bootc image.

use std::ffi::OsString;
use std::path::Path;

use anyhow::Result;
use xshell::{Shell, cmd};

use crate::images::Image;
use crate::{bound_binary, target_dir};

/// Where the container sees the target directory, which holds bootc-imagectl
/// and the test binary.
const TARGET: &str = "/usr/libexec/bootc-imagectl-test";

/// Run the test binary in a container of `image` with the target directory
/// bound in.
pub(crate) fn run(sh: &Shell, image: &Image, binary: &Path, args: &[OsString]) -> Result<()> {
    let volume = format!("{}:{TARGET}:ro", target_dir(binary)?.display());
    let test = bound_binary(binary, TARGET)?;
    let reference = image.reference();
    let env = image.test_env().map(|var| format!("--env={var}"));
    // libtest colorizes only when it knows the terminal type.
    cmd!(
        sh,
        "podman run --rm --network=none --env TERM {env...} --volume {volume} {reference} {test} {args...}"
    )
    .run()?;
    Ok(())
}
