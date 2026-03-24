/// Errors originating from CLI commands and TUI.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// A command received invalid arguments.
    #[error("Invalid argument: {0}")]
    InvalidArgument(String),

    /// The specified path does not exist or is not a directory.
    #[error("Invalid path: {path} — {reason}")]
    InvalidPath { path: String, reason: String },

    /// A subcommand failed.
    #[error("Command '{command}' failed: {reason}")]
    CommandFailed { command: String, reason: String },

    /// TUI rendering error.
    #[error("TUI error: {0}")]
    TuiError(String),

    /// IO error.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
