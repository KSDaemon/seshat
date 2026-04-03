//! # Seshat CLI
//!
//! CLI commands and TUI for developer interaction. Provides the human-facing
//! interface to Seshat's capabilities:
//!
//! - `seshat scan <path>` — scan a project and display analysis report
//! - `seshat serve` — start MCP server for AI agent connections
//! - `seshat status` — show indexed projects, submodules, and database info
//! - `seshat review` — interactive TUI for convention review (stub)
//! - `seshat init` — generate MCP configuration for detected AI clients (stub)
//!
//! Uses `clap` for argument parsing, `indicatif` for progress bars, and
//! `tracing-subscriber` for log output.

/// Command-line argument definitions (clap derive types).
pub mod args;
/// Application configuration loading from `seshat.toml`.
pub mod config;
/// Shared database path utilities (XDG resolution, project name extraction).
pub(crate) mod db;
/// CLI error types.
pub mod error;
/// Shared output formatting utilities (color, verbosity, bar charts, etc.).
pub mod format;
/// Scan report rendering (overview, conventions, next steps).
pub mod report;
/// Implementation of the `seshat scan` command.
pub mod scan;
/// Implementation of the `seshat serve` command.
pub mod serve;
/// Implementation of the `seshat status` command.
pub mod status;

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
            exclude_submodules,
        } => scan::run_scan(&path, verbose, quiet, exclude_submodules),

        Command::Serve { repo, host, port } => serve::run_serve(repo.as_deref(), host, port),

        Command::Status { verbose } => status::run_status(verbose),

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
