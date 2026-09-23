//! Check the booted Arch Linux image.

use std::io::Write as _;
use std::process::{Command, Stdio};

use anyhow::Result;
use bootc_imagectl::command::CommandRunExt;

#[test]
fn pacman_lists_packages() -> Result<()> {
    let packages = Command::new("pacman").arg("-Q").output_string()?;
    assert!(
        packages.lines().any(|line| line.starts_with("pacman ")),
        "{packages}"
    );
    Ok(())
}

#[test]
fn resolves_moved_users_through_nss() -> Result<()> {
    let archie = uzers::get_user_by_name("archie").expect("archie resolves");
    assert_eq!(archie.uid(), 1000);
    let groups: Vec<String> = uzers::get_user_groups("archie", archie.primary_group_id())
        .expect("the groups of archie")
        .iter()
        .map(|group| group.name().to_string_lossy().into_owned())
        .collect();
    assert!(groups.contains(&"wheel".to_owned()), "{groups:?}");
    assert_eq!(
        uzers::get_user_by_uid(969).map(|user| user.name().to_owned()),
        Some("avahi".into())
    );

    // The password hash comes from the privileged record.
    let shadow = Command::new("getent")
        .args(["shadow", "archie"])
        .output_string()?;
    assert!(shadow.starts_with("archie:$"), "{shadow}");
    Ok(())
}

#[test]
fn accepts_password_of_moved_user() -> Result<()> {
    // pam_unix verifies passwords through this helper, which reads a
    // NUL-terminated password from stdin.
    let verify = |password: &str| -> Result<bool> {
        let mut child = Command::new("/usr/bin/unix_chkpwd")
            .args(["archie", "nonull"])
            .stdin(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .expect("a piped stdin")
            .write_all(format!("{password}\0").as_bytes())?;
        Ok(child.wait()?.success())
    };
    assert!(verify("password")?);
    assert!(!verify("wrong")?);
    Ok(())
}

#[test]
fn sysusers_changes_nothing_at_boot() -> Result<()> {
    let result = Command::new("systemctl")
        .args([
            "show",
            "--value",
            "-p",
            "Result",
            "systemd-sysusers.service",
        ])
        .output_string()?;
    assert_eq!(result.trim(), "success");
    let journal = Command::new("journalctl")
        .args(["-b", "-u", "systemd-sysusers.service", "-o", "cat"])
        .output_string()?;
    assert!(!journal.contains("Creating"), "{journal}");

    // It found every user through NSS and every member in /etc/group, and
    // left both files as the image shipped them.
    let passwd = std::fs::read_to_string("/etc/passwd")?;
    let names: Vec<&str> = passwd
        .lines()
        .filter_map(|line| line.split(':').next())
        .collect();
    assert_eq!(names, ["root", "nobody"]);
    let group = std::fs::read_to_string("/etc/group")?;
    assert!(
        group.lines().any(|line| line == "wheel:x:998:archie"),
        "{group}"
    );
    Ok(())
}
