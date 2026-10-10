//! The systemd units an image enables.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use anyhow::{Context, Result};
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::CapStdExtDirExtUtf8;
use serde::Deserialize;

use crate::command::CommandRunExt;

/// Where `systemctl enable` links system units, relative to the rootfs.
const UNIT_DIR: &str = "etc/systemd/system";

/// Where packages install system units, relative to the rootfs.
const VENDOR_UNIT_DIR: &str = "usr/lib/systemd/system";

/// A unit file's enablement state, as in systemctl(1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum EnablementState {
    Enabled,
    EnabledRuntime,
    Linked,
    LinkedRuntime,
    Alias,
    Masked,
    MaskedRuntime,
    Static,
    Indirect,
    Disabled,
    Generated,
    Transient,
    Bad,
    /// A state newer than this list.
    #[serde(other)]
    Unknown,
}

/// What the preset policy does with a unit file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Preset {
    Enabled,
    Disabled,
    Ignored,
}

/// A unit file, as reported by `systemctl list-unit-files --output=json`.
#[derive(Debug, Deserialize)]
pub(super) struct UnitFile {
    #[serde(rename = "unit_file")]
    pub(super) name: String,
    pub(super) state: EnablementState,
    /// `None` for unit files the preset policy does not apply to, e.g.
    /// static ones.
    pub(super) preset: Option<Preset>,
}

/// The unit files of the rootfs at `/`.
///
/// # Errors
///
/// Fails if systemctl fails or its output cannot be parsed.
pub(super) fn unit_files() -> Result<Vec<UnitFile>> {
    let output = Command::new("systemctl")
        .args(["list-unit-files", "--root=/", "--output=json"])
        .output_string()?;
    serde_json::from_str(&output).context("parsing systemctl list-unit-files")
}

/// The unit file `name` of the rootfs at `/` and its drop-ins, in the order
/// systemd applies them. Each file follows a `# <path>` line, without its
/// comments.
///
/// # Errors
///
/// Fails if systemd-analyze fails.
pub(super) fn cat_config(name: &str) -> Result<String> {
    Command::new("systemd-analyze")
        .args(["--root=/", "--tldr", "cat-config"])
        .arg(format!("systemd/system/{name}"))
        .output_string()
}

/// Whether the unit `name` is enabled, including by a link in a vendor
/// `.wants/` directory or by the preset policy, which systemd applies on the
/// first boot.
///
/// # Errors
///
/// Fails if the vendor unit directory cannot be read.
pub(super) fn is_enabled(root: &Dir, unit_files: &[UnitFile], name: &str) -> Result<bool> {
    let Some(unit) = unit_files.iter().find(|unit| unit.name == name) else {
        return Ok(false);
    };
    Ok(match unit.state {
        EnablementState::Masked | EnablementState::MaskedRuntime => false,
        EnablementState::Enabled => true,
        _ => {
            unit.preset == Some(Preset::Enabled)
                || linked_units(root, VENDOR_UNIT_DIR)?.contains(name)
        }
    })
}

/// A unit name, as systemd.unit(5) defines it.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum UnitName<'a> {
    /// E.g. `sshd.service`.
    Plain,
    /// E.g. `getty@.service`.
    Template,
    /// E.g. `getty@tty1.service`, an instance of the template unit
    /// `getty@.service`.
    Instance { template: String, instance: &'a str },
}

impl<'a> From<&'a str> for UnitName<'a> {
    fn from(name: &'a str) -> Self {
        let Some((prefix, suffix)) = name.rsplit_once('.') else {
            return Self::Plain;
        };
        match prefix.split_once('@') {
            None => Self::Plain,
            Some((_, "")) => Self::Template,
            Some((prefix, instance)) => Self::Instance {
                template: format!("{prefix}@.{suffix}"),
                instance,
            },
        }
    }
}

