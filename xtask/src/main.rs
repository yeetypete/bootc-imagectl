//! Developer tasks, run with `cargo xtask`.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, ValueEnum};
use serde_json::{Value, json};
use xshell::{Shell, cmd};

use crate::images::Source;

mod container;
mod images;
mod install;
mod vm;

/// The repository the test images and examples are tagged under.
const REPOSITORY: &str = "localhost/bootc-imagectl-test";

/// chunkah OCI image.
const CHUNKAH: &str = "quay.io/coreos/chunkah:v0.7.1@sha256:71e593421c5de7dcc7c8c5c50e98d3bac572055a76f399f61a491d123c20a523";

/// The max layers chunkah splits an image into.
const CHUNKAH_MAX_LAYERS: u32 = 128;

/// The mtime chunkah clamps every file to.
const CHUNKAH_SOURCE_DATE_EPOCH: u32 = 0;

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
    /// Run every suite against one image.
    TestImage {
        #[command(flatten)]
        build: Build,
        /// The image's tag, e.g. `ubuntu` or `ubuntu-desktop-example`.
        image: String,
    },
    /// Build a test image and boot it in a VM with its console on this
    /// terminal.
    Vm {
        #[command(flatten)]
        build: Build,
        /// The image's tag, e.g. `ubuntu` or `ubuntu-homed`.
        image: String,
    },
    /// Build an example, install it onto a disk and boot it in a VM with a
    /// graphical console.
    Example {
        #[command(flatten)]
        build: Build,
        /// The example's directory in examples/, e.g. `ubuntu`.
        name: String,
    },
    /// Print the matrices of the test and cache pruning jobs as JSON,
    /// consumable by GitHub Actions.
    Matrix,
    /// Run a test binary against each of the suite's images. Cargo runs
    /// this through `test`.
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
    /// image and example is installed onto.
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

/// Which of the suite's images the tests run against.
#[derive(Debug, Args)]
struct Selection {
    /// Only run against the image with this tag, e.g. `fedora` or
    /// `fedora-homed`. May be repeated. Defaults to every image.
    #[arg(long = "image")]
    images: Vec<String>,
}

impl Build {
    /// This build without its cache destinations.
    fn without_cache_to(&self) -> Self {
        let options = self
            .options
            .iter()
            .filter(|option| !option.starts_with("--cache-to"))
            .cloned()
            .collect();
        Self { options }
    }

