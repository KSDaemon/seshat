//! SQLite implementation of [`FileIRRepository`].

use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection};
use seshat_core::{BranchId, ProjectFile};

use super::FileIRRepository;
use crate::StorageError;

/// SQLite-backed file IR repository.
#[derive(Debug, Clone)]
pub struct SqliteFileIRRepository {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteFileIRRepository {
    /// Create a new repository backed by the given connection.
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }
}

impl FileIRRepository for SqliteFileIRRepository {
    fn upsert(&self, branch_id: &BranchId, file: &ProjectFile) -> Result<(), StorageError> {
        let conn = self.conn.lock().map_err(|e| {
            StorageError::QueryError(format!("Failed to acquire connection lock: {e}"))
        })?;

        let file_path = file.path.to_string_lossy().to_string();
        let language_str = file.language.as_str();
        let ir_data = serde_json::to_vec(file)
            .map_err(|e| StorageError::SerializationError(e.to_string()))?;

        conn.execute(
            "INSERT INTO files_ir (branch_id, file_path, language, content_hash, ir_data, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))
             ON CONFLICT(branch_id, file_path) DO UPDATE SET
               language = excluded.language,
               content_hash = excluded.content_hash,
               ir_data = excluded.ir_data,
               updated_at = datetime('now')",
            params![
                branch_id.0,
                file_path,
                language_str,
                file.content_hash,
                ir_data,
            ],
        )
        .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        Ok(())
    }