/// The names of the units linked from the `.wants/`, `.requires/` and
/// `.upholds/` directories in `dir`, where `systemctl enable` creates them.
fn linked_units(root: &Dir, dir: &str) -> Result<BTreeSet<String>> {
    let mut linked = BTreeSet::new();
    let Some(units) = root.open_dir_optional(dir)? else {
        return Ok(linked);
    };
    for entry in units.entries()? {
        let entry = entry?;
        let dir = entry.file_name()?;
        let is_dependency_dir = [".wants", ".requires", ".upholds"]
            .iter()
            .any(|suffix| dir.ends_with(suffix));
        if !is_dependency_dir || !entry.file_type()?.is_dir() {
            continue;
        }
        for link in units.read_dir(&dir)? {
            linked.insert(link?.file_name()?);
        }
    }
    Ok(linked)
}

/// The enabled instance names of each template unit.
///
/// # Errors
///
/// Fails if the unit directory cannot be read.
pub(super) fn enabled_instances(root: &Dir) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let mut instances = BTreeMap::<String, BTreeSet<String>>::new();
    for name in linked_units(root, UNIT_DIR)? {
        if let UnitName::Instance { template, instance } = UnitName::from(name.as_str()) {
            instances
                .entry(template)
                .or_default()
                .insert(instance.to_owned());
        }
    }
    Ok(instances)
}

#[cfg(test)]
mod tests {
    use cap_std_ext::cap_tempfile::utf8::TempDir;
    use indoc::indoc;

    use super::*;
    use crate::testutil::rootfs;

    #[test]
    fn parses_unit_names() {
        assert_eq!(UnitName::from("sshd.service"), UnitName::Plain);
        assert_eq!(
            UnitName::from("dbus-org.freedesktop.login1.service"),
            UnitName::Plain
        );
        assert_eq!(UnitName::from("getty@.service"), UnitName::Template);
        assert_eq!(
            UnitName::from("getty@tty1.service"),
            UnitName::Instance {
                template: "getty@.service".to_owned(),
                instance: "tty1",
            }
        );
    }

    /// A rootfs that enables `sshd.service` and `serial-getty@ttyS0.service`
    /// and masks `getty@tty9.service`.
    fn image() -> Result<TempDir> {
        let root = rootfs()?;
        root.create_dir_all("etc/systemd/system/getty.target.wants")?;
        root.create_dir_all("etc/systemd/system/multi-user.target.wants")?;
        root.symlink(
            "../serial-getty@.service",
            "etc/systemd/system/getty.target.wants/serial-getty@ttyS0.service",
        )?;
        root.symlink(
            "../sshd.service",
            "etc/systemd/system/multi-user.target.wants/sshd.service",
        )?;
        root.symlink("null", "etc/systemd/system/getty@tty9.service")?;
        Ok(root)
    }

    #[test]
    fn finds_enabled_instances() -> Result<()> {
        let root = image()?;
        assert_eq!(
            enabled_instances(&root)?,
            BTreeMap::from([(
                "serial-getty@.service".to_owned(),
                BTreeSet::from(["ttyS0".to_owned()])
            )])
        );
        Ok(())
    }

    #[test]
    fn finds_nothing_without_unit_dir() -> Result<()> {
        let root = rootfs()?;
        assert!(enabled_instances(&root)?.is_empty());
        Ok(())
    }

    #[test]
    fn finds_enabled_units() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("usr/lib/systemd/system/sysinit.target.wants")?;
        root.symlink(
            "../vendor.service",
            "usr/lib/systemd/system/sysinit.target.wants/vendor.service",
        )?;
        let unit_files: Vec<UnitFile> = serde_json::from_str(indoc! {r#"
            [
              {"unit_file": "sshd.service", "state": "enabled", "preset": "disabled"},
              {"unit_file": "vendor.service", "state": "disabled", "preset": "disabled"},
              {"unit_file": "preset.service", "state": "disabled", "preset": "enabled"},
              {"unit_file": "disabled.service", "state": "disabled", "preset": "disabled"},
              {"unit_file": "masked.service", "state": "masked", "preset": "enabled"}
            ]
        "#})?;
        for (name, enabled) in [
            ("sshd.service", true),
            ("vendor.service", true),
            ("preset.service", true),
            ("disabled.service", false),
            ("masked.service", false),
            ("missing.service", false),
        ] {
            assert_eq!(is_enabled(&root, &unit_files, name)?, enabled, "{name}");
        }
        Ok(())
    }
}
