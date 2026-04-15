//! Command-line argument definitions via `clap` derive.
//!
//! All CLI types live here so that `seshat-bin` stays thin — it only
//! parses args via [`Cli::parse()`] and delegates to this crate.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Full version string including git hash: "0.1.0 (abc1234)".
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("GIT_HASH"), ")");

/// Seshat — the operating manual for your codebase, written for AI agents.
#[derive(Debug, Parser)]
#[command(
    name = "seshat",
    version = VERSION,
    about = "The operating manual for your codebase — written for AI agents",
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

        /// Exclude submodules from scanning (they are scanned by default).
        #[arg(long)]
        exclude_submodules: bool,
    },

    /// Start the MCP server for AI agent connections.
    Serve {
        /// Repository directory path or project name.
        /// Auto-detected from current working directory if omitted.
        repo: Option<PathBuf>,

        /// Host to bind the HTTP/SSE transport to (overrides config).
        #[arg(long)]
        host: Option<String>,

        /// Port for the HTTP/SSE transport (overrides config).
        #[arg(long)]
        port: Option<u16>,

        /// Log MCP tool calls to JSONL file for analysis.
        /// Default: $XDG_DATA_HOME/seshat/call-log.jsonl
        #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = "")]
        call_log: Option<PathBuf>,
    },

    /// Show indexed projects, submodules, and database info.
    Status {
        /// Show full database paths and additional detail.
        #[arg(long, short)]
        verbose: bool,
    },

    /// Interactive convention review.
    Review,

    /// Generate MCP configuration for detected AI clients.
    ///
    /// Auto-detects installed AI coding clients. By default uses smart scope:
    /// project-level config if it already exists, global config otherwise.
    /// For JSON configs, offers to auto-patch with backup. For JSONC, shows
    /// a copy-paste snippet.
    Init {
        /// Specific client to configure. Auto-detects all if omitted.
        /// Supported: claude-code, claude-desktop, opencode, cursor
        client: Option<String>,

        /// Always use project-level configs (in CWD / git root).
        /// Writes to .claude/settings.local.json, ./opencode.json, etc.
        #[arg(long, conflicts_with = "global")]
        project: bool,

        /// Always use global user configs (default fallback behaviour).
        #[arg(long, conflicts_with = "project")]
        global: bool,

        /// Show what would be done without writing any files.
        #[arg(long)]
        dry_run: bool,

        /// Only write MCP config; skip agent instructions, skills, and hooks.
        #[arg(long)]
        skip_instructions: bool,
    },
}
