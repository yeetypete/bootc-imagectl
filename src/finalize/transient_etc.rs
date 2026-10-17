//! Prepare the image for a transient /etc, which bootc mounts as a tmpfs
//! overlay of the image's /etc, discarding changes on every boot.
//!
//! systemd then treats every boot as the first boot (see machine-id(5)), so
//! this module relocates the state that must survive a reboot to /var:
//!
//! - sshd reads its host keys from /var/lib/ssh, where a unit generates the
//!   missing ones early in the boot.
//! - /run/ostree/auth.json links to /var/lib/ostree/auth.json, so that bootc
//!   can pull from a registry that requires a login (see bootc-secrets(7)).
//!
//! This module also fails the build if systemd-firstboot would prompt on every
//! boot.

use std::fmt::Write;

use anyhow::{Context, Result, ensure};
use cap_std_ext::camino::Utf8Path;
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::CapStdExtDirExtUtf8;
use serde::Deserialize;
use tracing::debug;

use super::GENERATED_HEADER;
use super::kargs::KARGS_DIR;
use super::layout::SETUP_ROOT_CONF;
use super::tmpfiles::USR_TMPFILES_DIR;
use super::units::{self, VENDOR_UNIT_DIR};

/// The setup-root configuration that makes /etc transient.
const SETUP_ROOT_TRANSIENT_ETC: &str = "[etc]\ntransient = true\n";

/// The part of bootc's setup-root configuration that mounts /etc, see
/// bootc-setup-root-conf(5).
#[derive(Debug, Default, Deserialize)]
struct SetupRootConfig {
    #[serde(default)]
    etc: MountConfig,
}

/// How bootc mounts a directory.
#[derive(Debug, Default, Deserialize)]
struct MountConfig {
    mount: Option<MountType>,
    #[serde(default)]
    transient: bool,
}

impl MountConfig {
    /// Whether bootc mounts the directory transient. `mount` takes precedence
    /// over the `transient` shorthand.
    fn is_transient(&self) -> bool {
        self.mount
            .map_or(self.transient, |mount| mount == MountType::Transient)
    }
}

/// The types of mounts bootc supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum MountType {
    #[serde(alias = "root")]
    None,
    Bind,
    Overlay,
    #[serde(alias = "volatile")]
    Transient,
}

/// A kargs.d file, see bootc-kernel-arguments(7).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct KargsConfig {
    kargs: Vec<String>,
    /// If set, bootc applies the file only on these architectures.
    match_architectures: Option<Vec<String>>,
}

impl KargsConfig {
    /// Whether bootc applies the file on the architecture finalize runs on.
    fn matches_architecture(&self) -> bool {
        self.match_architectures
            .as_ref()
            .is_none_or(|archs| archs.iter().any(|arch| arch == std::env::consts::ARCH))
    }
}

/// bootc's persistent pull secret.
const AUTH_FILE: &str = "/var/lib/ostree/auth.json";

/// The directory the host keys are in.
const SSH_HOST_KEY_DIR: &str = "/var/lib/ssh";

/// The host key types sshd loads by default.
const SSH_HOST_KEY_TYPES: &[&str] = &["ecdsa", "ed25519", "rsa"];

/// The sshd drop-in pointing sshd to the host keys.
const SSHD_DROPIN: &str = "etc/ssh/sshd_config.d/10-bootc-imagectl.conf";

/// The template unit generating a missing host key of the type it is
/// instantiated with.
const SSHD_KEYGEN_UNIT: &str = "bootc-imagectl-sshd-keygen@.service";

/// The tmpfiles.d file linking the pull secret into /run.
const TMPFILES: &str = "bootc-imagectl-transient-etc.conf";

/// A setting systemd-firstboot prompts for unless its file exists, see
/// systemd-firstboot(1).
struct FirstbootSetting {
    name: &'static str,
    file: &'static str,
    /// If set, it prompts only if one of these paths exists.
    only_if_exists: Option<&'static [&'static str]>,
}

/// The settings systemd-firstboot prompts for.
const FIRSTBOOT_SETTINGS: &[FirstbootSetting] = &[
    FirstbootSetting {
        name: "locale",
        file: "etc/locale.conf",
        only_if_exists: None,
    },
    FirstbootSetting {
        name: "time zone",
        file: "etc/localtime",
        only_if_exists: None,
    },
    FirstbootSetting {
        name: "keymap",
        file: "etc/vconsole.conf",
        only_if_exists: Some(KEYMAP_DIRS),
    },
];

/// The kernel argument that keeps systemd-firstboot from prompting.
const FIRSTBOOT_DISABLED: &str = "systemd.firstboot=no";

/// Where systemd looks for keymaps.
const KEYMAP_DIRS: &[&str] = &[
    "usr/share/keymaps",
    "usr/share/kbd/keymaps",
    "usr/lib/kbd/keymaps",
];

