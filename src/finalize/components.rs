//! Set the chunkah component of the package files.
//!
//! chunkah splits an image into layers by component. For package managers
//! chunkah does not yet support, e.g. dpkg
//! (<https://github.com/coreos/chunkah/issues/155>), finalize writes the
//! component into a `user.component` xattr, which chunkah applies to a
//! directory and everything below it. It is set on the topmost directories
//! owned by one component, and on the files outside them.

use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result};
use cap_std_ext::camino::{Utf8Path, Utf8PathBuf};
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::{CapStdExtDirExt, CapStdExtDirExtUtf8};
use tracing::info;

use crate::distro::{Components, Distro, PackageName};

/// The xattr chunkah reads a path's component from.
const XATTR: &str = "user.component";

/// The component of a path, or `Shared` if several claim it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner<'a> {
    Component(&'a PackageName),
    Shared,
}

impl Owner<'_> {
    /// The owner of a path claimed by both `self` and `other`.
    fn merge(self, other: Self) -> Self {
        if self == other { self } else { Self::Shared }
    }
}

/// A file or symlink a package installed, and its owner.
#[derive(Debug)]
struct PackageFile<'a> {
    owner: Owner<'a>,
    symlink: bool,
}

/// Set the component of the package files.
///
/// # Errors
///
/// Fails if the distribution does not implement chunkah xattrs, the package
/// database cannot be read or an xattr cannot be set.
pub(super) fn finalize(root: &Dir, distro: &dyn Distro) -> Result<()> {
    let components = distro.package_components(root)?;
    let files = package_files(root, &components)?;
    let dirs = wholly_owned_dirs(&files);
    for (dir, component) in &dirs {
        set_component(root, dir, component)?;
    }
    let mut set = dirs.len();
    for (path, file) in &files {
        // The kernel rejects user xattrs on symlinks.
        if file.symlink || path.ancestors().skip(1).any(|dir| dirs.contains_key(dir)) {
            continue;
        }
        // A shared path gets no component.
        if let Owner::Component(component) = file.owner {
            set_component(root, path, component)?;
            set += 1;
        }
    }
    info!(
        "set {XATTR} on {set} path(s) for {} component(s)",
        components.len()
    );
    Ok(())
}

/// The package files in the rootfs, by path. Directories are left out, since
/// packages may share them.
fn package_files<'a>(
    root: &Dir,
    components: &'a Components,
) -> Result<BTreeMap<Utf8PathBuf, PackageFile<'a>>> {
    let mut files: BTreeMap<Utf8PathBuf, PackageFile<'a>> = BTreeMap::new();
    let mut parents: HashMap<&Utf8Path, Option<Utf8PathBuf>> = HashMap::new();
    for (component, paths) in components {
        for path in paths {
            let path = path.strip_prefix("/").unwrap_or(path);
            let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
                continue;
            };
            // Resolves merged /usr aliases, e.g. /lib/udev to /usr/lib/udev.
            let parent = parents.entry(parent).or_insert_with(|| {
                if parent.as_str().is_empty() {
                    Some(Utf8PathBuf::new())
                } else {
                    root.canonicalize(parent).ok()
                }
            });
            let Some(parent) = parent else {
                continue;
            };
            let path = parent.join(name);
            let Some(meta) = root.symlink_metadata_optional(&path)? else {
                continue;
            };
            if meta.is_dir() {
                continue;
            }
            let claim = Owner::Component(component);
            files
                .entry(path)
                .and_modify(|file| file.owner = file.owner.merge(claim))
                .or_insert(PackageFile {
                    owner: claim,
                    symlink: meta.is_symlink(),
                });
        }
    }
    Ok(files)
}

