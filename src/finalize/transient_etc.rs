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

/// The directory the host keys are in.
const SSH_HOST_KEY_DIR: &str = "/var/lib/ssh";

/// The host key types sshd loads by default.
const SSH_HOST_KEY_TYPES: &[&str] = &["ecdsa", "ed25519", "rsa"];

/// The template unit generating a missing host key of the type it is
/// instantiated with.
const SSHD_KEYGEN_UNIT: &str = "bootc-imagectl-sshd-keygen@.service";

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
/// transient, or systemd-firstboot would prompt.
pub(super) fn finalize(root: &Dir) -> Result<()> {
    check_firstboot(root)?;
    write_setup_root_conf(root)?;
    write_machine_id_unit(root)?;
    if root.exists("etc/ssh/sshd_config") {
        write_sshd_keygen_units(root)?;
        write_ssh_host_key_links(root)?;
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

/// The host keys in /var that /etc/ssh links to with a transient /etc.
pub fn ssh_host_keys() -> impl Iterator<Item = Utf8PathBuf> {
    SSH_HOST_KEY_TYPES
        .iter()
        .map(|key_type| Utf8Path::new(SSH_HOST_KEY_DIR).join(format!("ssh_host_{key_type}_key")))
}

/// The links in /etc/ssh to the host keys and their public keys in /var.
fn ssh_host_key_links() -> impl Iterator<Item = (Utf8PathBuf, Utf8PathBuf)> {
    ssh_host_keys().flat_map(|key| {
        let public = key.with_extension("pub");
        [key, public].map(|target| {
            let name = target.file_name().expect("host keys have file names");
            (Utf8Path::new("/etc/ssh").join(name), target)
        })
    })
}

/// Generate the missing host keys in /var before sshd starts.
fn write_sshd_keygen_units(root: &Dir) -> Result<()> {
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

/// Link the host keys in /var into /etc/ssh, where sshd reads them.
fn write_ssh_host_key_links(root: &Dir) -> Result<()> {
    let links: Vec<_> = ssh_host_key_links().collect();
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
    fn enables_sshd_keygen_units() -> Result<()> {
        let root = rootfs()?;
        write_sshd_keygen_units(&root)?;
        assert_eq!(
            root.read_link_contents(format!(
                "{VENDOR_UNIT_DIR}/sysinit.target.wants/bootc-imagectl-sshd-keygen@ed25519.service"
            ))?,
            format!("../{SSHD_KEYGEN_UNIT}")
        );
        Ok(())
    }

    #[test]
    fn links_ssh_host_keys_into_etc() -> Result<()> {
        let root = rootfs()?;
        write_ssh_host_key_links(&root)?;
        let conf = root.read_to_string(format!(
            "{}/{SSH_HOST_KEYS_TMPFILES}",
            tmpfiles::USR_TMPFILES_DIR
        ))?;
        let declared = tmpfiles::declared_paths(&conf);
        for (link, _) in ssh_host_key_links() {
            assert!(declared.contains(link.as_str()), "{declared:?}");
        }
        Ok(())
    }
}
