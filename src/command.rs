//! Run external commands.

use std::process::Command;

use anyhow::{Context, Result, ensure};
use serde::de::DeserializeOwned;

/// Run a command to completion, failing if it does.
pub trait CommandRunExt {
    /// Run the command with inherited stdin, stdout and stderr.
    ///
    /// # Errors
    ///
    /// Fails if the command cannot start or exits unsuccessfully.
    fn run_inherited(&mut self) -> Result<()>;

    /// Run the command and return its standard output.
    ///
    /// # Errors
    ///
    /// Fails if the command cannot start, exits unsuccessfully, or prints
    /// something other than UTF-8. The error includes the command's stderr.
    fn run_get_string(&mut self) -> Result<String>;

    /// Run the command and parse its standard output as JSON.
    ///
    /// # Errors
    ///
    /// Fails like [`CommandRunExt::run_get_string`], or if the command does
    /// not print `T` as JSON.
    fn run_and_parse_json<T: DeserializeOwned>(&mut self) -> Result<T>;
}

impl CommandRunExt for Command {
    fn run_inherited(&mut self) -> Result<()> {
        let status = self
            .status()
            .with_context(|| format!("running {}", describe(self)))?;
        ensure!(status.success(), "{} failed with {status}", describe(self));
        Ok(())
    }

    fn run_get_string(&mut self) -> Result<String> {
        let output = self
            .output()
            .with_context(|| format!("running {}", describe(self)))?;
        ensure!(
            output.status.success(),
            "{} failed with {}: {}",
            describe(self),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim_end()
        );
        String::from_utf8(output.stdout)
            .with_context(|| format!("the output of {}", describe(self)))
    }

    fn run_and_parse_json<T: DeserializeOwned>(&mut self) -> Result<T> {
        let output = self.run_get_string()?;
        serde_json::from_str(&output)
            .with_context(|| format!("parsing the output of {}", describe(self)))
    }
}

/// The command line for error messages, e.g.
/// `systemctl list-unit-files --root=/`.
fn describe(command: &Command) -> String {
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}