/// Prepare the image for a transient /etc.
///
/// # Errors
///
/// Fails if the image already configures the mounts, systemd-firstboot would
/// prompt, or sshd does not read drop-ins.
pub(super) fn finalize(root: &Dir) -> Result<()> {
    check_firstboot(root)?;
    write_setup_root_conf(root)?;
    if root.exists("etc/ssh/sshd_config") {
        write_ssh_host_keys(root)?;
    }
    write_tmpfiles(root)
}

/// Fail if systemd-firstboot would prompt on every boot.
fn check_firstboot(root: &Dir) -> Result<()> {
    if !root.exists(format!("{VENDOR_UNIT_DIR}/systemd-firstboot.service"))
        || firstboot_disabled(root)?
    {
        return Ok(());
    }
    let missing: Vec<&str> = FIRSTBOOT_SETTINGS
        .iter()
        .filter(|setting| {
            setting
                .only_if_exists
                .is_none_or(|paths| paths.iter().any(|path| root.exists(path)))
        })
        .filter(|setting| root.symlink_metadata(setting.file).is_err())
        .map(|setting| setting.name)
        .collect();
    ensure!(
        missing.is_empty(),
        "systemd-firstboot would prompt for the {} on every boot. help: set them in the image with systemd-firstboot, e.g. `systemd-firstboot --locale=C.UTF-8 --timezone=UTC`, or add {FIRSTBOOT_DISABLED} to kargs.d",
        missing.join(", ")
    );
    Ok(())
}

