//! Run external commands.

use std::process::Command;

use anyhow::{Context, Result, bail};

/// Run a command to completion, failing if it does.
pub trait CommandRunExt {
    /// Run the command with inherited stdin, stdout and stderr.
    ///
    /// # Errors
    ///
    /// Fails if the command cannot start or exits unsuccessfully.
    fn run(&mut self) -> Result<()>;
}

impl CommandRunExt for Command {
    fn run(&mut self) -> Result<()> {
        let program = self.get_program().display().to_string();
        let status = self
            .status()
            .with_context(|| format!("running {program}"))?;
        if !status.success() {
            bail!("{program} failed with {status}");
        }
        Ok(())
    }
}
