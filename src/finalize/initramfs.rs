//! Build the initramfs next to the kernel under /usr/lib/modules.
//! This is where bootc expects the kernel andgit  initramfs.

use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use cap_std_ext::cap_std::fs::{Dir, Permissions, PermissionsExt};
use cap_std_ext::dirext::CapStdExtDirExt;
use tracing::debug;

use crate::command::CommandRunExt;

/// Where the kernel packages install to.
const MODULES: &str = "usr/lib/modules";

/// The kernel the image ships. bootc requires exactly one.
fn kernel_version(root: &Dir) -> Result<String> {
    let mut kvers = Vec::new();
    if let Some(modules) = root.open_dir_optional(MODULES)? {
        for entry in modules.entries()? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|name| anyhow!("kernel directory {} is not UTF-8", name.display()))?;
                kvers.push(name);
            }
        }
    }
    kvers.sort();
    match kvers.as_slice() {
        [kver] => Ok(kver.clone()),
        [] => bail!("no kernel under /{MODULES}"),
        _ => bail!(
            "expected exactly one kernel under /{MODULES}, found {}",
            kvers.join(" ")
        ),
    }
}

/// Build the initramfs.
pub(super) fn build_initramfs(root: &Dir) -> Result<()> {
    let kver = kernel_version(root)?;
    debug!("building the initramfs for {kver}");
    let moddir = format!("{MODULES}/{kver}");
    if !root.is_file(format!("{moddir}/vmlinuz")) {
        bail!("no kernel image at /{moddir}/vmlinuz");
    }
    let initramfs = format!("{moddir}/initramfs.img");

    Command::new("depmod").arg(&kver).run()?;
    Command::new("dracut")
        .args([
            "--force",
            "--no-hostonly",
            "--reproducible",
            "--zstd",
            "--verbose",
        ])
        .args(["--kver", &kver])
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

    #[test]
    fn finds_kernel() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all(format!("{MODULES}/7.1.11-arch1-1"))?;
        root.write(format!("{MODULES}/stray-file"), b"not a kernel")?;
        assert_eq!(kernel_version(&root)?, "7.1.11-arch1-1");
        Ok(())
    }

    #[test]
    fn rejects_no_kernel_or_multiple() -> Result<()> {
        let root = rootfs()?;
        assert!(kernel_version(&root).is_err());
        root.create_dir_all(format!("{MODULES}/7.1.11-arch1-1"))?;
        root.create_dir_all(format!("{MODULES}/7.1.12-arch1-1"))?;
        let err = kernel_version(&root).unwrap_err().to_string();
        assert!(err.contains("7.1.11-arch1-1 7.1.12-arch1-1"), "{err}");
        Ok(())
    }
}
