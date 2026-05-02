//! SQLite implementation of [`EmbeddingRepository`].

use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};

use super::{EmbeddingRepository, lock_conn};
use crate::StorageError;

/// A single code embedding record.
#[derive(Debug, Clone)]
pub struct EmbeddingRow {
    /// Branch this embedding belongs to.
    pub branch_id: String,
    /// Source file path.
    pub file_path: String,
    /// Item name (function name, type name, or export name).
    pub item_name: String,
    /// Item kind: `"function"`, `"type"`, or `"export"`.
    pub item_kind: String,
    /// Raw embedding vector (f32 values).
    pub embedding: Vec<f32>,
}

/// Input for upserting an embedding.
#[derive(Debug, Clone)]
pub struct EmbeddingInput {
    pub file_path: String,
    pub item_name: String,
    pub item_kind: String,
    pub embedding: Vec<f32>,
}

/// SQLite-backed embedding repository.
#[derive(Debug, Clone)]
pub struct SqliteEmbeddingRepository {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteEmbeddingRepository {
    /// Create a new repository backed by the given connection.
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }
}

/// Map a single database row to an `EmbeddingRow`.
fn row_to_embedding(row: &rusqlite::Row<'_>) -> rusqlite::Result<EmbeddingRow> {
    let blob: Vec<u8> = row.get(4)?;
    Ok(EmbeddingRow {
        branch_id: row.get(0)?,
        file_path: row.get(1)?,
        item_name: row.get(2)?,
        item_kind: row.get(3)?,
        embedding: bytes_to_f32s(&blob),
    })
}

impl EmbeddingRepository for SqliteEmbeddingRepository {
    fn upsert(&self, branch_id: &str, input: &EmbeddingInput) -> Result<(), StorageError> {
        let conn = lock_conn(&self.conn)?;
        let blob = f32s_to_bytes(&input.embedding);

        conn.execute(
            "INSERT INTO code_embeddings (branch_id, file_path, item_name, item_kind, embedding)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(branch_id, file_path, item_name, item_kind) DO UPDATE SET
               embedding = excluded.embedding",
            params![
                branch_id,
                input.file_path,
                input.item_name,
                input.item_kind,
                blob
            ],
        )?;

        Ok(())
    }

    fn upsert_batch(&self, branch_id: &str, inputs: &[EmbeddingInput]) -> Result<(), StorageError> {
        if inputs.is_empty() {
            return Ok(());
        }

        let conn = lock_conn(&self.conn)?;

        let tx = conn
            .unchecked_transaction()
            .map_err(|e| StorageError::QueryError(format!("Failed to begin transaction: {e}")))?;

        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO code_embeddings (branch_id, file_path, item_name, item_kind, embedding)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(branch_id, file_path, item_name, item_kind) DO UPDATE SET
                   embedding = excluded.embedding",
            )?;

            for input in inputs {
                let blob = f32s_to_bytes(&input.embedding);
                stmt.execute(params![
                    branch_id,
                    input.file_path,
                    input.item_name,
                    input.item_kind,
                    blob,
                ])?;
            }
        }

        tx.commit().map_err(|e| {
            StorageError::QueryError(format!("Failed to commit embedding batch: {e}"))
        })?;

        Ok(())
    }

    fn get_by_branch(&self, branch_id: &str) -> Result<Vec<EmbeddingRow>, StorageError> {
        let conn = lock_conn(&self.conn)?;

        let mut stmt = conn.prepare(
            "SELECT branch_id, file_path, item_name, item_kind, embedding
             FROM code_embeddings WHERE branch_id = ?1",
        )?;

        let rows = stmt.query_map(params![branch_id], row_to_embedding)?;

        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn get_by_file(
        &self,
        branch_id: &str,
        file_path: &str,
    ) -> Result<Vec<EmbeddingRow>, StorageError> {
        let conn = lock_conn(&self.conn)?;

        let mut stmt = conn.prepare(
            "SELECT branch_id, file_path, item_name, item_kind, embedding
             FROM code_embeddings WHERE branch_id = ?1 AND file_path = ?2",
        )?;

        let rows = stmt.query_map(params![branch_id, file_path], row_to_embedding)?;

        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn delete_by_file(&self, branch_id: &str, file_path: &str) -> Result<usize, StorageError> {
        let conn = lock_conn(&self.conn)?;

        let deleted = conn.execute(
            "DELETE FROM code_embeddings WHERE branch_id = ?1 AND file_path = ?2",
            params![branch_id, file_path],
        )?;

        Ok(deleted)
    }

    fn delete_by_branch(&self, branch_id: &str) -> Result<usize, StorageError> {
        let conn = lock_conn(&self.conn)?;

        let deleted = conn.execute(
            "DELETE FROM code_embeddings WHERE branch_id = ?1",
            params![branch_id],
        )?;

        Ok(deleted)
    }

    fn count_by_branch(&self, branch_id: &str) -> Result<usize, StorageError> {
        let conn = lock_conn(&self.conn)?;

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM code_embeddings WHERE branch_id = ?1",
            params![branch_id],
            |row| row.get(0),
        )?;

        Ok(usize::try_from(count).unwrap_or(0))
    }
}

