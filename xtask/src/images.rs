//! Render the test images in tests/images and the examples in examples/.
//!
//! Files ending in `.j2` are minijinja templates. Each test image is rendered
//! once per variant.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use anyhow::{Context, Result};
use minijinja::{Environment, UndefinedBehavior, Value, context};

/// The extension of template files.
const TEMPLATE_EXTENSION: &str = "j2";

/// The repository the test images and examples are tagged under.
const REPOSITORY: &str = "localhost/bootc-imagectl-test";

/// The examples without a regular user, whose user systemd-homed's first
/// boot wizard creates.
const HOMED_EXAMPLES: &[&str] = &["ubuntu-desktop", "ubuntu-transient-etc"];

/// The test images built without the homed variant.
const NO_HOMED_IMAGES: &[&str] = &["ubuntu-24.04"];

/// The test images built without the Secure Boot variant, whose systemd is
/// older than finalize's Secure Boot signing needs.
const NO_SECURE_BOOT_IMAGES: &[&str] = &["ubuntu-24.04"];

/// The examples with a transient /etc.
const TRANSIENT_ETC_EXAMPLES: &[&str] = &["ubuntu-transient-etc"];

/// The examples signed for Secure Boot.
const SECURE_BOOT_EXAMPLES: &[&str] = &["ubuntu-desktop"];

/// A variant of every test image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variant {
    /// A regular user is built into the image.
    Default,
    /// The image has no regular user. systemd-homed's first boot wizard
    /// creates it.
    Homed,
    /// [`Self::Default`] with a transient /etc.
    TransientEtc,
    /// The image is signed for Secure Boot.
    SecureBoot,
}

impl Variant {
    const ALL: [Self; 4] = [
        Self::Default,
        Self::Homed,
        Self::TransientEtc,
        Self::SecureBoot,
    ];

    /// Whether systemd-homed's first boot wizard creates the regular user.
    fn is_homed(self) -> bool {
        matches!(self, Self::Homed)
    }

    /// Whether /etc is transient.
    fn has_transient_etc(self) -> bool {
        matches!(self, Self::TransientEtc)
    }

    /// Whether the image is signed for Secure Boot.
    fn has_secure_boot(self) -> bool {
        matches!(self, Self::SecureBoot)
    }

    /// The variables the templates see.
    fn context(self) -> Value {
        context! {
            homed => self.is_homed(),
            transient_etc => self.has_transient_etc(),
            secureboot => self.has_secure_boot(),
        }
    }
}

/// Where an image comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// A test image in tests/images, built once per variant.
    Test,
    /// An example in examples/.
    Example,
}

impl Source {
    /// The variants each image is built as.
    fn variants(self) -> &'static [Variant] {
        match self {
            Self::Test => &Variant::ALL,
            Self::Example => &[Variant::Default],
        }
    }

    /// The directory holding the images, relative to the workspace root.
    fn dir(self) -> &'static str {
        match self {
            Self::Test => "tests/images",
            Self::Example => "examples",
        }
    }
}

/// A variant of a test image, or an example.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Image {
    /// The directory the image is rendered from.
    pub(crate) name: String,
    source: Source,
    variant: Variant,
}

impl Image {
    /// Whether systemd-homed's first boot wizard creates the image's regular
    /// user.
    pub(crate) fn is_homed(&self) -> bool {
        match self.source {
            Source::Test => self.variant.is_homed(),
            Source::Example => HOMED_EXAMPLES.contains(&self.name.as_str()),
        }
    }

    /// Whether the image has a transient /etc.
    pub(crate) fn has_transient_etc(&self) -> bool {
        match self.source {
            Source::Test => self.variant.has_transient_etc(),
            Source::Example => TRANSIENT_ETC_EXAMPLES.contains(&self.name.as_str()),
        }
    }