    /// Build the image `name` rendered into `dir` with the bootc-imagectl
    /// binary from `target`, passing `args` to the builder.
    fn build(
        &self,
        sh: &Shell,
        name: &str,
        dir: &Path,
        target: &Path,
        args: &[String],
    ) -> Result<()> {
        let options: Vec<String> = self
            .options
            .iter()
            .filter(|option| !option.is_empty())
            .map(|option| option.replace("{image}", name))
            .collect();
        cmd!(
            sh,
            "podman build {options...} --build-context bootc-imagectl={target} {args...} {dir}"
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

    /// The images the suite runs against.
    fn images(self) -> Result<Vec<images::Image>> {
        let sources: &[Source] = match self {
            Self::Container | Self::Vm => &[Source::Test],
            Self::Install => &[Source::Test, Source::Example],
        };
        images::images(workspace_dir()?, sources)
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
        Task::TestImage { build, image } => test_image(&sh, &build, &image),
        Task::Vm { build, image } => boot(&sh, &build, &image),
        Task::Example { build, name } => example(&sh, &build, &name),
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

/// The GitHub Actions matrices:
///
/// 1. The test jobs, one per image.
/// 2. The jobs pruning the build cache, one per cache package.
fn matrix() -> Result<Value> {
    let images = Suite::Install.images()?;
    let tests: Vec<Value> = images
        .iter()
        .map(|image| json!({ "name": format!("test-{}", image.tag()), "image": image.tag() }))
        .collect();
    let caches: BTreeSet<String> = images.iter().map(images::Image::build_name).collect();
    let prunes: Vec<Value> = caches
        .into_iter()
        .map(|cache| json!({ "cache": cache }))
        .collect();
    Ok(json!({
        "test": { "include": tests },
        "prune": { "include": prunes },
    }))
}

/// Run every suite that tests the image tagged `tag`, the install tests on
/// each filesystem.
fn test_image(sh: &Shell, build: &Build, tag: &str) -> Result<()> {
    // The install suite tests every image.
    find_image(&Suite::Install.images()?, tag)?;
    let selection = Selection {
        images: vec![tag.to_owned()],
    };
    // podman pushes even locally cached steps to --cache-to. Only let the
    // first suite push.
    let later = build.without_cache_to();
    let mut build = build;
    for suite in Suite::value_variants() {
        if !suite.images()?.iter().any(|image| image.tag() == tag) {
            continue;
        }
        match suite {
            Suite::Install => {
                for filesystem in install::FILESYSTEMS {
                    let _env = sh.push_env(install::FILESYSTEM_ENV, filesystem);
                    test(sh, build, &selection, *suite, &[])?;
                    build = &later;
                }
            }
            Suite::Container | Suite::Vm => {
                test(sh, build, &selection, *suite, &[])?;
                build = &later;
            }
        }
    }
    Ok(())
}

/// Run `cargo test` for the suite with this binary as the runner.
fn test(
    sh: &Shell,
    build: &Build,
    selection: &Selection,
    suite: Suite,
    args: &[OsString],
) -> Result<()> {
    let variants = suite.images()?;
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

/// The root of the workspace.
fn workspace_dir() -> Result<&'static Path> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("finding the workspace root")
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

/// Build each of the suite's selected images and run the test binary
/// against it. The tests of the other images live in modules named after
/// them and are skipped.
fn run(
    sh: &Shell,
    build: &Build,
    selection: &Selection,
    suite: Suite,
    binary: &Path,
    args: &[OsString],
) -> Result<()> {
    let variants = suite.images()?;
    let target = target_dir(binary)?;
    let selected = variants
        .iter()
        .filter(|image| selection.images.is_empty() || selection.images.contains(&image.tag()));
    for image in selected {
        let reference = build_image(sh, build, image, target)?;
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

/// Build bootc-imagectl and the test image tagged `tag`, then boot it in a
/// VM.
fn boot(sh: &Shell, build: &Build, tag: &str) -> Result<()> {
    let variants = Suite::Vm.images()?;
    let image = find_image(&variants, tag)?;
    let target = build_bootc_imagectl(sh)?;
    let reference = build_image(sh, build, image, &target)?;
    vm::boot(&reference)
}

/// Build bootc-imagectl and the example `name`, install it onto a disk and
/// boot it with a graphical console.
fn example(sh: &Shell, build: &Build, name: &str) -> Result<()> {
    let examples = images::images(workspace_dir()?, &[Source::Example])?;
    let image = examples
        .iter()
        .find(|image| image.name == name)
        .with_context(|| {
            let names: Vec<&str> = examples.iter().map(|image| image.name.as_str()).collect();
            format!(
                "no example named {name}, select one of: {}",
                names.join(", ")
            )
        })?;
    let target = build_bootc_imagectl(sh)?;
    let reference = build_image(sh, build, image, &target)?;
    let work = target.join("example");
    let disk = install::install_disk(sh, &reference, &work, install::Passphrase::Prompt)?;
    install::boot_gui(&disk)
}

/// Render `image` into `target`, build it and return its reference.
fn build_image(sh: &Shell, build: &Build, image: &images::Image, target: &Path) -> Result<String> {
    let tag = image.tag();
    let dir = target.join("images").join(&tag);
    let reference = format!("{REPOSITORY}:{tag}");
    image.render(workspace_dir()?, &dir)?;
    let name = image.build_name();
    let rootfs = format!("{reference}-rootfs");
    let rootfs_args = ["--target=rootfs".into(), format!("--tag={rootfs}")];
    build.build(sh, &name, &dir, target, &rootfs_args)?;
    let chunked = rechunk(sh, &rootfs, &dir)?;
    let args = [
        format!("--build-context=chunked=container-image://{chunked}"),
        format!("--tag={reference}"),
    ];
    build.build(sh, &name, &dir, target, &args)?;
    Ok(reference)
}

/// Rechunk the image `rootfs` with chunkah.
fn rechunk(sh: &Shell, rootfs: &str, dir: &Path) -> Result<String> {
    let config = cmd!(sh, "podman image inspect {rootfs}").read()?;
    let mount = format!("type=image,src={rootfs},dst=/chunkah");
    let volume = format!("{}:/srv:z", dir.display());
    let max_layers = CHUNKAH_MAX_LAYERS.to_string();
    let epoch = CHUNKAH_SOURCE_DATE_EPOCH.to_string();
    cmd!(
        sh,
        "podman run --rm --mount={mount} --volume={volume} --env=CHUNKAH_CONFIG_STR {CHUNKAH} build --max-layers={max_layers} --source-date-epoch={epoch} --output=oci:chunked"
    )
    .env("CHUNKAH_CONFIG_STR", config)
    .run()?;
    let layout = dir.join("chunked");
    Ok(cmd!(sh, "podman pull --quiet oci:{layout}").read()?)
}

/// Build bootc-imagectl and return the target directory holding it.
fn build_bootc_imagectl(sh: &Shell) -> Result<PathBuf> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    cmd!(sh, "{cargo} build --locked --bin bootc-imagectl").run()?;
    // cargo builds bootc-imagectl next to this binary.
    let xtask = std::env::current_exe()?;
    Ok(xtask
        .parent()
        .context("finding the target directory")?
        .to_owned())
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
