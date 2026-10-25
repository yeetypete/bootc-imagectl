//! Prepare the image for a transient /etc, which bootc mounts as a tmpfs
//! overlay of the image's /etc, discarding changes on every boot.
//!
//! systemd then treats every boot as the first boot (see machine-id(5)), so
//! this module relocates the state that must survive a reboot to /var:
//!
//! - The machine ID is kept in /var/lib/dbus/machine-id, which systemd reads
//!   when /etc/machine-id is uninitialized (see machine-id(5)). A unit writes
//!   the first boot's machine ID there.
//! - sshd's host keys are kept in /var/lib/ssh, where a unit generates the
//!   missing ones early in the boot. /etc/ssh links to them.
//! - /run/ostree/auth.json links to /var/lib/ostree/auth.json, so that bootc
//!   can pull from a registry that requires a login (see bootc-secrets(7)).
//!
//! This module also fails the build if systemd-firstboot would prompt on every
//! boot.

use std::process::Command;

use anyhow::{Context, Result, ensure};
use cap_std_ext::camino::{Utf8Path, Utf8PathBuf};
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::{CapStdExtDirExt, CapStdExtDirExtUtf8};
use tracing::debug;

use super::GENERATED_HEADER;
use super::kargs::{self, KARGS_DIR};
use super::layout::SETUP_ROOT_CONF;
use super::setup_root;
use super::tmpfiles::{self, Entry};
use super::units::{self, VENDOR_UNIT_DIR};
use crate::command::CommandRunExt;

/// bootc's persistent pull secret.
pub const PULL_SECRET: &str = "/var/lib/ostree/auth.json";

/// The link to [`PULL_SECRET`] where bootc reads it, see bootc-secrets(7).
pub const PULL_SECRET_LINK: &str = "/run/ostree/auth.json";

/// The D-Bus machine ID, where systemd reads the machine ID from if
/// /etc/machine-id is uninitialized, see machine-id(5).
pub const DBUS_MACHINE_ID: &str = "/var/lib/dbus/machine-id";

/// The directory [`DBUS_MACHINE_ID`] is in.
const DBUS_DIR: &str = "/var/lib/dbus";

/// Where the unit writes the machine ID before renaming it to
/// [`DBUS_MACHINE_ID`].
const DBUS_MACHINE_ID_STAGED: &str = "/var/lib/dbus/.machine-id";

/// The unit persisting the machine ID in [`DBUS_MACHINE_ID`].
const MACHINE_ID_UNIT: &str = "bootc-imagectl-machine-id-persist.service";

/// The directory the host keys are kept in.
pub const SSH_HOST_KEY_DIR: &str = "/var/lib/ssh";

/// The directory sshd and `ssh-keygen -A` keep the host keys in by default,
/// which links to [`SSH_HOST_KEY_DIR`].
const SSHD_HOST_KEY_DIR: &str = "/etc/ssh";

/// The unit generating the missing host keys.
const SSHD_KEYGEN_UNIT: &str = "bootc-imagectl-sshd-keygen.service";

/// The tmpfiles.d file linking the pull secret into /run.
const PULL_SECRET_TMPFILES: &str = "bootc-imagectl-pull-secret.conf";

/// The tmpfiles.d file linking the host keys into /etc/ssh.
const SSH_HOST_KEYS_TMPFILES: &str = "bootc-imagectl-ssh-host-keys.conf";

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
/// Fails if the image's bootc setup-root configuration does not make /etc
/// transient, systemd-firstboot would prompt, or sshd loads a host key that
/// `ssh-keygen -A` does not generate.
pub(super) fn finalize(root: &Dir) -> Result<()> {
    check_firstboot(root)?;
    write_setup_root_conf(root)?;
    write_machine_id_unit(root)?;
    if root.exists("etc/ssh/sshd_config") {
        let config = Command::new("sshd").arg("-G").run_get_string()?;
        let keys = generated_host_keys(&config)?;
        write_sshd_keygen_unit(root)?;
        write_ssh_host_key_links(root, &keys)?;
    }
    write_pull_secret_link(root)
}

