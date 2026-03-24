/// Errors originating from file watching and incremental updates.
#[derive(Debug, thiserror::Error)]
pub enum WatcherError {
    /// Failed to initialize the file watcher.
    #[error("Watcher initialization failed: {0}")]
    InitError(String),

    /// A file event could not be processed.
    #[error("Failed to process file event for {path}: {reason}")]
    EventProcessingError { path: String, reason: String },

    /// Branch detection failed.
    #[error("Branch detection failed: {0}")]
    BranchDetectionError(String),

    /// IO error.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
