//! Check `bootc-imagectl finalize` on Arch Linux.

use std::process::Command;
use std::sync::LazyLock;

use anyhow::Result;
use bootc_imagectl::passwd::{Entry, Group, Passwd, Shadow};
use cap_std_ext::cap_std::fs::MetadataExt;
use cap_std_ext::cap_std::fs_utf8::Dir;

use crate::finalize::{ROOT, initramfs_paths, names, var_tmpfiles};

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
fn locks_uids_of_package_users() {
    assert!(ROOT.exists("usr/lib/sysusers.d/00-bootc-imagectl.conf"));
    let uid = |name: &str| uzers::get_user_by_name(name).map(|user| user.uid());
    assert_eq!(uid("avahi"), Some(969));
    assert_eq!(uid("uuidd"), Some(970));
}

#[test]
fn moves_users_out_of_etc_and_resolves_them_through_nss() -> Result<()> {
    let root = Dir::from_cap_std(ROOT.try_clone()?);
    let names = |users: &[Passwd]| -> Vec<String> {
        users.iter().map(|user| user.name.to_string()).collect()
    };
    assert_eq!(names(&Passwd::read_all(&root)?), ["root", "nobody"]);
    let shadow: Vec<String> = Shadow::read_all(&root)?
        .iter()
        .map(|entry| entry.name.to_string())
        .collect();
    assert_eq!(shadow, ["root", "nobody"]);
    // Members stay in /etc/group, where systemd-sysusers maintains them.
    let wheel = Group::read_all(&root)?
        .into_iter()
        .find(|group| group.name.as_str() == "wheel")
        .expect("the wheel group");
    assert_eq!(wheel.members.len(), 1, "{wheel}");
    assert_eq!(wheel.members[0].as_str(), "archie", "{wheel}");

    let archie = uzers::get_user_by_name("archie").expect("archie resolves through NSS");
    assert_eq!(archie.uid(), 1000);
    let groups: Vec<String> = uzers::get_user_groups("archie", archie.primary_group_id())
        .expect("the groups of archie")
        .iter()
        .map(|group| group.name().to_string_lossy().into_owned())
        .collect();
    assert!(groups.contains(&"wheel".to_owned()), "{groups:?}");
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
fn writes_privileged_records_for_users_with_passwords() -> Result<()> {
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

#[test]
fn keeps_only_runtime_directories_in_var() -> Result<()> {
    assert_eq!(names("var")?, ["lock", "run", "tmp"]);
    Ok(())
}

#[test]
fn records_mail_spool_in_var_tmpfiles() -> Result<()> {
    let var = var_tmpfiles()?;
    assert!(var.contains("L /var/mail - - - - spool/mail\n"), "{var}");
    assert!(
        var.contains("d /var/spool/mail 1777 root root -\n"),
        "{var}"
    );
    Ok(())
}

#[test]
fn initramfs_carries_user_records() -> Result<()> {
    let paths = initramfs_paths()?;
    for path in ["usr/lib/userdb/avahi.user", "usr/lib/userdb/969.user"] {
        assert!(paths.iter().any(|found| found == path), "{path}");
    }
    Ok(())
}
