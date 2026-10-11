//! Apply the preset policy at build time, so that the first boot changes no
//! unit.
//!
//! On the first boot, systemd applies the systemd.preset(5) policy to all
//! units. Fedora's disables units the image enables, Debian's enables units
//! packages leave disabled, and either change lands in the machine's /etc,
//! where bootc keeps it across upgrades as local configuration.
//!
//! finalize instead records the enabled units in a preset file that sorts
//! first, adds a `disable *` catchall if the policy lacks one, and then
//! applies the policy with `systemctl preset-all --preset-mode=enable-only`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::process::Command;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::CapStdExtDirExtUtf8;
use tracing::{debug, info};

use super::GENERATED_HEADER;
use super::units::{self, EnablementState, UnitFile, UnitName};
use crate::command::CommandRunExt;

/// Where packages install their system preset files.
const PRESET_DIR: &str = "usr/lib/systemd/system-preset";

/// The generated preset file that enables the units the image enables.
const PRESET_FILE: &str = "usr/lib/systemd/system-preset/10-bootc-imagectl.preset";

/// The generated catchall preset file, for policies without one.
const DEFAULT_PRESET_FILE: &str = "usr/lib/systemd/system-preset/99-bootc-imagectl-default.preset";

/// What a preset directive does with the units it matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Enable,
    Disable,
    Ignore,
}

impl FromStr for Action {
    type Err = anyhow::Error;

    fn from_str(word: &str) -> Result<Self> {
        Ok(match word {
            "enable" => Self::Enable,
            "disable" => Self::Disable,
            "ignore" => Self::Ignore,
            _ => bail!("{word:?} is not a preset action"),
        })
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Ignore => "ignore",
        })
    }
}

/// A directive of a preset file, as systemd.preset(5) defines it.
#[derive(Debug, PartialEq, Eq)]
struct Directive {
    action: Action,
    /// A unit name, which may contain shell-style wildcards.
    pattern: String,
    /// The instances to enable instead of the default one, for a template
    /// unit.
    instances: Vec<String>,
}

impl Directive {
    fn new(action: Action, pattern: &str) -> Self {
        Self {
            action,
            pattern: pattern.to_owned(),
            instances: Vec::new(),
        }
    }

    /// Whether the directive matches every unit.
    fn is_catchall(&self) -> bool {
        self.pattern == "*"
    }
}

impl fmt::Display for Directive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.action, self.pattern)?;
        for instance in &self.instances {
            write!(f, " {instance}")?;
        }
        Ok(())
    }
}

impl FromStr for Directive {
    type Err = anyhow::Error;

    fn from_str(line: &str) -> Result<Self> {
        let mut words = line.split_whitespace();
        let action = words
            .next()
            .and_then(|word| word.parse().ok())
            .with_context(|| format!("{line:?} is not a preset directive"))?;
        let pattern = words
            .next()
            .with_context(|| format!("{line:?} has no unit name"))?;
        Ok(Self {
            instances: words.map(str::to_owned).collect(),
            ..Self::new(action, pattern)
        })
    }
}

