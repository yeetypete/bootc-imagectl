//! Developer tasks, run with `cargo xtask`.

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use xshell::{Shell, cmd};

mod container;
mod install;
mod vm;

/// The repository the test images are tagged under.
const REPOSITORY: &str = "localhost/bootc-imagectl-test";

#[derive(Debug, Parser)]
enum Task {
    /// Run the container, the VM or the install tests.
    Test {
        suite: Suite,
        /// Arguments for the test binary.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Run a test binary against each image in tests/images. Cargo runs
    /// this through `test`.
    #[command(hide = true)]
    Runner {
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
        Task::Test { suite, args } => test(&sh, suite, &args),
        Task::Runner {
            suite,
            binary,
            args,
        } => run(&sh, suite, &binary, &args),
    }
}

/// Run `cargo test` for the suite with this binary as the runner.
fn test(sh: &Shell, suite: Suite, args: &[OsString]) -> Result<()> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let name = suite.name();
    let xtask = std::env::current_exe()?;
    let runner = format!(
        "target.'cfg(unix)'.runner=['{}', 'runner', '{name}']",
        xtask.display()
    );
    cmd!(
        sh,
        "{cargo} test --locked --features {name} --test {name} --config {runner} -- {args...}"
    )
    .run()?;
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
fn run(sh: &Shell, suite: Suite, binary: &Path, args: &[OsString]) -> Result<()> {
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
        cmd!(
            sh,
            "podman build --build-context bootc-imagectl={target} --tag {image} {dir}"
        )
        .run()?;
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
