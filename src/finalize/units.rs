//! The systemd units an image enables.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::process::Command;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use camino::Utf8PathBuf;
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::CapStdExtDirExtUtf8;
use serde::Deserialize;
use tracing::debug;

use crate::command::CommandRunExt;

/// Where packages install system units, relative to the rootfs.
pub(super) const VENDOR_UNIT_DIR: &str = "usr/lib/systemd/system";

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
    pub(super) name: UnitName,
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
    let Some(unit) = unit_files.iter().find(|unit| unit.name.as_str() == name) else {
        return Ok(false);
    };
    Ok(match unit.state {
        EnablementState::Masked | EnablementState::MaskedRuntime => false,
        EnablementState::Enabled => true,
        _ => dependencies(root, VENDOR_UNIT_DIR)?.contains(name),
    })
}

/// Write the unit `name` with `content` to [`VENDOR_UNIT_DIR`], and enable it
/// in `wanted_by`.
///
/// # Errors
///
/// Fails if the unit or the symlink cannot be written.
pub(super) fn write_enabled_unit(
    root: &Dir,
    name: &str,
    content: &str,
    wanted_by: &str,
) -> Result<()> {
    write_unit(root, name, content)?;
    enable(root, name, name, wanted_by)
}

/// Write the unit `name` with `content` to [`VENDOR_UNIT_DIR`].
fn write_unit(root: &Dir, name: &str, content: &str) -> Result<()> {
    root.create_dir_all(VENDOR_UNIT_DIR)
        .with_context(|| format!("creating /{VENDOR_UNIT_DIR}"))?;
    let unit = format!("{VENDOR_UNIT_DIR}/{name}");
    debug!("writing /{unit}");
    root.atomic_write(&unit, content)
        .with_context(|| format!("writing /{unit}"))
}

/// Enable the unit file `unit` as `name` in `wanted_by`.
fn enable(root: &Dir, unit: &str, name: &str, wanted_by: &str) -> Result<()> {
    let wants = format!("{VENDOR_UNIT_DIR}/{wanted_by}.wants");
    root.create_dir_all(&wants)
        .with_context(|| format!("creating /{wants}"))?;
    let link = format!("{wants}/{name}");
    root.remove_file_optional(&link)?;
    root.symlink(format!("../{unit}"), &link)
        .with_context(|| format!("enabling {name} in /{wants}"))
}

/// A unit name, as systemd.unit(5) defines it.
#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub(super) enum UnitName {
    /// E.g. `sshd.service`.
    Plain(String),
    /// E.g. `getty@.service`.
    Template(Template),
    /// E.g. `getty@tty1.service`.
    Instance(Instance),
}

impl UnitName {
    pub(super) fn as_str(&self) -> &str {
        match self {
            Self::Plain(name) => name,
            Self::Template(template) => template.as_str(),
            Self::Instance(instance) => instance.as_str(),
        }
    }
}

impl FromStr for UnitName {
    type Err = anyhow::Error;

    fn from_str(name: &str) -> Result<Self> {
        // systemd ends the prefix at the first @ and starts the type suffix
        // after the last dot. The instance name between them may contain
        // both, e.g. foo@a@b.c.service has the instance name a@b.c.
        let (stem, suffix) = name
            .rsplit_once('.')
            .filter(|(stem, suffix)| !stem.is_empty() && !suffix.is_empty())
            .with_context(|| format!("{name:?} is not a unit name"))?;
        Ok(match stem.split_once('@') {
            None => Self::Plain(name.to_owned()),
            Some(("", _)) => bail!("{name:?} has no prefix"),
            Some((_, "")) => Self::Template(Template(name.to_owned())),
            Some((prefix, _)) => Self::Instance(Instance {
                name: name.to_owned(),
                template: Template(format!("{prefix}@.{suffix}")),
                at: prefix.len(),
                dot: stem.len(),
            }),
        })
    }
}

impl TryFrom<String> for UnitName {
    type Error = anyhow::Error;

    fn try_from(name: String) -> Result<Self> {
        name.parse()
    }
}

impl fmt::Display for UnitName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The name of a template unit, e.g. `getty@.service`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Template(String);

impl Template {
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }

    /// The instance `instance` of the template, e.g. `getty@tty1.service`
    /// for `tty1`.
    ///
    /// # Errors
    ///
    /// Fails if `instance` is empty.
    pub(super) fn instance(&self, instance: &str) -> Result<Instance> {
        self.0.replace("@.", &format!("@{instance}.")).parse()
    }
}

impl FromStr for Template {
    type Err = anyhow::Error;

    fn from_str(name: &str) -> Result<Self> {
        match name.parse()? {
            UnitName::Template(template) => Ok(template),
            _ => bail!("{name:?} is not a template unit"),
        }
    }
}

impl fmt::Display for Template {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The name of an instance of a template unit, e.g. `getty@tty1.service`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Instance {
    name: String,
    template: Template,
    /// The offsets of the first `@` and the last `.` in the name.
    at: usize,
    dot: usize,
}

impl Instance {
    pub(super) fn as_str(&self) -> &str {
        &self.name
    }

    /// The template this instantiates, e.g. `getty@.service`.
    pub(super) fn template(&self) -> &Template {
        &self.template
    }

    /// The instance string, e.g. `tty1`, between the first `@` and the type
    /// suffix.
    pub(super) fn instance(&self) -> &str {
        &self.name[self.at + 1..self.dot]
    }
}

impl FromStr for Instance {
    type Err = anyhow::Error;

