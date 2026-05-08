//! SQLite implementation of [`DecisionRepository`].
//!
//! Project-wide store for user-recorded decisions, keyed by
//! `description_hash`. Backs the V12 `decisions` table.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use super::{DecisionRepository, lock_conn};
use crate::StorageError;
use seshat_core::BranchId;

/// State of a recorded decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DecisionState {
    /// Convention approved during TUI review.
    Approved,
    /// Convention rejected during TUI review.
    Rejected,
    /// Convention partially adopted during TUI review.
    Partial,
    /// Decision recorded explicitly via MCP `record_decision`.
    Recorded,
}

impl DecisionState {
    /// SQL string form, matching the V12 `state` CHECK constraint.
    pub fn as_sql_str(&self) -> &'static str {
        match self {
            DecisionState::Approved => "approved",
            DecisionState::Rejected => "rejected",
            DecisionState::Partial => "partial",
            DecisionState::Recorded => "recorded",
        }
    }

    /// Parse a SQL string back into a [`DecisionState`].
    pub fn from_sql_str(s: &str) -> Result<Self, StorageError> {
        match s {
            "approved" => Ok(DecisionState::Approved),
            "rejected" => Ok(DecisionState::Rejected),
            "partial" => Ok(DecisionState::Partial),
            "recorded" => Ok(DecisionState::Recorded),
            other => Err(StorageError::QueryError(format!(
                "Invalid decision state in DB: {other}"
            ))),
        }
    }
}

/// Nature of a recorded decision (mirrors `KnowledgeNature` for the
/// subset of values valid in the `decisions.nature` CHECK constraint).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DecisionNature {
    Convention,
    Decision,
    Preference,
    Fact,
}

impl DecisionNature {
    /// SQL string form, matching the V12 `nature` CHECK constraint.
    pub fn as_sql_str(&self) -> &'static str {
        match self {
            DecisionNature::Convention => "convention",
            DecisionNature::Decision => "decision",
            DecisionNature::Preference => "preference",
            DecisionNature::Fact => "fact",
        }
    }

    /// Parse a SQL string back into a [`DecisionNature`].
    pub fn from_sql_str(s: &str) -> Result<Self, StorageError> {
        match s {
            "convention" => Ok(DecisionNature::Convention),
            "decision" => Ok(DecisionNature::Decision),
            "preference" => Ok(DecisionNature::Preference),
            "fact" => Ok(DecisionNature::Fact),
            other => Err(StorageError::QueryError(format!(
                "Invalid decision nature in DB: {other}"
            ))),
        }
    }
}

/// Weight (severity) of a recorded decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DecisionWeight {
    Rule,
    Strong,
}

impl DecisionWeight {
    /// SQL string form, matching the V12 `weight` CHECK constraint.
    pub fn as_sql_str(&self) -> &'static str {
        match self {
            DecisionWeight::Rule => "rule",
            DecisionWeight::Strong => "strong",
        }
    }

    /// Parse a SQL string back into a [`DecisionWeight`].
    pub fn from_sql_str(s: &str) -> Result<Self, StorageError> {
        match s {
            "rule" => Ok(DecisionWeight::Rule),
            "strong" => Ok(DecisionWeight::Strong),
            other => Err(StorageError::QueryError(format!(
                "Invalid decision weight in DB: {other}"
            ))),
        }
    }
}

/// Evidence example attached to a decision.
///
/// Serialised as JSON into the `decisions.examples` column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExampleEvidence {
    pub file: String,
    pub line: u32,
    pub end_line: u32,
    pub snippet: String,
}

/// A user-recorded decision row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub description_hash: String,
    pub description: String,
    pub state: DecisionState,
    pub nature: DecisionNature,
    pub weight: DecisionWeight,
    pub category: Option<String>,
    pub reason: Option<String>,
    pub examples: Vec<ExampleEvidence>,
    pub decided_on_branch: BranchId,
    pub decided_at: i64,
    pub updated_at: i64,
}

/// SQLite-backed decision repository.
#[derive(Debug, Clone)]
pub struct SqliteDecisionRepository {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteDecisionRepository {
    /// Create a new repository backed by the given connection.
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }
}

