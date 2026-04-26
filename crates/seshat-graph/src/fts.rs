//! FTS5 full-text search index management for convention descriptions.
//!
//! The `conventions_fts` FTS5 virtual table stores convention descriptions,
//! node IDs, and detector names. It is rebuilt after every scan to stay in
//! sync with the `nodes` table.
//!
//! # Usage
//!
//! - [`rebuild_fts_index`] — wipes and repopulates the FTS5 table from convention nodes.
//! - [`search_conventions`] — searches convention descriptions by keyword/phrase.

use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};
use seshat_core::NodeId;

use crate::error::GraphError;
use crate::{SOURCE_AUTO_DETECTED, SOURCE_USER};

/// Rebuild the FTS5 index from convention nodes in the `nodes` table.
///
/// Deletes all existing rows from `conventions_fts`, then re-inserts from
/// nodes where `ext_data.source` is `"auto_detected"` or `"user"`.
///
/// Call this after convention persistence during scan to keep the index current.
pub fn rebuild_fts_index(conn: &Arc<Mutex<Connection>>) -> Result<usize, GraphError> {
    let conn = crate::lock_conn(conn)?;

    // Clear the FTS5 table.
    conn.execute("DELETE FROM conventions_fts", [])
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Failed to clear FTS5 index: {e}"
            )))
        })?;

    // Re-insert from convention nodes (auto_detected + user),
    // deduplicating by description_hash: user node takes priority over auto-detected.
    let inserted = conn
         .execute(
              &format!(
                  "INSERT INTO conventions_fts (description, node_id, detector_name)
                  SELECT
                      n.description,
                      CAST(n.id AS TEXT),
                      COALESCE(json_extract(n.ext_data, '$.detector_name'), '')
                  FROM nodes n
                  WHERE json_extract(n.ext_data, '$.source') IN ('{SOURCE_AUTO_DETECTED}', '{SOURCE_USER}')
                    AND n.description_hash IS NOT NULL
                    AND n.id IN (
                      SELECT id FROM nodes n2
                      WHERE n2.description_hash = n.description_hash
                        AND json_extract(n2.ext_data, '$.source') IN ('{SOURCE_AUTO_DETECTED}', '{SOURCE_USER}')
                      ORDER BY
                        CASE json_extract(n2.ext_data, '$.source')
                          WHEN '{SOURCE_USER}' THEN 0
                          ELSE 1
                        END
                      LIMIT 1
                    )
                  UNION ALL
                  SELECT
                      n.description,
                      CAST(n.id AS TEXT),
                      COALESCE(json_extract(n.ext_data, '$.detector_name'), '')
                  FROM nodes n
                  WHERE json_extract(n.ext_data, '$.source') IN ('{SOURCE_AUTO_DETECTED}', '{SOURCE_USER}')
                    AND n.description_hash IS NULL"
              ),
              [],
          )
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Failed to rebuild FTS5 index: {e}"
            )))
        })?;

    tracing::info!(count = inserted, "Rebuilt FTS5 conventions index");
    Ok(inserted)
}

/// Search convention descriptions using FTS5 full-text search.
///
/// Returns matching node IDs ordered by FTS5 rank (best match first).
/// An empty query returns an empty result (not an error).
pub fn search_conventions(
    conn: &Arc<Mutex<Connection>>,
    query: &str,
) -> Result<Vec<NodeId>, GraphError> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(vec![]);
    }

    let conn = crate::lock_conn(conn)?;

    // Sanitize the query for FTS5: wrap each token in double quotes to prevent
    // FTS5 syntax errors from special characters. Tokens are split on whitespace.
    let sanitized = sanitize_fts_query(trimmed);

    let mut stmt = conn
        .prepare(
            "SELECT node_id, rank
             FROM conventions_fts
             WHERE conventions_fts MATCH ?1
             ORDER BY rank",
        )
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Failed to prepare FTS5 search: {e}"
            )))
        })?;

    let rows = stmt
        .query_map(params![sanitized], |row| {
            let node_id_str: String = row.get(0)?;
            let id: i64 = node_id_str.parse().map_err(|_| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(std::fmt::Error),
                )
            })?;
            Ok(NodeId(id))
        })
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "FTS5 search failed: {e}"
            )))
        })?;

    let mut results = Vec::new();
    for row in rows {
        match row {
            Ok(node_id) => results.push(node_id),
            Err(e) => {
                tracing::warn!("Skipping FTS5 row due to parse error: {e}");
            }
        }
    }

    Ok(results)
}

/// Insert a single convention entry into the FTS5 index.
///
/// Used when recording a new user decision (avoids full rebuild).
pub fn insert_fts_entry(
    conn: &Arc<Mutex<Connection>>,
    node_id: NodeId,
    description: &str,
    detector_name: &str,
) -> Result<(), GraphError> {
    let conn = crate::lock_conn(conn)?;

    conn.execute(
        "INSERT INTO conventions_fts (description, node_id, detector_name)
         VALUES (?1, ?2, ?3)",
        params![description, node_id.0.to_string(), detector_name],
    )
    .map_err(|e| {
        GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
            "Failed to insert FTS5 entry: {e}"
        )))
    })?;

    Ok(())
}

/// Delete a single convention entry from the FTS5 index by node ID.
///
/// Used when removing a user decision.
pub fn delete_fts_entry(conn: &Arc<Mutex<Connection>>, node_id: NodeId) -> Result<(), GraphError> {
    let conn = crate::lock_conn(conn)?;

    conn.execute(
        "DELETE FROM conventions_fts WHERE node_id = ?1",
        params![node_id.0.to_string()],
    )
    .map_err(|e| {
        GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
            "Failed to delete FTS5 entry: {e}"
        )))
    })?;

    Ok(())
}

