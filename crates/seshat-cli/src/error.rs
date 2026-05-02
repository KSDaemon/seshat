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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_argument_display() {
        let err = CliError::InvalidArgument("missing path".to_owned());
        assert_eq!(err.to_string(), "invalid argument: missing path");
    }

    #[test]
    fn invalid_path_display() {
        let err = CliError::InvalidPath {
            path: "/tmp/nope".to_owned(),
            reason: "not a directory".to_owned(),
        };
        assert_eq!(err.to_string(), "invalid path '/tmp/nope': not a directory");
    }

    #[test]
    fn command_failed_display() {
        let err = CliError::CommandFailed {
            command: "scan".to_owned(),
            reason: "disk full".to_owned(),
        };
        assert_eq!(err.to_string(), "scan failed: disk full");
    }

    #[test]
    fn tui_error_display() {
        let err = CliError::TuiError("buffer overflow".to_owned());
        assert_eq!(err.to_string(), "TUI error: buffer overflow");
    }

    #[test]
    fn io_with_path_display() {
        let err = CliError::IoWithPath {
            message: "failed to read".to_owned(),
            path: std::path::PathBuf::from("/tmp/file.txt"),
        };
        assert!(err.to_string().contains("failed to read"));
        assert!(err.to_string().contains("/tmp/file.txt"));
    }

    #[test]
    fn io_display() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        let err = CliError::Io(io_err);
        assert!(err.to_string().contains("IO error"));
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn io_from_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let cli_err: CliError = io_err.into();
        assert!(cli_err.to_string().contains("denied"));
    }

    #[test]
    fn scan_constructor() {
        let err = CliError::scan("no disk space");
        assert_eq!(err.to_string(), "scan failed: no disk space");
    }

    #[test]
    fn scan_constructor_with_number() {
        let err = CliError::scan(42);
        assert_eq!(err.to_string(), "scan failed: 42");
    }

    #[test]
    fn error_is_std_error() {
        fn takes_error(_: &dyn std::error::Error) {}
        let err = CliError::InvalidArgument("x".to_owned());
        takes_error(&err);
    }
}
