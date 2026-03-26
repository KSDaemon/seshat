use std::path::PathBuf;

/// Errors originating from the scanning pipeline.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// Failed to parse a source file.
    #[error("Failed to parse {path}: {reason}")]
    ParseError { path: PathBuf, reason: String },

    /// The file's language is not supported.
    #[error("Unsupported language: {0}")]
    UnsupportedLanguage(String),

    /// File discovery failed.
    #[error("Discovery error in {path}: {reason}")]
    DiscoveryError { path: PathBuf, reason: String },

    /// Failed to parse a dependency manifest.
    #[error("Manifest parse error in {path}: {reason}")]
    ManifestError { path: PathBuf, reason: String },

    /// Failed to parse a documentation file.
    #[error("Documentation parse error in {path}: {reason}")]
    DocumentationError { path: PathBuf, reason: String },

    /// IO error during scanning.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
