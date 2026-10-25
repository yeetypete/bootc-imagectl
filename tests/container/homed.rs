//! Check how finalize configured systemd-homed's first boot wizard.

use anyhow::Result;
use bootc_imagectl::testing::{self, Feature, IntegrationTest, Requirement, integration_test};

use crate::finalize::ROOT;

/// The drop-in finalize writes for the wizard.
const DROPIN: &str =
    "usr/lib/systemd/system/systemd-homed-firstboot.service.d/50-bootc-imagectl.conf";

fn adds_wizard_user_to_groups() -> Result<()> {
    let dropin = ROOT.read_to_string(DROPIN)?;
    let groups = testing::wizard_groups()?;
    let suffix = format!(" --member-of={}", groups.join(","));
    assert!(
        dropin
            .lines()
            .any(|line| line.starts_with("ExecStart=homectl firstboot ") && line.ends_with(&suffix)),
        "{dropin}"
    );
    Ok(())
}

const REQUIRES: &[Requirement] = &[Requirement::Feature(Feature::Homed)];

pub(crate) const TESTS: &[IntegrationTest] =
    &[integration_test!(adds_wizard_user_to_groups, REQUIRES)];
