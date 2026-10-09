//! Sign systemd-boot for Secure Boot and stage the keys systemd-boot enrolls.
//!
//! `bootctl install` copies `systemd-boot*.efi.signed` to the ESP in place of
//! the unsigned binary next to it, see bootctl(1). bootc copies the keys to
//! `loader/keys` on the ESP, from where systemd-boot enrolls them on a machine
//! in setup mode, see loader.conf(5).

use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use cap_std_ext::camino::{Utf8Path, Utf8PathBuf};
use cap_std_ext::cap_std::ambient_authority;
use cap_std_ext::cap_std::fs_utf8::{Dir, Permissions, PermissionsExt};
use cap_std_ext::dirext::CapStdExtDirExtUtf8;
use tracing::{debug, info};

use super::systemd;
use crate::command::CommandRunExt;

/// The oldest systemd whose bootctl supports `--secure-boot-auto-enroll`.
const MIN_VERSION: u32 = 257;

/// Where systemd-boot's EFI binaries are.
const SYSTEMD_BOOT_DIR: &str = "usr/lib/systemd/boot/efi";

/// The signing tool. Fedora and Arch ship it in systemd, Debian and Ubuntu
/// in systemd-repart.
const SBSIGN: &str = "usr/lib/systemd/systemd-sbsign";

/// Where bootc copies the keys to the ESP from.
const KEYS_DIR: &str = "usr/lib/bootc/install/secureboot-keys/auto";

/// The keys, as bootctl names them on the ESP.
const KEYS: [&str; 3] = ["PK.auth", "KEK.auth", "db.auth"];

/// Sign systemd-boot with `key` and stage `cert` for enrollment.
///
/// # Errors
///
/// Fails if the image's systemd is too old, the image lacks systemd-boot or
/// the signing tool, or signing fails.
pub(super) fn finalize(root: &Dir, key: &Utf8Path, cert: &Utf8Path) -> Result<()> {
    let version = systemd::version()?;
    ensure!(
        version >= MIN_VERSION,
        "the image has systemd {version} but Secure Boot needs systemd {MIN_VERSION} or newer"
    );
    sign_systemd_boot(root, key, cert)?;
    write_keys(root, key, cert)
}

/// The paths of systemd-boot's EFI binaries, one per architecture,
/// relative to the rootfs.
fn systemd_boot_binaries(root: &Dir) -> Result<Vec<Utf8PathBuf>> {
    let mut paths = Vec::new();
    if let Some(dir) = root.open_dir_optional(SYSTEMD_BOOT_DIR)? {
        for entry in dir.entries()? {
            let name = entry?.file_name()?;
            if name.starts_with("systemd-boot") && Utf8Path::new(&name).extension() == Some("efi") {
                paths.push(Utf8Path::new(SYSTEMD_BOOT_DIR).join(name));
            }
        }
    }
    paths.sort();
    if paths.is_empty() {
        bail!("the image lacks systemd-boot's EFI binaries in /{SYSTEMD_BOOT_DIR}");
    }
    Ok(paths)
}

/// Write a signed copy of each systemd-boot binary next to it. A signed copy,
/// if shipped by the distro package is replaced.
fn sign_systemd_boot(root: &Dir, key: &Utf8Path, cert: &Utf8Path) -> Result<()> {
    ensure!(
        root.is_file(SBSIGN),
        "the image lacks /{SBSIGN}. help: install it, e.g. from systemd-ukify on Fedora or systemd-repart on Debian and Ubuntu"
    );
    for unsigned in systemd_boot_binaries(root)? {
        let signed = format!("{unsigned}.signed");
        root.remove_file_optional(&signed)
            .with_context(|| format!("removing /{signed}"))?;
        info!("signing /{unsigned}");
        Command::new(format!("/{SBSIGN}"))
            .arg("sign")
            .arg(format!("--private-key={key}"))
            .arg(format!("--certificate={cert}"))
            .arg(format!("--output=/{signed}"))
            .arg(format!("/{unsigned}"))
            .output_string()?;
    }
    Ok(())
}

/// Write `cert` as the PK, KEK and db, signed by `key`. bootctl writes them
/// to a scratch ESP, from where they are copied into the image.
fn write_keys(root: &Dir, key: &Utf8Path, cert: &Utf8Path) -> Result<()> {
    let esp = tempfile::Builder::new()
        .prefix("bootc-imagectl-esp-")
        .tempdir()
        .context("creating a scratch ESP")?;
    let output = Command::new("bootctl")
        .args([
            "install",
            "--no-variables",
            // Only the keys are kept from the scratch ESP.
            "--random-seed=no",
            "--secure-boot-auto-enroll=yes",
        ])
        .arg(format!("--private-key={key}"))
        .arg(format!("--certificate={cert}"))
        // Unlike --esp-path, this skips the check that the ESP is a mount.
        .env("SYSTEMD_ESP_PATH", esp.path())
        .output_string()?;
    debug!("bootctl install: {output}");

    let esp = Dir::open_ambient_dir(
        Utf8Path::from_path(esp.path()).context("the scratch ESP path is not UTF-8")?,
        ambient_authority(),
    )?;
    let from = esp
        .open_dir("loader/keys/auto")
        .context("opening the keys bootctl wrote")?;
    root.create_dir_all(KEYS_DIR)
        .with_context(|| format!("creating /{KEYS_DIR}"))?;
    let to = root.open_dir(KEYS_DIR)?;
    for name in KEYS {
        from.copy(name, &to, name)
            .with_context(|| format!("copying {name} to /{KEYS_DIR}"))?;
        to.set_permissions(name, Permissions::from_mode(0o644))?;
    }
    info!("wrote the Secure Boot keys to /{KEYS_DIR}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::rootfs;

    #[test]
    fn finds_systemd_boot_binaries() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all(SYSTEMD_BOOT_DIR)?;
        for name in [
            "systemd-bootx64.efi",
            "systemd-bootia32.efi",
            "systemd-bootx64.efi.signed",
            "linuxx64.efi.stub",
            "addonx64.efi.stub",
        ] {
            root.write(format!("{SYSTEMD_BOOT_DIR}/{name}"), b"")?;
        }
        assert_eq!(
            systemd_boot_binaries(&root)?,
            [
                Utf8Path::new(SYSTEMD_BOOT_DIR).join("systemd-bootia32.efi"),
                Utf8Path::new(SYSTEMD_BOOT_DIR).join("systemd-bootx64.efi"),
            ]
        );
        Ok(())
    }

    #[test]
    fn fails_without_systemd_boot() -> Result<()> {
        let root = rootfs()?;
        let err = systemd_boot_binaries(&root).unwrap_err().to_string();
        assert!(err.contains("lacks systemd-boot"), "{err}");
        Ok(())
    }
}
