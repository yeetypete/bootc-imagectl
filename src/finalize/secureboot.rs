//! Sign systemd-boot for Secure Boot and stage the keys systemd-boot enrolls.
//!
//! Signing uses sbsigntools. `bootctl install` copies
//! `systemd-boot*.efi.signed` to the ESP in place of the unsigned binary next
//! to it, see bootctl(1). bootc copies the keys to `loader/keys` on the ESP,
//! from where systemd-boot enrolls them on a machine in setup mode, see
//! loader.conf(5).

use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use cap_std_ext::camino::{Utf8Path, Utf8PathBuf};
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::CapStdExtDirExtUtf8;
use tracing::info;
use uuid::{Uuid, uuid};

use crate::cli::SecureBootOpts;
use crate::command::CommandRunExt;

/// Where systemd-boot's EFI binaries are, relative to the rootfs.
pub(crate) const SYSTEMD_BOOT_DIR: &str = "usr/lib/systemd/boot/efi";

/// The sbsigntools commands signing needs.
const TOOLS: [&str; 2] = ["sbsign", "sbvarsign"];

/// Where bootc copies the keys to the ESP from.
const KEYS_DIR: &str = "usr/lib/bootc/install/secureboot-keys/auto";

/// The owner of each signature list entry, which the firmware ignores.
const OWNER_GUID: [u8; 16] = Uuid::nil().to_bytes_le();

/// The type GUID of an X.509 certificate in an EFI signature list.
const X509_GUID: [u8; 16] = uuid!("a5c059a1-94e4-4aa7-87b5-ab155c2bf072").to_bytes_le();

/// The attributes the UEFI specification requires for PK, KEK and db, which
/// the signature in each .auth file covers. sbvarsign always adds
/// TIME_BASED_AUTHENTICATED_WRITE_ACCESS, which is also required.
const ATTRIBUTES: &str = "NON_VOLATILE,BOOTSERVICE_ACCESS,RUNTIME_ACCESS";

/// The variables systemd-boot enrolls.
const VARIABLES: [&str; 3] = ["PK", "KEK", "db"];

/// Sign systemd-boot and stage the certificate for enrollment.
///
/// # Errors
///
/// Fails if the image lacks systemd-boot or a signing tool, or signing fails.
pub(super) fn finalize(root: &Dir, opts: &SecureBootOpts) -> Result<()> {
    let missing: Vec<_> = TOOLS
        .into_iter()
        .filter(|tool| which::which(tool).is_err())
        .collect();
    ensure!(
        missing.is_empty(),
        "the image lacks tools Secure Boot signing needs: {}. help: install sbsigntool on Debian and Ubuntu, or sbsigntools on Fedora and Arch",
        missing.join(", ")
    );
    sign_systemd_boot(root, &opts.private_key, &opts.certificate)?;
    write_keys(root, &opts.private_key, &opts.certificate)
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

/// Write a signed copy of each systemd-boot binary next to it, replacing one
/// a package shipped.
fn sign_systemd_boot(root: &Dir, key: &Utf8Path, cert: &Utf8Path) -> Result<()> {
    for unsigned in systemd_boot_binaries(root)? {
        let signed = format!("{unsigned}.signed");
        root.remove_file_optional(&signed)
            .with_context(|| format!("removing /{signed}"))?;
        info!("signing /{unsigned}");
        Command::new("sbsign")
            .arg("--key")
            .arg(key)
            .arg("--cert")
            .arg(cert)
            .arg("--output")
            .arg(format!("/{signed}"))
            .arg(format!("/{unsigned}"))
            .run_get_string()?;
    }
    Ok(())
}

/// Write `cert` as the PK, KEK and db, signed by `key`, for systemd-boot to
/// enroll.
fn write_keys(root: &Dir, key: &Utf8Path, cert: &Utf8Path) -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("bootc-imagectl-keys-")
        .tempdir()
        .context("creating a scratch directory")?;
    let scratch = Utf8Path::from_path(tmp.path()).context("the scratch path is not UTF-8")?;
    let pem = std::fs::read(cert).with_context(|| format!("reading {cert}"))?;
    let der = to_der(&pem).with_context(|| format!("reading {cert}"))?;
    let list = scratch.join("cert.esl");
    std::fs::write(&list, signature_list(OWNER_GUID, &der))
        .with_context(|| format!("writing {list}"))?;

    root.create_dir_all(KEYS_DIR)
        .with_context(|| format!("creating /{KEYS_DIR}"))?;
    for variable in VARIABLES {
        Command::new("sbvarsign")
            .arg("--key")
            .arg(key)
            .arg("--cert")
            .arg(cert)
            .args(["--attr", ATTRIBUTES, "--output"])
            .arg(format!("/{KEYS_DIR}/{variable}.auth"))
            .arg(variable)
            .arg(&list)
            .run_get_string()?;
    }
    info!("wrote the Secure Boot keys to /{KEYS_DIR}");
    Ok(())
}

