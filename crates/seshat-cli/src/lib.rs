//! # Seshat CLI
//!
//! CLI commands and TUI for developer interaction. Provides the human-facing
//! interface to Seshat's capabilities:
//!
//! - `seshat scan <path>` — scan a project and display analysis report
//! - `seshat serve` — start MCP server for AI agent connections
//! - `seshat status` — show indexed projects, watcher, and server state
//! - `seshat review` — interactive TUI for convention review
//! - `seshat init` — generate MCP configuration for detected AI clients
//!
//! Uses `clap` for argument parsing, `ratatui` for TUI, `indicatif` for
//! progress bars, and `owo-colors` for colored output.

pub mod error;

pub use error::CliError;
