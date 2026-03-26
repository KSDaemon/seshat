/// Errors originating from the storage layer.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// Database open or creation failed.
    #[error("Failed to open database at {path}: {reason}")]
    OpenError { path: String, reason: String },

    /// A database migration failed.
    #[error("Migration failed: {0}")]
    MigrationError(String),

    /// A query returned unexpected results.
    #[error("Query error: {0}")]
    QueryError(String),

    /// Serialization/deserialization of IR data failed.
    #[error("IR serialization error: {0}")]
    SerializationError(String),

    /// Cached IR has a stale schema version and must be re-parsed.
    #[error("Stale IR: cached version {cached} != current version {current}")]
    StaleIR { cached: u8, current: u8 },

    /// The requested entity was not found.
    #[error("{entity} not found: {id}")]
    NotFound { entity: String, id: String },

    /// SQLite error.
    #[error("SQLite error: {0}")]
    Sqlite(String),

    /// IO error.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