/// Maximum number of `?` parameters per chunked `IN (...)` SELECT.
///
/// SQLite's default `SQLITE_MAX_VARIABLE_NUMBER` is 999 on older builds
/// and 32766 on newer ones. 500 keeps us comfortably under either limit
/// while still amortising round-trips. PRD §US-008 also specifies 500.
const HASH_BULK_CHUNK_SIZE: usize = 500;

const SELECT_COLUMNS: &str = "description_hash, description, state, nature, weight, \
                              category, reason, examples, decided_on_branch, \
                              decided_at, updated_at";

impl DecisionRepository for SqliteDecisionRepository {
    #[tracing::instrument(skip(self, decision))]
    fn upsert(&self, decision: &Decision) -> Result<(), StorageError> {
        let conn = lock_conn(&self.conn)?;

        let examples_json = serde_json::to_string(&decision.examples)
            .map_err(|e| StorageError::SerializationError(e.to_string()))?;

        conn.execute(
            "INSERT INTO decisions (
                 description_hash, description, state, nature, weight,
                 category, reason, examples, decided_on_branch,
                 decided_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(description_hash) DO UPDATE SET
                 description       = excluded.description,
                 state             = excluded.state,
                 nature            = excluded.nature,
                 weight            = excluded.weight,
                 category          = excluded.category,
                 reason            = excluded.reason,
                 examples          = excluded.examples,
                 decided_on_branch = excluded.decided_on_branch,
                 decided_at        = excluded.decided_at,
                 updated_at        = excluded.updated_at",
            params![
                decision.description_hash,
                decision.description,
                decision.state.as_sql_str(),
                decision.nature.as_sql_str(),
                decision.weight.as_sql_str(),
                decision.category,
                decision.reason,
                examples_json,
                decision.decided_on_branch.0,
                decision.decided_at,
                decision.updated_at,
            ],
        )?;

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    fn get_by_hash(&self, hash: &str) -> Result<Option<Decision>, StorageError> {
        let conn = lock_conn(&self.conn)?;
        let sql = format!("SELECT {SELECT_COLUMNS} FROM decisions WHERE description_hash = ?1");
        let result = conn.query_row(&sql, params![hash], row_to_decision);

        match result {
            Ok(row) => Ok(Some(row?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StorageError::from(e)),
        }
    }

    #[tracing::instrument(skip(self, hashes))]
    fn get_by_hashes(&self, hashes: &[&str]) -> Result<HashMap<String, Decision>, StorageError> {
        if hashes.is_empty() {
            return Ok(HashMap::new());
        }

        let conn = lock_conn(&self.conn)?;
        let mut out: HashMap<String, Decision> = HashMap::with_capacity(hashes.len());

        for chunk in hashes.chunks(HASH_BULK_CHUNK_SIZE) {
            let placeholders = (1..=chunk.len())
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "SELECT {SELECT_COLUMNS} FROM decisions WHERE description_hash IN ({placeholders})"
            );

            let mut stmt = conn.prepare(&sql)?;
            let params_vec: Vec<&dyn rusqlite::types::ToSql> = chunk
                .iter()
                .map(|h| h as &dyn rusqlite::types::ToSql)
                .collect();
            let rows = stmt.query_map(params_vec.as_slice(), row_to_decision)?;
            for row in rows {
                let decision = row??;
                out.insert(decision.description_hash.clone(), decision);
            }
        }

        Ok(out)
    }

    #[tracing::instrument(skip(self))]
    fn find_by_hash_prefix(&self, prefix: &str) -> Result<Vec<Decision>, StorageError> {
        let conn = lock_conn(&self.conn)?;
        // GLOB pushes prefix matching down to the index — the PK on
        // description_hash supports a range scan for `xxx*` queries.
        // Pre-fix the CLI did this filter in Rust after a full repo.list(),
        // O(N) per call regardless of how selective the prefix was.
        let glob_pattern = format!("{prefix}*");
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM decisions
             WHERE description_hash GLOB ?1
             ORDER BY description_hash ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![glob_pattern], row_to_decision)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    #[tracing::instrument(skip(self))]
    fn delete(&self, hash: &str) -> Result<(), StorageError> {
        let conn = lock_conn(&self.conn)?;
        // Idempotent: missing rows are not an error.
        conn.execute(
            "DELETE FROM decisions WHERE description_hash = ?1",
            params![hash],
        )?;
        Ok(())
    }

    #[tracing::instrument(skip(self, new_decision))]
    fn rekey(&self, old_hash: &str, new_decision: &Decision) -> Result<(), StorageError> {
        let conn = lock_conn(&self.conn)?;

        let examples_json = serde_json::to_string(&new_decision.examples)
            .map_err(|e| StorageError::SerializationError(e.to_string()))?;

        // Atomic DELETE + INSERT inside a single transaction so a crash
        // between them cannot lose the row. Sibling repos use
        // `unchecked_transaction` against the shared MutexGuard for the
        // same reason — the Mutex already serialises writers within a
        // process, so we rely on it for the "no nested transaction"
        // invariant the unchecked variant assumes.
        let tx = conn.unchecked_transaction()?;

        tx.execute(
            "DELETE FROM decisions WHERE description_hash = ?1",
            params![old_hash],
        )?;

        // Plain INSERT (not UPSERT): if the new PK already exists, surface
        // it as a UNIQUE constraint failure so the caller can return a
        // domain-specific error instead of silently clobbering the row.
        tx.execute(
            "INSERT INTO decisions (
                 description_hash, description, state, nature, weight,
                 category, reason, examples, decided_on_branch,
                 decided_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                new_decision.description_hash,
                new_decision.description,
                new_decision.state.as_sql_str(),
                new_decision.nature.as_sql_str(),
                new_decision.weight.as_sql_str(),
                new_decision.category,
                new_decision.reason,
                examples_json,
                new_decision.decided_on_branch.0,
                new_decision.decided_at,
                new_decision.updated_at,
            ],
        )?;

        tx.commit()?;
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    fn count_by_state(&self, state: DecisionState) -> Result<usize, StorageError> {
        let conn = lock_conn(&self.conn)?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM decisions WHERE state = ?1",
            params![state.as_sql_str()],
            |row| row.get(0),
        )?;
        // SQLite COUNT(*) returns i64; on 32-bit targets `as usize`
        // would silently truncate above 2^31. usize::try_from surfaces
        // the overflow as a typed StorageError.
        usize::try_from(count).map_err(|e| {
            StorageError::QueryError(format!(
                "decisions count {count} overflows usize on this target: {e}"
            ))
        })
    }

