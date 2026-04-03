//! SQLite implementation of [`RepoMetadataRepository`].

use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};

use super::RepoMetadataRepository;
use crate::StorageError;

/// SQLite-backed repo metadata repository.
#[derive(Debug, Clone)]
pub struct SqliteRepoMetadataRepository {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteRepoMetadataRepository {
    /// Create a new repository backed by the given connection.
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    fn conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StorageError> {
        self.conn.lock().map_err(|e| {
            StorageError::QueryError(format!("Failed to acquire connection lock: {e}"))
        })
    }
}

impl RepoMetadataRepository for SqliteRepoMetadataRepository {
    #[tracing::instrument(skip(self))]
    fn get(&self, key: &str) -> Result<Option<String>, StorageError> {
        let conn = self.conn()?;
        let result = conn.query_row(
            "SELECT value FROM repo_metadata WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        );

        match result {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StorageError::from(e)),
        }
    }

    #[tracing::instrument(skip(self))]
    fn set(&self, key: &str, value: &str) -> Result<(), StorageError> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO repo_metadata (key, value)
             VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    fn get_all(&self) -> Result<Vec<(String, String)>, StorageError> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT key, value FROM repo_metadata ORDER BY key")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    fn test_repo() -> SqliteRepoMetadataRepository {
        let db = Database::open(":memory:").expect("in-memory DB");
        SqliteRepoMetadataRepository::new(db.connection().clone())
    }

    #[test]
    fn set_and_get() {
        let repo = test_repo();
        repo.set("project_name", "seshat")
            .expect("set should succeed");

        let value = repo
            .get("project_name")
            .expect("get should succeed")
            .expect("value should exist");
        assert_eq!(value, "seshat");
    }

    #[test]
    fn get_missing_key() {
        let repo = test_repo();
        let result = repo.get("nonexistent").expect("get should not error");
        assert!(result.is_none());
    }

    #[test]
    fn set_overwrites_existing() {
        let repo = test_repo();
        repo.set("key", "value1").expect("first set");
        repo.set("key", "value2").expect("second set");

        let value = repo.get("key").unwrap().unwrap();
        assert_eq!(value, "value2");
    }

    #[test]
    fn get_all_returns_sorted() {
        let repo = test_repo();
        repo.set("z_key", "z_val").expect("set");
        repo.set("a_key", "a_val").expect("set");
        repo.set("m_key", "m_val").expect("set");

        let all = repo.get_all().expect("get_all should succeed");
        assert_eq!(all.len(), 3);
        assert_eq!(all[0], ("a_key".to_string(), "a_val".to_string()));
        assert_eq!(all[1], ("m_key".to_string(), "m_val".to_string()));
        assert_eq!(all[2], ("z_key".to_string(), "z_val".to_string()));
    }

    #[test]
    fn get_all_empty() {
        let repo = test_repo();
        let all = repo.get_all().expect("get_all should succeed");
        assert!(all.is_empty());
    }

    #[test]
    fn multiple_keys() {
        let repo = test_repo();
        repo.set("project_name", "seshat").expect("set");
        repo.set("file_count", "420").expect("set");
        repo.set("last_scan_time", "1700000000").expect("set");

        assert_eq!(repo.get("project_name").unwrap().unwrap(), "seshat");
        assert_eq!(repo.get("file_count").unwrap().unwrap(), "420");
        assert_eq!(repo.get("last_scan_time").unwrap().unwrap(), "1700000000");
    }
}