/// The topmost directories whose package files all come from one component.
fn wholly_owned_dirs<'a>(
    files: &'a BTreeMap<Utf8PathBuf, PackageFile<'a>>,
) -> BTreeMap<&'a Utf8Path, &'a PackageName> {
    let mut owners: HashMap<&Utf8Path, Owner<'a>> = HashMap::new();
    for (path, file) in files {
        // The last ancestor is the rootfs itself.
        for dir in path
            .ancestors()
            .skip(1)
            .filter(|dir| !dir.as_str().is_empty())
        {
            owners
                .entry(dir)
                .and_modify(|owner| *owner = owner.merge(file.owner))
                .or_insert(file.owner);
        }
    }
    let component = |dir: &Utf8Path| match owners.get(dir) {
        Some(Owner::Component(component)) => Some(*component),
        _ => None,
    };
    owners
        .keys()
        .filter(|dir| dir.parent().and_then(component).is_none())
        .filter_map(|dir| Some((*dir, component(dir)?)))
        .collect()
}

/// Set the component of `path`.
fn set_component(root: &Dir, path: &Utf8Path, component: &PackageName) -> Result<()> {
    root.as_cap_std()
        .setxattr(path, XATTR, component.as_str())
        .with_context(|| format!("setting the {XATTR} xattr of /{path} to {component}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TestDistro, rootfs};

    fn components(entries: &[(&str, &[&str])]) -> Result<Components> {
        entries
            .iter()
            .map(|(component, paths)| {
                let paths = paths.iter().map(Utf8PathBuf::from).collect();
                Ok((component.parse()?, paths))
            })
            .collect()
    }

    fn component_of(root: &Dir, path: &str) -> Result<Option<String>> {
        let value = root.as_cap_std().getxattr(path, XATTR)?;
        Ok(value.map(String::from_utf8).transpose()?)
    }

    #[test]
    fn sets_components() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("usr/bin")?;
        root.create_dir_all("usr/share/bash/sub")?;
        root.create_dir_all("usr/lib/udev")?;
        root.symlink("usr/lib", "lib")?;
        root.write("init", "")?;
        root.write("usr/bin/bash", "")?;
        root.write("usr/bin/udevadm", "")?;
        root.symlink("bash", "usr/bin/sh")?;
        root.write("usr/share/bash/sub/file", "")?;
        root.write("usr/lib/udev/rule", "")?;
        let components = components(&[
            (
                "bash",
                &[
                    "/.",
                    "/usr",
                    "/usr/bin",
                    "/usr/bin/bash",
                    "/usr/bin/sh",
                    "/usr/share/bash/sub/file",
                    "/usr/share/doc/bash/excluded",
                    "/init",
                ],
            ),
            ("systemd", &["/usr/bin/udevadm", "/lib/udev/rule"]),
        ])?;
        let files = package_files(&root, &components)?;
        assert_eq!(
            Vec::from_iter(files.keys().map(|path| path.as_str())),
            [
                "init",
                "usr/bin/bash",
                "usr/bin/sh",
                "usr/bin/udevadm",
                "usr/lib/udev/rule",
                "usr/share/bash/sub/file"
            ]
        );
        let dirs = wholly_owned_dirs(&files);
        assert_eq!(
            BTreeMap::from_iter(dirs.iter().map(|(dir, name)| (*dir, name.as_str()))),
            BTreeMap::from([
                (Utf8Path::new("usr/lib"), "systemd"),
                (Utf8Path::new("usr/share"), "bash"),
            ])
        );

        for (path, component) in [
            ("usr/bin/bash", "bash"),
            ("usr/bin/udevadm", "systemd"),
            ("usr/lib", "systemd"),
            ("usr/share", "bash"),
        ] {
            set_component(&root, Utf8Path::new(path), &component.parse()?)?;
            assert_eq!(component_of(&root, path)?.as_deref(), Some(component));
        }
        Ok(())
    }

    #[test]
    fn skips_shared_paths() -> Result<()> {
        let root = rootfs()?;
        root.create_dir_all("usr/bin")?;
        root.write("usr/bin/diverted", "")?;
        let components =
            components(&[("a", &["/usr/bin/diverted"]), ("b", &["/usr/bin/diverted"])])?;
        let files = package_files(&root, &components)?;
        assert!(wholly_owned_dirs(&files).is_empty());
        Ok(())
    }

    #[test]
    fn fails_if_not_implemented() -> Result<()> {
        let root = rootfs()?;
        let err = finalize(&root, &TestDistro::default()).unwrap_err();
        assert!(err.to_string().contains("not implemented"), "{err}");
        Ok(())
    }
}
