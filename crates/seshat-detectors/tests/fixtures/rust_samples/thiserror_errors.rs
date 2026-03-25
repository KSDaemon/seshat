// Sample: thiserror error type patterns
// Expected detections: thiserror dependency, error enum, #[from] conversion

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Query error: {details}")]
    QueryFailed { details: String },

    #[error("Record not found: table={table}, id={id}")]
    NotFound { table: String, id: i64 },

    #[error("IO error")]
    Io(#[from] std::io::Error),
}

pub type DbResult<T> = Result<T, DatabaseError>;

pub fn connect(url: &str) -> DbResult<()> {
    if url.is_empty() {
        return Err(DatabaseError::ConnectionFailed("Empty URL".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_failed() {
        let result = connect("");
        assert!(matches!(result, Err(DatabaseError::ConnectionFailed(_))));
    }

    #[test]
    fn test_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let db_err: DatabaseError = io_err.into();
        assert!(matches!(db_err, DatabaseError::Io(_)));
    }
}
