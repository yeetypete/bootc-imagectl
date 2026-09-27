//! Partition the disk with systemd-repart.

use std::fs;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow};
use cap_std_ext::camino::{Utf8Path, Utf8PathBuf};
use serde::Deserialize;

use crate::cli::Encrypt;
use crate::command::CommandRunExt;

/// The file name of the ESP's definition.
const ESP_FILE: &str = "10-esp.conf";

/// The ESP's repart.d(5) definition.
///
/// systemd-boot keeps every kernel and initramfs on the ESP, and bootc keeps
/// several deployments, so the usual 512M is too small.
const ESP_DEFINITION: &str = "\
[Partition]
Type=esp
Format=vfat
Label=esp
SizeMinBytes=1G
SizeMaxBytes=1G
";

/// The file name of the root partition's definition.
const ROOT_FILE: &str = "20-root.conf";

/// The root partition's repart.d(5) definition, filling the rest of
/// the disk. `Encrypt=` is appended.
const ROOT_DEFINITION: &str = "\
[Partition]
Type=root
Format=ext4
Label=root
";

/// The partitions systemd-repart created.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Partitions {
    pub(super) esp: Utf8PathBuf,
    pub(super) root: Utf8PathBuf,
}

/// A partition, as `systemd-repart --json` reports it.
#[derive(Debug, Deserialize)]
struct Partition {
    /// The definition the partition was created from.
    file: Utf8PathBuf,
    node: Utf8PathBuf,
}

/// Write the definitions into a new `repart.d` directory in `dir`.
pub(super) fn write_definitions(dir: &Utf8Path, encrypt: Encrypt) -> Result<Utf8PathBuf> {
    let definitions = dir.join("repart.d");
    fs::create_dir(&definitions).with_context(|| format!("creating {definitions}"))?;
    let root = format!("{ROOT_DEFINITION}Encrypt={encrypt}\n");
    for (name, content) in [(ESP_FILE, ESP_DEFINITION), (ROOT_FILE, root.as_str())] {
        let path = definitions.join(name);
        fs::write(&path, content).with_context(|| format!("writing {path}"))?;
    }
    Ok(definitions)
}

/// Wipe `device` and create the partitions `definitions` define.
pub(super) fn partition(
    device: &Utf8Path,
    definitions: &Utf8Path,
    key_file: Option<&Utf8Path>,
) -> Result<Partitions> {
    let mut command = Command::new("systemd-repart");
    command
        .arg(format!("--definitions={definitions}"))
        .args(["--empty=force", "--dry-run=no", "--json=short"])
        // composefs verifies the deployment with fs-verity, which
        // systemd-repart does not enable by default.
        .env("SYSTEMD_REPART_MKFS_OPTIONS_EXT4", "-O verity")
        .stderr(Stdio::inherit());
    if let Some(key_file) = key_file {
        command.arg(format!("--key-file={key_file}"));
    }
    let output = command.arg(device).output_string()?;
    let partitions: Vec<Partition> =
        serde_json::from_str(&output).context("parsing systemd-repart's output")?;
    find_partitions(&partitions)
}

/// Find the ESP and the root partition by the definitions they were created
/// from.
fn find_partitions(partitions: &[Partition]) -> Result<Partitions> {
    let node = |definition: &str| {
        partitions
            .iter()
            .find(|partition| partition.file.file_name() == Some(definition))
            .map(|partition| partition.node.clone())
            .ok_or_else(|| anyhow!("systemd-repart did not create {definition}"))
    };
    Ok(Partitions {
        esp: node(ESP_FILE)?,
        root: node(ROOT_FILE)?,
    })
}

#[cfg(test)]
mod tests {
    use indoc::indoc;

    use super::*;

    #[test]
    fn writes_encrypted_definitions() {
        let dir = tempfile::tempdir().unwrap();
        let dir = Utf8Path::from_path(dir.path()).unwrap();
        let definitions = write_definitions(dir, Encrypt::KeyFile).unwrap();
        assert!(
            fs::read_to_string(definitions.join("10-esp.conf"))
                .unwrap()
                .contains("Type=esp\n")
        );
        assert_eq!(
            fs::read_to_string(definitions.join("20-root.conf")).unwrap(),
            indoc! {"
                [Partition]
                Type=root
                Format=ext4
                Label=root
                Encrypt=key-file
            "}
        );
    }

    #[test]
    fn writes_unencrypted_definitions() {
        let dir = tempfile::tempdir().unwrap();
        let dir = Utf8Path::from_path(dir.path()).unwrap();
        let definitions = write_definitions(dir, Encrypt::Off).unwrap();
        assert!(
            fs::read_to_string(definitions.join("20-root.conf"))
                .unwrap()
                .ends_with("Encrypt=off\n")
        );
    }

    #[test]
    fn finds_partitions_in_repart_output() {
        let output = r#"[
            {"type":"esp","file":"/run/x/repart.d/10-esp.conf","node":"/dev/vdb1"},
            {"type":"root-x86-64","file":"/run/x/repart.d/20-root.conf","node":"/dev/vdb2"}
        ]"#;
        let partitions: Vec<Partition> = serde_json::from_str(output).unwrap();
        assert_eq!(
            find_partitions(&partitions).unwrap(),
            Partitions {
                esp: "/dev/vdb1".into(),
                root: "/dev/vdb2".into(),
            }
        );
    }

    #[test]
    fn fails_without_root_partition() {
        let output = r#"[{"file":"/run/x/repart.d/10-esp.conf","node":"/dev/vdb1"}]"#;
        let partitions: Vec<Partition> = serde_json::from_str(output).unwrap();
        let err = find_partitions(&partitions).unwrap_err();
        assert_eq!(
            err.to_string(),
            "systemd-repart did not create 20-root.conf"
        );
    }
}
