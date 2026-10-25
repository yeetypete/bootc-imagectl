//! The test harness and the checks shared by the container, the install and
//! the VM tests, built only with their features.

use std::fmt;
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result, bail};
use cap_std_ext::cap_std::ambient_authority;
use cap_std_ext::cap_std::fs_utf8::Dir;
use libtest_mimic::{Arguments, Completion, Trial};
use rustix::time::{ClockId, clock_gettime};

use crate::distro;
use crate::id::{Gid, Uid};
use crate::passwd::{Entry as _, Group, Gshadow, Passwd, Shadow};

/// The account files in /etc contain only root and nobody.
///
/// # Errors
///
/// Fails if a file cannot be read.
///
/// # Panics
///
/// Panics if a file contains other accounts or a backup exists.
pub fn etc_contains_only_root_and_nobody(root: &Dir) -> Result<()> {
    let users = Passwd::read_all(root)?;
    let uids: Vec<Uid> = users.iter().map(|user| user.uid).collect();
    assert_eq!(uids, [Uid::ROOT, Uid::NOBODY]);
    let names: Vec<&str> = users.iter().map(|user| user.name.as_str()).collect();
    let shadow = Shadow::read_all(root)?;
    let shadow: Vec<&str> = shadow.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(shadow, names);

    let groups = Group::read_all(root)?;
    let gids: Vec<Gid> = groups.iter().map(|group| group.gid).collect();
    assert_eq!(gids, [Gid::ROOT, Gid::NOBODY]);
    let names: Vec<&str> = groups.iter().map(|group| group.name.as_str()).collect();
    let gshadow = Gshadow::read_all(root)?;
    let gshadow: Vec<&str> = gshadow.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(gshadow, names);

    for name in ["passwd", "shadow", "group", "gshadow"] {
        assert!(!root.exists(format!("etc/{name}-")), "/etc/{name}- exists");
    }
    Ok(())
}

/// The time the system booted.
///
/// # Panics
///
/// Panics if `CLOCK_BOOTTIME` is negative, which the kernel never reports.
#[must_use]
pub fn boot_time() -> SystemTime {
    let since_boot = Duration::try_from(clock_gettime(ClockId::Boottime))
        .expect("CLOCK_BOOTTIME is never negative");
    SystemTime::now() - since_boot
}

/// Whether `path`, or the symlink at `path`, was last written before the
/// system booted.
///
/// # Errors
///
/// Fails if the metadata of `path` cannot be read.
pub fn written_before_boot(path: &Path) -> Result<bool> {
    let modified = path
        .symlink_metadata()
        .with_context(|| format!("reading {}", path.display()))?
        .modified()?;
    Ok(modified < boot_time())
}

/// The accounts a test image without systemd-homed bakes in.
#[derive(Debug)]
pub struct ImageAccounts {
    /// Users packages created, by name and UID. They have no password.
    pub system_users: &'static [(&'static str, u32)],
    /// The regular user, by name and UID.
    pub user: (&'static str, u32),
    /// The regular user's password.
    pub password: &'static str,
    /// A group the regular user is a member of besides its own.
    pub group: &'static str,
}

/// The accounts of the test image for the distribution under test.
///
/// # Errors
///
/// Fails if the distribution has no test image.
pub fn image_accounts() -> Result<&'static ImageAccounts> {
    Ok(match image().distro {
        "arch" => &ImageAccounts {
            system_users: &[("avahi", 969), ("uuidd", 970)],
            user: ("archie", 1000),
            password: "archie",
            group: "wheel",
        },
        "debian" => &ImageAccounts {
            system_users: &[("sshd", 990), ("systemd-network", 997)],
            user: ("debian", 1000),
            password: "debian",
            group: "sudo",
        },
        "fedora" => &ImageAccounts {
            system_users: &[("systemd-coredump", 997), ("systemd-oom", 998)],
            user: ("fedora", 1000),
            password: "fedora",
            group: "wheel",
        },
        "ubuntu" => &ImageAccounts {
            system_users: &[("sshd", 992), ("systemd-network", 998)],
            user: ("ubuntu", 1000),
            password: "ubuntu",
            group: "sudo",
        },
        name => bail!("no test image for {name}"),
    })
}

