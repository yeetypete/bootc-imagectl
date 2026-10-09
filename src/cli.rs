//! CLI for finalizing a rootfs into a bootc image and installing
//! a bootc image onto a disk. Only the bootc composefs backend is
//! supported.

use anyhow::Result;
use cap_std_ext::camino::Utf8PathBuf;
use clap::{Args, Parser, Subcommand, ValueEnum};

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
            Command::Finalize(opts) => finalize::finalize(&opts),
            Command::Install(opts) => install::install(&opts),
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
    /// The image's sysusers lock file, e.g. /usr/lib/sysusers.d/00-bootc-imagectl.lock.conf.
    ///
    /// The lock file lists every user and group the build creates with a fixed
    /// UID and GID. finalize fails the build if any account is missing from it
    /// and prints the lines to add to the lock file. Commit this file next to
    /// your Containerfile.
    #[arg(long, value_name = "PATH")]
    pub sysusers_lock: Utf8PathBuf,

    /// The private key that signs for Secure Boot, in PEM form, e.g.
    /// `/run/secureboot.key` from a build secret.
    #[arg(long, value_name = "PATH", requires = "secureboot_certificate")]
    pub secureboot_private_key: Option<Utf8PathBuf>,

    /// The certificate of --secureboot-private-key, in PEM form.
    #[arg(long, value_name = "PATH", requires = "secureboot_private_key")]
    pub secureboot_certificate: Option<Utf8PathBuf>,
}

/// Options for `install`.
#[derive(Debug, Args)]
pub struct InstallOpts {
    /// The disk to install to, e.g. /dev/nvme0n1. Every partition on it is
    /// destroyed. Without it, install lists the disks and asks for one.
    pub device: Option<Utf8PathBuf>,

    /// The root partition's filesystem.
    #[arg(long, value_enum, default_value_t = Filesystem::Ext4)]
    pub filesystem: Filesystem,

    /// How to encrypt the root partition.
    #[arg(long, value_enum, default_value_t = Encrypt::Passphrase)]
    pub encrypt: Encrypt,

    /// Read the root partition's passphrase from a file instead of prompting
    /// for it. The whole file is the passphrase, including any trailing
    /// newline.
    #[arg(long, value_name = "PATH")]
    pub key_file: Option<Utf8PathBuf>,

    /// The image to install, in containers-transports(5) form, e.g.
    /// `docker://docker.io/example/image:latest`. Defaults to the image of the
    /// podman container this runs in.
    #[arg(long, value_name = "IMGREF")]
    pub source_imgref: Option<String>,

    /// The registry reference the installed system updates from, e.g.
    /// `docker.io/example/image:latest`. Defaults to the source.
    #[arg(long, value_name = "IMGREF")]
    pub target_imgref: Option<String>,

    /// Wipe the disk without asking for confirmation.
    #[arg(long, requires = "device")]
    pub yes: bool,
}

/// The root partition's filesystem. Must support fs-verity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Filesystem {
    /// Format the root partition with ext4.
    Ext4,
    /// Format the root partition with btrfs.
    Btrfs,
}

impl Filesystem {
    /// The filesystem's name, as `mount` and repart.d(5) `Format=` accept it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Ext4 => "ext4",
            Self::Btrfs => "btrfs",
        }
    }
}

/// How `install` encrypts the root partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Encrypt {
    /// Encrypt the root partition with LUKS2, unlocked by a passphrase.
    Passphrase,
    /// Leave the root partition unencrypted.
    Off,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Cli;

    #[test]
    fn verify_cli() {
        Cli::command().debug_assert();
    }
}
