use std::process::ExitCode;

use bootc_imagectl::cli::Cli;
use clap::Parser;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;

// The final error must reach the user regardless of the `RUST_LOG` filter.
// We bypass tracing and go straight to stderr.
#[allow(clippy::print_stderr)]
fn main() -> ExitCode {
    init_tracing();
    match Cli::parse().run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Log to stderr at `info` and above. `RUST_LOG` overrides the log level.
fn init_tracing() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .without_time()
        .with_target(false)
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();
}
