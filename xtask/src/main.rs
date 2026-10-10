//! Developer tasks, run with `cargo xtask`.

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, ValueEnum};
use serde_json::{Value, json};
use xshell::{Shell, cmd};

mod container;
mod images;
mod install;
mod vm;

/// The repository the test images are tagged under.
const REPOSITORY: &str = "localhost/bootc-imagectl-test";

/// The environment variable setting `Build`.
const BUILD_OPTIONS_ENV: &str = "BOOTC_IMAGECTL_BUILD_OPTIONS";

#[derive(Debug, Parser)]
enum Task {
    /// Run the container, the VM or the install tests.
    Test {
        #[command(flatten)]
        build: Build,
        #[command(flatten)]
        selection: Selection,
        suite: Suite,
        /// Arguments for the test binary.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Build a test image and boot it in a VM with its console on this
    /// terminal.
    Vm {
        #[command(flatten)]
        build: Build,
        /// The image's tag, e.g. `ubuntu` or `ubuntu-homed`.
        image: String,
    },
    /// Print the matrix of test jobs as JSON, consumable by GitHub Actions.
    Matrix,
    /// Run a test binary against each variant of the images in
    /// tests/images. Cargo runs this through `test`.
    #[command(hide = true)]
    Runner {
        #[command(flatten)]
        build: Build,
        #[command(flatten)]
        selection: Selection,
        suite: Suite,
        binary: PathBuf,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
}

/// A test target in Cargo.toml.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Suite {
    /// The tests in tests/container, run in a container of each image.
    Container,
    /// The tests in tests/vm, run in a VM booted from each image.
    Vm,
    /// The tests in tests/install, run in a VM booted from a disk each
    /// image is installed onto.
    Install,
}

/// How the test images are built. `test` passes this to the runner
/// through the environment.
#[derive(Debug, Args)]
struct Build {
    /// An option passed to the builder, such as a cache. `{image}` is
    /// replaced by the image name, which its variants share.
    #[arg(long = "build-option", env = BUILD_OPTIONS_ENV, value_delimiter = ' ', allow_hyphen_values = true)]
    options: Vec<String>,
}

/// Which images in tests/images the tests run against.
#[derive(Debug, Args)]
struct Selection {
    /// Only run against the image with this tag, e.g. `fedora` or
    /// `fedora-homed`. May be repeated. Defaults to every image.
    #[arg(long = "image")]
    images: Vec<String>,
}

impl Build {
    /// Build the image `name` rendered into `dir` as `image`, with the
    /// bootc-imagectl binary from `target`.
    fn build(&self, sh: &Shell, name: &str, image: &str, dir: &Path, target: &Path) -> Result<()> {
        let options: Vec<String> = self
            .options
            .iter()
            .filter(|option| !option.is_empty())
            .map(|option| option.replace("{image}", name))
            .collect();
        cmd!(
            sh,
            "podman build {options...} --build-context bootc-imagectl={target} --tag {image} {dir}"
        )
        .run()?;
        Ok(())
    }
}

impl Suite {
    fn name(self) -> &'static str {
        match self {
            Self::Container => "container",
            Self::Vm => "vm",
            Self::Install => "install",
        }
    }

    /// Run the test binary against `image`.
    fn run(self, sh: &Shell, image: &str, binary: &Path, args: &[OsString]) -> Result<()> {
        match self {
            Self::Container => container::run(sh, image, binary, args),
            Self::Vm => vm::run(sh, image, binary, args),
            Self::Install => install::run(sh, image, binary, args),
        }
    }
}

fn main() -> Result<()> {
    let sh = Shell::new()?;
    match Task::parse() {
        Task::Test {
            build,
            selection,
            suite,
            args,
        } => test(&sh, &build, &selection, suite, &args),
        Task::Vm { build, image } => boot(&sh, &build, &image),
        Task::Matrix => {
            println!("{}", serde_json::to_string_pretty(&matrix()?)?);
            Ok(())
        }
        Task::Runner {
            build,
            selection,
            suite,
            binary,
            args,
        } => run(&sh, &build, &selection, suite, &binary, &args),
    }
}

/// The GitHub Actions matrix of test jobs.
fn matrix() -> Result<Value> {
    let tags: Vec<String> = images::images(&images_dir()?)?
        .iter()
        .map(images::Image::tag)
        .collect();
    let mut jobs = Vec::new();
    for suite in Suite::value_variants() {
        for image in &tags {
            let name = format!("test-{}-{image}", suite.name());
            match suite {
                Suite::Install => jobs.extend(install::FILESYSTEMS.map(|filesystem| {
                    json!({
                        "name": format!("{name}-{filesystem}"),
                        "suite": suite.name(),
                        "image": image,
                        "filesystem": filesystem,
                    })
                })),
                Suite::Container | Suite::Vm => jobs.push(json!({
                    "name": name,
                    "suite": suite.name(),
                    "image": image,
                })),
            }
        }
    }
    Ok(json!({ "include": jobs }))
}