    fn get_by_path(
        &self,
        branch_id: &BranchId,
        file_path: &str,
    ) -> Result<ProjectFile, StorageError> {
        let conn = self.conn.lock().map_err(|e| {
            StorageError::QueryError(format!("Failed to acquire connection lock: {e}"))
        })?;

        conn.query_row(
            "SELECT ir_data FROM files_ir WHERE branch_id = ?1 AND file_path = ?2",
            params![branch_id.0, file_path],
            row_to_project_file,
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => StorageError::NotFound {
                entity: "FileIR".to_string(),
                id: format!("{}/{}", branch_id.0, file_path),
            },
            other => StorageError::Sqlite(other.to_string()),
        })
    }

    fn get_by_branch(&self, branch_id: &BranchId) -> Result<Vec<ProjectFile>, StorageError> {
        let conn = self.conn.lock().map_err(|e| {
            StorageError::QueryError(format!("Failed to acquire connection lock: {e}"))
        })?;

        let mut stmt = conn
            .prepare("SELECT ir_data FROM files_ir WHERE branch_id = ?1")
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        let rows = stmt
            .query_map(params![branch_id.0], row_to_project_file)
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| StorageError::Sqlite(e.to_string()))
    }

    fn delete_by_path(&self, branch_id: &BranchId, file_path: &str) -> Result<(), StorageError> {
        let conn = self.conn.lock().map_err(|e| {
            StorageError::QueryError(format!("Failed to acquire connection lock: {e}"))
        })?;

        let affected = conn
            .execute(
                "DELETE FROM files_ir WHERE branch_id = ?1 AND file_path = ?2",
                params![branch_id.0, file_path],
            )
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        if affected == 0 {
            return Err(StorageError::NotFound {
                entity: "FileIR".to_string(),
                id: format!("{}/{}", branch_id.0, file_path),
            });
        }

        Ok(())
    }

    fn check_content_hash(
        &self,
        branch_id: &BranchId,
        file_path: &str,
        content_hash: &str,
    ) -> Result<bool, StorageError> {
        let conn = self.conn.lock().map_err(|e| {
            StorageError::QueryError(format!("Failed to acquire connection lock: {e}"))
        })?;

        let result: Result<String, _> = conn.query_row(
            "SELECT content_hash FROM files_ir WHERE branch_id = ?1 AND file_path = ?2",
            params![branch_id.0, file_path],
            |row| row.get(0),
        );

        match result {
            Ok(stored_hash) => Ok(stored_hash == content_hash),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(e) => Err(StorageError::Sqlite(e.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map a rusqlite Row (ir_data BLOB) to a `ProjectFile`.
fn row_to_project_file(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectFile> {
    let ir_data: Vec<u8> = row.get(0)?;
    serde_json::from_slice(&ir_data).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Blob, Box::new(e))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;
    use seshat_core::test_helpers::make_project_file;
    use seshat_core::Language;

    /// Helper: create an in-memory DB and return a `SqliteFileIRRepository`.
    fn test_repo() -> SqliteFileIRRepository {
        let db = Database::open(":memory:").expect("in-memory DB");
        SqliteFileIRRepository::new(db.connection().clone())
    }

    #[test]
    fn upsert_insert_and_get_by_path() {
        let repo = test_repo();
        let branch = BranchId::from("main");
        let mut file = make_project_file(Language::Rust);
        file.path = "src/main.rs".into();
        file.content_hash = "abc123".to_string();

        repo.upsert(&branch, &file).expect("upsert should succeed");

        let fetched = repo
            .get_by_path(&branch, "src/main.rs")
            .expect("get_by_path should succeed");
        assert_eq!(fetched.path.to_string_lossy(), "src/main.rs");
        assert_eq!(fetched.language, Language::Rust);
        assert_eq!(fetched.content_hash, "abc123");
    }

    #[test]
    fn upsert_updates_existing() {
        let repo = test_repo();
        let branch = BranchId::from("main");
        let mut file = make_project_file(Language::Rust);
        file.path = "src/lib.rs".into();
        file.content_hash = "hash_v1".to_string();

        repo.upsert(&branch, &file).expect("first upsert");

        // Update the same file with new hash
        file.content_hash = "hash_v2".to_string();
        repo.upsert(&branch, &file).expect("second upsert");

        let fetched = repo.get_by_path(&branch, "src/lib.rs").unwrap();
        assert_eq!(fetched.content_hash, "hash_v2");

        // Should only be one record for this branch
        let all = repo.get_by_branch(&branch).unwrap();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn get_by_path_not_found() {
        let repo = test_repo();
        let branch = BranchId::from("main");
        let result = repo.get_by_path(&branch, "nonexistent.rs");
        assert!(matches!(result, Err(StorageError::NotFound { .. })));
    }

    #[test]
    fn get_by_branch() {
        let repo = test_repo();
        let branch_a = BranchId::from("branch-a");
        let branch_b = BranchId::from("branch-b");

        let mut f1 = make_project_file(Language::Rust);
        f1.path = "src/one.rs".into();
        f1.content_hash = "h1".to_string();

        let mut f2 = make_project_file(Language::Python);
        f2.path = "src/two.py".into();
        f2.content_hash = "h2".to_string();

        let mut f3 = make_project_file(Language::TypeScript);
        f3.path = "src/three.ts".into();
        f3.content_hash = "h3".to_string();

        repo.upsert(&branch_a, &f1).unwrap();
        repo.upsert(&branch_a, &f2).unwrap();
        repo.upsert(&branch_b, &f3).unwrap();

        let a_files = repo.get_by_branch(&branch_a).unwrap();
        assert_eq!(a_files.len(), 2);

        let b_files = repo.get_by_branch(&branch_b).unwrap();
        assert_eq!(b_files.len(), 1);
        assert_eq!(b_files[0].language, Language::TypeScript);
    }

    #[test]
    fn delete_by_path() {
        let repo = test_repo();
        let branch = BranchId::from("main");
        let mut file = make_project_file(Language::Rust);
        file.path = "src/delete_me.rs".into();
        file.content_hash = "d1".to_string();

        repo.upsert(&branch, &file).unwrap();
        repo.delete_by_path(&branch, "src/delete_me.rs")
            .expect("delete should succeed");

        let result = repo.get_by_path(&branch, "src/delete_me.rs");
        assert!(matches!(result, Err(StorageError::NotFound { .. })));
    }

    #[test]
    fn delete_by_path_not_found() {
        let repo = test_repo();
        let branch = BranchId::from("main");
        let result = repo.delete_by_path(&branch, "nonexistent.rs");
        assert!(matches!(result, Err(StorageError::NotFound { .. })));
    }

    #[test]
    fn check_content_hash_matches() {
        let repo = test_repo();
        let branch = BranchId::from("main");
        let mut file = make_project_file(Language::Rust);
        file.path = "src/check.rs".into();
        file.content_hash = "correct_hash".to_string();

        repo.upsert(&branch, &file).unwrap();

        assert!(repo
            .check_content_hash(&branch, "src/check.rs", "correct_hash")
            .unwrap());
    }

    #[test]
    fn check_content_hash_mismatch() {
        let repo = test_repo();
        let branch = BranchId::from("main");
        let mut file = make_project_file(Language::Rust);
        file.path = "src/check.rs".into();
        file.content_hash = "hash_a".to_string();

        repo.upsert(&branch, &file).unwrap();

        assert!(!repo
            .check_content_hash(&branch, "src/check.rs", "hash_b")
            .unwrap());
    }

    #[test]
    fn check_content_hash_no_record() {
        let repo = test_repo();
        let branch = BranchId::from("main");

        assert!(!repo
            .check_content_hash(&branch, "nonexistent.rs", "any_hash")
            .unwrap());
    }

    #[test]
    fn all_language_variants_roundtrip() {
        let repo = test_repo();
        let branch = BranchId::from("main");

        let languages = [
            Language::Rust,
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
        ];

        for lang in languages {
            let mut file = make_project_file(lang);
            file.path = format!("test.{}", lang.extensions()[0]).into();
            file.content_hash = format!("hash_{lang}");

            repo.upsert(&branch, &file).unwrap();

            let fetched = repo
                .get_by_path(&branch, &file.path.to_string_lossy())
                .unwrap();
            assert_eq!(
                fetched.language, lang,
                "language roundtrip failed for {lang}"
            );
        }
    }
}
