//! # Seshat
//!
//! Binary entry point for the Seshat CLI tool and MCP server.
//! Thin wrapper — parses args and delegates to `seshat-cli`.

fn main() {
    if let Err(e) = seshat_cli::run() {
        // Structured error output: "error: {message}" with optional hint.
        eprintln!("error: {e}");
        if let seshat_cli::CliError::InvalidPath { .. } = &e {
            eprintln!("hint: provide a path to an existing project directory");
        }
        std::process::exit(1);
    }
}
