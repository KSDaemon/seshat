//! SQLite implementation of [`SubmoduleRepository`].

use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};

use super::{SubmoduleRepository, lock_conn};
use crate::StorageError;

/// A row from the `submodules` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmoduleRow {
    /// Auto-incremented primary key.
    pub id: i64,
    /// Mount path relative to repo root (e.g. `"vendor/lib"`).
    pub relative_path: String,
    /// Human-readable submodule name (typically the basename).
    pub name: String,
    /// Absolute path to the submodule's dedicated `.db` file.
    pub db_path: String,
    /// Current HEAD commit hash of the submodule (for change detection).
    pub commit_hash: Option<String>,
    /// ISO-8601 creation timestamp.
    pub created_at: String,
    /// ISO-8601 last-update timestamp.
    pub updated_at: String,
}

/// Input for inserting or updating a submodule record.
/// Does not include `id`, `created_at`, or `updated_at` (managed by DB).
#[derive(Debug, Clone)]
pub struct SubmoduleInput {
    /// Mount path relative to repo root.
    pub relative_path: String,
    /// Human-readable submodule name.
    pub name: String,
    /// Absolute path to the submodule's dedicated `.db` file.
    pub db_path: String,
    /// Current HEAD commit hash.
    pub commit_hash: Option<String>,
}

/// SQLite-backed submodule repository.
#[derive(Debug, Clone)]
pub struct SqliteSubmoduleRepository {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteSubmoduleRepository {
    /// Create a new repository backed by the given connection.
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }
}

impl SubmoduleRepository for SqliteSubmoduleRepository {
    #[tracing::instrument(skip(self))]
    fn insert(&self, input: &SubmoduleInput) -> Result<SubmoduleRow, StorageError> {
        let conn = lock_conn(&self.conn)?;
        conn.execute(
            "INSERT INTO submodules (relative_path, name, db_path, commit_hash)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                input.relative_path,
                input.name,
                input.db_path,
                input.commit_hash
            ],
        )?;
        let id = conn.last_insert_rowid();

