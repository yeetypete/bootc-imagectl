//! User and group IDs.

use std::fmt;
use std::str::FromStr;

use anyhow::{Context as _, Result, ensure};
use serde::Serialize;

/// `(uid_t) -1`, which libc APIs use as a placeholder.
const INVALID: u32 = u32::MAX;

/// The 16-bit `(uid_t) -1`, left over from when IDs were 16 bits.
const INVALID_16_BIT: u32 = u16::MAX as u32;

/// Whether systemd accepts the ID.
const fn is_valid(id: u32) -> bool {
    id != INVALID && id != INVALID_16_BIT
}

/// Parse a UID or GID the way systemd does. It takes decimal digits only,
/// without a sign or leading zeros.
pub(crate) fn parse(field: &str) -> Result<u32> {
    let id = field
        .parse::<u32>()
        .ok()
        // str::parse also accepts a leading + and leading zeros.
        .filter(|id| id.to_string() == field)
        .with_context(|| format!("{field:?} is not a UID or GID"))?;
    ensure!(is_valid(id), "{id} is not a valid UID or GID");
    Ok(id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Uid(u32);

impl Uid {
    /// root.
    pub const ROOT: Self = Self(0);
    /// nobody, the kernel's overflow UID.
    pub const NOBODY: Self = Self(65534);

    /// The UID with the number, unless systemd treats it as invalid.
    #[must_use]
    pub const fn new(raw: u32) -> Option<Self> {
        if is_valid(raw) { Some(Self(raw)) } else { None }
    }

    #[must_use]
    pub const fn as_raw(self) -> u32 {
        self.0
    }

    /// Whether the UID is root's or nobody's, which systemd calls intrinsic.
    #[must_use]
    pub const fn is_intrinsic(self) -> bool {
        matches!(self, Self::ROOT | Self::NOBODY)
    }

    /// The GID with the same number, which systemd-sysusers gives the group
    /// a `u` line creates.
    #[must_use]
    pub const fn matching_gid(self) -> Gid {
        Gid(self.0)
    }
}

impl TryFrom<u32> for Uid {
    type Error = anyhow::Error;

    fn try_from(raw: u32) -> Result<Self> {
        Self::new(raw).with_context(|| format!("{raw} is not a valid UID"))
    }
}

impl FromStr for Uid {
    type Err = anyhow::Error;

    fn from_str(field: &str) -> Result<Self> {
        parse(field).map(Self)
    }
}

impl fmt::Display for Uid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Gid(u32);

impl Gid {
    /// root.
    pub const ROOT: Self = Self(0);
    /// nobody, the kernel's overflow GID.
    pub const NOBODY: Self = Self(65534);

    /// The GID with the number, unless systemd treats it as invalid.
    #[must_use]
    pub const fn new(raw: u32) -> Option<Self> {
        if is_valid(raw) { Some(Self(raw)) } else { None }
    }

    #[must_use]
    pub const fn as_raw(self) -> u32 {
        self.0
    }

    /// Whether the GID is root's or nobody's, which systemd calls intrinsic.
    #[must_use]
    pub const fn is_intrinsic(self) -> bool {
        matches!(self, Self::ROOT | Self::NOBODY)
    }
}

impl TryFrom<u32> for Gid {
    type Error = anyhow::Error;

    fn try_from(raw: u32) -> Result<Self> {
        Self::new(raw).with_context(|| format!("{raw} is not a valid GID"))
    }
}

impl FromStr for Gid {
    type Err = anyhow::Error;

    fn from_str(field: &str) -> Result<Self> {
        parse(field).map(Self)
    }
}

impl fmt::Display for Gid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Add;

    use static_assertions::assert_not_impl_any;

    use super::*;

    assert_not_impl_any!(Uid: From<u32>, PartialEq<u32>, PartialEq<Gid>, Add<u32>);
    assert_not_impl_any!(Gid: From<u32>, PartialEq<u32>, PartialEq<Uid>, Add<u32>);

    #[test]
    fn rejects_placeholder_ids() -> Result<()> {
        assert_eq!("969".parse::<Uid>()?, Uid::new(969).expect("a valid UID"));
        for field in ["65535", "4294967295"] {
            let err = field.parse::<Gid>().unwrap_err().to_string();
            assert!(err.contains("not a valid UID or GID"), "{err}");
        }
        assert!(Uid::try_from(65535).is_err());
        assert!(Gid::new(u32::MAX).is_none());
        Ok(())
    }

    #[test]
    fn rejects_non_decimal_forms() {
        for field in ["", "-1", "+1", "01", "0x10", "1 ", "4294967296"] {
            assert!(field.parse::<Uid>().is_err(), "{field:?}");
        }
    }

    #[test]
    fn classifies_root_and_nobody_as_intrinsic() {
        assert!(Uid::ROOT.is_intrinsic());
        assert!(Gid::NOBODY.is_intrinsic());
        assert!(!Uid::new(1000).expect("a valid UID").is_intrinsic());
    }

    #[test]
    fn matching_gid_keeps_number() {
        let uid = Uid::new(969).expect("a valid UID");
        assert_eq!(uid.matching_gid().as_raw(), 969);
    }
}