/// Whether a kargs.d file disables systemd-firstboot's prompts.
fn firstboot_disabled(root: &Dir) -> Result<bool> {
    let Some(dir) = root.open_dir_optional(KARGS_DIR)? else {
        return Ok(false);
    };
    for entry in dir.entries()? {
        let name = entry?.file_name()?;
        if Utf8Path::new(&name).extension() != Some("toml") {
            continue;
        }
        let config: KargsConfig = toml::from_str(&dir.read_to_string(&name)?)
            .with_context(|| format!("parsing /{KARGS_DIR}/{name}"))?;
        if config.matches_architecture()
            && config.kargs.iter().any(|karg| karg == FIRSTBOOT_DISABLED)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Make /etc transient in bootc's setup-root configuration.
fn write_setup_root_conf(root: &Dir) -> Result<()> {
    if root.exists(SETUP_ROOT_CONF) {
        let conf = root.read_to_string(SETUP_ROOT_CONF)?;
        if !conf.trim().is_empty() {
            let conf: SetupRootConfig =
                toml::from_str(&conf).with_context(|| format!("parsing /{SETUP_ROOT_CONF}"))?;
            ensure!(
                conf.etc.is_transient(),
                "/{SETUP_ROOT_CONF} does not make /etc transient. help: add `[etc]` with `transient = true` to it"
            );
            return Ok(());
        }
    }
    root.create_dir_all("usr/lib/composefs")?;
    debug!("writing /{SETUP_ROOT_CONF}");
    root.write(SETUP_ROOT_CONF, SETUP_ROOT_TRANSIENT_ETC)
        .with_context(|| format!("writing /{SETUP_ROOT_CONF}"))
}

/// Point sshd to host keys in /var, generated before sshd starts.
fn write_ssh_host_keys(root: &Dir) -> Result<()> {
    let config = root.read_to_string("etc/ssh/sshd_config")?;
    ensure!(
        config.lines().any(|line| {
            line.trim_start().starts_with("Include") && line.contains("sshd_config.d/")
        }),
        "/etc/ssh/sshd_config does not include /etc/ssh/sshd_config.d, where finalize points sshd to the host keys in {SSH_HOST_KEY_DIR}"
    );
    let mut dropin = format!("{GENERATED_HEADER}\n");
    for key_type in SSH_HOST_KEY_TYPES {
        let _ = writeln!(dropin, "HostKey {SSH_HOST_KEY_DIR}/ssh_host_{key_type}_key");
    }
    root.create_dir_all("etc/ssh/sshd_config.d")?;
    debug!("writing /{SSHD_DROPIN}");
    root.write(SSHD_DROPIN, dropin)
        .with_context(|| format!("writing /{SSHD_DROPIN}"))?;

    let unit = format!(
        "{GENERATED_HEADER}
[Unit]
Description=OpenSSH %i Server Key Generation
DefaultDependencies=no
RequiresMountsFor={SSH_HOST_KEY_DIR}
After=local-fs.target
Before=sysinit.target shutdown.target
Conflicts=shutdown.target
ConditionFileNotEmpty=|!{SSH_HOST_KEY_DIR}/ssh_host_%i_key

[Service]
Type=oneshot
RemainAfterExit=yes
StateDirectory=ssh
ExecStartPre=rm -f {SSH_HOST_KEY_DIR}/ssh_host_%i_key {SSH_HOST_KEY_DIR}/ssh_host_%i_key.pub
ExecStart=ssh-keygen -q -t %i -N \"\" -C \"\" -f {SSH_HOST_KEY_DIR}/ssh_host_%i_key
"
    );
    units::write_enabled_template(
        root,
        SSHD_KEYGEN_UNIT,
        &unit,
        SSH_HOST_KEY_TYPES,
        "sysinit.target",
    )
}

/// Link bootc's pull secret in /var into /run, where bootc reads it.
fn write_tmpfiles(root: &Dir) -> Result<()> {
    let path = format!("{USR_TMPFILES_DIR}/{TMPFILES}");
    root.create_dir_all(USR_TMPFILES_DIR)?;
    debug!("writing /{path}");
    root.write(
        &path,
        format!("{GENERATED_HEADER}\nL /run/ostree/auth.json - - - - {AUTH_FILE}\n"),
    )
    .with_context(|| format!("writing /{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::rootfs;

    /// A rootfs with systemd-firstboot, and `files` among the ones it checks.
    fn firstboot_image(files: &[&str]) -> Result<cap_std_ext::cap_tempfile::utf8::TempDir> {
        let root = rootfs()?;
        root.create_dir_all(VENDOR_UNIT_DIR)?;
        root.write(format!("{VENDOR_UNIT_DIR}/systemd-firstboot.service"), "")?;
        root.create_dir_all("etc")?;
        for file in files {
            root.write(file, "")?;
        }
        Ok(root)
    }

    #[test]
    fn rejects_image_with_firstboot_prompts() -> Result<()> {
        let root = firstboot_image(&["etc/locale.conf"])?;
        root.create_dir_all("usr/share/keymaps")?;
        let error = check_firstboot(&root).unwrap_err().to_string();
        assert!(
            error.starts_with(
                "systemd-firstboot would prompt for the time zone, keymap on every boot."
            ),
            "{error}"
        );
        Ok(())
    }

    #[test]
    fn ignores_keymap_without_keymaps() -> Result<()> {
        let root = firstboot_image(&["etc/locale.conf", "etc/localtime"])?;
        check_firstboot(&root)
    }

    #[test]
    fn accepts_disabled_firstboot_prompts() -> Result<()> {
        let root = firstboot_image(&[])?;
        root.create_dir_all(KARGS_DIR)?;
        root.write(
            format!("{KARGS_DIR}/10-firstboot.toml"),
            "kargs = [\"systemd.firstboot=no\"]\n",
        )?;
        check_firstboot(&root)
    }

    #[test]
    fn makes_etc_transient() -> Result<()> {
        let root = rootfs()?;
        write_setup_root_conf(&root)?;
        assert_eq!(
            root.read_to_string(SETUP_ROOT_CONF)?,
            SETUP_ROOT_TRANSIENT_ETC
        );
        // Again, as on a rerun.
        write_setup_root_conf(&root)?;
        Ok(())
    }

    #[test]
    fn keeps_image_setup_root_conf_with_transient_etc() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("usr/lib/composefs")?;
        let conf = "[root]\ntransient = true\n\n[etc]\nmount = \"transient\"\n";
        root.write(SETUP_ROOT_CONF, conf)?;
        write_setup_root_conf(&root)?;
        assert_eq!(root.read_to_string(SETUP_ROOT_CONF)?, conf);

        root.write(SETUP_ROOT_CONF, "[root]\ntransient = true\n")?;
        assert!(write_setup_root_conf(&root).is_err());
        Ok(())
    }

    #[test]
    fn keeps_ssh_host_keys_in_var() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("etc/ssh")?;
        root.write(
            "etc/ssh/sshd_config",
            "Include /etc/ssh/sshd_config.d/*.conf\n",
        )?;
        write_ssh_host_keys(&root)?;
        let dropin = root.read_to_string(SSHD_DROPIN)?;
        assert!(
            dropin.contains("HostKey /var/lib/ssh/ssh_host_ed25519_key\n"),
            "{dropin}"
        );
        let unit = root.read_to_string(format!("{VENDOR_UNIT_DIR}/{SSHD_KEYGEN_UNIT}"))?;
        assert!(
            unit.contains("ConditionFileNotEmpty=|!/var/lib/ssh/ssh_host_%i_key\n"),
            "{unit}"
        );
        assert_eq!(
            root.read_link_contents(format!(
                "{VENDOR_UNIT_DIR}/sysinit.target.wants/bootc-imagectl-sshd-keygen@ed25519.service"
            ))?,
            format!("../{SSHD_KEYGEN_UNIT}")
        );
        Ok(())
    }

    #[test]
    fn requires_sshd_config_include() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("etc/ssh")?;
        root.write("etc/ssh/sshd_config", "PermitRootLogin no\n")?;
        assert!(write_ssh_host_keys(&root).is_err());
        Ok(())
    }
}
