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
pub mod db;
/// Debug command: dump conventions with evidence snippets from the DB.
pub mod debug;
/// CLI error types.
pub mod error;
/// Shared output formatting utilities (color, verbosity, bar charts, etc.).
pub mod format;
/// Implementation of the `seshat init` command.
pub mod init;
/// Agent instruction file management (upsert, skill install, hooks).
pub mod instructions;
/// Scan report rendering (overview, conventions, next steps).
pub mod report;
/// Implementation of the `seshat review` command.
pub mod review;
/// Implementation of the `seshat scan` command.
pub mod scan;
/// Implementation of the `seshat serve` command.
pub mod serve;
/// Implementation of the `seshat status` command.
pub mod status;
/// TUI components for interactive convention review.
pub mod tui;
/// Implementation of the `seshat uninstall` command.
pub mod uninstall;
/// Implementation of the `seshat update` command.
pub mod update;
/// Version check cache utilities for self-update.
pub mod version_cache;

pub use args::{Cli, Command};
pub use db::{find_git_root, get_current_branch};
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

    // Print background update notice for all commands except update/update --check.
    // Uses the 24h cache so at most one GitHub API call per day.
    // Network failures are silently ignored — no delay, no output.
    // Goes to stderr so MCP protocol consumers are unaffected.
    if !matches!(cli.command, Command::Update { .. }) {
        update::check_and_print_update_notice();
    }

    match cli.command {
        Command::Scan {
            path,
            verbose,
            quiet,
            exclude_submodules,
        } => scan::run_scan(&path, verbose, quiet, exclude_submodules),

        Command::Serve {
            repo,
            host,
            port,
            call_log,
        } => serve::run_serve(repo.as_deref(), host, port, call_log),

        Command::Status { verbose } => status::run_status(verbose),

        Command::Review => review::run_review(None),

        Command::DebugSnippets { path } => {
            let resolved = db::resolve_project(path.as_deref(), "debug")?;
            let branch = db::detect_branch(&resolved.project_root);
            debug::run_debug(&resolved.db_path, &branch)
        }

        Command::Init {
            client,
            project,
            global,
            dry_run,
            skip_instructions,
        } => {
            let scope = if project {
                init::ScopeRequest::Project
            } else if global {
                init::ScopeRequest::Global
            } else {
                init::ScopeRequest::Auto
            };
            init::run_init(client.as_deref(), scope, dry_run, skip_instructions)
        }

        Command::Uninstall {
            client,
            project,
            global,
            dry_run,
        } => {
            let scope = if project {
                uninstall::ScopeRequest::Project
            } else if global {
                uninstall::ScopeRequest::Global
            } else {
                uninstall::ScopeRequest::Auto
            };
            uninstall::run_uninstall(client.as_deref(), scope, dry_run)
        }

        Command::Update { check } => update::run_update(check),
    }
}
