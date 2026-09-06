//! Boot the finalized image under bcvk and inspect it.
//!
//! Requires KVM, bcvk and podman on the host. run.sh runs the tests once for
//! each image in tests/images (see the justfile). The tests live in modules
//! named after the subcommand they exercise. A module named after an image
//! contains the tests specific to that image.

use std::path::Path;
use std::process::Command;
use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use bootc_imagectl::command::CommandRunExt;

mod finalize;

/// The repository the test images are tagged under.
const REPOSITORY: &str = "localhost/bootc-imagectl-test";

/// The finalized image called in run.sh.
static IMAGE: LazyLock<String> = LazyLock::new(|| {
    let name = std::env::var("BOOTC_IMAGECTL_TEST_IMAGE")
        .expect("BOOTC_IMAGECTL_TEST_IMAGE is not set, use `just test-vm`");
    build_image(&name).expect("building the image")
});

/// Build the image in tests/images/`name`, finalize it in a container
/// and commit the result, returning its tag.
fn build_image(name: &str) -> Result<String> {
    let built = format!("{REPOSITORY}:{name}");
    let finalized = format!("{REPOSITORY}:{name}-finalized");
    let container = format!("bootc-imagectl-test-build-{name}");
    Command::new("podman")
        .args(["build", "--tag", &built])
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/images")
                .join(name),
        )
        .run()?;
    Command::new("podman")
        .args(["run", "--replace", "--network=none", "--name", &container])
        .arg(format!(
            "--volume={}:/usr/libexec/bootc-imagectl:ro",
            env!("CARGO_BIN_EXE_bootc-imagectl")
        ))
        .args([&built, "/usr/libexec/bootc-imagectl", "finalize"])
        .run()?;
    // A commit records the container's command as the image's, so put the
    // Containerfile's back.
    Command::new("podman")
        .args(["commit", "--quiet", "--change", r#"CMD ["/sbin/init"]"#])
        .args([&container, &finalized])
        .run()?;
    Command::new("podman").args(["rm", &container]).run()?;
    Ok(finalized)
}

/// Boot the image in a VM and run `command` in it over ssh, returning what
/// it printed. Fails with the returned output.
pub(crate) fn run_in_vm(command: &str) -> Result<String> {
    let output = Command::new("bcvk")
        .args(["ephemeral", "run-ssh", "--rm", &IMAGE, command])
        .output()
        .context("running bcvk")?;
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    print!("{printed}");
    if !output.status.success() {
        bail!("the command failed with {}\n{printed}", output.status);
    }
    Ok(printed)
}
