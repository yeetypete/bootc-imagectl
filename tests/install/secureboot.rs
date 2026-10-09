//! Check whether the installed system booted with Secure Boot on.

use std::fs;
use std::io;

use anyhow::Result;

/// The `SecureBoot` EFI variable.
const SECURE_BOOT: &str =
    "/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c";

/// Whether Secure Boot is on. It is on if the byte after the variable's
/// 4-byte attributes is 1.
pub(crate) fn enabled() -> Result<bool> {
    match fs::read(SECURE_BOOT) {
        Ok(var) => Ok(var.get(4) == Some(&1)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
