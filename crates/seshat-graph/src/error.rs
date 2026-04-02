/// Errors originating from graph queries and intelligence logic.
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    /// The requested repository has not been scanned.
    #[error("Repository not scanned: {path}")]
    RepoNotScanned { path: String },

    /// A query returned no results.
    #[error("No results for query: {0}")]
    EmptyResult(String),

    /// Invalid input provided by the caller.
    #[error("Invalid input: {0}")]
    InvalidInput(String),

    /// Storage layer error.
    #[error("Storage error: {0}")]
    Storage(#[from] seshat_storage::StorageError),

    /// Cache error.
    #[error("Cache error: {0}")]
    CacheError(String),
}