    fn from_str(name: &str) -> Result<Self> {
        match name.parse()? {
            UnitName::Instance(instance) => Ok(instance),
            _ => bail!("{name:?} is not an instance of a template unit"),
        }
    }
}

impl fmt::Display for Instance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

/// A unit's `systemctl is-enabled --full` output: its enablement state,
/// followed by its installation targets, the indented symlinks that enable it.
#[derive(Debug)]
struct IsEnabled {
    symlinks: Vec<Utf8PathBuf>,
}

impl FromStr for IsEnabled {
    type Err = anyhow::Error;

    fn from_str(output: &str) -> Result<Self> {
        let mut lines = output.lines();
        lines.next().context("no state")?;
        let symlinks = lines
            .map(|line| {
                line.strip_prefix("  ")
                    .map(Utf8PathBuf::from)
                    .with_context(|| format!("{line:?} is not an indented symlink"))
            })
            .collect::<Result<_>>()?;
        Ok(Self { symlinks })
    }
}

/// The names of the units symlinked from the `.wants/`, `.requires/` and
/// `.upholds/` directories in `dir`, which systemd adds as dependencies.
fn dependencies(root: &Dir, dir: &str) -> Result<BTreeSet<String>> {
    let mut dependencies = BTreeSet::new();
    let Some(units) = root.open_dir_optional(dir)? else {
        return Ok(dependencies);
    };
    for entry in units.entries()? {
        let entry = entry?;
        let name = entry.file_name()?;
        let is_dependency_dir = [".wants", ".requires", ".upholds"]
            .iter()
            .any(|suffix| name.ends_with(suffix));
        if !is_dependency_dir || !entry.file_type()?.is_dir() {
            continue;
        }
        for symlink in units.read_dir(&name)? {
            dependencies.insert(symlink?.file_name()?);
        }
    }
    Ok(dependencies)
}

/// The enabled instances of each template unit in `unit_files`.
///
/// # Errors
///
/// Fails if systemctl fails.
pub(super) fn enabled_instances(
    unit_files: &[UnitFile],
) -> Result<BTreeMap<Template, BTreeSet<Instance>>> {
    // A template is indirect if it has enabled instances but no default
    // instance.
    let templates = unit_files
        .iter()
        .filter_map(|unit| match (&unit.name, unit.state) {
            (
                UnitName::Template(template),
                EnablementState::Enabled | EnablementState::Indirect,
            ) => Some(template),
            _ => None,
        });
    let mut instances = BTreeMap::new();
    for template in templates {
        let output: IsEnabled = Command::new("systemctl")
            .args(["is-enabled", "--root=/", "--full", template.as_str()])
            .output_string()?
            .parse()
            .with_context(|| format!("parsing systemctl is-enabled {template}"))?;
        // The symlinks include aliases, e.g. autovt@.service of
        // getty@.service.
        let enabled: BTreeSet<Instance> = output
            .symlinks
            .iter()
            .filter_map(|symlink| symlink.file_name()?.parse::<Instance>().ok())
            .filter(|instance| instance.template() == template)
            .collect();
        // Skip a template without enabled instances, as a preset enabling it
        // could enable units the image leaves disabled.
        if !enabled.is_empty() {
            instances.insert(template.clone(), enabled);
        }
    }
    Ok(instances)
}

#[cfg(test)]
mod tests {
    use indoc::indoc;

    use super::*;
    use crate::testutil::rootfs;

    #[test]
    fn parses_unit_names() -> Result<()> {
        for name in ["sshd.service", "dbus-org.freedesktop.login1.service"] {
            assert_eq!(name.parse::<UnitName>()?, UnitName::Plain(name.to_owned()));
        }
        assert_eq!(
            "getty@.service".parse::<UnitName>()?,
            UnitName::Template(Template("getty@.service".to_owned()))
        );
        let instance: Instance = "getty@tty1.service".parse()?;
        assert_eq!(instance.template().as_str(), "getty@.service");
        assert_eq!(instance.instance(), "tty1");
        assert_eq!(instance.template().instance("tty1")?, instance);
        assert!(instance.template().instance("").is_err());
        let instance: Instance = "foo@a@b.c.service".parse()?;
        assert_eq!(instance.template().as_str(), "foo@.service");
        assert_eq!(instance.instance(), "a@b.c");
        assert!("getty@tty1.service".parse::<Template>().is_err());
        assert!("getty@.service".parse::<Instance>().is_err());
        for name in ["sshd", ".service", "sshd.", "@tty1.service"] {
            assert!(name.parse::<UnitName>().is_err(), "{name:?}");
        }
        Ok(())
    }

    #[test]
    fn parses_is_enabled() -> Result<()> {
        let output = indoc! {"
            enabled
              /etc/systemd/system/getty.target.wants/getty@tty1.service
              /etc/systemd/system/autovt@.service
        "};
        assert_eq!(
            output.parse::<IsEnabled>()?.symlinks,
            [
                "/etc/systemd/system/getty.target.wants/getty@tty1.service",
                "/etc/systemd/system/autovt@.service",
            ]
        );
        assert!("disabled\n".parse::<IsEnabled>()?.symlinks.is_empty());
        assert!("".parse::<IsEnabled>().is_err());
        assert!(
            "enabled\n/etc/systemd/system/x.service\n"
                .parse::<IsEnabled>()
                .is_err()
        );
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
