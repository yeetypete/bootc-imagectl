//! Run bootc's linter on the rootfs.

use std::process::Command;

use anyhow::Result;
use tracing::debug;

use crate::command::CommandRunExt;

/// Run `bootc container lint` with fatal warnings.
pub(super) fn bootc_lint() -> Result<()> {
    debug!("linting");
    Command::new("bootc")
        .args(["container", "lint", "--fatal-warnings"])
        .run()
}
