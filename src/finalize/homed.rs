//! Make the user that systemd-homed's first boot wizard creates an
//! administrator.
//!
//! `systemd-homed-firstboot.service` creates the first regular user without
//! the groups the distributions' installers assign it. finalize only changes
//! this unit if it is enabled in the image.

use anyhow::{Context, Result, bail, ensure};
use cap_std_ext::camino::Utf8Path;
use cap_std_ext::cap_std::fs_utf8::Dir;
use tracing::info;

use super::units;
use crate::distro::Distro;
use crate::passwd::{Entry, Group, Names};
use crate::sysusers::Name;

/// The wizard's unit.
const UNIT: &str = "systemd-homed-firstboot.service";

/// The drop-in finalize writes, relative to the rootfs.
const DROPIN_DIR: &str = "usr/lib/systemd/system/systemd-homed-firstboot.service.d";
const DROPIN: &str = "50-bootc-imagectl.conf";

/// Add the wizard's user to the admin group and the default user groups.
///
/// # Errors
///
/// Fails if the image lacks the admin group or the wizard's command cannot
/// be extended.
pub(super) fn finalize(root: &Dir, distro: &dyn Distro) -> Result<()> {
    if !units::is_enabled(root, &units::unit_files()?, UNIT)? {
        return Ok(());
    }
    configure(root, distro, &units::cat_config(UNIT)?)
}

/// Configure the wizard from its [`units::cat_config`] output.
fn configure(root: &Dir, distro: &dyn Distro, config: &str) -> Result<()> {
    let groups = groups(root, distro)?;
    let command = exec_start(config).with_context(|| format!("reading the command of {UNIT}"))?;
    write_dropin(root, command, &groups)?;
    info!("{UNIT} adds the user it creates to {}", Names(&groups));
    Ok(())
}

/// The admin group and the default user groups the image has.
fn groups(root: &Dir, distro: &dyn Distro) -> Result<Vec<Name>> {
    let existing = Group::read_all(root)?;
    let find = |name: &str| existing.iter().find(|group| group.name.as_str() == name);
    let admin = distro.admin_group();
    let admin = find(admin).with_context(|| {
        format!(
            "{UNIT} is enabled but the image has no {admin} group. The user it creates could not administer the system"
        )
    })?;
    let extra = distro
        .default_user_groups()
        .iter()
        .filter_map(|name| find(name));
    Ok(std::iter::once(admin)
        .chain(extra)
        .map(|group| group.name.clone())
        .collect())
}