    /// The image's tag.
    pub(crate) fn tag(&self) -> String {
        match (self.source, self.variant) {
            (Source::Example, _) => format!("{}-example", self.name),
            (Source::Test, Variant::Default) => self.name.clone(),
            (Source::Test, Variant::Homed) => format!("{}-homed", self.name),
            (Source::Test, Variant::TransientEtc) => format!("{}-transient-etc", self.name),
            (Source::Test, Variant::SecureBoot) => format!("{}-secureboot", self.name),
        }
    }

    /// The image's reference once built.
    pub(crate) fn reference(&self) -> String {
        format!("{REPOSITORY}:{}", self.tag())
    }

    /// The name `{image}` stands for in build options.
    pub(crate) fn build_name(&self) -> String {
        match self.source {
            Source::Test => self.name.clone(),
            Source::Example => self.tag(),
        }
    }

    /// Whether the image is signed for Secure Boot.
    pub(crate) fn has_secure_boot(&self) -> bool {
        match self.source {
            Source::Test => self.variant.has_secure_boot(),
            Source::Example => SECURE_BOOT_EXAMPLES.contains(&self.name.as_str()),
        }
    }

    /// The environment that tells the test binary it runs against this image,
    /// as `NAME=VALUE`.
    pub(crate) fn test_env(&self) -> [String; 2] {
        let features: Vec<&str> = [
            ("homed", self.is_homed()),
            ("transient-etc", self.has_transient_etc()),
            ("secureboot", self.has_secure_boot()),
        ]
        .into_iter()
        .filter_map(|(feature, has)| has.then_some(feature))
        .collect();
        [
            format!("BOOTC_IMAGECTL_TEST_IMAGE={}", self.reference()),
            format!("BOOTC_IMAGECTL_TEST_FEATURES={}", features.join(",")),
        ]
    }

    /// Render the image from the workspace at `root` into `out`, replacing
    /// `out` contents.
    pub(crate) fn render(&self, root: &Path, out: &Path) -> Result<()> {
        if out.exists() {
            fs::remove_dir_all(out).with_context(|| format!("removing {}", out.display()))?;
        }
        let mut env = Environment::new();
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        env.set_keep_trailing_newline(true);
        env.set_trim_blocks(true);
        env.set_lstrip_blocks(true);
        let src = root.join(self.source.dir()).join(&self.name);
        render_dir(&env, &self.variant.context(), &src, out)
    }
}

/// The images from `sources` in the workspace at `root`, with every variant
/// of the test images.
pub(crate) fn images(root: &Path, sources: &[Source]) -> Result<Vec<Image>> {
    let mut images = Vec::new();
    for &source in sources {
        for name in names(&root.join(source.dir()))? {
            let variants = source.variants().iter().filter(|variant| {
                !(variant.is_homed() && NO_HOMED_IMAGES.contains(&name.as_str())
                    || variant.has_secure_boot() && NO_SECURE_BOOT_IMAGES.contains(&name.as_str()))
            });
            images.extend(variants.map(|&variant| Image {
                name: name.clone(),
                source,
                variant,
            }));
        }
    }
    Ok(images)
}

/// The names of the images in `dir`, sorted.
fn names(dir: &Path) -> Result<Vec<String>> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_>>()?;
    names.sort();
    Ok(names)
}

/// Copy `src` to `dst`, rendering the templates.
fn render_dir(env: &Environment<'_>, context: &Value, src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let path = entry.path();
        let out = dst.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            render_dir(env, context, &path, &out)?;
        } else if file_type.is_symlink() {
            symlink(fs::read_link(&path)?, &out)
                .with_context(|| format!("linking {}", out.display()))?;
        } else if path
            .extension()
            .is_some_and(|ext| ext == TEMPLATE_EXTENSION)
        {
            let template =
                fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            let rendered = env
                .render_named_str(&path.to_string_lossy(), &template, context)
                .with_context(|| format!("rendering {}", path.display()))?;
            let out = out.with_extension("");
            fs::write(&out, rendered).with_context(|| format!("writing {}", out.display()))?;
        } else {
            fs::copy(&path, &out).with_context(|| format!("copying {}", path.display()))?;
        }
    }
    Ok(())
}
