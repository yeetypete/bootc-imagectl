//! Read the UID ranges of login.defs(5).

use std::collections::HashMap;
use std::str::FromStr;

use anyhow::{Context, Result, ensure};
use cap_std_ext::cap_std::fs_utf8::Dir;
use cap_std_ext::dirext::CapStdExtDirExt;

/// The file, relative to the rootfs.
const PATH: &str = "etc/login.defs";

/// The shadow-utils default of `UID_MIN`.
const UID_MIN: u32 = 1000;

/// The shadow-utils default of `UID_MAX`.
const UID_MAX: u32 = 60000;

/// The UID ranges of login.defs(5). A key the file leaves unset takes the
/// shadow-utils default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginDefs {
    /// `SYS_UID_MAX`: the highest system UID. Every UID up to it is a system
    /// UID, including the ones below `SYS_UID_MIN`, which are reserved for
    /// static allocation.
    pub sys_uid_max: u32,
    /// `UID_MIN`: the lowest regular UID.
    pub uid_min: u32,
    /// `UID_MAX`: the highest regular UID.
    pub uid_max: u32,
}

impl Default for LoginDefs {
    fn default() -> Self {
        Self {
            sys_uid_max: UID_MIN - 1,
            uid_min: UID_MIN,
            uid_max: UID_MAX,
        }
    }
}

impl LoginDefs {
    /// Read /etc/login.defs, or take the defaults if it does not exist.
    ///
    /// # Errors
    ///
    /// Fails if the file cannot be read or a UID range key is not a number.
    pub fn read(root: &Dir) -> Result<Self> {
        let Some(content) = root
            .as_cap_std()
            .read_to_string_optional(PATH)
            .with_context(|| format!("reading /{PATH}"))?
        else {
            return Ok(Self::default());
        };
        content.parse().with_context(|| format!("parsing /{PATH}"))
    }

    /// Whether the UID is a system UID.
    #[must_use]
    pub fn is_system(&self, uid: u32) -> bool {
        uid <= self.sys_uid_max
    }

    /// Whether the UID is a regular UID.
    #[must_use]
    pub fn is_regular(&self, uid: u32) -> bool {
        (self.uid_min..=self.uid_max).contains(&uid)
    }
}

/// Parse the file: one `KEY VALUE` pair per line, comments start with `#`.
/// The last value of a key wins. `SYS_UID_MAX` defaults to `UID_MIN - 1`.
impl FromStr for LoginDefs {
    type Err = anyhow::Error;

    fn from_str(content: &str) -> Result<Self> {
        let mut values = HashMap::new();
        for (line, number) in content.lines().zip(1..) {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
            values.insert(key, (value.trim(), number));
        }
        let uid = |key: &str| -> Result<Option<u32>> {
            values
                .get(key)
                .map(|(value, number)| {
                    value
                        .parse()
                        .with_context(|| format!("line {number}: {key} is {value:?}, not a UID"))
                })
                .transpose()
        };
        let uid_min = uid("UID_MIN")?.unwrap_or(UID_MIN);
        let uid_max = uid("UID_MAX")?.unwrap_or(UID_MAX);
        let sys_uid_max = uid("SYS_UID_MAX")?.unwrap_or(uid_min.saturating_sub(1));
        ensure!(
            uid_min <= uid_max,
            "UID_MIN {uid_min} is above UID_MAX {uid_max}"
        );
        Ok(Self {
            sys_uid_max,
            uid_min,
            uid_max,
        })
    }
}

#[cfg(test)]
mod tests {
    use indoc::indoc;

    use super::*;
    use crate::testutil::rootfs;

    #[test]
    fn parses_uid_ranges() -> Result<()> {
        let defs: LoginDefs = indoc! {"
            # Min/max values for automatic uid selection in useradd
            UID_MIN\t\t\t 2000
            UID_MAX			 50000
            SYS_UID_MIN		  500
            SYS_UID_MAX		  1500
            UID_MIN 3000
        "}
        .parse()?;
        assert_eq!(
            defs,
            LoginDefs {
                sys_uid_max: 1500,
                uid_min: 3000,
                uid_max: 50000,
            }
        );
        assert!(defs.is_system(33));
        assert!(defs.is_system(1500));
        assert!(!defs.is_system(1501));
        assert!(defs.is_regular(3000));
        assert!(!defs.is_regular(2999));
        Ok(())
    }

    #[test]
    fn sys_uid_max_defaults_to_below_uid_min() -> Result<()> {
        let defs: LoginDefs = "UID_MIN 500\n".parse()?;
        assert_eq!(defs.sys_uid_max, 499);
        assert_eq!("".parse::<LoginDefs>()?, LoginDefs::default());
        Ok(())
    }

    #[test]
    fn rejects_key_without_number() {
        let err = format!("{:#}", "UID_MIN\n".parse::<LoginDefs>().unwrap_err());
        assert!(
            err.starts_with("line 1: UID_MIN is \"\", not a UID"),
            "{err}"
        );
        let err = format!("{:#}", "UID_MAX 1x\n".parse::<LoginDefs>().unwrap_err());
        assert!(
            err.starts_with("line 1: UID_MAX is \"1x\", not a UID"),
            "{err}"
        );
        assert!("UID_MIN 10\nUID_MAX 5\n".parse::<LoginDefs>().is_err());
    }

    #[test]
    fn reads_login_defs_file_or_takes_defaults() -> Result<()> {
        let root = rootfs()?;
        assert_eq!(LoginDefs::read(&root)?, LoginDefs::default());
        root.create_dir("etc")?;
        root.write(PATH, "SYS_UID_MAX 300\n")?;
        assert_eq!(LoginDefs::read(&root)?.sys_uid_max, 300);
        Ok(())
    }
}
