//! Keep the units the image enables from being disabled on first boot.
//!
//! On the first boot, systemd applies the distribution's systemd.preset(5)
//! files to all units, which on some distributions (e.g. Fedora) also
//! disables the units they do not enable. `bootc-imagectl finalize`
//! generates a preset file that sorts before the distribution's and enables
//! them, ensuring that they stay enabled after the first boot.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::process::Command;

use anyhow::{Context, Result};
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::CapStdExtDirExtUtf8;
use serde::Deserialize;
use tracing::{debug, info};

use crate::command::CommandRunExt;

/// Where packages install their system preset files.
const PRESET_DIR: &str = "usr/lib/systemd/system-preset";

/// The generated preset file.
const PRESET_FILE: &str = "usr/lib/systemd/system-preset/10-bootc-imagectl.preset";

/// Where `systemctl enable` links system units.
const UNIT_DIR: &str = "etc/systemd/system";

/// A unit file's enablement state, as in systemctl(1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum EnablementState {
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
enum Preset {
    Enabled,
    Disabled,
    Ignored,
}

/// A unit file, as reported by `systemctl list-unit-files --output=json`.
#[derive(Debug, Deserialize)]
struct UnitFile {
    #[serde(rename = "unit_file")]
    name: String,
    state: EnablementState,
    /// `None` for unit files the preset policy does not apply to, e.g.
    /// static ones.
    preset: Option<Preset>,
}

/// An `enable` directive of a preset file.
#[derive(Debug, PartialEq, Eq)]
struct Enable {
    unit: String,
    /// The instance names to enable, for a template unit.
    instances: Vec<String>,
}

impl fmt::Display for Enable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "enable {}", self.unit)?;
        for instance in &self.instances {
            write!(f, " {instance}")?;
        }
        Ok(())
    }
}

/// A unit name, as systemd.unit(5) defines it.
#[derive(Debug, PartialEq, Eq)]
enum UnitName<'a> {
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

/// The enabled instance names of each template unit, from the `.wants/`,
/// `.requires/` and `.upholds/` directories `systemctl enable` links them
/// into.
fn enabled_instances(root: &Dir) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let mut instances = BTreeMap::<String, BTreeSet<String>>::new();
    let Some(units) = root.open_dir_optional(UNIT_DIR)? else {
        return Ok(instances);
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
            let name = link?.file_name()?;
            if let UnitName::Instance { template, instance } = UnitName::from(name.as_str()) {
                instances
                    .entry(template)
                    .or_default()
                    .insert(instance.to_owned());
            }
        }
    }
    Ok(instances)
}

/// The directives that enable the units the image enables but the preset
/// policy disables.
fn enable_directives(
    units: &[UnitFile],
    instances: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<Enable> {
    let mut directives = Vec::new();
    for unit in units
        .iter()
        .filter(|unit| unit.preset == Some(Preset::Disabled))
    {
        match UnitName::from(unit.name.as_str()) {
            // For a template unit, the directive enables only the instances
            // it lists.
            UnitName::Template => {
                if let Some(instances) = instances.get(&unit.name) {
                    directives.push(Enable {
                        unit: unit.name.clone(),
                        instances: instances.iter().cloned().collect(),
                    });
                }
            }
            UnitName::Plain if unit.state == EnablementState::Enabled => {
                directives.push(Enable {
                    unit: unit.name.clone(),
                    instances: Vec::new(),
                });
            }
            _ => {}
        }
    }
    directives
}

/// Write the preset file for the rootfs at `/`.
pub(super) fn finalize(root: &Dir) -> Result<()> {
    debug!("recording enabled units in a preset file");
    root.remove_file_optional(PRESET_FILE)
        .with_context(|| format!("removing /{PRESET_FILE}"))?;

    let output = Command::new("systemctl")
        .args(["list-unit-files", "--root=/", "--output=json"])
        .output_string()?;
    let units: Vec<UnitFile> =
        serde_json::from_str(&output).context("parsing systemctl list-unit-files")?;
    let directives = enable_directives(&units, &enabled_instances(root)?);
    if directives.is_empty() {
        debug!("the preset policy enables every unit the image enables");
        return Ok(());
    }

    let mut content = String::from("# Generated by bootc-imagectl. Do not edit.\n");
    for directive in &directives {
        content.push_str(&directive.to_string());
        content.push('\n');
    }
    root.create_dir_all(PRESET_DIR)?;
    root.write(PRESET_FILE, content)
        .with_context(|| format!("writing /{PRESET_FILE}"))?;
    info!("recorded {} units in /{PRESET_FILE}", directives.len());
    Ok(())
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
    fn finds_enabled_instances() -> Result<()> {
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
        let instances = enabled_instances(&root)?;
        assert_eq!(
            instances,
            BTreeMap::from([(
                "serial-getty@.service".to_owned(),
                BTreeSet::from(["ttyS0".to_owned()])
            )])
        );
        Ok(())
    }

    #[test]
    fn enables_units_disabled_by_preset_policy() -> Result<()> {
        let units: Vec<UnitFile> = serde_json::from_str(indoc! {r#"
            [
              {"unit_file": "sshd.service", "state": "enabled", "preset": "enabled"},
              {"unit_file": "systemd-networkd.service", "state": "enabled", "preset": "disabled"},
              {"unit_file": "remote-integritysetup.target", "state": "disabled", "preset": "enabled"},
              {"unit_file": "podman.service", "state": "disabled", "preset": "disabled"},
              {"unit_file": "systemd-journald.service", "state": "static", "preset": null},
              {"unit_file": "getty@.service", "state": "enabled", "preset": "enabled"},
              {"unit_file": "serial-getty@.service", "state": "indirect", "preset": "disabled"},
              {"unit_file": "container-getty@.service", "state": "disabled", "preset": "disabled"},
              {"unit_file": "foo.service", "state": "something-new", "preset": "ignored"}
            ]
        "#})?;
        let instances = BTreeMap::from([
            (
                "getty@.service".to_owned(),
                BTreeSet::from(["tty2".to_owned()]),
            ),
            (
                "serial-getty@.service".to_owned(),
                BTreeSet::from(["ttyS0".to_owned(), "ttyS1".to_owned()]),
            ),
        ]);
        let directives: Vec<String> = enable_directives(&units, &instances)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            directives,
            [
                "enable systemd-networkd.service",
                "enable serial-getty@.service ttyS0 ttyS1",
            ]
        );
        Ok(())
    }
}
