//! Check the moved users in the booted image, for every distribution.

use std::io::Write as _;
use std::os::unix::fs::MetadataExt as _;
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result};
use bootc_imagectl::command::CommandRunExt;
use bootc_imagectl::id::Gid;
use bootc_imagectl::passwd::{Entry as _, Group, Passwd};
use cap_std_ext::cap_std::ambient_authority;
use cap_std_ext::cap_std::fs_utf8::Dir;
use rustix::time::{ClockId, clock_gettime};

pub(crate) struct MovedUser {
    pub(crate) name: &'static str,
    pub(crate) uid: u32,
    /// A group the user is a member of through a membership file.
    pub(crate) group: &'static str,
    pub(crate) password: &'static str,
    /// A system user the image moved too, by name and UID.
    pub(crate) system_user: (&'static str, u32),
}

/// The user and the system user resolve through NSS, with the user's groups
/// and its password hash from the privileged record.
pub(crate) fn resolves_through_nss(user: &MovedUser) -> Result<()> {
    let found = uzers::get_user_by_name(user.name).expect("the user resolves");
    assert_eq!(found.uid(), user.uid);
    let groups: Vec<String> = uzers::get_user_groups(user.name, found.primary_group_id())
        .expect("the groups of the user")
        .iter()
        .map(|group| group.name().to_string_lossy().into_owned())
        .collect();
    assert!(groups.contains(&user.group.to_owned()), "{groups:?}");
    let (name, uid) = user.system_user;
    assert_eq!(
        uzers::get_user_by_uid(uid).map(|found| found.name().to_owned()),
        Some(name.into())
    );

    let shadow = Command::new("getent")
        .args(["shadow", user.name])
        .output_string()?;
    assert!(shadow.starts_with(&format!("{}:$", user.name)), "{shadow}");
    Ok(())
}

/// `pam_unix` accepts the user's password and rejects another.
pub(crate) fn accepts_password(user: &MovedUser) -> Result<()> {
    // pam_unix verifies passwords through this helper, which reads a
    // NUL-terminated password from stdin.
    let helper = ["/usr/bin/unix_chkpwd", "/usr/sbin/unix_chkpwd"]
        .into_iter()
        .find(|path| std::path::Path::new(path).exists())
        .expect("pam_unix's helper");
    let verify = |password: &str| -> Result<bool> {
        let mut child = Command::new(helper)
            .args([user.name, "nonull"])
            .stdin(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .expect("a piped stdin")
            .write_all(format!("{password}\0").as_bytes())?;
        Ok(child.wait()?.success())
    };
    assert!(verify(user.password)?);
    assert!(!verify("wrong")?);
    Ok(())
}

/// The first boot, including systemd-sysusers, left the account files in
/// /etc unchanged.
pub(crate) fn account_files_unchanged() -> Result<()> {
    let result = Command::new("systemctl")
        .args(["show", "-P", "Result", "systemd-sysusers.service"])
        .output_string()?;
    assert_eq!(result.trim(), "success");

    // Check that the boot rewrote no account file. /etc is an overlay, so a
    // rewrite copies the file with a new mtime.
    let boot = boot_time();
    for file in ["/etc/passwd", "/etc/shadow", "/etc/group", "/etc/gshadow"] {
        let mtime = std::fs::metadata(file)
            .with_context(|| format!("reading {file}"))?
            .mtime();
        assert!(mtime < boot, "{file} was written at boot");
    }

    let root = Dir::open_ambient_dir("/", ambient_authority())?;
    let users = Passwd::read_all(&root)?;
    let names: Vec<&str> = users.iter().map(|user| user.name.as_str()).collect();
    assert_eq!(names, ["root", "nobody"]);
    let gids: Vec<Gid> = Group::read_all(&root)?
        .iter()
        .map(|group| group.gid)
        .collect();
    assert_eq!(gids, [Gid::ROOT, Gid::NOBODY]);
    Ok(())
}

/// The time the system booted, in seconds since the epoch.
fn boot_time() -> i64 {
    let now = clock_gettime(ClockId::Realtime);
    let since_boot = clock_gettime(ClockId::Boottime);
    now.tv_sec - since_boot.tv_sec
}