/// The groups finalize has systemd-homed's first boot wizard add its user
/// to on the distribution under test, in the order finalize writes them.
///
/// # Errors
///
/// Fails if the distribution has no test image.
pub fn wizard_groups() -> Result<&'static [&'static str]> {
    Ok(match image().distro {
        "arch" | "fedora" => &["wheel"],
        "debian" => &[
            "sudo", "audio", "cdrom", "dip", "floppy", "video", "plugdev", "lpadmin",
        ],
        "ubuntu" => &["sudo", "adm", "cdrom", "dip", "plugdev", "users"],
        name => bail!("no test image for {name}"),
    })
}

/// The reference of the image under test, which the xtask sets.
const IMAGE_ENV: &str = "BOOTC_IMAGECTL_TEST_IMAGE";

/// The features of the image under test, separated by commas, which the
/// xtask sets.
const FEATURES_ENV: &str = "BOOTC_IMAGECTL_TEST_FEATURES";

/// What a test needs from the image under test to apply to it.
#[derive(Debug)]
pub enum Requirement {
    /// The image is one of these distributions, by os-release `ID`.
    Distro(&'static [&'static str]),
    /// The image has this tag.
    Image(&'static str),
    /// systemd-homed's first boot wizard creates the regular user.
    Homed,
    /// A regular user is baked into the image.
    RegularUser,
    /// The image has a transient /etc.
    TransientEtc,
}

impl fmt::Display for Requirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Distro(distros) => write!(f, "{}", distros.join(" or ")),
            Self::Image(tag) => write!(f, "the {tag} image"),
            Self::Homed => f.write_str("systemd-homed"),
            Self::RegularUser => f.write_str("a regular user"),
            Self::TransientEtc => f.write_str("a transient /etc"),
        }
    }
}

/// A test, registered in a list its suite passes to [`run`].
#[derive(Debug)]
pub struct Test {
    /// The test's path, starting with the suite's crate.
    pub path: &'static str,
    pub run: fn() -> Result<()>,
    /// What the image must provide, or the test is ignored.
    pub requires: &'static [Requirement],
}

/// A [`Test`] for the function `$test` in this module, ignored unless the
/// image meets every requirement in `$requires`.
#[macro_export]
macro_rules! trial {
    ($test:ident) => {
        $crate::trial!($test, &[])
    };
    ($test:ident, $requires:expr) => {
        $crate::testing::Test {
            path: concat!(module_path!(), "::", stringify!($test)),
            run: $test,
            requires: $requires,
        }
    };
}

/// The image under test, set by [`run`].
static IMAGE: OnceLock<TestImage> = OnceLock::new();

/// The image under test.
#[derive(Debug)]
pub struct TestImage {
    /// The image's reference, e.g. `localhost/bootc-imagectl-test:ubuntu`.
    pub reference: String,
    /// The image's distribution, by os-release `ID`.
    pub distro: &'static str,
    features: Vec<String>,
}

/// The image the running suite tests.
///
/// # Panics
///
/// Panics outside a suite started by [`run`].
#[must_use]
pub fn image() -> &'static TestImage {
    IMAGE.get().expect("the suite runs through testing::run")
}

impl TestImage {
    /// Read the image under test from the environment the xtask sets.
    fn from_env() -> Result<Self> {
        let reference = std::env::var(IMAGE_ENV).with_context(|| format!("reading {IMAGE_ENV}"))?;
        let features = std::env::var(FEATURES_ENV)
            .with_context(|| format!("reading {FEATURES_ENV}"))?
            .split(',')
            .filter(|feature| !feature.is_empty())
            .map(str::to_owned)
            .collect();
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

    fn has(&self, feature: &str) -> bool {
        self.features.iter().any(|found| found == feature)
    }

    fn meets(&self, requirement: &Requirement) -> bool {
        match requirement {
            Requirement::Distro(distros) => distros.contains(&self.distro),
            Requirement::Image(tag) => self.tag() == *tag,
            Requirement::Homed => self.has("homed"),
            Requirement::RegularUser => !self.has("homed"),
            Requirement::TransientEtc => self.has("transient-etc"),
        }
    }
}

/// Run `tests` against the image under test with libtest's command line.
/// Tests the image does not meet the requirements of are reported as
/// ignored, with the first unmet requirement as the reason.
///
/// # Panics
///
/// Panics if the xtask did not describe the image under test.
pub fn run(tests: &[&[Test]]) -> ! {
    let args = Arguments::from_args();
    let image = IMAGE.get_or_init(|| TestImage::from_env().expect("the image under test"));
    let trials = tests
        .iter()
        .flat_map(|tests| tests.iter())
        .map(|test| {
            // Drop the suite's crate, as libtest does.
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
