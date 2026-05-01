//! Golden files computation — top convention-compliant files.
//!
//! Golden files are the most convention-compliant files in the project,
//! identified by their `convention_compliance_count` in the `files_ir` table.
//! AI agents can reference these files as exemplars when generating code.
//!
//! # Usage
//!
//! ```ignore
//! let golden = get_golden_files(conn, 5)?;
//! for gf in &golden {
//!     println!("{}: {} conventions", gf.path, gf.conventions_count);
//! }
//! ```

use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};
use serde::Serialize;
use seshat_core::BranchId;

use crate::error::GraphError;

/// Default number of golden files to return.
pub const DEFAULT_GOLDEN_FILES_LIMIT: usize = 5;

/// A top convention-compliant file (golden file).
#[derive(Debug, Clone, Serialize)]
pub struct GoldenFile {
    /// Relative file path within the project.
    pub path: String,
    /// Number of conventions this file follows.
    pub conventions_count: u32,
    /// Last git commit timestamp (Unix seconds), if available.
    pub last_modified: Option<i64>,
}

/// Get the top convention-compliant files (golden files) for a specific branch.
///
/// Returns files ordered by `convention_compliance_count` descending, limited
/// to `limit` results. Files with zero conventions are excluded.
pub fn get_golden_files(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &BranchId,
    limit: usize,
) -> Result<Vec<GoldenFile>, GraphError> {
    let conn = crate::lock_conn(conn)?;

    let mut stmt = conn
        .prepare(
            "SELECT file_path, convention_compliance_count, last_commit_date
             FROM files_ir
             WHERE branch_id = ?1
               AND convention_compliance_count > 0
             ORDER BY convention_compliance_count DESC
             LIMIT ?2",
        )
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Failed to prepare golden files query: {e}"
            )))
        })?;

    let rows = stmt
        .query_map(params![branch_id.0, limit as i64], |row| {
            Ok(GoldenFile {
                path: row.get(0)?,
                conventions_count: row.get(1)?,
                last_modified: row.get(2)?,
            })
        })
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Golden files query failed: {e}"
            )))
        })?;

    let mut results = Vec::new();
    for row in rows {
        match row {
            Ok(gf) => results.push(gf),
            Err(e) => {
                tracing::warn!("Skipping golden file row due to error: {e}");
            }
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    fn default_branch() -> BranchId {
        BranchId::from("main")
    }
    use super::*;
    use seshat_core::Language;
    use seshat_core::test_helpers::make_project_file;
    use seshat_storage::{FileIRRepository, SqliteFileIRRepository};

    use crate::test_helpers::test_conn;

    /// Helper: insert a file and set its compliance count.
    fn insert_file_with_compliance(
        conn: &Arc<Mutex<Connection>>,
        path: &str,
        compliance_count: u32,
        last_commit_date: Option<i64>,
    ) {
        let repo = SqliteFileIRRepository::new(conn.clone());
        let branch = seshat_core::BranchId::from("main");

        let mut file = make_project_file(Language::Rust);
        file.path = path.into();
        file.content_hash = format!("hash_{path}");

        repo.upsert(&branch, &file, last_commit_date).unwrap();

        // Directly update compliance count.
        let c = conn.lock().unwrap();
        c.execute(
            "UPDATE files_ir SET convention_compliance_count = ?1
             WHERE branch_id = 'main' AND file_path = ?2",
            params![compliance_count, path],
        )
        .unwrap();
    }

    #[test]
    fn get_golden_files_returns_top_by_compliance() {
        let conn = test_conn();

        insert_file_with_compliance(&conn, "src/best.rs", 10, Some(1_700_000_000));
        insert_file_with_compliance(&conn, "src/good.rs", 7, Some(1_699_000_000));
        insert_file_with_compliance(&conn, "src/ok.rs", 3, None);
        insert_file_with_compliance(&conn, "src/poor.rs", 1, Some(1_698_000_000));

        let golden = get_golden_files(&conn, &default_branch(), 5).unwrap();
        assert_eq!(golden.len(), 4);
        assert_eq!(golden[0].path, "src/best.rs");
        assert_eq!(golden[0].conventions_count, 10);
        assert_eq!(golden[0].last_modified, Some(1_700_000_000));
        assert_eq!(golden[1].path, "src/good.rs");
        assert_eq!(golden[1].conventions_count, 7);
        assert_eq!(golden[2].path, "src/ok.rs");
        assert_eq!(golden[2].conventions_count, 3);
        assert_eq!(golden[2].last_modified, None);
        assert_eq!(golden[3].path, "src/poor.rs");
        assert_eq!(golden[3].conventions_count, 1);
    }

    #[test]
    fn get_golden_files_respects_limit() {
        let conn = test_conn();

        insert_file_with_compliance(&conn, "src/a.rs", 10, None);
        insert_file_with_compliance(&conn, "src/b.rs", 8, None);
        insert_file_with_compliance(&conn, "src/c.rs", 6, None);
        insert_file_with_compliance(&conn, "src/d.rs", 4, None);
        insert_file_with_compliance(&conn, "src/e.rs", 2, None);

        let golden = get_golden_files(&conn, &default_branch(), 3).unwrap();
        assert_eq!(golden.len(), 3);
        assert_eq!(golden[0].conventions_count, 10);
        assert_eq!(golden[2].conventions_count, 6);
    }

    #[test]
    fn get_golden_files_excludes_zero_compliance() {
        let conn = test_conn();

        insert_file_with_compliance(&conn, "src/good.rs", 5, None);
        insert_file_with_compliance(&conn, "src/zero.rs", 0, None);

        let golden = get_golden_files(&conn, &default_branch(), 10).unwrap();
        assert_eq!(golden.len(), 1);
        assert_eq!(golden[0].path, "src/good.rs");
    }

    #[test]
    fn get_golden_files_empty_table() {
        let conn = test_conn();
        let golden = get_golden_files(&conn, &default_branch(), 5).unwrap();
        assert!(golden.is_empty());
    }

    #[test]
    fn get_golden_files_default_limit_is_5() {
        let conn = test_conn();

        for i in 0..10 {
            insert_file_with_compliance(&conn, &format!("src/file_{i}.rs"), (10 - i) as u32, None);
        }

        let golden =
            get_golden_files(&conn, &default_branch(), DEFAULT_GOLDEN_FILES_LIMIT).unwrap();
        assert_eq!(golden.len(), 5);
        assert_eq!(golden[0].conventions_count, 10);
        assert_eq!(golden[4].conventions_count, 6);
    }

    #[test]
    fn get_golden_files_filters_by_branch_id() {
        let conn = test_conn();
        let branch = default_branch();
        insert_file_with_compliance(&conn, "src/file.rs", 10, None);
        let other = BranchId::from("feature");
        let repo = seshat_storage::SqliteFileIRRepository::new(conn.clone());
        let mut file = seshat_core::test_helpers::make_project_file(seshat_core::Language::Rust);
        file.path = "src/file.rs".into();
        file.content_hash = "hash_f".to_string();
        repo.upsert(&other, &file, None).unwrap();
        let c = conn.lock().unwrap();
        c.execute(
            "UPDATE files_ir SET convention_compliance_count = 10
             WHERE branch_id = 'feature' AND file_path = 'src/file.rs'",
            [],
        )
        .unwrap();
        drop(c);
        let golden = get_golden_files(&conn, &branch, 5).unwrap();
        assert_eq!(golden.len(), 1, "should return only main-branch file");
        assert_eq!(golden[0].path, "src/file.rs");
    }
}
