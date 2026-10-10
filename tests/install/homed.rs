//! Check the user systemd-homed's first boot wizard created.

use std::process::Command;

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;

/// The user the xtask passes a credential for.
const USER: &str = "alice";

/// systemd-homed manages the user, which is a member of `groups`.
pub(crate) fn adds_wizard_user_to(groups: &[&str]) -> Result<()> {
    Command::new("homectl")
        .args(["inspect", USER])
        .output_string()?;
    let user = uzers::get_user_by_name(USER).expect("the user resolves through NSS");
    let found: Vec<String> = uzers::get_user_groups(USER, user.primary_group_id())
        .expect("the groups of the user")
        .iter()
        .map(|group| group.name().to_string_lossy().into_owned())
        .collect();
    for group in groups {
        assert!(
            found.iter().any(|found| found == group),
            "{group}: {found:?}"
        );
    }
    Ok(())
}