// ─── Serialization helpers ───────────────────────────────────────────────────

/// Convert a slice of f32 values to raw little-endian bytes for BLOB storage.
pub fn f32s_to_bytes(values: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for v in values {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes
}

/// Convert raw little-endian bytes back to f32 values.
pub fn bytes_to_f32s(bytes: &[u8]) -> Vec<f32> {
    if bytes.len() % 4 != 0 {
        tracing::warn!(
            len = bytes.len(),
            "embedding blob has non-f32-aligned length; trailing {} bytes will be dropped",
            bytes.len() % 4
        );
    }
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    fn test_repo() -> SqliteEmbeddingRepository {
        let db = Database::open(":memory:").expect("in-memory DB");
        SqliteEmbeddingRepository::new(db.connection().clone())
    }

    #[test]
    fn upsert_and_retrieve_single() {
        let repo = test_repo();
        let input = EmbeddingInput {
            file_path: "src/main.rs".to_string(),
            item_name: "handle_request".to_string(),
            item_kind: "function".to_string(),
            embedding: vec![0.1, 0.2, 0.3],
        };

        repo.upsert("main", &input).unwrap();

        let rows = repo.get_by_branch("main").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].file_path, "src/main.rs");
        assert_eq!(rows[0].item_name, "handle_request");
        assert_eq!(rows[0].item_kind, "function");
        assert_eq!(rows[0].embedding.len(), 3);
        assert!((rows[0].embedding[0] - 0.1).abs() < f32::EPSILON);
        assert!((rows[0].embedding[1] - 0.2).abs() < f32::EPSILON);
        assert!((rows[0].embedding[2] - 0.3).abs() < f32::EPSILON);
    }

    #[test]
    fn upsert_overwrites_existing() {
        let repo = test_repo();
        let input1 = EmbeddingInput {
            file_path: "src/main.rs".to_string(),
            item_name: "foo".to_string(),
            item_kind: "function".to_string(),
            embedding: vec![1.0, 2.0],
        };
        repo.upsert("main", &input1).unwrap();

        let input2 = EmbeddingInput {
            file_path: "src/main.rs".to_string(),
            item_name: "foo".to_string(),
            item_kind: "function".to_string(),
            embedding: vec![3.0, 4.0],
        };
        repo.upsert("main", &input2).unwrap();

        let rows = repo.get_by_branch("main").unwrap();
        assert_eq!(rows.len(), 1);
        assert!((rows[0].embedding[0] - 3.0).abs() < f32::EPSILON);
    }

    #[test]
    fn upsert_batch_stores_multiple() {
        let repo = test_repo();
        let inputs = vec![
            EmbeddingInput {
                file_path: "src/a.rs".to_string(),
                item_name: "fn_a".to_string(),
                item_kind: "function".to_string(),
                embedding: vec![0.1, 0.2],
            },
            EmbeddingInput {
                file_path: "src/b.rs".to_string(),
                item_name: "TypeB".to_string(),
                item_kind: "type".to_string(),
                embedding: vec![0.3, 0.4],
            },
            EmbeddingInput {
                file_path: "src/c.rs".to_string(),
                item_name: "export_c".to_string(),
                item_kind: "export".to_string(),
                embedding: vec![0.5, 0.6],
            },
        ];

        repo.upsert_batch("main", &inputs).unwrap();

        let rows = repo.get_by_branch("main").unwrap();
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn upsert_batch_empty_is_noop() {
        let repo = test_repo();
        repo.upsert_batch("main", &[]).unwrap();
        let rows = repo.get_by_branch("main").unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn get_by_file_filters_correctly() {
        let repo = test_repo();
        let inputs = vec![
            EmbeddingInput {
                file_path: "src/a.rs".to_string(),
                item_name: "fn_a".to_string(),
                item_kind: "function".to_string(),
                embedding: vec![0.1],
            },
            EmbeddingInput {
                file_path: "src/a.rs".to_string(),
                item_name: "TypeA".to_string(),
                item_kind: "type".to_string(),
                embedding: vec![0.2],
            },
            EmbeddingInput {
                file_path: "src/b.rs".to_string(),
                item_name: "fn_b".to_string(),
                item_kind: "function".to_string(),
                embedding: vec![0.3],
            },
        ];
        repo.upsert_batch("main", &inputs).unwrap();

        let a_rows = repo.get_by_file("main", "src/a.rs").unwrap();
        assert_eq!(a_rows.len(), 2);

        let b_rows = repo.get_by_file("main", "src/b.rs").unwrap();
        assert_eq!(b_rows.len(), 1);
    }

    #[test]
    fn delete_by_file_removes_correct_rows() {
        let repo = test_repo();
        let inputs = vec![
            EmbeddingInput {
                file_path: "src/a.rs".to_string(),
                item_name: "fn_a".to_string(),
                item_kind: "function".to_string(),
                embedding: vec![0.1],
            },
            EmbeddingInput {
                file_path: "src/b.rs".to_string(),
                item_name: "fn_b".to_string(),
                item_kind: "function".to_string(),
                embedding: vec![0.2],
            },
        ];
        repo.upsert_batch("main", &inputs).unwrap();

        let deleted = repo.delete_by_file("main", "src/a.rs").unwrap();
        assert_eq!(deleted, 1);

        let remaining = repo.get_by_branch("main").unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].file_path, "src/b.rs");
    }

    #[test]
    fn delete_by_branch_clears_all() {
        let repo = test_repo();
        let inputs = vec![
            EmbeddingInput {
                file_path: "src/a.rs".to_string(),
                item_name: "fn_a".to_string(),
                item_kind: "function".to_string(),
                embedding: vec![0.1],
            },
            EmbeddingInput {
                file_path: "src/b.rs".to_string(),
                item_name: "fn_b".to_string(),
                item_kind: "function".to_string(),
                embedding: vec![0.2],
            },
        ];
        repo.upsert_batch("main", &inputs).unwrap();

        let deleted = repo.delete_by_branch("main").unwrap();
        assert_eq!(deleted, 2);

        let remaining = repo.get_by_branch("main").unwrap();
        assert!(remaining.is_empty());
    }

    #[test]
    fn count_by_branch() {
        let repo = test_repo();
        assert_eq!(repo.count_by_branch("main").unwrap(), 0);

        let inputs = vec![
            EmbeddingInput {
                file_path: "src/a.rs".to_string(),
                item_name: "fn_a".to_string(),
                item_kind: "function".to_string(),
                embedding: vec![0.1],
            },
            EmbeddingInput {
                file_path: "src/b.rs".to_string(),
                item_name: "fn_b".to_string(),
                item_kind: "function".to_string(),
                embedding: vec![0.2],
            },
        ];
        repo.upsert_batch("main", &inputs).unwrap();

        assert_eq!(repo.count_by_branch("main").unwrap(), 2);
    }

    #[test]
    fn branch_isolation() {
        let repo = test_repo();
        let input_main = EmbeddingInput {
            file_path: "src/a.rs".to_string(),
            item_name: "fn_a".to_string(),
            item_kind: "function".to_string(),
            embedding: vec![0.1],
        };
        let input_dev = EmbeddingInput {
            file_path: "src/b.rs".to_string(),
            item_name: "fn_b".to_string(),
            item_kind: "function".to_string(),
            embedding: vec![0.2],
        };
        repo.upsert("main", &input_main).unwrap();
        repo.upsert("dev", &input_dev).unwrap();

        assert_eq!(repo.count_by_branch("main").unwrap(), 1);
        assert_eq!(repo.count_by_branch("dev").unwrap(), 1);

        repo.delete_by_branch("dev").unwrap();
        assert_eq!(repo.count_by_branch("main").unwrap(), 1);
        assert_eq!(repo.count_by_branch("dev").unwrap(), 0);
    }

    // ── Serialization roundtrip tests ────────────────────────────────────

    #[test]
    fn f32_bytes_roundtrip() {
        let original = vec![0.1_f32, -0.5, 1.0, 0.0, f32::MAX, f32::MIN];
        let bytes = f32s_to_bytes(&original);
        assert_eq!(bytes.len(), original.len() * 4);
        let restored = bytes_to_f32s(&bytes);
        assert_eq!(original, restored);
    }

    #[test]
    fn f32_bytes_empty() {
        let bytes = f32s_to_bytes(&[]);
        assert!(bytes.is_empty());
        let restored = bytes_to_f32s(&bytes);
        assert!(restored.is_empty());
    }
}
