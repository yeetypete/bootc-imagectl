//! bootc's setup-root configuration, see bootc-setup-root-conf(5).

use serde::{Deserialize, Serialize};

/// The setup-root configuration.
#[derive(Debug, Default, Deserialize, Serialize)]
pub(super) struct Config {
    /// How bootc mounts /etc.
    #[serde(default)]
    pub(super) etc: MountConfig,
}

impl Config {
    /// The configuration that makes /etc transient.
    pub(super) const TRANSIENT_ETC: Self = Self {
        etc: MountConfig {
            mount: None,
            transient: true,
        },
    };
}

/// How bootc mounts a directory.
#[derive(Debug, Default, Deserialize, Serialize)]
pub(super) struct MountConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    mount: Option<MountType>,
    #[serde(default)]
    transient: bool,
}

impl MountConfig {
    /// Whether bootc mounts the directory transient. `mount` takes precedence
    /// over the `transient` shorthand.
    pub(super) fn is_transient(&self) -> bool {
        self.mount
            .map_or(self.transient, |mount| mount == MountType::Transient)
    }
}

/// The types of mounts bootc supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum MountType {
    #[serde(alias = "root")]
    None,
    Bind,
    Overlay,
    #[serde(alias = "volatile")]
    Transient,
}
