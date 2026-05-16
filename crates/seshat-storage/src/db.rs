//! Database lifecycle: open, WAL mode, migrations.

use std::path::Path;
use std::sync::{Arc, Mutex};

use refinery::embed_migrations;
use rusqlite::{Connection, params};

use crate::StorageError;
use crate::ir_serialization::{IR_SCHEMA_VERSION, deserialize_ir};
use crate::repository::{extract_definitions, extract_imports};

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

        // Populate the symbol-index tables from any existing `files_ir`
        // rows.  Gated on "symbol_definitions empty AND files_ir non-empty"
        // so re-opening a populated DB is a no-op.
        backfill_symbol_index(&conn).map_err(|e| {
            StorageError::MigrationError(format!("V13 symbol-index backfill failed: {e}"))
        })?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Returns a reference to the underlying connection wrapped in `Arc<Mutex<_>>`.
    pub fn connection(&self) -> &Arc<Mutex<Connection>> {
        &self.conn
    }
}

/// Populate `symbol_definitions` and `symbol_imports` for every row in
/// `files_ir` whose IR matches the current schema version.
///
/// Idempotent:
/// - Skips the whole pass when `symbol_definitions` is already non-empty
///   (any earlier successful backfill or scan will have inserted rows).
/// - When it does run, it `DELETE`s the existing rows for each
///   `(branch_id, file_path)` before inserting, so re-running on a partially
///   populated DB still produces the right end state.
///
/// Stale IR rows (rows with an older `ir_schema_version`) are skipped — they
/// will be re-scanned and indexed when the user next runs `seshat scan`,
/// matching how the file-IR layer already treats them.
fn backfill_symbol_index(conn: &Connection) -> Result<(), StorageError> {
    // Gate on `symbol_definitions` only — `symbol_imports` is allowed to be
    // legitimately empty for a project with no concrete-named imports (e.g.
    // single-file unit-test fixtures).  Real-world risk of a "definitions
    // populated, imports table externally truncated" half-state is mitigated
    // by the fact that the backfill itself runs inside a single transaction
    // (so a crash mid-flight rolls back the entire write).
    let already_populated: i64 = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM symbol_definitions LIMIT 1)",
        [],
        |row| row.get(0),
    )?;
    if already_populated != 0 {
        return Ok(());
    }

    let files_ir_total: i64 =
        conn.query_row("SELECT COUNT(*) FROM files_ir", [], |row| row.get(0))?;
    if files_ir_total == 0 {
        return Ok(());
    }

    // Materialise the (branch_id, file_path, ir_data) triples first so the
    // prepared SELECT is dropped before we BEGIN the write transaction.
    // SQLite tolerates nested statements on the same connection, but keeping
    // read/write phases separate avoids depending on that.
    struct StaleRow {
        branch_id: String,
        file_path: String,
        ir_data: Vec<u8>,
    }
    let rows: Vec<StaleRow> = {
        let mut stmt = conn.prepare(
            "SELECT branch_id, file_path, ir_data FROM files_ir
             WHERE ir_schema_version = ?1",
        )?;
        let iter = stmt.query_map(params![i64::from(IR_SCHEMA_VERSION)], |row| {
            Ok(StaleRow {
                branch_id: row.get(0)?,
                file_path: row.get(1)?,
                ir_data: row.get(2)?,
            })
        })?;
        iter.collect::<Result<Vec<_>, _>>()?
    };

    let tx = conn
        .unchecked_transaction()
        .map_err(|e| StorageError::QueryError(format!("begin V13 backfill tx: {e}")))?;

    let mut indexed = 0_u64;
    let mut skipped = 0_u64;

    {
        let mut delete_defs = tx.prepare_cached(
            "DELETE FROM symbol_definitions WHERE branch_id = ?1 AND file_path = ?2",
        )?;
        let mut delete_imps = tx.prepare_cached(
            "DELETE FROM symbol_imports WHERE branch_id = ?1 AND importer_file = ?2",
        )?;
        let mut insert_def = tx.prepare_cached(
            "INSERT INTO symbol_definitions
                (branch_id, symbol_name, file_path, line, end_line, kind, is_public, snippet)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        let mut insert_imp = tx.prepare_cached(
            "INSERT INTO symbol_imports (branch_id, imported_name, importer_file)
             VALUES (?1, ?2, ?3)",
        )?;

        for row in rows {
            let project_file = match deserialize_ir(&row.ir_data) {
                Ok(pf) => pf,
                Err(e) => {
                    tracing::warn!(
                        "V13 backfill: skipping {}:{} — IR deserialize failed: {e}",
                        row.branch_id,
                        row.file_path,
                    );
                    skipped += 1;
                    continue;
                }
            };

            delete_defs.execute(params![row.branch_id, row.file_path])?;
            delete_imps.execute(params![row.branch_id, row.file_path])?;

            for def in extract_definitions(&project_file) {
                insert_def.execute(params![
                    row.branch_id,
                    def.symbol_name,
                    def.file_path,
                    def.line,
                    def.end_line,
                    def.kind.as_str(),
                    i64::from(def.is_public),
                    def.snippet,
                ])?;
            }
            for imp in extract_imports(&project_file) {
                insert_imp
                    .execute(params![row.branch_id, imp.imported_name, imp.importer_file,])?;
            }
            indexed += 1;
        }
    }

    tx.commit()
        .map_err(|e| StorageError::QueryError(format!("commit V13 backfill tx: {e}")))?;

    if skipped > 0 {
        tracing::info!(
            "V13 backfill: indexed {indexed} files, skipped {skipped} stale files \
             (run `seshat scan` to re-index them)",
        );
    } else {
        tracing::info!("V13 backfill: indexed {indexed} files");
    }
    Ok(())
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
        assert!(
            tables.contains(&"symbol_definitions".to_string()),
            "missing symbol_definitions table"
        );
        assert!(
            tables.contains(&"symbol_imports".to_string()),
            "missing symbol_imports table"
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
        assert!(
            indexes.contains(&"idx_symbol_definitions_branch_name".to_string()),
            "missing idx_symbol_definitions_branch_name"
        );
        assert!(
            indexes.contains(&"idx_symbol_imports_branch_name".to_string()),
            "missing idx_symbol_imports_branch_name"
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

    // ── V13 symbol-index backfill tests ──────────────────────────────────

    /// Build a Rust IR `ProjectFile` with one public function, one type, one
    /// export, and a mix of concrete + wildcard + namespace imports.
    fn rust_fixture(path: &str) -> seshat_core::ProjectFile {
        use seshat_core::{
            Export, Function, Import, Language, LanguageIR, ProjectFile, RustIR, TypeDef,
            TypeDefKind,
        };

        ProjectFile {
            path: PathBuf::from(path),
            language: Language::Rust,
            content_hash: "h".to_owned(),
            imports: vec![
                Import {
                    module: "foo".to_owned(),
                    names: vec!["Bar".to_owned()],
                    is_type_only: false,
                    line: 1,
                },
                Import {
                    module: "wild".to_owned(),
                    names: vec!["*".to_owned()],
                    is_type_only: false,
                    line: 2,
                },
            ],
            exports: vec![Export {
                name: "exported".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 30,
                end_line: 30,
            }],
            functions: vec![Function {
                name: "do_thing".to_owned(),
                is_public: true,
                is_async: false,
                line: 10,
                end_line: 12,
                parameters: vec!["x".to_owned()],
                doc_comment: None,
            }],
            types: vec![TypeDef {
                name: "Widget".to_owned(),
                kind: TypeDefKind::Struct,
                is_public: true,
                line: 20,
                end_line: 25,
                doc_comment: None,
            }],
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
            file_doc: None,
        }
    }

    fn python_fixture(path: &str) -> seshat_core::ProjectFile {
        use seshat_core::{
            Function, Import, Language, LanguageIR, ProjectFile, PythonIR, TypeDef, TypeDefKind,
        };

        ProjectFile {
            path: PathBuf::from(path),
            language: Language::Python,
            content_hash: "h".to_owned(),
            imports: vec![Import {
                module: "os".to_owned(),
                names: vec!["path".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            exports: Vec::new(),
            functions: vec![Function {
                name: "helper".to_owned(),
                is_public: false,
                is_async: false,
                line: 5,
                end_line: 7,
                parameters: vec![],
                doc_comment: None,
            }],
            types: vec![TypeDef {
                name: "MyClass".to_owned(),
                kind: TypeDefKind::Class,
                is_public: true,
                line: 10,
                end_line: 20,
                doc_comment: None,
            }],
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Python(PythonIR::default()),
            file_doc: None,
        }
    }

    fn ts_fixture(path: &str) -> seshat_core::ProjectFile {
        use seshat_core::{
            Export, Function, Import, Language, LanguageIR, ProjectFile, TypeDef, TypeDefKind,
            TypeScriptIR,
        };

        ProjectFile {
            path: PathBuf::from(path),
            language: Language::TypeScript,
            content_hash: "h".to_owned(),
            imports: vec![
                Import {
                    module: "react".to_owned(),
                    names: vec!["React".to_owned()],
                    is_type_only: false,
                    line: 1,
                },
                Import {
                    module: "namespaced".to_owned(),
                    names: vec!["* as alias".to_owned()],
                    is_type_only: false,
                    line: 2,
                },
            ],
            exports: vec![Export {
                name: "App".to_owned(),
                is_default: true,
                is_type_only: false,
                line: 10,
                end_line: 30,
            }],
            functions: vec![Function {
                name: "App".to_owned(),
                is_public: true,
                is_async: false,
                line: 10,
                end_line: 30,
                parameters: vec![],
                doc_comment: None,
            }],
            types: vec![TypeDef {
                name: "AppProps".to_owned(),
                kind: TypeDefKind::Interface,
                is_public: true,
                line: 5,
                end_line: 8,
                doc_comment: None,
            }],
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::TypeScript(TypeScriptIR::default()),
            file_doc: None,
        }
    }

    fn js_fixture(path: &str) -> seshat_core::ProjectFile {
        use seshat_core::{
            Export, Function, Import, JavaScriptIR, Language, LanguageIR, ProjectFile, TypeDef,
            TypeDefKind,
        };

        ProjectFile {
            path: PathBuf::from(path),
            language: Language::JavaScript,
            content_hash: "h".to_owned(),
            imports: vec![Import {
                module: "lodash".to_owned(),
                names: vec!["map".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            exports: vec![Export {
                name: "handler".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 12,
                end_line: 25,
            }],
            functions: vec![Function {
                name: "handler".to_owned(),
                is_public: true,
                is_async: true,
                line: 12,
                end_line: 25,
                parameters: vec![],
                doc_comment: None,
            }],
            types: vec![TypeDef {
                name: "Handler".to_owned(),
                kind: TypeDefKind::Class,
                is_public: true,
                line: 4,
                end_line: 10,
                doc_comment: None,
            }],
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::JavaScript(JavaScriptIR::default()),
            file_doc: None,
        }
    }

    /// Insert a `files_ir` row directly with serialized IR — bypasses the
    /// repository so we can simulate "DB existed before V13 ran".
    fn insert_files_ir_row(conn: &Connection, branch: &str, file: &seshat_core::ProjectFile) {
        let ir_bytes = crate::ir_serialization::serialize_ir(file).expect("serialize");
        conn.execute(
            "INSERT INTO files_ir
                (branch_id, file_path, language, content_hash, ir_data, ir_schema_version,
                 last_commit_date, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, datetime('now'))",
            params![
                branch,
                file.path.to_string_lossy().as_ref(),
                file.language.as_str(),
                file.content_hash,
                ir_bytes,
                i64::from(IR_SCHEMA_VERSION),
            ],
        )
        .expect("insert files_ir row");
    }

    fn count_rows(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |row| row.get(0)).expect("count")
    }

    #[test]
    fn backfill_noop_on_fresh_in_memory_db() {
        // Empty DB: no files_ir rows → backfill must short-circuit and leave
        // both symbol tables empty.
        let db = Database::open(":memory:").expect("open");
        let conn = db.connection().lock().unwrap();
        assert_eq!(
            count_rows(&conn, "SELECT COUNT(*) FROM symbol_definitions"),
            0
        );
        assert_eq!(count_rows(&conn, "SELECT COUNT(*) FROM symbol_imports"), 0);
    }

    #[test]
    fn backfill_populates_pre_v13_db_on_next_open() {
        // Simulate an existing DB whose files_ir was populated before V13
        // existed: open the (already-migrated) DB once, seed files_ir, wipe
        // symbol_definitions, then `backfill_symbol_index` should refill it.
        let tmp = TempDir::new("backfill_populate");
        let db_path = tmp.path().join("test.db");

        {
            let db = Database::open(&db_path).expect("first open");
            let conn = db.connection().lock().unwrap();
            insert_files_ir_row(&conn, "main", &rust_fixture("src/lib.rs"));
            insert_files_ir_row(&conn, "main", &python_fixture("pkg/mod.py"));
            insert_files_ir_row(&conn, "main", &ts_fixture("src/app.tsx"));
            insert_files_ir_row(&conn, "main", &js_fixture("src/handler.js"));
            // Wipe the symbol tables so the second open's backfill gate
            // ("symbol_definitions empty") fires.
            conn.execute("DELETE FROM symbol_definitions", []).unwrap();
            conn.execute("DELETE FROM symbol_imports", []).unwrap();
        }

        {
            let db = Database::open(&db_path).expect("second open");
            let conn = db.connection().lock().unwrap();
            // Rust: fn + type + export = 3.  Python: fn + type = 2 (no export).
            // TS:   fn + type + export = 3.  JS:   fn + type + export = 3.
            // Total = 11 definitions.
            assert_eq!(
                count_rows(&conn, "SELECT COUNT(*) FROM symbol_definitions"),
                11
            );
            // Imports: Rust → 1 ("Bar"), Python → 1 ("path"), TS → 1 ("React")
            // (wildcards filtered), JS → 1 ("map").  Total = 4.
            assert_eq!(count_rows(&conn, "SELECT COUNT(*) FROM symbol_imports"), 4);
        }
    }

    #[test]
    fn backfill_is_idempotent_running_twice() {
        // Running the backfill on an already-populated DB should be a no-op —
        // counts stay stable.
        let tmp = TempDir::new("backfill_idempotent");
        let db_path = tmp.path().join("test.db");

        {
            let db = Database::open(&db_path).expect("first open");
            let conn = db.connection().lock().unwrap();
            insert_files_ir_row(&conn, "main", &rust_fixture("src/lib.rs"));
            conn.execute("DELETE FROM symbol_definitions", []).unwrap();
            conn.execute("DELETE FROM symbol_imports", []).unwrap();
        }

        let counts_after_first = {
            let db = Database::open(&db_path).expect("second open");
            let conn = db.connection().lock().unwrap();
            (
                count_rows(&conn, "SELECT COUNT(*) FROM symbol_definitions"),
                count_rows(&conn, "SELECT COUNT(*) FROM symbol_imports"),
            )
        };

        // Third open — symbol_definitions is non-empty so the gate skips the
        // backfill; counts must not change.
        let counts_after_second = {
            let db = Database::open(&db_path).expect("third open");
            let conn = db.connection().lock().unwrap();
            (
                count_rows(&conn, "SELECT COUNT(*) FROM symbol_definitions"),
                count_rows(&conn, "SELECT COUNT(*) FROM symbol_imports"),
            )
        };

        assert_eq!(counts_after_first, counts_after_second);
        assert_eq!(counts_after_first.0, 3);
        assert_eq!(counts_after_first.1, 1);
    }

    #[test]
    fn backfill_excludes_defining_file_imports_for_wildcards() {
        // The IR `imports` contains a wildcard plus a concrete name — only
        // the concrete one should land in `symbol_imports`.
        let tmp = TempDir::new("backfill_wildcards");
        let db_path = tmp.path().join("test.db");

        {
            let db = Database::open(&db_path).expect("open");
            let conn = db.connection().lock().unwrap();
            insert_files_ir_row(&conn, "main", &rust_fixture("src/lib.rs"));
            conn.execute("DELETE FROM symbol_definitions", []).unwrap();
            conn.execute("DELETE FROM symbol_imports", []).unwrap();
        }

        let db = Database::open(&db_path).expect("open after seed");
        let conn = db.connection().lock().unwrap();

        let imports: Vec<String> = conn
            .prepare("SELECT imported_name FROM symbol_imports ORDER BY imported_name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect();

        assert_eq!(imports, vec!["Bar".to_owned()]);
    }

    #[test]
    fn backfill_skips_stale_ir_rows() {
        // A row tagged with an older `ir_schema_version` cannot be
        // deserialized — backfill must skip it without aborting the whole
        // pass.
        let tmp = TempDir::new("backfill_stale");
        let db_path = tmp.path().join("test.db");

        {
            let db = Database::open(&db_path).expect("open");
            let conn = db.connection().lock().unwrap();
            // Insert a fresh row + a row tagged as stale (older schema version)
            // with a placeholder blob.
            insert_files_ir_row(&conn, "main", &rust_fixture("src/fresh.rs"));
            conn.execute(
                "INSERT INTO files_ir
                    (branch_id, file_path, language, content_hash, ir_data, ir_schema_version,
                     last_commit_date, updated_at)
                 VALUES ('main','src/stale.rs','rust','h',?1, ?2, NULL, datetime('now'))",
                params![vec![0u8, 0u8, 0u8], i64::from(IR_SCHEMA_VERSION) - 1],
            )
            .unwrap();
            conn.execute("DELETE FROM symbol_definitions", []).unwrap();
            conn.execute("DELETE FROM symbol_imports", []).unwrap();
        }

        let db = Database::open(&db_path).expect("reopen");
        let conn = db.connection().lock().unwrap();
        // Only fresh row should contribute its definitions (3) and imports (1).
        assert_eq!(
            count_rows(&conn, "SELECT COUNT(*) FROM symbol_definitions"),
            3
        );
        assert_eq!(count_rows(&conn, "SELECT COUNT(*) FROM symbol_imports"), 1);
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
