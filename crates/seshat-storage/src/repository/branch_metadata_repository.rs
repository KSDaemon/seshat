//! SQLite implementation of [`BranchMetadataRepository`].

use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};

use super::{BranchMetadataRepository, lock_conn};
use crate::StorageError;

/// SQLite-backed branch metadata repository.
#[derive(Debug, Clone)]
pub struct SqliteBranchMetadataRepository {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteBranchMetadataRepository {
    /// Create a new repository backed by the given connection.
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }
}

impl BranchMetadataRepository for SqliteBranchMetadataRepository {
    #[tracing::instrument(skip(self))]
    fn get(&self, branch_id: &str, key: &str) -> Result<Option<String>, StorageError> {
        let conn = lock_conn(&self.conn)?;
        let result = conn.query_row(
            "SELECT value FROM branch_metadata WHERE branch_id = ?1 AND key = ?2",
            params![branch_id, key],
            |row| row.get::<_, String>(0),
        );

        match result {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StorageError::from(e)),
        }
    }

    #[tracing::instrument(skip(self))]
    fn set(&self, branch_id: &str, key: &str, value: &str) -> Result<(), StorageError> {
        let conn = lock_conn(&self.conn)?;
        conn.execute(
            "INSERT INTO branch_metadata (branch_id, key, value, updated_at)
             VALUES (?1, ?2, ?3, unixepoch())
             ON CONFLICT(branch_id, key) DO UPDATE
             SET value = excluded.value, updated_at = excluded.updated_at",
            params![branch_id, key, value],
        )?;
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    fn list(&self, branch_id: &str) -> Result<Vec<(String, String)>, StorageError> {
        let conn = lock_conn(&self.conn)?;
        let mut stmt = conn
            .prepare("SELECT key, value FROM branch_metadata WHERE branch_id = ?1 ORDER BY key")?;
        let rows = stmt.query_map(params![branch_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    #[tracing::instrument(skip(self))]
    fn delete(&self, branch_id: &str, key: &str) -> Result<(), StorageError> {
        let conn = lock_conn(&self.conn)?;
        conn.execute(
            "DELETE FROM branch_metadata WHERE branch_id = ?1 AND key = ?2",
            params![branch_id, key],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    /// Build a fresh in-memory DB and pre-register two branch rows so tests
    /// can write `branch_metadata` rows without tripping the FK to
    /// `branches(branch_id)`.
    fn test_repo() -> SqliteBranchMetadataRepository {
        let db = Database::open(":memory:").expect("in-memory DB");
        {
            let conn = db.connection().lock().expect("lock conn");
            conn.execute(
                "INSERT INTO branches (branch_id) VALUES (?1)",
                params!["main"],
            )
            .expect("seed branches/main");
            conn.execute(
                "INSERT INTO branches (branch_id) VALUES (?1)",
                params!["feature"],
            )
            .expect("seed branches/feature");
        }
        SqliteBranchMetadataRepository::new(db.connection().clone())
    }

    #[test]
    fn set_and_get() {
        let repo = test_repo();
        repo.set("main", "workspace_crates", "[\"a\"]")
            .expect("set should succeed");

        let value = repo
            .get("main", "workspace_crates")
            .expect("get should succeed")
            .expect("value should exist");
        assert_eq!(value, "[\"a\"]");
    }

    #[test]
    fn get_missing_key() {
        let repo = test_repo();
        let result = repo
            .get("main", "nonexistent")
            .expect("get should not error");
        assert!(result.is_none());
    }

    #[test]
    fn set_overwrites_existing() {
        let repo = test_repo();
        repo.set("main", "k", "v1").expect("first set");
        repo.set("main", "k", "v2").expect("second set");

        let value = repo.get("main", "k").unwrap().unwrap();
        assert_eq!(value, "v2");
    }

    #[test]
    fn list_returns_all_keys_for_branch() {
        let repo = test_repo();
        repo.set("main", "z_key", "z_val").expect("set z");
        repo.set("main", "a_key", "a_val").expect("set a");
        repo.set("main", "m_key", "m_val").expect("set m");

        let all = repo.list("main").expect("list should succeed");
        assert_eq!(all.len(), 3);
        assert_eq!(all[0], ("a_key".to_string(), "a_val".to_string()));
        assert_eq!(all[1], ("m_key".to_string(), "m_val".to_string()));
        assert_eq!(all[2], ("z_key".to_string(), "z_val".to_string()));
    }

    #[test]
    fn list_empty_branch() {
        let repo = test_repo();
        let all = repo.list("main").expect("list should succeed");
        assert!(all.is_empty());
    }

    #[test]
    fn delete_removes_row() {
        let repo = test_repo();
        repo.set("main", "k", "v").expect("set");
        assert!(repo.get("main", "k").unwrap().is_some());

        repo.delete("main", "k").expect("delete should succeed");
        assert!(repo.get("main", "k").unwrap().is_none());
    }

    #[test]
    fn delete_missing_key_is_noop() {
        let repo = test_repo();
        repo.delete("main", "nonexistent")
            .expect("deleting missing key should not error");
    }

    #[test]
    fn branches_are_isolated() {
        // Writes under one branch_id must not bleed into another: the same
        // key on two branches stays distinct.
        let repo = test_repo();
        repo.set("main", "workspace_crates", "[\"a\"]")
            .expect("set on main");
        repo.set("feature", "workspace_crates", "[\"a\",\"b\"]")
            .expect("set on feature");

        assert_eq!(
            repo.get("main", "workspace_crates").unwrap().unwrap(),
            "[\"a\"]",
            "main's value must not be overwritten by feature's set",
        );
        assert_eq!(
            repo.get("feature", "workspace_crates").unwrap().unwrap(),
            "[\"a\",\"b\"]",
            "feature's value must be independently stored",
        );

        // list per-branch returns only that branch's rows.
        let main_rows = repo.list("main").unwrap();
        assert_eq!(main_rows.len(), 1);
        let feature_rows = repo.list("feature").unwrap();
        assert_eq!(feature_rows.len(), 1);

        // Deleting on one branch does not touch the other.
        repo.delete("main", "workspace_crates").unwrap();
        assert!(repo.get("main", "workspace_crates").unwrap().is_none());
        assert!(
            repo.get("feature", "workspace_crates").unwrap().is_some(),
            "feature's row must survive a delete on main",
        );
    }
}
