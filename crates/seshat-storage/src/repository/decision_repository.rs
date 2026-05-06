//! SQLite implementation of [`DecisionRepository`].
//!
//! Project-wide store for user-recorded decisions, keyed by
//! `description_hash`. Backs the V12 `decisions` table.
//!
//! Skeleton only — method bodies are filled in during US-002.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::DecisionRepository;
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

/// Nature of a recorded decision (mirrors `KnowledgeNature` for the
/// subset of values valid in the `decisions.nature` CHECK constraint).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DecisionNature {
    Convention,
    Decision,
    Preference,
    Fact,
}

/// Weight (severity) of a recorded decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DecisionWeight {
    Rule,
    Strong,
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

impl DecisionRepository for SqliteDecisionRepository {
    fn upsert(&self, _decision: &Decision) -> Result<(), StorageError> {
        let _ = &self.conn;
        unimplemented!("US-002: SqliteDecisionRepository::upsert")
    }

    fn get_by_hash(&self, _hash: &str) -> Result<Option<Decision>, StorageError> {
        unimplemented!("US-002: SqliteDecisionRepository::get_by_hash")
    }

    fn get_by_hashes(&self, _hashes: &[&str]) -> Result<HashMap<String, Decision>, StorageError> {
        unimplemented!("US-002: SqliteDecisionRepository::get_by_hashes")
    }

    fn delete(&self, _hash: &str) -> Result<(), StorageError> {
        unimplemented!("US-002: SqliteDecisionRepository::delete")
    }

    fn count_by_state(&self, _state: DecisionState) -> Result<usize, StorageError> {
        unimplemented!("US-002: SqliteDecisionRepository::count_by_state")
    }

    fn list(&self) -> Result<Vec<Decision>, StorageError> {
        unimplemented!("US-002: SqliteDecisionRepository::list")
    }

    fn list_by_state(&self, _state: DecisionState) -> Result<Vec<Decision>, StorageError> {
        unimplemented!("US-002: SqliteDecisionRepository::list_by_state")
    }
}