/// Fail if systemd-firstboot would prompt on every boot.
fn check_firstboot(root: &Dir) -> Result<()> {
    if !root.exists(format!("{VENDOR_UNIT_DIR}/systemd-firstboot.service"))
        || firstboot_disabled(root)?
    {
        return Ok(());
    }
    let mut missing = Vec::new();
    for setting in FIRSTBOOT_SETTINGS {
        let prompts = setting
            .only_if_exists
            .is_none_or(|paths| paths.iter().any(|path| root.exists(path)));
        if prompts && root.symlink_metadata_optional(setting.file)?.is_none() {
            missing.push(setting.name);
        }
    }
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
        if !kargs::Config::filename_matches(&name) {
            continue;
        }
        let config: kargs::Config = toml::from_str(&dir.read_to_string(&name)?)
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
    let conf = root
        .as_cap_std()
        .read_to_string_optional(SETUP_ROOT_CONF)
        .with_context(|| format!("reading /{SETUP_ROOT_CONF}"))?;
    if let Some(conf) = conf.filter(|conf| !conf.trim().is_empty()) {
        let conf: setup_root::Config =
            toml::from_str(&conf).with_context(|| format!("parsing /{SETUP_ROOT_CONF}"))?;
        ensure!(
            conf.etc.is_transient(),
            "/{SETUP_ROOT_CONF} does not make /etc transient. help: add `[etc]` with `transient = true`"
        );
        return Ok(());
    }
    root.create_dir_all("usr/lib/composefs")?;
    debug!("writing /{SETUP_ROOT_CONF}");
    root.write(
        SETUP_ROOT_CONF,
        toml::to_string(&setup_root::Config::TRANSIENT_ETC)?,
    )
    .with_context(|| format!("writing /{SETUP_ROOT_CONF}"))
}

/// Write the machine ID to [`DBUS_MACHINE_ID`] unless it is already there,
/// before dbus's tmpfiles.d links it to /etc/machine-id.
fn write_machine_id_unit(root: &Dir) -> Result<()> {
    let unit = format!(
        "{GENERATED_HEADER}
[Unit]
Description=Save machine-id to /var
Documentation=man:machine-id(5)
DefaultDependencies=no
RequiresMountsFor={DBUS_DIR}
After=local-fs.target
Before=sysinit.target systemd-tmpfiles-setup.service shutdown.target
Conflicts=shutdown.target
ConditionPathIsSymbolicLink=|{DBUS_MACHINE_ID}
ConditionFileNotEmpty=|!{DBUS_MACHINE_ID}

[Service]
Type=oneshot
RemainAfterExit=yes
StateDirectory=dbus
ExecStart=cp /etc/machine-id {DBUS_MACHINE_ID_STAGED}
ExecStart=sync {DBUS_MACHINE_ID_STAGED}
ExecStart=mv -fT {DBUS_MACHINE_ID_STAGED} {DBUS_MACHINE_ID}
TimeoutSec=90s
"
    );
    units::write_enabled_unit(root, MACHINE_ID_UNIT, &unit, "sysinit.target")
}

/// The host keys the sshd configuration `config` (generated by `sshd -G`)
/// loads.
///
/// # Errors
///
/// Fails if sshd loads a host key that `ssh-keygen -A` does not generate.
fn generated_host_keys(config: &str) -> Result<Vec<Utf8PathBuf>> {
    let keys: Vec<Utf8PathBuf> = config
        .lines()
        .filter_map(|line| line.split_once(' '))
        // OpenSSH 10.6 and newer print the keywords in camel case.
        .filter(|(keyword, _)| keyword.eq_ignore_ascii_case("hostkey"))
        .map(|(_, path)| path.into())
        .collect();
    for key in &keys {
        let generated = key.parent() == Some(Utf8Path::new(SSHD_HOST_KEY_DIR))
            && key
                .file_name()
                .is_some_and(|name| name.starts_with("ssh_host_") && name.ends_with("_key"));
        ensure!(
            generated,
            "sshd loads the host key {key}, which ssh-keygen -A does not generate. help: remove its HostKey line so that sshd loads the default host keys in {SSHD_HOST_KEY_DIR}"
        );
    }
    Ok(keys)
}

/// Generate the missing host keys in /var before sshd starts.
fn write_sshd_keygen_unit(root: &Dir) -> Result<()> {
    let unit = format!(
        "{GENERATED_HEADER}
[Unit]
Description=OpenSSH Server Key Generation
DefaultDependencies=no
RequiresMountsFor={SSH_HOST_KEY_DIR}
After=local-fs.target
Before=sysinit.target shutdown.target
Conflicts=shutdown.target

[Service]
Type=oneshot
RemainAfterExit=yes
StateDirectory=ssh
BindPaths={SSH_HOST_KEY_DIR}:{SSHD_HOST_KEY_DIR}
ExecStart=ssh-keygen -A
"
    );
    units::write_enabled_unit(root, SSHD_KEYGEN_UNIT, &unit, "sysinit.target")
}

/// The links in /etc/ssh to `keys` and their public keys in /var.
fn ssh_host_key_links(keys: &[Utf8PathBuf]) -> Vec<(Utf8PathBuf, Utf8PathBuf)> {
    keys.iter()
        .flat_map(|key| [key.clone(), key.with_extension("pub")])
        .map(|link| {
            let name = link.file_name().expect("host keys have file names");
            let target = Utf8Path::new(SSH_HOST_KEY_DIR).join(name);
            (link, target)
        })
        .collect()
}

