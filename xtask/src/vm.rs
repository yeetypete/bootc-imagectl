//! Run tests in a VM booted from a bootc image.

use std::ffi::OsString;
use std::path::Path;

use anyhow::Result;
use xshell::{Shell, cmd};

use crate::{bound_binary, target_dir};

/// Where the VM sees the target directory, as bcvk names its binds.
const TARGET: &str = "/run/virtiofs-mnt-target";

/// Finalize the image, boot it in a VM with the target directory bound in
/// and run the test binary in it over ssh.
pub(crate) fn run(sh: &Shell, image: &str, binary: &Path, args: &[OsString]) -> Result<()> {
    let target = target_dir(binary)?;
    let finalized = finalize(sh, image, target)?;
    let bind = format!("{}:target", target.display());
    let test = bound_binary(binary, TARGET)?;
    cmd!(
        sh,
        "bcvk ephemeral run-ssh --rm --bind {bind} {finalized} {test} {args...}"
    )
    .run()?;
    Ok(())
}

/// Finalize the image in a container with bootc-imagectl bound in from
/// `target` and commit the result, returning its tag.
fn finalize(sh: &Shell, image: &str, target: &Path) -> Result<String> {
    let finalized = format!("{image}-finalized");
    let container = "bootc-imagectl-test-finalize";
    let volume = format!(
        "{}/bootc-imagectl:/usr/libexec/bootc-imagectl:ro",
        target.display()
    );
    cmd!(
        sh,
        "podman run --replace --network=none --name {container} --volume {volume} {image} /usr/libexec/bootc-imagectl finalize"
    )
    .run()?;
    // A commit records the container's CMD as the image's, so set
    // it manually.
    let change = r#"CMD ["/sbin/init"]"#;
    cmd!(
        sh,
        "podman commit --quiet --change {change} {container} {finalized}"
    )
    .run()?;
    cmd!(sh, "podman rm {container}").run()?;
    Ok(finalized)
}
