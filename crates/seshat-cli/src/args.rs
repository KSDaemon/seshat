//! Command-line argument definitions via `clap` derive.
//!
//! All CLI types live here so that `seshat-bin` stays thin — it only
//! parses args via [`Cli::parse()`] and delegates to this crate.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Full version string including git hash: "0.1.0 (abc1234)".
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("GIT_HASH"), ")");

/// Seshat — convention detection for AI-assisted development.
#[derive(Debug, Parser)]
#[command(
    name = "seshat",
    version = VERSION,
    about = "Convention detection for AI-assisted development",
    long_about = None,
)]
pub struct Cli {
    /// The subcommand to execute.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Scan a project directory and display analysis report.
    Scan {
        /// Path to the project directory to scan.
        path: PathBuf,

        /// Show verbose output: skipped files, detector details, timing.
        #[arg(long, short)]
        verbose: bool,

        /// Show only errors and final summary.
        #[arg(long, short)]
        quiet: bool,
    },

    /// Start the MCP server for AI agent connections.
    Serve,

    /// Show indexed projects, watcher, and server state.
    Status,

    /// Interactive convention review.
    Review,

    /// Generate MCP configuration for detected AI clients.
    Init,
}
