//! Check `bootc-imagectl finalize` on Arch Linux.

use std::process::Command;
use std::sync::LazyLock;

use anyhow::Result;
use bootc_imagectl::passwd::{Entry, Passwd};
use cap_std_ext::cap_std::fs::MetadataExt;
use cap_std_ext::cap_std::fs_utf8::Dir;

use crate::finalize::{ROOT, names};

#[test]
fn moves_pacman_database_and_removes_its_indexes() -> Result<()> {
    let pacman_conf = ROOT.read_to_string("etc/pacman.conf")?;
    assert!(
        pacman_conf
            .lines()
            .any(|line| line == "DBPath = /usr/lib/sysimage/pacman/"),
        "{pacman_conf}"
    );
    assert!(ROOT.exists("usr/lib/sysimage/pacman/local/ALPM_DB_VERSION"));
    assert!(names("usr/lib/sysimage/pacman/sync")?.is_empty());
    Ok(())
}

#[test]
fn pacman_lists_packages_from_moved_database() -> Result<()> {
    LazyLock::force(&ROOT);
    let output = Command::new("pacman").arg("-Q").output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let packages = String::from_utf8(output.stdout)?;
    assert!(
        packages.lines().any(|line| line.starts_with("pacman ")),
        "{packages}"
    );
    Ok(())
}

#[test]
fn locks_uids_of_packages_users() -> Result<()> {
    assert!(ROOT.exists("usr/lib/sysusers.d/00-bootc-imagectl.conf"));
    let root = Dir::from_cap_std(ROOT.try_clone()?);
    let users = Passwd::read_all(&root)?;
    let uid = |name: &str| {
        users
            .iter()
            .find(|user| user.name.as_str() == name)
            .map(|user| user.uid)
    };
    assert_eq!(uid("avahi"), Some(969));
    assert_eq!(uid("uuidd"), Some(970));
    Ok(())
}

#[test]
fn writes_user_records_to_userdb() -> Result<()> {
    let record = ROOT.read_to_string("usr/lib/userdb/avahi.user")?;
    assert!(record.contains("\"disposition\": \"system\""), "{record}");
    assert!(record.contains("\"uid\": 969"), "{record}");
    assert_eq!(
        ROOT.read_link("usr/lib/userdb/969.user")?,
        std::path::Path::new("avahi.user")
    );
    assert!(!ROOT.exists("usr/lib/userdb/root.user"));
    Ok(())
}

#[test]
fn writes_privileged_records_for_users_with_a_password() -> Result<()> {
    let record = ROOT.read_to_string("usr/lib/userdb/archie.user-privileged")?;
    assert!(
        record.contains("\"hashedPassword\": [\n      \"$"),
        "{record}"
    );
    assert_eq!(
        ROOT.metadata("usr/lib/userdb/archie.user-privileged")?
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        ROOT.read_link("usr/lib/userdb/1000.user-privileged")?,
        std::path::Path::new("archie.user-privileged")
    );
    assert!(
        !ROOT
            .read_to_string("usr/lib/userdb/archie.user")?
            .contains("locked")
    );
    assert!(
        ROOT.read_to_string("usr/lib/userdb/avahi.user")?
            .contains("\"locked\": true")
    );
    assert!(!ROOT.exists("usr/lib/userdb/avahi.user-privileged"));
    Ok(())
}

#[test]
fn writes_membership_files_for_primary_and_auxiliary_groups() -> Result<()> {
    assert_eq!(
        ROOT.read_to_string("usr/lib/userdb/archie:archie.membership")?,
        "{}\n"
    );
    assert!(ROOT.exists("usr/lib/userdb/archie:wheel.membership"));
    assert!(ROOT.exists("usr/lib/userdb/avahi:avahi.membership"));
    assert!(!ROOT.exists("usr/lib/userdb/root:root.membership"));
    Ok(())
}