/// The directives that enable the units the image enables.
fn enable_directives(
    units: &[UnitFile],
    instances: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<Directive> {
    units
        .iter()
        .filter_map(|unit| match UnitName::from(unit.name.as_str()) {
            // For a template unit, the directive enables only the instances
            // it lists.
            UnitName::Template => instances.get(&unit.name).map(|instances| Directive {
                instances: instances.iter().cloned().collect(),
                ..Directive::new(Action::Enable, &unit.name)
            }),
            UnitName::Plain if unit.state == EnablementState::Enabled => {
                Some(Directive::new(Action::Enable, &unit.name))
            }
            _ => None,
        })
        .collect()
}

/// The directives of all preset files, as `systemd-analyze cat-config`
/// prints them.
struct Policy(Vec<Directive>);

impl Policy {
    /// Whether a directive matches every unit, e.g. `disable *`.
    fn has_catchall(&self) -> bool {
        self.0.iter().any(Directive::is_catchall)
    }
}

impl FromStr for Policy {
    type Err = anyhow::Error;

    fn from_str(config: &str) -> Result<Self> {
        config
            .lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .map(str::parse)
            .collect::<Result<_>>()
            .map(Self)
    }
}

/// Write `directives` to the preset file `path`.
fn write(root: &Dir, path: &str, directives: &[Directive]) -> Result<()> {
    let mut content = format!("{GENERATED_HEADER}\n");
    for directive in directives {
        content.push_str(&directive.to_string());
        content.push('\n');
    }
    root.write(path, content)
        .with_context(|| format!("writing /{path}"))
}

/// Apply the preset policy to the rootfs at `/`, keeping the units it
/// enables enabled.
pub(super) fn finalize(root: &Dir) -> Result<()> {
    debug!("applying the preset policy");
    for file in [PRESET_FILE, DEFAULT_PRESET_FILE] {
        root.remove_file_optional(file)
            .with_context(|| format!("removing /{file}"))?;
    }
    root.create_dir_all(PRESET_DIR)
        .with_context(|| format!("creating /{PRESET_DIR}"))?;

    let units = units::unit_files()?;
    let directives = enable_directives(&units, &units::enabled_instances(&units)?);
    if !directives.is_empty() {
        write(root, PRESET_FILE, &directives)?;
        info!("recorded {} units in /{PRESET_FILE}", directives.len());
    }

    let policy: Policy = units::cat_config("systemd/system-preset")?
        .parse()
        .context("parsing the preset policy")?;
    if !policy.has_catchall() {
        write(
            root,
            DEFAULT_PRESET_FILE,
            &[Directive::new(Action::Disable, "*")],
        )?;
        info!("the preset policy has no catchall, wrote /{DEFAULT_PRESET_FILE}");
    }

    // `--preset-mode=full` would also remove symlinks packages create without
    // systemctl, e.g. Debian's display-manager.service alias.
    Command::new("systemctl")
        .args(["preset-all", "--root=/", "--preset-mode=enable-only"])
        .run()
}

#[cfg(test)]
mod tests {
    use indoc::indoc;

    use super::*;

    #[test]
    fn enables_units_the_image_enables() -> Result<()> {
        let units: Vec<UnitFile> = serde_json::from_str(indoc! {r#"
            [
              {"unit_file": "sshd.service", "state": "enabled", "preset": "enabled"},
              {"unit_file": "remote-integritysetup.target", "state": "disabled", "preset": "enabled"},
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
                BTreeSet::from(["ttyS1".to_owned(), "ttyS0".to_owned()]),
            ),
        ]);
        let directives: Vec<String> = enable_directives(&units, &instances)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            directives,
            [
                "enable sshd.service",
                "enable getty@.service tty2",
                "enable serial-getty@.service ttyS0 ttyS1",
            ]
        );
        Ok(())
    }

    #[test]
    fn parses_directives() -> Result<()> {
        for line in [
            "enable getty@.service tty2 tty3",
            "disable *",
            "ignore foo.service",
        ] {
            assert_eq!(line.parse::<Directive>()?.to_string(), line);
        }
        assert_eq!(
            " disable  *.socket ".parse::<Directive>()?,
            Directive::new(Action::Disable, "*.socket")
        );
        for line in [
            "# /usr/lib/systemd/system-preset/90-systemd.preset",
            "enable",
            "",
        ] {
            assert!(line.parse::<Directive>().is_err(), "{line:?}");
        }
        Ok(())
    }

    #[test]
    fn finds_catchall() -> Result<()> {
        let config = indoc! {"
            # /usr/lib/systemd/system-preset/90-systemd.preset
            enable getty@.service
            disable *.socket
        "};
        assert!(!config.parse::<Policy>()?.has_catchall());
        for catchall in ["disable  *", "enable *", "ignore *"] {
            let policy: Policy = format!("{config}{catchall}\n").parse()?;
            assert!(policy.has_catchall(), "{catchall:?}");
        }
        assert!(format!("{config}bogus *\n").parse::<Policy>().is_err());
        Ok(())
    }
}
