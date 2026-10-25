//! Check the installed Ubuntu desktop example.

use std::process::Command;

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;
use bootc_imagectl::testing::{IntegrationTest, Requirement, integration_test};

fn starts_display_manager() -> Result<()> {
    Command::new("systemctl")
        .args(["is-active", "graphical.target", "gdm.service"])
        .run_get_string()?;
    Ok(())
}

const REQUIRES: &[Requirement] = &[Requirement::Image("ubuntu-desktop-example")];

pub(crate) const TESTS: &[IntegrationTest] = &[integration_test!(starts_display_manager, REQUIRES)];