    #[tracing::instrument(skip(self))]
    fn list(&self) -> Result<Vec<Decision>, StorageError> {
        let conn = lock_conn(&self.conn)?;
        let sql = format!("SELECT {SELECT_COLUMNS} FROM decisions ORDER BY decided_at DESC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], row_to_decision)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    #[tracing::instrument(skip(self))]
    fn list_by_state(&self, state: DecisionState) -> Result<Vec<Decision>, StorageError> {
        let conn = lock_conn(&self.conn)?;
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM decisions WHERE state = ?1 ORDER BY decided_at DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![state.as_sql_str()], row_to_decision)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }
}

/// Map a row to a [`Decision`].
///
/// The outer `rusqlite::Result` carries column-extraction errors; the inner
/// `Result<Decision, StorageError>` carries enum-parse and JSON-decode errors
/// (so callers see a typed [`StorageError`] for those rather than an opaque
/// `rusqlite::Error`).
fn row_to_decision(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<Decision, StorageError>> {
    let description_hash: String = row.get(0)?;
    let description: String = row.get(1)?;
    let state_s: String = row.get(2)?;
    let nature_s: String = row.get(3)?;
    let weight_s: String = row.get(4)?;
    let category: Option<String> = row.get(5)?;
    let reason: Option<String> = row.get(6)?;
    let examples_s: Option<String> = row.get(7)?;
    let decided_on_branch_s: String = row.get(8)?;
    let decided_at: i64 = row.get(9)?;
    let updated_at: i64 = row.get(10)?;

    Ok((|| {
        let state = DecisionState::from_sql_str(&state_s)?;
        let nature = DecisionNature::from_sql_str(&nature_s)?;
        let weight = DecisionWeight::from_sql_str(&weight_s)?;
        let examples: Vec<ExampleEvidence> = match examples_s {
            Some(s) if !s.is_empty() => serde_json::from_str(&s)
                .map_err(|e| StorageError::SerializationError(e.to_string()))?,
            _ => Vec::new(),
        };
        Ok(Decision {
            description_hash,
            description,
            state,
            nature,
            weight,
            category,
            reason,
            examples,
            decided_on_branch: BranchId(decided_on_branch_s),
            decided_at,
            updated_at,
        })
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    fn test_repo() -> SqliteDecisionRepository {
        let db = Database::open(":memory:").expect("in-memory DB");
        SqliteDecisionRepository::new(db.connection().clone())
    }

    fn make_decision(hash: &str, state: DecisionState) -> Decision {
        Decision {
            description_hash: hash.to_string(),
            description: format!("desc for {hash}"),
            state,
            nature: DecisionNature::Convention,
            weight: DecisionWeight::Rule,
            category: Some("logging".to_string()),
            reason: Some("because tests".to_string()),
            examples: vec![ExampleEvidence {
                file: "src/lib.rs".to_string(),
                line: 1,
                end_line: 3,
                snippet: "tracing::info!()".to_string(),
            }],
            decided_on_branch: BranchId("main".to_string()),
            decided_at: 1_700_000_000,
            updated_at: 1_700_000_000,
        }
    }

    #[test]
    fn empty_table_lookups_return_none_or_empty() {
        let repo = test_repo();

        assert!(repo.get_by_hash("missing").unwrap().is_none());
        assert!(repo.list().unwrap().is_empty());
        assert!(
            repo.list_by_state(DecisionState::Approved)
                .unwrap()
                .is_empty()
        );
        assert_eq!(repo.count_by_state(DecisionState::Approved).unwrap(), 0);
        assert!(repo.get_by_hashes(&["a", "b"]).unwrap().is_empty());
        assert!(repo.get_by_hashes(&[]).unwrap().is_empty());
    }

    #[test]
    fn upsert_and_get_round_trip() {
        let repo = test_repo();
        let d = make_decision("abc12345", DecisionState::Approved);

        repo.upsert(&d).expect("upsert");

        let fetched = repo.get_by_hash("abc12345").unwrap().expect("row exists");
        assert_eq!(fetched, d);
    }

    #[test]
    fn upsert_replaces_on_conflict() {
        let repo = test_repo();
        let mut d = make_decision("hashX", DecisionState::Approved);
        repo.upsert(&d).unwrap();

        // Mutate every field except the PK and re-upsert.
        d.description = "updated".to_string();
        d.state = DecisionState::Rejected;
        d.nature = DecisionNature::Decision;
        d.weight = DecisionWeight::Strong;
        d.category = None;
        d.reason = Some("reconsidered".to_string());
        d.examples.clear();
        d.decided_on_branch = BranchId("feature".to_string());
        d.decided_at = 1_700_001_000;
        d.updated_at = 1_700_001_000;

        repo.upsert(&d).unwrap();

        let fetched = repo.get_by_hash("hashX").unwrap().expect("row exists");
        assert_eq!(fetched, d);

        // Still only one row total.
        assert_eq!(repo.list().unwrap().len(), 1);
    }

    #[test]
    fn get_by_hashes_mixed_found_and_missing() {
        let repo = test_repo();
        repo.upsert(&make_decision("h1", DecisionState::Approved))
            .unwrap();
        repo.upsert(&make_decision("h2", DecisionState::Recorded))
            .unwrap();

        let lookup = repo.get_by_hashes(&["h1", "h2", "missing"]).unwrap();
        assert_eq!(lookup.len(), 2);
        assert!(lookup.contains_key("h1"));
        assert!(lookup.contains_key("h2"));
        assert!(!lookup.contains_key("missing"));

        assert_eq!(lookup.get("h1").unwrap().state, DecisionState::Approved);
        assert_eq!(lookup.get("h2").unwrap().state, DecisionState::Recorded);
    }

    #[test]
    fn get_by_hashes_chunks_above_limit() {
        // Verifies the chunk-on-IN logic actually iterates more than once
        // and still returns the correct rows. We insert HASH_BULK_CHUNK_SIZE+5
        // rows and ask for all of them.
        let repo = test_repo();

        let total = HASH_BULK_CHUNK_SIZE + 5;
        let hashes: Vec<String> = (0..total).map(|i| format!("h{i:06}")).collect();
        for h in &hashes {
            repo.upsert(&make_decision(h, DecisionState::Approved))
                .unwrap();
        }

        let refs: Vec<&str> = hashes.iter().map(String::as_str).collect();
        let lookup = repo.get_by_hashes(&refs).unwrap();

        assert_eq!(lookup.len(), total);
        for h in &hashes {
            assert!(lookup.contains_key(h), "missing {h}");
        }
    }

    #[test]
    fn count_by_state_filters_correctly() {
        let repo = test_repo();
        repo.upsert(&make_decision("a", DecisionState::Approved))
            .unwrap();
        repo.upsert(&make_decision("b", DecisionState::Approved))
            .unwrap();
        repo.upsert(&make_decision("c", DecisionState::Rejected))
            .unwrap();
        repo.upsert(&make_decision("d", DecisionState::Recorded))
            .unwrap();

        assert_eq!(repo.count_by_state(DecisionState::Approved).unwrap(), 2);
        assert_eq!(repo.count_by_state(DecisionState::Rejected).unwrap(), 1);
        assert_eq!(repo.count_by_state(DecisionState::Recorded).unwrap(), 1);
        assert_eq!(repo.count_by_state(DecisionState::Partial).unwrap(), 0);
    }

    #[test]
    fn delete_is_idempotent() {
        let repo = test_repo();
        repo.upsert(&make_decision("zz", DecisionState::Approved))
            .unwrap();

        // First delete removes the row.
        repo.delete("zz").expect("first delete");
        assert!(repo.get_by_hash("zz").unwrap().is_none());

        // Second delete on the same (now-missing) hash must still succeed.
        repo.delete("zz").expect("second delete idempotent");
        // Deleting an entirely unknown hash also succeeds.
        repo.delete("never-existed").expect("delete unknown");
    }

    #[test]
    fn rekey_migrates_row_to_new_pk() {
        let repo = test_repo();
        let original = make_decision("oldhash", DecisionState::Approved);
        repo.upsert(&original).unwrap();

        let new_decision = Decision {
            description_hash: "newhash".to_string(),
            description: "rewritten description".to_string(),
            ..original.clone()
        };
        repo.rekey("oldhash", &new_decision).expect("rekey");

        // Old PK is gone; new PK holds the migrated row.
        assert!(repo.get_by_hash("oldhash").unwrap().is_none());
        let fetched = repo
            .get_by_hash("newhash")
            .unwrap()
            .expect("row exists at new PK");
        assert_eq!(fetched.description_hash, "newhash");
        assert_eq!(fetched.description, "rewritten description");
        // Non-PK fields preserved from new_decision.
        assert_eq!(fetched.state, original.state);
        assert_eq!(fetched.nature, original.nature);
    }

    #[test]
    fn rekey_to_colliding_pk_returns_error_and_leaves_both_rows_intact() {
        let repo = test_repo();
        let row_a = make_decision("a", DecisionState::Approved);
        let row_b = make_decision("b", DecisionState::Approved);
        repo.upsert(&row_a).unwrap();
        repo.upsert(&row_b).unwrap();

        let proposed = Decision {
            description_hash: "b".to_string(),
            description: "would clobber b".to_string(),
            ..row_a.clone()
        };
        let result = repo.rekey("a", &proposed);

        assert!(
            result.is_err(),
            "rekey to a PK that already exists must fail loudly"
        );

        // Atomicity: both original rows survive the failed rekey.
        let fetched_a = repo
            .get_by_hash("a")
            .unwrap()
            .expect("row a must still exist after rejected rekey");
        assert_eq!(fetched_a, row_a);
        let fetched_b = repo
            .get_by_hash("b")
            .unwrap()
            .expect("row b must still exist after rejected rekey");
        assert_eq!(fetched_b, row_b);
    }

    #[test]
    fn rekey_when_old_hash_missing_still_inserts_new_row() {
        // Defensive: if the caller asks to rekey a row that no longer
        // exists (raced with a concurrent delete), the DELETE step is a
        // no-op and the INSERT proceeds. The caller is treated as having
        // requested an upsert at the new PK. This matches the wider
        // codebase convention that DELETE is idempotent.
        let repo = test_repo();
        let new_decision = make_decision("fresh", DecisionState::Approved);

        repo.rekey("never-existed", &new_decision).expect("rekey");

        let fetched = repo
            .get_by_hash("fresh")
            .unwrap()
            .expect("new row inserted");
        assert_eq!(fetched, new_decision);
    }

    #[test]
    fn list_orders_by_decided_at_desc() {
        let repo = test_repo();

        let mut older = make_decision("older", DecisionState::Approved);
        older.decided_at = 1_700_000_000;
        let mut middle = make_decision("middle", DecisionState::Approved);
        middle.decided_at = 1_700_000_500;
        let mut newer = make_decision("newer", DecisionState::Approved);
        newer.decided_at = 1_700_001_000;

        // Insert out of order to make sure ORDER BY (not insertion order) is used.
        repo.upsert(&middle).unwrap();
        repo.upsert(&older).unwrap();
        repo.upsert(&newer).unwrap();

        let list = repo.list().unwrap();
        let hashes: Vec<&str> = list.iter().map(|d| d.description_hash.as_str()).collect();
        assert_eq!(hashes, vec!["newer", "middle", "older"]);
    }

    #[test]
    fn list_by_state_filters_and_orders() {
        let repo = test_repo();

        let mut a = make_decision("a", DecisionState::Approved);
        a.decided_at = 1_700_000_001;
        let mut b = make_decision("b", DecisionState::Rejected);
        b.decided_at = 1_700_000_002;
        let mut c = make_decision("c", DecisionState::Approved);
        c.decided_at = 1_700_000_003;

        repo.upsert(&a).unwrap();
        repo.upsert(&b).unwrap();
        repo.upsert(&c).unwrap();

        let approved = repo.list_by_state(DecisionState::Approved).unwrap();
        assert_eq!(approved.len(), 2);
        // Newest first.
        assert_eq!(approved[0].description_hash, "c");
        assert_eq!(approved[1].description_hash, "a");

        let rejected = repo.list_by_state(DecisionState::Rejected).unwrap();
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].description_hash, "b");

        assert!(
            repo.list_by_state(DecisionState::Partial)
                .unwrap()
                .is_empty()
        );
    }

    /// Read the `detail` column from an `EXPLAIN QUERY PLAN` row by NAME
    /// rather than by index. SQLite has historically kept the EQP output
    /// at columns `(id, parent, notused, detail)` but pinning index 3 is
    /// brittle if a future SQLite version reorders or adds a column.
    /// Looking the index up via `column_names()` makes the test resilient.
    fn explain_plan_detail(conn: &rusqlite::Connection, sql: &str) -> Vec<String> {
        let eqp = format!("EXPLAIN QUERY PLAN {sql}");
        let mut stmt = conn.prepare(&eqp).expect("prepare EQP");
        let detail_col = stmt
            .column_names()
            .iter()
            .position(|n| *n == "detail")
            .unwrap_or_else(|| {
                panic!(
                    "EQP must expose a `detail` column; got: {:?}",
                    stmt.column_names()
                )
            });

        let rows = stmt
            .query_map([], |row| row.get::<_, String>(detail_col))
            .expect("query EQP");
        rows.filter_map(|r| r.ok()).collect()
    }

    #[test]
    fn get_by_hash_uses_index_not_scan() {
        // EXPLAIN QUERY PLAN must report a SEARCH on the description_hash
        // primary-key index, never a full SCAN. If the schema regresses
        // (e.g. PK dropped or column renamed), this test fails fast.
        let repo = test_repo();
        let conn = lock_conn(&repo.conn).unwrap();

        let plan = explain_plan_detail(
            &conn,
            "SELECT description_hash FROM decisions WHERE description_hash = 'any'",
        )
        .join("\n");

        assert!(
            plan.contains("SEARCH") && !plan.contains("SCAN"),
            "expected indexed SEARCH, got plan: {plan}"
        );
    }

    #[test]
    fn secondary_indexes_exist() {
        // P6: V12 schema declares two secondary indexes (idx_decisions_state,
        // idx_decisions_decided_on_branch). A migration regression that drops
        // either is invisible to the existing PK-only EQP test, so probe
        // sqlite_master directly. The PK index is auto-named
        // `sqlite_autoindex_decisions_1` and intentionally NOT asserted —
        // dropping the PK would already break get_by_hash_uses_index_not_scan.
        let repo = test_repo();
        let conn = lock_conn(&repo.conn).unwrap();

        let names: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='decisions'")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        for required in ["idx_decisions_state", "idx_decisions_decided_on_branch"] {
            assert!(
                names.iter().any(|n| n == required),
                "V12 schema must define {required}; existing indexes on `decisions`: {names:?}"
            );
        }
    }

    #[test]
    fn count_by_state_uses_index_not_scan() {
        // P6: count_by_state filters by `state`, which the schema indexes
        // via idx_decisions_state. Confirm the planner chooses that index
        // — otherwise large decisions tables degrade to O(N) per call.
        let repo = test_repo();
        let conn = lock_conn(&repo.conn).unwrap();

        let plan = explain_plan_detail(
            &conn,
            "SELECT COUNT(*) FROM decisions WHERE state = 'approved'",
        )
        .join("\n");

        // Planner choice on an empty table can fall back to scan, so seed
        // a row first so EQP has a realistic dataset to plan against.
        // (Re-running on a populated repo for a more honest signal.)
        drop(conn);
        let _ = repo.upsert(&make_decision("any", DecisionState::Approved));
        let conn = lock_conn(&repo.conn).unwrap();
        let plan_populated = explain_plan_detail(
            &conn,
            "SELECT COUNT(*) FROM decisions WHERE state = 'approved'",
        )
        .join("\n");

        assert!(
            plan_populated.contains("idx_decisions_state"),
            "count_by_state must use idx_decisions_state; \
             empty-table plan: {plan}; populated-table plan: {plan_populated}"
        );
    }

    #[test]
    fn enum_sql_round_trips() {
        // Defensive: catches accidental drift between SQL CHECK constraints
        // and the enum mappings. If a new variant is added without a SQL
        // string, this fails.
        for s in [
            DecisionState::Approved,
            DecisionState::Rejected,
            DecisionState::Partial,
            DecisionState::Recorded,
        ] {
            assert_eq!(DecisionState::from_sql_str(s.as_sql_str()).unwrap(), s);
        }
        for n in [
            DecisionNature::Convention,
            DecisionNature::Decision,
            DecisionNature::Preference,
            DecisionNature::Fact,
        ] {
            assert_eq!(DecisionNature::from_sql_str(n.as_sql_str()).unwrap(), n);
        }
        for w in [DecisionWeight::Rule, DecisionWeight::Strong] {
            assert_eq!(DecisionWeight::from_sql_str(w.as_sql_str()).unwrap(), w);
        }

        assert!(DecisionState::from_sql_str("bogus").is_err());
        assert!(DecisionNature::from_sql_str("bogus").is_err());
        assert!(DecisionWeight::from_sql_str("bogus").is_err());
    }

    #[test]
    fn examples_serialise_as_json_array() {
        let repo = test_repo();
        let mut d = make_decision("ex", DecisionState::Recorded);
        d.examples = vec![
            ExampleEvidence {
                file: "a.rs".to_string(),
                line: 1,
                end_line: 1,
                snippet: "x".to_string(),
            },
            ExampleEvidence {
                file: "b.rs".to_string(),
                line: 10,
                end_line: 12,
                snippet: "y".to_string(),
            },
        ];
        repo.upsert(&d).unwrap();

        let fetched = repo.get_by_hash("ex").unwrap().unwrap();
        assert_eq!(fetched.examples.len(), 2);
        assert_eq!(fetched.examples[0].file, "a.rs");
        assert_eq!(fetched.examples[1].line, 10);
    }

    #[test]
    fn nullable_columns_round_trip() {
        let repo = test_repo();
        let mut d = make_decision("none-fields", DecisionState::Approved);
        d.category = None;
        d.reason = None;
        d.examples = Vec::new();
        repo.upsert(&d).unwrap();

        let fetched = repo.get_by_hash("none-fields").unwrap().unwrap();
        assert!(fetched.category.is_none());
        assert!(fetched.reason.is_none());
        assert!(fetched.examples.is_empty());
    }
}
