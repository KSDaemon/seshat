//! # Seshat CLI
//!
//! CLI commands and TUI for developer interaction. Provides the human-facing
//! interface to Seshat's capabilities:
//!
//! - `seshat scan <path>` — scan a project and display analysis report
//! - `seshat serve` — start MCP server for AI agent connections (stub)
//! - `seshat status` — show indexed projects, watcher, and server state (stub)
//! - `seshat review` — interactive TUI for convention review (stub)
//! - `seshat init` — generate MCP configuration for detected AI clients (stub)
//!
//! Uses `clap` for argument parsing, `indicatif` for progress bars, and
//! `tracing-subscriber` for log output.

pub mod args;
pub mod config;
pub mod error;
pub mod format;
pub mod report;
pub mod scan;

pub use args::{Cli, Command};
pub use error::CliError;
pub use format::Verbosity;

use clap::Parser;
use tracing_subscriber::EnvFilter;

/// Parse CLI arguments, initialize logging, and dispatch to the appropriate
/// command handler.
///
/// This is the single entry point called by `seshat-bin/main.rs`.
pub fn run() -> Result<(), CliError> {
    let cli = Cli::parse();

    // Initialize tracing. SESHAT_LOG env var controls level (e.g. "debug").
    // Default to "warn" so that library tracing doesn't clutter CLI output.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("SESHAT_LOG").unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    match cli.command {
        Command::Scan {
            path,
            verbose,
            quiet,
        } => scan::run_scan(&path, verbose, quiet),

        Command::Serve => {
            eprintln!("error: `seshat serve` is not yet implemented");
            std::process::exit(1);
        }

        Command::Status => {
            eprintln!("error: `seshat status` is not yet implemented");
            std::process::exit(1);
        }

        Command::Review => {
            eprintln!("error: `seshat review` is not yet implemented");
            std::process::exit(1);
        }

        Command::Init => {
            eprintln!("error: `seshat init` is not yet implemented");
            std::process::exit(1);
        }
    }
}
