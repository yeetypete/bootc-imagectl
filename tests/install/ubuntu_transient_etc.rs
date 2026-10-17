//! Check the installed Ubuntu system with a transient /etc.

use anyhow::Result;

use crate::{homed, status, transient_etc};

#[test]
fn updates_from_target_imgref() -> Result<()> {
    status::updates_from("localhost/bootc-imagectl-test:ubuntu-transient-etc")
}

#[test]
fn adds_wizard_user_to_groups() -> Result<()> {
    homed::adds_wizard_user_to(&["sudo", "adm", "cdrom", "dip", "plugdev", "users"])
}

#[test]
fn stores_wizard_user_unencrypted() -> Result<()> {
    homed::stores_wizard_user_unencrypted()
}

#[test]
fn mounts_etc_transient() -> Result<()> {
    transient_etc::mounts_etc_transient()
}

#[test]
fn keeps_wizard_user() -> Result<()> {
    transient_etc::keeps_wizard_user()
}

#[test]
fn keeps_machine_id() -> Result<()> {
    transient_etc::keeps_machine_id()
}

#[test]
fn keeps_ssh_host_keys() -> Result<()> {
    transient_etc::keeps_ssh_host_keys()
}

#[test]
fn links_pull_secret() -> Result<()> {
    transient_etc::links_pull_secret()
}
