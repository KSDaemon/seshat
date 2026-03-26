// Fixture: Rust error types

use std::io;

/// Main application error type.
#[derive(Debug)]
pub enum AppError {
    Io(io::Error),
    Config(String),
    NotFound { resource: String },
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::Config(msg) => write!(f, "Config error: {msg}"),
            Self::NotFound { resource } => write!(f, "Not found: {resource}"),
        }
    }
}

impl std::error::Error for AppError {}

impl From<io::Error> for AppError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
