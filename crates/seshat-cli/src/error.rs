//! CLI error types.

/// Errors originating from CLI commands and TUI.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// A command received invalid arguments.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// The specified path does not exist or is not a directory.
    #[error("invalid path '{path}': {reason}")]
    InvalidPath {
        /// The path that was invalid.
        path: String,
        /// Why the path is invalid.
        reason: String,
    },

    /// A subcommand failed.
    #[error("{command} failed: {reason}")]
    CommandFailed {
        /// Which command failed.
        command: String,
        /// Why it failed.
        reason: String,
    },

    /// TUI rendering error.
    #[error("TUI error: {0}")]
    TuiError(String),

    /// IO error with path context.
    #[error("{message} (path: {path})")]
    IoWithPath {
        /// Human-readable description of the operation that failed.
        message: String,
        /// The file or directory involved.
        path: std::path::PathBuf,
    },

    /// IO error.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl CliError {
    /// Shorthand for `CommandFailed { command: "scan", reason }`.
    pub fn scan(reason: impl std::fmt::Display) -> Self {
        Self::CommandFailed {
            command: "scan".to_owned(),
            reason: reason.to_string(),
        }
    }
}