/// Run `cargo test` for the suite with this binary as the runner.
fn test(
    sh: &Shell,
    build: &Build,
    selection: &Selection,
    suite: Suite,
    args: &[OsString],
) -> Result<()> {
    let variants = images::images(&images_dir()?)?;
    for tag in &selection.images {
        find_image(&variants, tag)?;
    }
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let name = suite.name();
    let xtask = std::env::current_exe()?;
    let mut runner = vec![xtask.display().to_string(), "runner".into()];
    for image in &selection.images {
        runner.extend(["--image".into(), image.clone()]);
    }
    runner.push(name.into());
    let runner = format!(
        "target.'cfg(unix)'.runner={}",
        serde_json::to_string(&runner)?
    );
    let mut cargo = cmd!(
        sh,
        "{cargo} test --locked --features {name} --test {name} --config {runner} -- {args...}"
    );
    // An empty variable would pass an empty option.
    cargo = if build.options.is_empty() {
        cargo.env_remove(BUILD_OPTIONS_ENV)
    } else {
        cargo.env(BUILD_OPTIONS_ENV, build.options.join(" "))
    };
    cargo.run()?;
    Ok(())
}

/// The directory holding the test images.
fn images_dir() -> Result<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent();
    Ok(root
        .context("finding the workspace root")?
        .join("tests/images"))
}

/// The image tagged `tag` among `variants`.
fn find_image<'a>(variants: &'a [images::Image], tag: &str) -> Result<&'a images::Image> {
    variants
        .iter()
        .find(|image| image.tag() == tag)
        .with_context(|| {
            let tags: Vec<String> = variants.iter().map(images::Image::tag).collect();
            format!("no image tagged {tag}, select one of: {}", tags.join(", "))
        })
}

/// Build each selected variant of the images in tests/images and run the
/// test binary against it. The tests of the other images live in modules
/// named after them and are skipped.
fn run(
    sh: &Shell,
    build: &Build,
    selection: &Selection,
    suite: Suite,
    binary: &Path,
    args: &[OsString],
) -> Result<()> {
    let images = images_dir()?;
    let variants = images::images(&images)?;
    let target = target_dir(binary)?;
    let selected = variants
        .iter()
        .filter(|image| selection.images.is_empty() || selection.images.contains(&image.tag()));
    for image in selected {
        let tag = image.tag();
        let dir = target.join("images").join(&tag);
        let reference = format!("{REPOSITORY}:{tag}");
        image.render(&images, &dir)?;
        build.build(sh, &image.name, &reference, &dir, target)?;
        let mut args = args.to_vec();
        for other in variants.iter().filter(|other| *other != image) {
            args.push("--skip".into());
            args.push(format!("{}::", other.module()).into());
        }
        let color = if std::io::stdout().is_terminal() {
            "always"
        } else {
            "never"
        };
        args.push(format!("--color={color}").into());
        suite.run(sh, &reference, binary, &args)?;
    }
    Ok(())
}

/// Build bootc-imagectl and the image tagged `tag`, then boot it in a VM.
fn boot(sh: &Shell, build: &Build, tag: &str) -> Result<()> {
    let images = images_dir()?;
    let variants = images::images(&images)?;
    let image = find_image(&variants, tag)?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    cmd!(sh, "{cargo} build --locked --bin bootc-imagectl").run()?;
    // cargo builds bootc-imagectl next to this binary.
    let xtask = std::env::current_exe()?;
    let target = xtask.parent().context("finding the target directory")?;
    let dir = target.join("images").join(tag);
    let reference = format!("{REPOSITORY}:{tag}");
    image.render(&images, &dir)?;
    build.build(sh, &image.name, &reference, &dir, target)?;
    vm::boot(&reference)
}

/// The target profile directory holding `binary` and bootc-imagectl.
fn target_dir(binary: &Path) -> Result<&Path> {
    binary
        .parent()
        .and_then(Path::parent)
        .with_context(|| format!("{} is not in a target directory", binary.display()))
}

/// The path of `binary` with the target directory bound at `target`.
fn bound_binary(binary: &Path, target: &str) -> Result<PathBuf> {
    let relative = binary.strip_prefix(target_dir(binary)?)?;
    Ok(Path::new(target).join(relative))
}