/// Link the host `keys` sshd loads from /etc/ssh to the ones in /var.
fn write_ssh_host_key_links(root: &Dir, keys: &[Utf8PathBuf]) -> Result<()> {
    let links = ssh_host_key_links(keys);
    let entries = links.iter().map(|(link, target)| Entry::Symlink {
        path: link.as_str(),
        target: target.as_str(),
    });
    tmpfiles::write_config(root, SSH_HOST_KEYS_TMPFILES, entries)
}

/// Link bootc's pull secret in /var into /run, where bootc reads it.
fn write_pull_secret_link(root: &Dir) -> Result<()> {
    let link = Entry::Symlink {
        path: PULL_SECRET_LINK,
        target: PULL_SECRET,
    };
    tmpfiles::write_config(root, PULL_SECRET_TMPFILES, [link])
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
    fn accepts_unset_keymap_without_installed_keymaps() -> Result<()> {
        let root = firstboot_image(&["etc/locale.conf", "etc/localtime"])?;
        check_firstboot(&root)
    }

    #[test]
    fn accepts_disabled_firstboot_prompts() -> Result<()> {
        let root = firstboot_image(&[])?;
        root.create_dir_all(KARGS_DIR)?;
        let config = kargs::Config {
            kargs: vec![FIRSTBOOT_DISABLED.to_owned()],
            match_architectures: None,
        };
        root.write(
            format!("{KARGS_DIR}/10-firstboot.toml"),
            toml::to_string(&config)?,
        )?;
        check_firstboot(&root)
    }

    #[test]
    fn makes_etc_transient() -> Result<()> {
        let root = rootfs()?;
        write_setup_root_conf(&root)?;
        let conf: setup_root::Config = toml::from_str(&root.read_to_string(SETUP_ROOT_CONF)?)?;
        assert!(conf.etc.is_transient());
        // Again, as on a rerun.
        write_setup_root_conf(&root)?;
        Ok(())
    }

    #[test]
    fn keeps_image_setup_root_conf_with_transient_etc() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("usr/lib/composefs")?;
        let conf = toml::to_string(&setup_root::Config {
            etc: setup_root::MountConfig {
                mount: Some(setup_root::MountType::Transient),
                transient: false,
            },
        })?;
        root.write(SETUP_ROOT_CONF, &conf)?;
        write_setup_root_conf(&root)?;
        assert_eq!(root.read_to_string(SETUP_ROOT_CONF)?, conf);

        root.write(
            SETUP_ROOT_CONF,
            toml::to_string(&setup_root::Config::default())?,
        )?;
        assert!(write_setup_root_conf(&root).is_err());
        Ok(())
    }

    #[test]
    fn enables_machine_id_unit() -> Result<()> {
        let root = rootfs()?;
        write_machine_id_unit(&root)?;
        assert_eq!(
            root.read_link_contents(format!(
                "{VENDOR_UNIT_DIR}/sysinit.target.wants/{MACHINE_ID_UNIT}"
            ))?,
            format!("../{MACHINE_ID_UNIT}")
        );
        Ok(())
    }

    #[test]
    fn enables_sshd_keygen_unit() -> Result<()> {
        let root = rootfs()?;
        write_sshd_keygen_unit(&root)?;
        assert_eq!(
            root.read_link_contents(format!(
                "{VENDOR_UNIT_DIR}/sysinit.target.wants/{SSHD_KEYGEN_UNIT}"
            ))?,
            format!("../{SSHD_KEYGEN_UNIT}")
        );
        Ok(())
    }

    #[test]
    fn reads_generated_host_keys() -> Result<()> {
        let config = "Port 22\nHostKey /etc/ssh/ssh_host_rsa_key\nHostKey /etc/ssh/ssh_host_mldsa44_ed25519_key\n";
        assert_eq!(
            generated_host_keys(config)?,
            [
                "/etc/ssh/ssh_host_rsa_key",
                "/etc/ssh/ssh_host_mldsa44_ed25519_key"
            ]
        );
        assert!(generated_host_keys("HostKey /etc/ssh/my_key\n").is_err());
        assert!(generated_host_keys("HostKey /var/lib/ssh/ssh_host_rsa_key\n").is_err());
        Ok(())
    }

    #[test]
    fn links_ssh_host_keys_into_etc() -> Result<()> {
        let root = rootfs()?;
        let keys = generated_host_keys("HostKey /etc/ssh/ssh_host_ed25519_key\n")?;
        write_ssh_host_key_links(&root, &keys)?;
        let conf = root.read_to_string(format!(
            "{}/{SSH_HOST_KEYS_TMPFILES}",
            tmpfiles::USR_TMPFILES_DIR
        ))?;
        let declared = tmpfiles::declared_paths(&conf);
        for (link, _) in ssh_host_key_links(&keys) {
            assert!(declared.contains(link.as_str()), "{declared:?}");
        }
        Ok(())
    }
}
