//! Build the initramfs next to the kernel under /usr/lib/modules.
//! This is where bootc expects the kernel and initramfs.

use std::process::Command;

use anyhow::{Context, Result, bail};
use cap_std_ext::cap_std::fs_utf8::{Dir, Permissions, PermissionsExt};
use cap_std_ext::dirext::CapStdExtDirExtUtf8;
use tracing::debug;

use super::GENERATED_HEADER;
use crate::command::CommandRunExt;
use crate::distro::Distro;
use crate::userdb::DROPIN_DIR;

/// Where the kernel packages install to.
const MODULES: &str = "usr/lib/modules";

/// Where packages install their dracut configuration.
const DRACUT_CONF_DIR: &str = "usr/lib/dracut/dracut.conf.d";

/// The dracut configuration for every initramfs built in the image.
const DRACUT_CONF: &str = "usr/lib/dracut/dracut.conf.d/10-bootc-imagectl.conf";

/// The directories under /usr/lib/modules.
fn module_dirs(root: &Dir) -> Result<Vec<String>> {
    let mut names = Vec::new();
    if let Some(modules) = root.open_dir_optional(MODULES)? {
        for entry in modules.entries()? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                names.push(entry.file_name()?);
            }
        }
    }
    Ok(names)
}

/// The kernel the image ships. bootc requires exactly one located at
/// /usr/lib/modules/<kver>/vmlinuz.
fn kernel_version(root: &Dir) -> Result<String> {
    let mut kvers = Vec::new();
    for name in module_dirs(root)? {
        if root.is_file(format!("{MODULES}/{name}/vmlinuz")) {
            kvers.push(name);
        } else {
            debug!("skipping /{MODULES}/{name}, which has no vmlinuz");
        }
    }
    kvers.sort();
    match kvers.as_slice() {
        [kver] => Ok(kver.clone()),
        [] => bail!("no kernel image under /{MODULES}"),
        _ => bail!(
            "expected exactly one kernel under /{MODULES}, found {}",
            kvers.join(" ")
        ),
    }
}

/// Write the dracut configuration, see dracut.conf(5).
fn write_dracut_conf(root: &Dir) -> Result<()> {
    let mut records = Vec::new();
    if let Some(userdb) = root.open_dir_optional(DROPIN_DIR)? {
        for entry in userdb.entries()? {
            records.push(format!("/{DROPIN_DIR}/{}", entry?.file_name()?));
        }
    }
    records.sort();
    let records = records.join(" ");
    let content = format!(
        r#"{GENERATED_HEADER}
reproducible="yes"
compress="zstd"
hostonly="no"
add_dracutmodules+=" bootc "
install_items+=" /etc/nsswitch.conf {records} "
"#
    );
    root.create_dir_all(DRACUT_CONF_DIR)
        .with_context(|| format!("creating /{DRACUT_CONF_DIR}"))?;
    root.write(DRACUT_CONF, content)
        .with_context(|| format!("writing /{DRACUT_CONF}"))
}

/// Stage the kernel and build the initramfs.
pub(super) fn finalize(root: &Dir, distro: &dyn Distro) -> Result<()> {
    for name in module_dirs(root)? {
        distro
            .stage_kernel(root, &name)
            .with_context(|| format!("staging the kernel {name}"))?;
    }
    let kver = kernel_version(root)?;
    debug!("building the initramfs for {kver}");
    let moddir = format!("{MODULES}/{kver}");
    let initramfs = format!("{moddir}/initramfs.img");

    write_dracut_conf(root)?;
    Command::new("depmod").arg(&kver).run()?;
    Command::new("dracut")
        .args(["--force", "--verbose", "--kver", &kver])
        .arg(format!("/{initramfs}"))
        .run()?;
    root.set_permissions(&initramfs, Permissions::from_mode(0o600))
        .with_context(|| format!("setting the mode of /{initramfs}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::rootfs;

    /// A kernel's module directory with its kernel image staged.
    fn install_kernel(root: &Dir, kver: &str) -> Result<()> {
        root.create_dir_all(format!("{MODULES}/{kver}"))?;
        root.write(format!("{MODULES}/{kver}/vmlinuz"), b"kernel")?;
        Ok(())
    }

    #[test]
    fn finds_kernel() -> Result<()> {
        let root = rootfs()?;
        install_kernel(&root, "7.1.11-arch1-1")?;
        root.write(format!("{MODULES}/stray-file"), b"not a kernel")?;
        root.create_dir_all(format!("{MODULES}/kernel/extra"))?;
        assert_eq!(kernel_version(&root)?, "7.1.11-arch1-1");
        Ok(())
    }

    #[test]
    fn configures_dracut() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all(DROPIN_DIR)?;
        root.write(format!("{DROPIN_DIR}/root.user"), b"{}")?;
        write_dracut_conf(&root)?;
        let conf = root.read_to_string(DRACUT_CONF)?;
        assert!(conf.contains("add_dracutmodules+=\" bootc \"\n"), "{conf}");
        assert!(
            conf.contains("install_items+=\" /etc/nsswitch.conf /usr/lib/userdb/root.user \"\n"),
            "{conf}"
        );
        Ok(())
    }

    #[test]
    fn rejects_missing_or_multiple_kernels() -> Result<()> {
        let root = rootfs()?;
        assert!(kernel_version(&root).is_err());
        install_kernel(&root, "7.1.11-arch1-1")?;
        install_kernel(&root, "7.1.12-arch1-1")?;
        let err = kernel_version(&root).unwrap_err().to_string();
        assert!(err.contains("7.1.11-arch1-1 7.1.12-arch1-1"), "{err}");
        Ok(())
    }
}
