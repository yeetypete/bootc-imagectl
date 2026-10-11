//! The systemd units an image enables.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::CapStdExtDirExtUtf8;
use serde::Deserialize;

use crate::command::CommandRunExt;

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

/// A unit file, as reported by `systemctl list-unit-files --output=json`.
#[derive(Debug, Deserialize)]
pub(super) struct UnitFile {
    #[serde(rename = "unit_file")]
    pub(super) name: String,
    pub(super) state: EnablementState,
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

/// The configuration `name` of the rootfs at `/`, e.g. `systemd/system-preset`,
/// with its drop-ins in the order systemd reads them. Each file follows a
/// `# <path>` line, without its comments.
///
/// # Errors
///
/// Fails if systemd-analyze fails.
pub(super) fn cat_config(name: &str) -> Result<String> {
    Command::new("systemd-analyze")
        .args(["--root=/", "--tldr", "cat-config", name])
        .output_string()
}

/// Whether the unit `name` is enabled, including by a symlink in a vendor
/// `.wants/` directory, which systemctl reports as disabled.
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
        _ => linked_units(root, VENDOR_UNIT_DIR)?.contains(name),
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
/// `.upholds/` directories in `dir`.
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

/// The enabled instance names of each template unit in `unit_files`.
///
/// # Errors
///
/// Fails if systemctl fails.
pub(super) fn enabled_instances(unit_files: &[UnitFile]) -> Result<BTreeMap<String, Vec<String>>> {
    // A template is indirect if it has enabled instances but no default
    // instance.
    let templates: Vec<&str> = unit_files
        .iter()
        .filter(|unit| {
            UnitName::from(unit.name.as_str()) == UnitName::Template
                && matches!(
                    unit.state,
                    EnablementState::Enabled | EnablementState::Indirect
                )
        })
        .map(|unit| unit.name.as_str())
        .collect();
    if templates.is_empty() {
        return Ok(BTreeMap::new());
    }
    let output = Command::new("systemctl")
        .args(["is-enabled", "--root=/", "--full"])
        .args(&templates)
        .output_string()?;
    Ok(parse_instances(&templates, &output))
}

/// The instances of `templates` linked in `systemctl is-enabled --full`
/// output, which lists each template's state followed by its indented symlinks.
fn parse_instances(templates: &[&str], output: &str) -> BTreeMap<String, Vec<String>> {
    let mut instances = BTreeMap::<String, Vec<String>>::new();
    let mut templates = templates.iter();
    let mut template = None;
    for line in output.lines() {
        let Some(link) = line.strip_prefix("  ") else {
            template = templates.next();
            continue;
        };
        let (Some(template), Some(name)) = (template, Path::new(link).file_name()) else {
            continue;
        };
        // The symlinks include aliases, e.g. autovt@.service of getty@.service.
        if let UnitName::Instance {
            template: of,
            instance,
        } = UnitName::from(name.to_string_lossy().as_ref())
            && of == **template
        {
            instances
                .entry((*template).to_owned())
                .or_default()
                .push(instance.to_owned());
        }
    }
    instances
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn parses_enabled_instances() {
        let output = indoc! {"
            indirect
              /etc/systemd/system/getty.target.wants/serial-getty@ttyS1.service
              /etc/systemd/system/getty.target.wants/serial-getty@ttyS0.service
            enabled
              /etc/systemd/system/getty.target.wants/getty@tty1.service
              /etc/systemd/system/autovt@.service
        "};
        assert_eq!(
            parse_instances(&["serial-getty@.service", "getty@.service"], output),
            BTreeMap::from([
                ("getty@.service".to_owned(), vec!["tty1".to_owned()]),
                (
                    "serial-getty@.service".to_owned(),
                    vec!["ttyS1".to_owned(), "ttyS0".to_owned()]
                ),
            ])
        );
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
              {"unit_file": "disabled.service", "state": "disabled", "preset": "enabled"},
              {"unit_file": "masked.service", "state": "masked", "preset": "enabled"}
            ]
        "#})?;
        for (name, enabled) in [
            ("sshd.service", true),
            ("vendor.service", true),
            ("disabled.service", false),
            ("masked.service", false),
            ("missing.service", false),
        ] {
            assert_eq!(is_enabled(&root, &unit_files, name)?, enabled, "{name}");
        }
        Ok(())
    }
}
