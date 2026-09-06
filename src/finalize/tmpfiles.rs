//! Patch the tmpfiles.d(5) files systemd ships to match the toplevel symlinks
//! (see layout.rs).

use anyhow::{Context, Result};
use cap_std_ext::cap_std::fs::Dir;
use cap_std_ext::dirext::CapStdExtDirExt;
use tracing::debug;

/// Where packages install their tmpfiles.d files. Generated files go here too.
pub(super) const USR_DIR: &str = "usr/lib/tmpfiles.d";

/// Patch the tmpfiles.d files systemd ships to match the toplevel symlinks.
pub(super) fn patch_tmpfiles(root: &Dir) -> Result<()> {
    debug!("patching tmpfiles.d entries");

    // home.conf turns /home and /srv into real directories, which conflicts
    // with the /home -> var/home and /srv -> var/srv symlinks. Nothing else in
    // the file applies.
    root.remove_file_optional(format!("{USR_DIR}/home.conf"))
        .context("removing home.conf")?;

    // provision.conf writes the credential-provisioned root ssh key to /root,
    // now a symlink to /var/roothome. Point it there directly. Drop its
    // /var/roothome line, since the image declares that directory itself and
    // systemd warns about duplicates at boot.
    let provision = format!("{USR_DIR}/provision.conf");
    if let Some(content) = root
        .read_to_string_optional(&provision)
        .context("reading provision.conf")?
    {
        let patched: String = content
            .lines()
            .map(|line| line.replace(" /root", " /var/roothome"))
            .filter(|line| !line.starts_with("d- /var/roothome "))
            .map(|line| line + "\n")
            .collect();
        root.write(&provision, patched)
            .context("writing provision.conf")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::rootfs;

    #[test]
    fn removes_home_conf() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all(USR_DIR)?;
        root.write(format!("{USR_DIR}/home.conf"), "Q /home 0755 - - -\n")?;

        patch_tmpfiles(&root)?;

        assert!(!root.exists(format!("{USR_DIR}/home.conf")));
        Ok(())
    }

    #[test]
    fn points_provision_conf_at_var_roothome() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all(USR_DIR)?;
        root.write(
            format!("{USR_DIR}/provision.conf"),
            "# Provision SSH key for root\n\
             d- /root/.ssh :0700 root :root -\n\
             f^ /root/.ssh/authorized_keys :0600 root :root - ssh.authorized_keys.root\n\
             d- /var/roothome :0700 root :root -\n",
        )?;

        patch_tmpfiles(&root)?;

        assert_eq!(
            root.read_to_string(format!("{USR_DIR}/provision.conf"))?,
            "# Provision SSH key for root\n\
             d- /var/roothome/.ssh :0700 root :root -\n\
             f^ /var/roothome/.ssh/authorized_keys :0600 root :root - ssh.authorized_keys.root\n"
        );
        Ok(())
    }

    #[test]
    fn is_idempotent() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all(USR_DIR)?;
        root.write(
            format!("{USR_DIR}/provision.conf"),
            "d- /root/.ssh :0700 root :root -\nd- /var/roothome :0700 root :root -\n",
        )?;

        patch_tmpfiles(&root)?;
        patch_tmpfiles(&root)?;

        assert_eq!(
            root.read_to_string(format!("{USR_DIR}/provision.conf"))?,
            "d- /var/roothome/.ssh :0700 root :root -\n"
        );
        Ok(())
    }

    #[test]
    fn skips_missing_files() -> Result<()> {
        let root = rootfs()?;
        patch_tmpfiles(&root)?;
        Ok(())
    }
}
