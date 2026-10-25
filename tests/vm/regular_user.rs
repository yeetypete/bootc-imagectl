//! Check the regular user built into the booted image.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;
use bootc_imagectl::testing::{self, Feature, IntegrationTest, Requirement, integration_test};

/// The user and the system users resolve through NSS, with the user's
/// groups and its password hash from the privileged record.
fn resolves_moved_users_through_nss() -> Result<()> {
    let accounts = testing::image_accounts()?;
    let (name, uid) = accounts.user;
    let found = uzers::get_user_by_name(name).expect("the user resolves");
    assert_eq!(found.uid(), uid);
    let groups: Vec<String> = uzers::get_user_groups(name, found.primary_group_id())
        .expect("the groups of the user")
        .iter()
        .map(|group| group.name().to_string_lossy().into_owned())
        .collect();
    assert!(groups.contains(&accounts.group.to_owned()), "{groups:?}");
    for &(system_user, uid) in accounts.system_users {
        assert_eq!(
            uzers::get_user_by_uid(uid).map(|found| found.name().to_owned()),
            Some(system_user.into())
        );
    }

    let shadow = Command::new("getent")
        .args(["shadow", name])
        .run_get_string()?;
    assert!(shadow.starts_with(&format!("{name}:$")), "{shadow}");
    Ok(())
}

/// `pam_unix` accepts the user's password and rejects another.
fn accepts_password_of_moved_user() -> Result<()> {
    let accounts = testing::image_accounts()?;
    let (name, _) = accounts.user;
    // pam_unix verifies passwords through this helper, which reads a
    // NUL-terminated password from stdin.
    let helper = ["/usr/bin/unix_chkpwd", "/usr/sbin/unix_chkpwd"]
        .into_iter()
        .find(|path| Path::new(path).exists())
        .expect("pam_unix's helper");
    let verify = |password: &str| -> Result<bool> {
        let mut child = Command::new(helper)
            .args([name, "nonull"])
            .stdin(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .expect("a piped stdin")
            .write_all(format!("{password}\0").as_bytes())?;
        Ok(child.wait()?.success())
    };
    assert!(verify(accounts.password)?);
    assert!(!verify("wrong")?);
    Ok(())
}

const REQUIRES: &[Requirement] = &[Requirement::Without(Feature::Homed)];

pub(crate) const TESTS: &[IntegrationTest] = &[
    integration_test!(resolves_moved_users_through_nss, REQUIRES),
    integration_test!(accepts_password_of_moved_user, REQUIRES),
];
