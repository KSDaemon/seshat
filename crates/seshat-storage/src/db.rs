//! Database lifecycle: open, WAL mode, migrations.

use std::path::Path;
use std::sync::{Arc, Mutex};

use refinery::embed_migrations;
use rusqlite::Connection;

use crate::StorageError;

// Embed migration files from the `migrations/` directory at compile time.
embed_migrations!("migrations");

/// Time SQLite waits for a held write lock before returning `SQLITE_BUSY`.
const BUSY_TIMEOUT_MS: u64 = 5_000;

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

        // Wait up to 5 s for a held write lock instead of failing instantly with
        // SQLITE_BUSY. Writers serialise on the same Mutex<Connection> within
        // a process, but a separate process (e.g. `seshat scan` running while
        // `seshat serve` is mid-sync) holds an OS-level lock that the Mutex
        // does not see — busy_timeout is the standard SQLite remedy.
        conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS))
            .map_err(|e| StorageError::OpenError {
                path: path_str.clone(),
                reason: format!("Failed to set busy_timeout: {e}"),
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
        assert!(
            tables.contains(&"code_embeddings".to_string()),
            "missing code_embeddings table"
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
    fn open_sets_busy_timeout() {
        let db = Database::open(":memory:").expect("should open");
        let conn = db.connection().lock().unwrap();

        // rusqlite::Connection has no `busy_timeout` getter, so probe it
        // through PRAGMA. Value is in milliseconds.
        let timeout: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .expect("query busy_timeout");

        assert_eq!(
            timeout,
            i64::try_from(BUSY_TIMEOUT_MS).unwrap(),
            "Database::open must configure busy_timeout to {BUSY_TIMEOUT_MS} ms; \
             a value of 0 makes concurrent writers fail with SQLITE_BUSY immediately."
        );
    }

    #[test]
    fn concurrent_writer_waits_instead_of_failing_with_busy() {
        // Two separate Database handles on the same on-disk file simulate
        // two processes (e.g. `seshat scan` racing `seshat serve`). The first
        // holds an exclusive write txn for ~200 ms; the second's write must
        // succeed instead of returning SQLITE_BUSY.
        let tmp = TempDir::new("busy_timeout");
        let db_path = tmp.path().join("test.db");

        let db1 = Database::open(&db_path).expect("open db1");
        let db2 = Database::open(&db_path).expect("open db2");

        let writer = std::thread::spawn(move || {
            let conn = db1.connection().lock().unwrap();
            // BEGIN IMMEDIATE acquires the RESERVED write lock straight away.
            conn.execute("BEGIN IMMEDIATE", [])
                .expect("begin immediate");
            conn.execute(
                "INSERT INTO metadata (key, value) VALUES (?1, ?2)",
                rusqlite::params!["writer1", "value1"],
            )
            .expect("insert in writer1");
            std::thread::sleep(std::time::Duration::from_millis(200));
            conn.execute("COMMIT", []).expect("commit writer1");
        });

        // Give writer1 enough time to take the lock.
        std::thread::sleep(std::time::Duration::from_millis(50));

        let started_at = std::time::Instant::now();
        let result = {
            let conn = db2.connection().lock().unwrap();
            conn.execute(
                "INSERT INTO metadata (key, value) VALUES (?1, ?2)",
                rusqlite::params!["writer2", "value2"],
            )
        };
        let waited = started_at.elapsed();

        writer.join().expect("writer1 thread");

        assert!(
            result.is_ok(),
            "concurrent writer must succeed (waited busy_timeout, then proceeded), \
             got: {result:?}"
        );
        assert!(
            waited >= std::time::Duration::from_millis(50),
            "concurrent writer must have waited for the held lock, but returned in {waited:?}"
        );
        assert!(
            waited < std::time::Duration::from_millis(BUSY_TIMEOUT_MS),
            "concurrent writer should not have hit the full busy_timeout ceiling \
             (writer1 only held the lock for ~200 ms), but waited {waited:?}"
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