        conn.query_row(
            "SELECT id, relative_path, name, db_path, commit_hash, created_at, updated_at
             FROM submodules WHERE id = ?1",
            params![id],
            row_to_submodule,
        )
        .map_err(Into::into)
    }

    #[tracing::instrument(skip(self))]
    fn update(&self, input: &SubmoduleInput) -> Result<(), StorageError> {
        let conn = lock_conn(&self.conn)?;
        let affected = conn.execute(
            "UPDATE submodules
             SET name = ?1, db_path = ?2, commit_hash = ?3, updated_at = datetime('now')
             WHERE relative_path = ?4",
            params![
                input.name,
                input.db_path,
                input.commit_hash,
                input.relative_path
            ],
        )?;

        if affected == 0 {
            return Err(StorageError::NotFound {
                entity: "Submodule",
                id: input.relative_path.clone(),
            });
        }
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    fn upsert(&self, input: &SubmoduleInput) -> Result<(), StorageError> {
        let conn = lock_conn(&self.conn)?;
        conn.execute(
            "INSERT INTO submodules (relative_path, name, db_path, commit_hash)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(relative_path) DO UPDATE SET
                 name = excluded.name,
                 db_path = excluded.db_path,
                 commit_hash = excluded.commit_hash,
                 updated_at = datetime('now')",
            params![
                input.relative_path,
                input.name,
                input.db_path,
                input.commit_hash
            ],
        )?;
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    fn delete(&self, relative_path: &str) -> Result<(), StorageError> {
        let conn = lock_conn(&self.conn)?;
        let affected = conn.execute(
            "DELETE FROM submodules WHERE relative_path = ?1",
            params![relative_path],
        )?;

        if affected == 0 {
            return Err(StorageError::NotFound {
                entity: "Submodule",
                id: relative_path.to_string(),
            });
        }
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    fn list(&self) -> Result<Vec<SubmoduleRow>, StorageError> {
        let conn = lock_conn(&self.conn)?;
        let mut stmt = conn.prepare(
            "SELECT id, relative_path, name, db_path, commit_hash, created_at, updated_at
             FROM submodules ORDER BY relative_path",
        )?;
        let rows = stmt.query_map([], row_to_submodule)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    #[tracing::instrument(skip(self))]
    fn find_by_path(&self, relative_path: &str) -> Result<Option<SubmoduleRow>, StorageError> {
        let conn = lock_conn(&self.conn)?;
        let result = conn.query_row(
            "SELECT id, relative_path, name, db_path, commit_hash, created_at, updated_at
             FROM submodules WHERE relative_path = ?1",
            params![relative_path],
            row_to_submodule,
        );

        match result {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StorageError::from(e)),
        }
    }
}

/// Map a rusqlite `Row` to a [`SubmoduleRow`].
fn row_to_submodule(row: &rusqlite::Row<'_>) -> rusqlite::Result<SubmoduleRow> {
    Ok(SubmoduleRow {
        id: row.get(0)?,
        relative_path: row.get(1)?,
        name: row.get(2)?,
        db_path: row.get(3)?,
        commit_hash: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    fn test_repo() -> SqliteSubmoduleRepository {
        let db = Database::open(":memory:").expect("in-memory DB");
        SqliteSubmoduleRepository::new(db.connection().clone())
    }

    fn make_input(path: &str) -> SubmoduleInput {
        SubmoduleInput {
            relative_path: path.to_string(),
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            db_path: format!("/data/seshat/repos/project/{path}.db"),
            commit_hash: Some("abc123".to_string()),
        }
    }

    #[test]
    fn insert_and_find_by_path() {
        let repo = test_repo();
        let input = make_input("vendor/lib");

        let inserted = repo.insert(&input).expect("insert should succeed");
        assert_eq!(inserted.relative_path, "vendor/lib");
        assert_eq!(inserted.name, "lib");
        assert_eq!(inserted.db_path, "/data/seshat/repos/project/vendor/lib.db");
        assert_eq!(inserted.commit_hash, Some("abc123".to_string()));
        assert!(inserted.id > 0);

        let found = repo
            .find_by_path("vendor/lib")
            .expect("find should succeed")
            .expect("row should exist");
        assert_eq!(found.id, inserted.id);
        assert_eq!(found.relative_path, "vendor/lib");
    }

    #[test]
    fn find_by_path_not_found() {
        let repo = test_repo();
        let result = repo
            .find_by_path("nonexistent")
            .expect("find should not error");
        assert!(result.is_none());
    }

    #[test]
    fn insert_duplicate_path_errors() {
        let repo = test_repo();
        let input = make_input("vendor/lib");

        repo.insert(&input).expect("first insert should succeed");
        let result = repo.insert(&input);
        assert!(result.is_err(), "duplicate relative_path should fail");
    }

    #[test]
    fn update_existing() {
        let repo = test_repo();
        let input = make_input("vendor/lib");
        repo.insert(&input).expect("insert");

        let updated_input = SubmoduleInput {
            relative_path: "vendor/lib".to_string(),
            name: "lib-renamed".to_string(),
            db_path: "/data/seshat/repos/project/vendor/lib.db".to_string(),
            commit_hash: Some("def456".to_string()),
        };

        repo.update(&updated_input).expect("update should succeed");

        let found = repo.find_by_path("vendor/lib").unwrap().unwrap();
        assert_eq!(found.name, "lib-renamed");
        assert_eq!(found.commit_hash, Some("def456".to_string()));
    }

    #[test]
    fn update_nonexistent_errors() {
        let repo = test_repo();
        let input = make_input("nonexistent");

        let result = repo.update(&input);
        assert!(result.is_err(), "updating nonexistent should fail");
    }

    #[test]
    fn delete_existing() {
        let repo = test_repo();
        let input = make_input("vendor/lib");
        repo.insert(&input).expect("insert");

        repo.delete("vendor/lib").expect("delete should succeed");

        let found = repo.find_by_path("vendor/lib").unwrap();
        assert!(found.is_none(), "deleted row should not be found");
    }

    #[test]
    fn delete_nonexistent_errors() {
        let repo = test_repo();
        let result = repo.delete("nonexistent");
        assert!(result.is_err(), "deleting nonexistent should fail");
    }

    #[test]
    fn list_returns_sorted_by_path() {
        let repo = test_repo();
        repo.insert(&make_input("vendor/z-lib")).expect("insert");
        repo.insert(&make_input("vendor/a-lib")).expect("insert");
        repo.insert(&make_input("deps/core")).expect("insert");

        let rows = repo.list().expect("list should succeed");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].relative_path, "deps/core");
        assert_eq!(rows[1].relative_path, "vendor/a-lib");
        assert_eq!(rows[2].relative_path, "vendor/z-lib");
    }

    #[test]
    fn list_empty() {
        let repo = test_repo();
        let rows = repo.list().expect("list should succeed");
        assert!(rows.is_empty());
    }

    #[test]
    fn insert_with_no_commit_hash() {
        let repo = test_repo();
        let input = SubmoduleInput {
            relative_path: "vendor/lib".to_string(),
            name: "lib".to_string(),
            db_path: "/data/seshat/repos/project/vendor/lib.db".to_string(),
            commit_hash: None,
        };

        let inserted = repo.insert(&input).expect("insert should succeed");
        assert!(inserted.commit_hash.is_none());
    }

    #[test]
    fn upsert_inserts_new() {
        let repo = test_repo();
        repo.upsert(&make_input("vendor/lib"))
            .expect("upsert should succeed");

        let found = repo.find_by_path("vendor/lib").unwrap().unwrap();
        assert_eq!(found.relative_path, "vendor/lib");
        assert_eq!(found.commit_hash, Some("abc123".to_string()));
    }

    #[test]
    fn upsert_updates_existing() {
        let repo = test_repo();
        repo.insert(&make_input("vendor/lib")).expect("insert");

        let updated = SubmoduleInput {
            relative_path: "vendor/lib".to_string(),
            name: "lib-v2".to_string(),
            db_path: "/new/path.db".to_string(),
            commit_hash: Some("def456".to_string()),
        };
        repo.upsert(&updated).expect("upsert should succeed");

        let found = repo.find_by_path("vendor/lib").unwrap().unwrap();
        assert_eq!(found.name, "lib-v2");
        assert_eq!(found.db_path, "/new/path.db");
        assert_eq!(found.commit_hash, Some("def456".to_string()));
    }
}
