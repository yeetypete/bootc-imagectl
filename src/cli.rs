//! CLI for finalizing a rootfs into a bootc image and installing
//! a bootc image onto a disk. Only the bootc composefs backend is
//! supported.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::{finalize, install};

#[derive(Debug, Parser)]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// Run the selected subcommand.
    ///
    /// # Errors
    ///
    /// Returns the subcommand's error.
    pub fn run(self) -> Result<()> {
        match self.command {
            Command::Finalize(opts) => finalize::finalize(opts),
            Command::Install(opts) => install::install(opts),
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Turn the rootfs into a bootc image.
    Finalize(FinalizeOpts),
    /// Install a finalized image onto a disk.
    Install(InstallOpts),
}

/// Options for `finalize`.
#[derive(Debug, Args)]
pub struct FinalizeOpts {
    /// The image's sysusers lock file, e.g. /usr/lib/sysusers.d/00-bootc-imagectl.conf.
    ///
    /// The lock file lists every user and group the build creates with a fixed
    /// UID and GID. finalize fails the build if any account is missing from it
    /// and prints the lines to add to the lock file. Commit this file next to
    /// your Containerfile.
    #[arg(long, value_name = "PATH")]
    pub sysusers_lock: PathBuf,
}

/// Options for `install`.
#[derive(Debug, Args)]
pub struct InstallOpts {}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Cli;

    #[test]
    fn verify_cli() {
        Cli::command().debug_assert();
    }
}
