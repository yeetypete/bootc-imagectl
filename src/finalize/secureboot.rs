//! Sign systemd-boot and stage the Secure Boot variables it enrolls.
//!
//! Signing uses sbsigntools. `bootctl install` copies
//! `systemd-boot*.efi.signed` to the ESP in place of the unsigned binary next
//! to it, see bootctl(1). bootc copies the variables to `loader/keys` on the
//! ESP, from where systemd-boot enrolls them on a machine in setup mode, see
//! loader.conf(5).

use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use cap_std_ext::camino::{Utf8Path, Utf8PathBuf};
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::CapStdExtDirExtUtf8;
use tracing::{info, warn};
use uuid::{Uuid, uuid};

use crate::cli::SecureBootOpts;
use crate::command::CommandRunExt;

/// Where systemd-boot's EFI binaries are, relative to the rootfs.
pub(crate) const SYSTEMD_BOOT_DIR: &str = "usr/lib/systemd/boot/efi";

/// The sbsigntools commands signing needs.
const TOOLS: [&str; 2] = ["sbsign", "sbvarsign"];

/// Where bootc copies the Secure Boot variables to the ESP from.
const KEYS_DIR: &str = "usr/lib/bootc/install/secureboot-keys/auto";

/// The signature owner of our certificate, which the firmware ignores.
const OWNER_GUID: [u8; 16] = Uuid::nil().to_bytes_le();

/// The signature type of an X.509 certificate, `EFI_CERT_X509_GUID`.
const X509_GUID: [u8; 16] = uuid!("a5c059a1-94e4-4aa7-87b5-ab155c2bf072").to_bytes_le();

/// The attributes the UEFI specification requires for PK, KEK and db, which
/// the signature in each .auth file covers. sbvarsign always adds
/// `TIME_BASED_AUTHENTICATED_WRITE_ACCESS`, which is also required.
const ATTRIBUTES: &str = "NON_VOLATILE,BOOTSERVICE_ACCESS,RUNTIME_ACCESS";

/// The signature owner of Microsoft's certificates.
const MICROSOFT_GUID: [u8; 16] = uuid!("77fa9abd-0359-4d32-bd60-28f4e78f784b").to_bytes_le();

/// Microsoft's KEK certificates, which sign db and dbx updates such as the
/// revocations fwupd installs. From
/// <https://github.com/microsoft/secureboot_objects/tree/586ca9475c079377a2fe1f15fb73329f470eb281/PreSignedObjects>,
/// see `secureboot/microsoft/LICENSE`.
const MICROSOFT_KEK: [&[u8]; 2] = [
    include_bytes!("secureboot/microsoft/KEK/kek-ca-2011.der"),
    include_bytes!("secureboot/microsoft/KEK/kek-2k-ca-2023.der"),
];

/// Microsoft's db certificates, which sign firmware drivers such as those of
/// graphics cards, third-party boot loaders such as shim, and Windows. From
/// the same source as [`MICROSOFT_KEK`].
const MICROSOFT_DB: [&[u8]; 5] = [
    include_bytes!("secureboot/microsoft/db/uefi-ca-2011.der"),
    include_bytes!("secureboot/microsoft/db/uefi-ca-2023.der"),
    include_bytes!("secureboot/microsoft/db/option-rom-uefi-ca-2023.der"),
    include_bytes!("secureboot/microsoft/db/windows-pca-2011.der"),
    include_bytes!("secureboot/microsoft/db/windows-uefi-ca-2023.der"),
];

/// Sign systemd-boot and stage the Secure Boot variables.
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
    if opts.no_microsoft {
        warn!(
            "enrolling the keys without Microsoft's certificates. Firmware drivers Microsoft signed, such as those of graphics cards, may fail to load"
        );
    }
    sign_systemd_boot(root, &opts.private_key, &opts.certificate)?;
    write_keys(root, opts)
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

/// Write the PK, KEK and db Secure Boot variables for systemd-boot to enroll,
/// signed by the key in `opts`.
fn write_keys(root: &Dir, opts: &SecureBootOpts) -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("bootc-imagectl-keys-")
        .tempdir()
        .context("creating a scratch directory")?;
    let scratch = Utf8Path::from_path(tmp.path()).context("the scratch path is not UTF-8")?;
    let cert = &opts.certificate;
    let pem = std::fs::read(cert).with_context(|| format!("reading {cert}"))?;
    let der = to_der(&pem).with_context(|| format!("reading {cert}"))?;

    root.create_dir_all(KEYS_DIR)
        .with_context(|| format!("creating /{KEYS_DIR}"))?;
    for (variable, database) in signature_databases(&der, !opts.no_microsoft) {
        let database_file = scratch.join(format!("{variable}.esl"));
        std::fs::write(&database_file, database)
            .with_context(|| format!("writing {database_file}"))?;
        Command::new("sbvarsign")
            .arg("--key")
            .arg(&opts.private_key)
            .arg("--cert")
            .arg(cert)
            .args(["--attr", ATTRIBUTES, "--output"])
            .arg(format!("/{KEYS_DIR}/{variable}.auth"))
            .arg(variable)
            .arg(&database_file)
            .run_get_string()?;
    }
    info!("wrote the Secure Boot keys to /{KEYS_DIR}");
    Ok(())
}

/// The signature database of each Secure Boot variable systemd-boot enrolls.
/// Each holds a signature list of the DER certificate `cert`. With
/// `microsoft`, KEK and db also hold signature lists of Microsoft's
/// certificates.
fn signature_databases(cert: &[u8], microsoft: bool) -> [(&'static str, Vec<u8>); 3] {
    let own = signature_list(OWNER_GUID, cert);
    let with = |certs: &[&[u8]]| {
        let mut database = own.clone();
        if microsoft {
            for cert in certs {
                database.extend(signature_list(MICROSOFT_GUID, cert));
            }
        }
        database
    };
    [
        ("PK", own.clone()),
        ("KEK", with(&MICROSOFT_KEK)),
        ("db", with(&MICROSOFT_DB)),
    ]
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

/// The signature list of the DER certificate `cert` with the signature owner
/// `owner`. It holds the list header, then one signature of the owner and the
/// certificate. See
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
    fn adds_microsoft_certificates_to_kek_and_db() {
        let databases = signature_databases(b"cert", true);
        let own = signature_list(OWNER_GUID, b"cert");
        let owners = |database: &[u8]| {
            database
                .windows(16)
                .filter(|window| *window == MICROSOFT_GUID)
                .count()
        };
        assert_eq!(databases[0], ("PK", own.clone()));
        assert_eq!(databases[1].0, "KEK");
        assert!(databases[1].1.starts_with(&own));
        assert_eq!(owners(&databases[1].1), MICROSOFT_KEK.len());
        assert_eq!(databases[2].0, "db");
        assert!(databases[2].1.starts_with(&own));
        assert_eq!(owners(&databases[2].1), MICROSOFT_DB.len());
    }

    #[test]
    fn leaves_out_microsoft_certificates() {
        let own = signature_list(OWNER_GUID, b"cert");
        for (_, database) in signature_databases(b"cert", false) {
            assert_eq!(database, own);
        }
    }

    #[test]
    fn fails_without_systemd_boot() -> Result<()> {
        let root = rootfs()?;
        let err = systemd_boot_binaries(&root).unwrap_err().to_string();
        assert!(err.contains("lacks systemd-boot"), "{err}");
        Ok(())
    }
}