/// The command the unit runs without our drop-in.
fn exec_start(config: &str) -> Result<&str> {
    let ours = format!("/{DROPIN_DIR}/{DROPIN}");
    let mut path = Utf8Path::new("");
    let mut commands = Vec::new();
    for line in config.lines() {
        if let Some(header) = line.strip_prefix("# ") {
            path = Utf8Path::new(header);
            continue;
        }
        let Some((key, command)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "ExecStart" || path == ours {
            continue;
        }
        let is_dropin = path.parent().and_then(Utf8Path::extension) == Some("d");
        ensure!(
            !is_dropin || path.file_name() < Some(DROPIN),
            "{path} sets ExecStart= after the drop-in adding the groups. help: pass --member-of there instead"
        );
        match command.trim() {
            "" => commands.clear(),
            command => commands.push(command),
        }
    }
    match commands[..] {
        [command] if !command.ends_with('\\') => Ok(command),
        [_] => bail!("ExecStart= continues on the next line"),
        [] => bail!("no ExecStart="),
        _ => bail!("more than one ExecStart="),
    }
}

/// Write the drop-in that runs `command` with `--member-of`.
fn write_dropin(root: &Dir, command: &str, groups: &[Name]) -> Result<()> {
    root.create_dir_all(DROPIN_DIR)
        .with_context(|| format!("creating /{DROPIN_DIR}"))?;
    let path = format!("{DROPIN_DIR}/{DROPIN}");
    root.write(
        &path,
        format!(
            "[Service]\nExecStart=\nExecStart={command} --member-of={}\n",
            Names(groups)
        ),
    )
    .with_context(|| format!("writing /{path}"))
}

#[cfg(test)]
mod tests {
    use cap_std_ext::cap_tempfile::utf8::TempDir;
    use indoc::indoc;

    use super::*;
    use crate::testutil::{TestDistro, rootfs};

    const DISTRO: TestDistro = TestDistro {
        owner: None,
        not_installed: &[],
    };

    /// The vendor unit, as [`units::cat_config`] prints it.
    const VENDOR: &str = indoc! {"
        # /usr/lib/systemd/system/systemd-homed-firstboot.service
        [Unit]
        ConditionFirstBoot=yes

        [Service]
        Type=oneshot
        ExecStart=homectl firstboot --prompt-new-user --prompt-groups=no
    "};

    /// A rootfs with `groups`.
    fn image(groups: &str) -> Result<TempDir> {
        let root = rootfs()?;
        root.create_dir("etc")?;
        root.write("etc/group", format!("root:x:0:\n{groups}"))?;
        Ok(root)
    }

    #[test]
    fn adds_wizard_user_to_groups() -> Result<()> {
        let root = image("wheel:x:10:\nadm:x:4:\n")?;
        configure(&root, &DISTRO, VENDOR)?;
        assert_eq!(
            root.read_to_string(format!("{DROPIN_DIR}/{DROPIN}"))?,
            "[Service]\nExecStart=\nExecStart=homectl firstboot --prompt-new-user --prompt-groups=no --member-of=wheel,adm\n"
        );
        Ok(())
    }

    #[test]
    fn skips_groups_image_lacks() -> Result<()> {
        let root = image("wheel:x:10:\n")?;
        configure(&root, &DISTRO, VENDOR)?;
        let dropin = root.read_to_string(format!("{DROPIN_DIR}/{DROPIN}"))?;
        assert!(dropin.ends_with(" --member-of=wheel\n"), "{dropin}");
        Ok(())
    }

    #[test]
    fn requires_admin_group() -> Result<()> {
        let root = image("")?;
        let err = format!("{:#}", configure(&root, &DISTRO, VENDOR).unwrap_err());
        assert!(err.contains("no wheel group"), "{err}");
        Ok(())
    }

    #[test]
    fn parses_exec_start() {
        assert_eq!(
            exec_start(VENDOR).unwrap(),
            "homectl firstboot --prompt-new-user --prompt-groups=no"
        );
        assert_eq!(
            exec_start("ExecStart = homectl firstboot\n").unwrap(),
            "homectl firstboot"
        );
    }

    #[test]
    fn applies_earlier_dropins() {
        let config = format!(
            "{VENDOR}\n{}",
            indoc! {"
                # /etc/systemd/system/systemd-homed-firstboot.service.d/10-local.conf
                [Service]
                ExecStart=
                ExecStart=homectl firstboot --prompt-new-user --prompt-shell=yes

                # /usr/lib/systemd/system/systemd-homed-firstboot.service.d/50-bootc-imagectl.conf
                [Service]
                ExecStart=
                ExecStart=homectl firstboot --prompt-new-user --member-of=wheel
            "}
        );
        assert_eq!(
            exec_start(&config).unwrap(),
            "homectl firstboot --prompt-new-user --prompt-shell=yes"
        );
    }

    #[test]
    fn rejects_later_dropins() {
        let config = format!(
            "{VENDOR}\n{}",
            indoc! {"
                # /etc/systemd/system/systemd-homed-firstboot.service.d/60-local.conf
                [Service]
                ExecStart=
                ExecStart=homectl firstboot --prompt-new-user
            "}
        );
        let err = exec_start(&config).unwrap_err().to_string();
        assert!(err.contains("60-local.conf sets ExecStart="), "{err}");
    }

    #[test]
    fn rejects_invalid_exec_start() {
        assert!(exec_start("[Service]\n").is_err());
        assert!(exec_start("ExecStart=a\nExecStart=b\n").is_err());
        assert!(exec_start("ExecStart=a \\\n  b\n").is_err());
    }
}
