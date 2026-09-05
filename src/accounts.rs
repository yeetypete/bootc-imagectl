//! Keeps system account ids stable across image builds. Seeds the id
//! registry before packages are installed and writes the `sysusers.d`
//! declarations at finalize time.

use anyhow::{Result, bail};

use crate::cli::SeedAccountsOpts;

/// Run the `seed-accounts` subcommand.
///
/// # Errors
///
/// Fails until the subcommand is implemented.
pub fn seed(_opts: SeedAccountsOpts) -> Result<()> {
    bail!("seed-accounts is not yet implemented")
}
