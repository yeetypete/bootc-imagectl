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

/// The examples with a transient /etc.
const TRANSIENT_ETC_EXAMPLES: &[&str] = &["ubuntu-transient-etc"];

/// The examples signed for Secure Boot.
const SECURE_BOOT_EXAMPLES: &[&str] = &["ubuntu-desktop"];

/// A variant of every test image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variant {
    /// A regular user is baked into the image.
    Default,
    /// The image has no regular user. systemd-homed's first boot wizard
    /// creates it.
    Homed,
    /// [`Self::Homed`] with a transient /etc.
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

    /// The variables the templates see.
    fn context(self) -> Value {
        context! {
            homed => matches!(self, Self::Homed | Self::TransientEtc),
            transient_etc => self == Self::TransientEtc,
            secureboot => self == Self::SecureBoot,
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
    /// Whether the image has a transient /etc.
    pub(crate) fn transient_etc(&self) -> bool {
        match self.source {
            Source::Test => self.variant == Variant::TransientEtc,
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

    /// The name `{image}` stands for in build options.
    pub(crate) fn build_name(&self) -> String {
        match self.source {
            Source::Test => self.name.clone(),
            Source::Example => self.tag(),
        }
    }

    /// Whether the image is signed for Secure Boot.
    pub(crate) fn secure_boot(&self) -> bool {
        match self.source {
            Source::Test => self.variant == Variant::SecureBoot,
            Source::Example => SECURE_BOOT_EXAMPLES.contains(&self.name.as_str()),
        }
    }

    /// The module holding the image's tests, named after its tag.
    pub(crate) fn module(&self) -> String {
        self.tag().replace('-', "_")
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
            images.extend(source.variants().iter().map(|&variant| Image {
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
