//! Check how finalize configured systemd-homed's first boot wizard.

use anyhow::Result;

use crate::finalize::ROOT;

/// The drop-in finalize writes for the wizard.
const DROPIN: &str =
    "usr/lib/systemd/system/systemd-homed-firstboot.service.d/50-bootc-imagectl.conf";

/// The wizard adds the user it creates to `groups`.
pub(crate) fn adds_wizard_user_to(groups: &[&str]) -> Result<()> {
    let dropin = ROOT.read_to_string(DROPIN)?;
    let suffix = format!(" --member-of={}", groups.join(","));
    assert!(
        dropin
            .lines()
            .any(|line| line.starts_with("ExecStart=homectl firstboot ") && line.ends_with(&suffix)),
        "{dropin}"
    );
    Ok(())
}
