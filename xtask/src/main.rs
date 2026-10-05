//! Developer tasks, run with `cargo xtask`.

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, ValueEnum};
use xshell::{Shell, cmd};

mod container;
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
        /// The image's name, e.g. `ubuntu`.
        image: String,
    },
    /// Run a test binary against each image in tests/images. Cargo runs
    /// this through `test`.
    #[command(hide = true)]
    Runner {
        #[command(flatten)]
        build: Build,
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
    /// replaced by the image name.
    #[arg(long = "build-option", env = BUILD_OPTIONS_ENV, value_delimiter = ' ', allow_hyphen_values = true)]
    options: Vec<String>,
}

impl Build {
    /// Build the image in `dir` as `image`, with the bootc-imagectl binary
    /// from `target`.
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
}

fn main() -> Result<()> {
    let sh = Shell::new()?;
    match Task::parse() {
        Task::Test { build, suite, args } => test(&sh, &build, suite, &args),
        Task::Vm { build, image } => boot(&sh, &build, &image),
        Task::Runner {
            build,
            suite,
            binary,
            args,
        } => run(&sh, &build, suite, &binary, &args),
    }
}

/// Run `cargo test` for the suite with this binary as the runner.
fn test(sh: &Shell, build: &Build, suite: Suite, args: &[OsString]) -> Result<()> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let name = suite.name();
    let xtask = std::env::current_exe()?;
    let runner = format!(
        "target.'cfg(unix)'.runner=['{}', 'runner', '{name}']",
        xtask.display()
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

/// The sorted names of the test images.
fn image_names(images: &Path) -> Result<Vec<String>> {
    let mut names: Vec<String> = std::fs::read_dir(images)
        .with_context(|| format!("reading {}", images.display()))?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_>>()?;
    names.sort();
    Ok(names)
}

/// Build each image in tests/images and run the test binary against it.
/// The tests of the other images live in modules named after them and are
/// skipped.
fn run(sh: &Shell, build: &Build, suite: Suite, binary: &Path, args: &[OsString]) -> Result<()> {
    let images = images_dir()?;
    let names = image_names(&images)?;
    let run = match suite {
        Suite::Container => container::run,
        Suite::Vm => vm::run,
        Suite::Install => install::run,
    };
    for name in &names {
        let image = format!("{REPOSITORY}:{name}");
        let dir = images.join(name);
        let target = target_dir(binary)?;
        build.build(sh, name, &image, &dir, target)?;
        let mut args = args.to_vec();
        for other in names.iter().filter(|other| *other != name) {
            args.push("--skip".into());
            args.push(format!("{other}::").into());
        }
        let color = if std::io::stdout().is_terminal() {
            "always"
        } else {
            "never"
        };
        args.push(format!("--color={color}").into());
        run(sh, &image, binary, &args)?;
    }
    Ok(())
}

/// Build bootc-imagectl and the image `name`, then boot it in a VM.
fn boot(sh: &Shell, build: &Build, name: &str) -> Result<()> {
    let images = images_dir()?;
    let names = image_names(&images)?;
    if !names.iter().any(|other| other == name) {
        bail!(
            "no image is named {name}, select one of: {}",
            names.join(", ")
        );
    }
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    cmd!(sh, "{cargo} build --locked --bin bootc-imagectl").run()?;
    // cargo builds bootc-imagectl next to this binary.
    let xtask = std::env::current_exe()?;
    let target = xtask.parent().context("finding the target directory")?;
    let image = format!("{REPOSITORY}:{name}");
    build.build(sh, name, &image, &images.join(name), target)?;
    vm::boot(&image)
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
