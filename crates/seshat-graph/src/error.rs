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

    /// The requested knowledge node does not exist (graph queries against
    /// the `nodes` table).
    #[error("Node not found: {0}")]
    NodeNotFound(String),

    /// The requested decision row does not exist in the V12 `decisions`
    /// table. Distinct from NodeNotFound so the MCP envelope can surface
    /// `DECISION_NOT_FOUND` for decision-tool callers without confusing
    /// them with the (no-longer-applicable) "node" terminology.
    #[error("Decision not found: {0}")]
    DecisionNotFound(String),

    /// Attempted to modify an auto-detected convention (only user decisions
    /// can be updated/removed).
    #[error("Not a user decision: {0}")]
    NotUserDecision(String),

    /// Storage layer error.
    #[error("Storage error: {0}")]
    Storage(#[from] seshat_storage::StorageError),

    /// Cache error.
    #[error("Cache error: {0}")]
    CacheError(String),
}

impl GraphError {
    /// Shorthand for wrapping a rusqlite error as a storage query error.
    ///
    /// Replaces the verbose `GraphError::Storage(StorageError::QueryError(format!(...)))`.
    pub fn query(msg: impl std::fmt::Display) -> Self {
        Self::Storage(seshat_storage::StorageError::QueryError(msg.to_string()))
    }
}
