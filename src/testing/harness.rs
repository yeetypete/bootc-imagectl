//! Test harness the container, install and VM tests.

use std::fmt;
use std::str::FromStr;
use std::sync::LazyLock;

use anyhow::{Context as _, Result, bail};
use cap_std_ext::cap_std::ambient_authority;
use cap_std_ext::cap_std::fs_utf8::Dir;
use libtest_mimic::{Arguments, Completion, Trial};

use crate::distro;

/// The reference of the image under test, which the xtask sets.
const IMAGE_ENV: &str = "BOOTC_IMAGECTL_TEST_IMAGE";

/// The features of the image under test, separated by commas, which the
/// xtask sets.
const FEATURES_ENV: &str = "BOOTC_IMAGECTL_TEST_FEATURES";

/// A feature the xtask reports for the image under test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    /// systemd-homed's first boot wizard creates the regular user.
    Homed,
    /// The image has a transient /etc.
    TransientEtc,
    /// The image is signed for Secure Boot.
    SecureBoot,
}

impl FromStr for Feature {
    type Err = anyhow::Error;

    fn from_str(name: &str) -> Result<Self> {
        match name {
            "homed" => Ok(Self::Homed),
            "transient-etc" => Ok(Self::TransientEtc),
            "secureboot" => Ok(Self::SecureBoot),
            _ => bail!("unknown feature {name}"),
        }
    }
}

impl fmt::Display for Feature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Homed => "systemd-homed",
            Self::TransientEtc => "a transient /etc",
            Self::SecureBoot => "Secure Boot",
        })
    }
}

/// What a test needs from the image under test to apply to it.
#[derive(Debug)]
pub enum Requirement {
    /// The image is one of these distributions, by os-release `ID`.
    Distro(&'static [&'static str]),
    /// The image has this tag.
    Image(&'static str),
    /// The image has this feature.
    Feature(Feature),
    /// The image lacks this feature.
    Without(Feature),
}

impl fmt::Display for Requirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Distro(distros) => write!(f, "{}", distros.join(" or ")),
            Self::Image(tag) => write!(f, "the {tag} image"),
            Self::Feature(feature) => feature.fmt(f),
            Self::Without(feature) => write!(f, "no {feature}"),
        }
    }
}

/// A test, registered in a list its suite passes to [`run`].
#[derive(Debug)]
pub struct IntegrationTest {
    /// The test's path, starting with the suite's crate.
    pub path: &'static str,
    pub run: fn() -> Result<()>,
    /// What the image must provide, or the test is ignored.
    pub requires: &'static [Requirement],
}

/// An [`IntegrationTest`] for the function `$test` in this module, ignored unless the
/// image meets every requirement in `$requires`.
#[macro_export]
macro_rules! integration_test {
    ($test:ident) => {
        $crate::integration_test!($test, &[])
    };
    ($test:ident, $requires:expr) => {
        $crate::testing::IntegrationTest {
            path: concat!(module_path!(), "::", stringify!($test)),
            run: $test,
            requires: $requires,
        }
    };
}

/// The image under test, read from the environment the xtask sets.
static IMAGE: LazyLock<TestImage> =
    LazyLock::new(|| TestImage::from_env().expect("the image under test"));

/// The image under test.
#[derive(Debug)]
pub struct TestImage {
    /// The image's reference, e.g. `localhost/bootc-imagectl-test:ubuntu`.
    pub reference: String,
    /// The image's distribution, by os-release `ID`.
    pub distro: &'static str,
    features: Vec<Feature>,
}

/// The image the running suite tests.
///
/// # Panics
///
/// Panics if the xtask did not describe the image under test.
#[must_use]
pub fn image() -> &'static TestImage {
    &IMAGE
}

impl TestImage {
    /// Read the image under test from the environment the xtask sets.
    fn from_env() -> Result<Self> {
        let reference = std::env::var(IMAGE_ENV).with_context(|| format!("reading {IMAGE_ENV}"))?;
        let features = std::env::var(FEATURES_ENV)
            .with_context(|| format!("reading {FEATURES_ENV}"))?
            .split(',')
            .filter(|feature| !feature.is_empty())
            .map(str::parse)
            .collect::<Result<_>>()
            .with_context(|| format!("parsing {FEATURES_ENV}"))?;
        let root = Dir::open_ambient_dir("/", ambient_authority()).context("opening /")?;
        Ok(Self {
            reference,
            distro: distro::detect(&root)?.name(),
            features,
        })
    }

    /// The image's tag, e.g. `ubuntu`.
    fn tag(&self) -> &str {
        self.reference
            .rsplit_once(':')
            .map_or(&self.reference, |(_, tag)| tag)
    }

    fn meets(&self, requirement: &Requirement) -> bool {
        match requirement {
            Requirement::Distro(distros) => distros.contains(&self.distro),
            Requirement::Image(tag) => self.tag() == *tag,
            Requirement::Feature(feature) => self.features.contains(feature),
            Requirement::Without(feature) => !self.features.contains(feature),
        }
    }
}

/// Run `tests` against the image under test with libtest.
/// Tests the image does not meet the requirements of are reported as
/// ignored, with the first unmet requirement as the reason.
///
/// # Panics
///
/// Panics if the xtask did not describe the image under test.
pub fn run(tests: &[&[IntegrationTest]]) -> ! {
    let args = Arguments::from_args();
    let image = image();
    let trials = tests
        .iter()
        .flat_map(|tests| tests.iter())
        .map(|test| {
            let name = test
                .path
                .split_once("::")
                .map_or(test.path, |(_, name)| name);
            let unmet = test.requires.iter().find(|r| !image.meets(r));
            let run = test.run;
            Trial::ignorable_test(name, move || match unmet {
                Some(requirement) => Ok(Completion::ignored_with(format!("needs {requirement}"))),
                None => run()
                    .map(|()| Completion::Completed)
                    .map_err(|e| format!("{e:?}").into()),
            })
        })
        .collect();
    libtest_mimic::run(&args, trials).exit()
}