/// The DER form of the PEM certificate `pem`.
fn to_der(pem: &[u8]) -> Result<Vec<u8>> {
    let pem = pem::parse(pem).context("parsing PEM")?;
    ensure!(
        pem.tag() == "CERTIFICATE",
        "found a {} instead of a certificate",
        pem.tag()
    );
    Ok(pem.into_contents())
}

/// The EFI signature list of the DER certificate `cert`, owned by `owner`:
/// the list header, then the owner and the certificate. See
/// `EFI_SIGNATURE_LIST` in
/// <https://github.com/tianocore/edk2/blob/master/MdePkg/Include/Guid/ImageAuthentication.h>.
fn signature_list(owner: [u8; 16], cert: &[u8]) -> Vec<u8> {
    const HEADER: usize = 16 + 3 * 4;
    let signature_size = owner.len() + cert.len();
    let size = |n: usize| {
        u32::try_from(n)
            .expect("a certificate under 4 GiB")
            .to_le_bytes()
    };
    let mut list = Vec::with_capacity(HEADER + signature_size);
    list.extend(X509_GUID);
    list.extend(size(HEADER + signature_size));
    // No signature header.
    list.extend(0u32.to_le_bytes());
    list.extend(size(signature_size));
    list.extend(owner);
    list.extend(cert);
    list
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
    fn converts_certificate_to_der() -> Result<()> {
        let pem = b"-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n";
        assert_eq!(to_der(pem)?, [1, 2, 3]);
        Ok(())
    }

    #[test]
    fn rejects_other_pem() {
        let pem = b"-----BEGIN PRIVATE KEY-----\nAQID\n-----END PRIVATE KEY-----\n";
        let err = to_der(pem).unwrap_err().to_string();
        assert!(
            err.contains("PRIVATE KEY instead of a certificate"),
            "{err}"
        );
    }

    #[test]
    fn builds_signature_list() {
        let list = signature_list(OWNER_GUID, b"cert");
        assert_eq!(list.len(), 48);
        assert_eq!(list[..16], X509_GUID);
        // UEFI stores the first three fields of a GUID little-endian.
        assert_eq!(list[..8], [0xa1, 0x59, 0xc0, 0xa5, 0xe4, 0x94, 0xa7, 0x4a]);
        // The list, header and signature sizes.
        assert_eq!(list[16..28], [48, 0, 0, 0, 0, 0, 0, 0, 20, 0, 0, 0]);
        assert_eq!(list[28..44], OWNER_GUID);
        assert_eq!(&list[44..], b"cert");
    }

    #[test]
    fn fails_without_systemd_boot() -> Result<()> {
        let root = rootfs()?;
        let err = systemd_boot_binaries(&root).unwrap_err().to_string();
        assert!(err.contains("lacks systemd-boot"), "{err}");
        Ok(())
    }
}
