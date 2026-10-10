//! Render the test images in tests/images from their templates.
//!
//! Files ending in `.j2` are minijinja templates. Each image is rendered once
//! per variant.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use anyhow::{Context, Result};
use minijinja::{Environment, UndefinedBehavior, Value, context};

/// The extension of template files.
const TEMPLATE_EXTENSION: &str = "j2";

/// A variant of every test image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variant {
    /// A regular user is baked into the image.
    Default,
    /// The image has no regular user. systemd-homed's first boot wizard
    /// creates it.
    Homed,
}

impl Variant {
    const ALL: [Self; 2] = [Self::Default, Self::Homed];

    /// The variables the templates see.
    fn context(self) -> Value {
        context! { homed => self == Self::Homed }
    }
}

/// A variant of a test image.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Image {
    /// The directory in tests/images the image is rendered from.
    pub(crate) name: String,
    variant: Variant,
}

impl Image {
    /// The image's tag.
    pub(crate) fn tag(&self) -> String {
        match self.variant {
            Variant::Default => self.name.clone(),
            Variant::Homed => format!("{}-homed", self.name),
        }
    }

    /// The module holding the image's tests, named after its tag.
    pub(crate) fn module(&self) -> String {
        self.tag().replace('-', "_")
    }

    /// Render the image from `images` into `out`, replacing `out` contents.
    pub(crate) fn render(&self, images: &Path, out: &Path) -> Result<()> {
        if out.exists() {
            fs::remove_dir_all(out).with_context(|| format!("removing {}", out.display()))?;
        }
        let mut env = Environment::new();
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        env.set_keep_trailing_newline(true);
        env.set_trim_blocks(true);
        env.set_lstrip_blocks(true);
        render_dir(&env, &self.variant.context(), &images.join(&self.name), out)
    }
}

/// Every variant of the images in `dir`, sorted by tag.
pub(crate) fn images(dir: &Path) -> Result<Vec<Image>> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_>>()?;
    names.sort();
    Ok(names
        .into_iter()
        .flat_map(|name| {
            Variant::ALL.map(|variant| Image {
                name: name.clone(),
                variant,
            })
        })
        .collect())
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