/// Sanitize an FTS5 query string to prevent syntax errors.
///
/// Wraps each whitespace-delimited token in double quotes so that special
/// characters (colons, hyphens, etc.) are treated as literals. Tokens are
/// joined with spaces (implicit AND in FTS5).
fn sanitize_fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|token| {
            // Escape any embedded double quotes by doubling them.
            let escaped = token.replace('"', "\"\"");
            format!("\"{escaped}\"")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_helpers::test_conn;

    /// Helper: insert a convention node with ext_data.source and return its assigned ID.
    fn insert_convention(
        conn: &Arc<Mutex<Connection>>,
        description: &str,
        source: &str,
        detector_name: &str,
    ) -> NodeId {
        let c = conn.lock().unwrap();
        c.execute(
            "INSERT INTO nodes (branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data)
             VALUES ('main', 'convention', 'strong', 0.9, 10, 12, ?1, ?2)",
            params![
                description,
                serde_json::json!({
                    "source": source,
                    "detector_name": detector_name,
                })
                .to_string(),
            ],
        )
        .unwrap();
        NodeId(c.last_insert_rowid())
    }

    #[test]
    fn rebuild_fts_index_populates_from_convention_nodes() {
        let conn = test_conn();

        // Insert convention nodes.
        insert_convention(
            &conn,
            "Uses thiserror for error handling",
            "auto_detected",
            "error_handling",
        );
        insert_convention(
            &conn,
            "Snake case naming convention",
            "auto_detected",
            "naming",
        );
        insert_convention(&conn, "Always use Result type", "user", "");

        // Insert a fact node (should NOT appear in FTS).
        {
            let c = conn.lock().unwrap();
            c.execute(
                "INSERT INTO nodes (branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data)
                 VALUES ('main', 'fact', 'moderate', 0.5, 1, 1, 'Module: seshat-core', NULL)",
                [],
            )
            .unwrap();
        }

        let count = rebuild_fts_index(&conn).unwrap();
        assert_eq!(
            count, 3,
            "should index 3 convention nodes (2 auto + 1 user)"
        );
    }

    #[test]
    fn rebuild_fts_index_clears_old_data() {
        let conn = test_conn();

        insert_convention(&conn, "Old convention", "auto_detected", "test");
        rebuild_fts_index(&conn).unwrap();

        // Verify it's indexed.
        let results = search_conventions(&conn, "old").unwrap();
        assert_eq!(results.len(), 1);

        // Now delete the node and rebuild.
        {
            let c = conn.lock().unwrap();
            c.execute("DELETE FROM nodes", []).unwrap();
        }
        let count = rebuild_fts_index(&conn).unwrap();
        assert_eq!(count, 0, "should be empty after nodes cleared");

        let results = search_conventions(&conn, "old").unwrap();
        assert!(
            results.is_empty(),
            "search should return nothing after rebuild"
        );
    }

    #[test]
    fn search_conventions_finds_matching_descriptions() {
        let conn = test_conn();

        let id1 = insert_convention(
            &conn,
            "Uses thiserror for error handling",
            "auto_detected",
            "error_handling",
        );
        let id2 = insert_convention(
            &conn,
            "Snake case naming convention for functions",
            "auto_detected",
            "naming",
        );
        let _id3 = insert_convention(&conn, "Always use Result type for errors", "user", "");

        rebuild_fts_index(&conn).unwrap();

        // Search for "error" — should match id1 (description contains "error").
        // FTS5 matches whole tokens, so "errors" != "error" — id3 may not match.
        let results = search_conventions(&conn, "error").unwrap();
        assert!(!results.is_empty(), "should find results for 'error'");
        assert!(results.contains(&id1));

        // Search for "naming" — should match id2.
        let results = search_conventions(&conn, "naming").unwrap();
        assert!(!results.is_empty(), "should find results for 'naming'");
        assert!(results.contains(&id2));
    }

    #[test]
    fn search_conventions_empty_query_returns_empty() {
        let conn = test_conn();
        insert_convention(&conn, "Some convention", "auto_detected", "test");
        rebuild_fts_index(&conn).unwrap();

        let results = search_conventions(&conn, "").unwrap();
        assert!(results.is_empty());

        let results = search_conventions(&conn, "   ").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_conventions_no_matches_returns_empty() {
        let conn = test_conn();
        insert_convention(&conn, "Uses thiserror", "auto_detected", "error_handling");
        rebuild_fts_index(&conn).unwrap();

        let results = search_conventions(&conn, "nonexistent_xyz").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn insert_fts_entry_makes_searchable() {
        let conn = test_conn();

        // Insert directly without rebuild.
        let node_id = NodeId(42);
        insert_fts_entry(&conn, node_id, "Custom user decision about logging", "").unwrap();

        let results = search_conventions(&conn, "logging").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], node_id);
    }

    #[test]
    fn delete_fts_entry_removes_from_search() {
        let conn = test_conn();

        let node_id = NodeId(42);
        insert_fts_entry(&conn, node_id, "Decision about database", "").unwrap();

        // Verify searchable.
        let results = search_conventions(&conn, "database").unwrap();
        assert_eq!(results.len(), 1);

        // Delete and verify gone.
        delete_fts_entry(&conn, node_id).unwrap();
        let results = search_conventions(&conn, "database").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn sanitize_fts_query_wraps_tokens() {
        assert_eq!(
            sanitize_fts_query("error handling"),
            "\"error\" \"handling\""
        );
        assert_eq!(sanitize_fts_query("thiserror"), "\"thiserror\"");
        assert_eq!(sanitize_fts_query("snake_case"), "\"snake_case\"");
        // Special chars are safely quoted.
        assert_eq!(sanitize_fts_query("foo:bar"), "\"foo:bar\"");
    }
}
