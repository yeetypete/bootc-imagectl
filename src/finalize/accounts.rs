//! Fix the UIDs and GIDs of the users and groups the build created with the
//! sysusers lock file, and move the users and their group memberships from
//! /etc into /usr/lib/userdb, where an upgrade replaces them.

use anyhow::Result;
use cap_std_ext::camino::Utf8Path;
use cap_std_ext::cap_std::fs_utf8::Dir;
use tracing::{debug, info};

use crate::passwd::{Entry, Group, Gshadow, Passwd, Shadow};
use crate::sysusers;
use crate::sysusers::lockfile::LockFile;

/// Read the account files in /etc, the sysusers.d files and the lock file at
/// `lock`, an absolute path in the image.
pub(super) fn read_accounts(root: &Dir, lock: &Utf8Path) -> Result<()> {
    let lock = lock.strip_prefix("/").unwrap_or(lock);

    let users = Passwd::read_all(root)?;
    let groups = Group::read_all(root)?;
    let shadow = Shadow::read_all(root)?;
    let gshadow = Gshadow::read_all(root)?;
    debug!(
        "read {} users and {} groups from /etc, with {} shadow and {} gshadow entries",
        users.len(),
        groups.len(),
        shadow.len(),
        gshadow.len()
    );

    let files = sysusers::read_all(root)?;
    debug!("read {} sysusers.d files", files.len());
    if let Some(lock) = LockFile::read(root, lock)? {
        debug!(
            "the sysusers lock file has {} blocks with {} entries",
            lock.blocks.len(),
            lock.entries().count()
        );
    } else {
        info!("no sysusers lock file at /{lock} yet");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::rootfs;

    #[test]
    fn reads_with_and_without_a_lock_file() -> Result<()> {
        let root = rootfs()?;
        root.create_dir("etc")?;
        root.write("etc/passwd", "root:x:0:0:root:/root:/bin/bash\n")?;
        root.write("etc/group", "root:x:0:\n")?;
        root.write("etc/shadow", "root:*:::::::\n")?;
        root.write("etc/gshadow", "root:::root\n")?;
        let lock = Utf8Path::new("/usr/lib/sysusers.d/00-bootc-imagectl.conf");
        read_accounts(&root, lock)?;

        root.create_dir_all("usr/lib/sysusers.d")?;
        root.write(
            "usr/lib/sysusers.d/00-bootc-imagectl.conf",
            "# package: x\ng x 1\n",
        )?;
        read_accounts(&root, lock)?;

        root.write("usr/lib/sysusers.d/00-bootc-imagectl.conf", "g x -\n")?;
        let err = format!("{:#}", read_accounts(&root, lock).unwrap_err());
        assert!(err.contains("00-bootc-imagectl.conf"), "{err}");
        Ok(())
    }
}
