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

    /// Run the command and return its standard output.
    ///
    /// # Errors
    ///
    /// Fails if the command cannot start, exits unsuccessfully, or prints
    /// something other than UTF-8. The error includes the command's stderr.
    fn output_string(&mut self) -> Result<String>;
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

    fn output_string(&mut self) -> Result<String> {
        let program = self.get_program().display().to_string();
        let output = self
            .output()
            .with_context(|| format!("running {program}"))?;
        if !output.status.success() {
            bail!(
                "{program} failed with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim_end()
            );
        }
        String::from_utf8(output.stdout).with_context(|| format!("the output of {program}"))
    }
}
