//! Database lifecycle: open, WAL mode, migrations.

use std::path::Path;
use std::sync::{Arc, Mutex};

use refinery::embed_migrations;
use rusqlite::Connection;

use crate::StorageError;

// Embed migration files from the `migrations/` directory at compile time.
embed_migrations!("migrations");

/// Core database handle. Wraps an `Arc<Mutex<Connection>>` for write access.
///
/// # Usage
/// ```no_run
/// use seshat_storage::Database;
/// let db = Database::open("seshat.db").unwrap();
/// ```
#[derive(Debug, Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    /// Opens (or creates) a SQLite database at `path`, enables WAL mode,
    /// and applies any pending migrations.
    ///
    /// For in-memory databases (testing), pass `":memory:"`.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, StorageError> {
        let path_ref = path.as_ref();
        let path_str = path_ref.to_string_lossy().to_string();

        let mut conn = Connection::open(path_ref).map_err(|e| StorageError::OpenError {
            path: path_str.clone(),
            reason: e.to_string(),
        })?;

        // Enable WAL mode for concurrent readers.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| StorageError::OpenError {
                path: path_str.clone(),
                reason: format!("Failed to set WAL mode: {e}"),
            })?;

        // Enable foreign key enforcement.
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| StorageError::OpenError {
                path: path_str.clone(),
                reason: format!("Failed to enable foreign keys: {e}"),
            })?;

        // Apply pending migrations.
        migrations::runner()
            .run(&mut conn)
            .map_err(|e| StorageError::MigrationError(e.to_string()))?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Returns a reference to the underlying connection wrapped in `Arc<Mutex<_>>`.
    pub fn connection(&self) -> &Arc<Mutex<Connection>> {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Helper: create a temporary directory that is cleaned up on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("seshat_test_{name}_{}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn migration_applies_on_fresh_in_memory_db() {
        let db = Database::open(":memory:").expect("should open in-memory DB");
        let conn = db.connection().lock().unwrap();

        // Verify all five tables exist.
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(tables.contains(&"nodes".to_string()), "missing nodes table");
        assert!(tables.contains(&"edges".to_string()), "missing edges table");
        assert!(
            tables.contains(&"files_ir".to_string()),
            "missing files_ir table"
        );
        assert!(
            tables.contains(&"metadata".to_string()),
            "missing metadata table"
        );
        assert!(
            tables.contains(&"package_metadata".to_string()),
            "missing package_metadata table"
        );

        // Verify indexes exist.
        let indexes: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='index' AND name LIKE 'idx_%' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(
            indexes.contains(&"idx_nodes_branch_id".to_string()),
            "missing idx_nodes_branch_id"
        );
        assert!(
            indexes.contains(&"idx_nodes_nature".to_string()),
            "missing idx_nodes_nature"
        );
        assert!(
            indexes.contains(&"idx_edges_source_id".to_string()),
            "missing idx_edges_source_id"
        );
        assert!(
            indexes.contains(&"idx_edges_target_id".to_string()),
            "missing idx_edges_target_id"
        );
        assert!(
            indexes.contains(&"idx_files_ir_branch_path".to_string()),
            "missing idx_files_ir_branch_path"
        );
        assert!(
            indexes.contains(&"idx_package_metadata_registry".to_string()),
            "missing idx_package_metadata_registry"
        );
        assert!(
            indexes.contains(&"idx_package_metadata_fetched_at".to_string()),
            "missing idx_package_metadata_fetched_at"
        );
    }

    #[test]
    fn reopening_existing_db_is_idempotent() {
        let tmp = TempDir::new("reopen");
        let db_path = tmp.path().join("test.db");

        // First open: creates DB and runs migrations.
        {
            let db = Database::open(&db_path).expect("first open should succeed");
            let conn = db.connection().lock().unwrap();
            conn.execute(
                "INSERT INTO metadata (key, value) VALUES (?1, ?2)",
                rusqlite::params!["test_key", "test_value"],
            )
            .expect("insert should work");
        }

        // Second open: should not fail and data should persist.
        {
            let db = Database::open(&db_path).expect("second open should succeed");
            let conn = db.connection().lock().unwrap();

            let value: String = conn
                .query_row(
                    "SELECT value FROM metadata WHERE key = ?1",
                    rusqlite::params!["test_key"],
                    |row| row.get(0),
                )
                .expect("data should persist across reopens");

            assert_eq!(value, "test_value");
        }
    }
}
