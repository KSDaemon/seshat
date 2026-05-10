//! # Seshat Graph
//!
//! Knowledge graph intelligence layer. All query logic, duplicate detection,
//! and graduated response generation lives here. The MCP crate calls into
//! this crate — graph is the brain, MCP is the mouth.
//!
//! Responsibilities:
//! - `query_project_context` — project overview with languages, modules,
//!   dependencies
//! - `query_convention` — convention lookup by topic with FTS5
//! - `query_code_pattern` — code pattern search (FTS5 + optional vector)
//! - `validate_approach` — graduated response with verdict, summary,
//!   and categorized findings
//! - `query_dependencies` — dependency analysis with blast radius
//! - Convention aggregate recalculation (warm tier)
//! - Cross-reference code conventions vs documentation
//! - LRU cache for IR and frequent queries

use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::Connection;

pub mod code_pattern;
pub mod conventions;
pub mod cross_reference;
pub mod decisions;
pub mod dependencies;
pub mod detection;
pub mod diff_impact;
pub mod error;
pub mod fts;
pub mod golden_files;
pub mod project_context;
pub mod validate_approach;

/// Value for `ext_data.source` when convention was auto-detected by scan.
pub const SOURCE_AUTO_DETECTED: &str = "auto_detected";
/// Value for `ext_data.source` when decision was recorded by user/agent.
pub const SOURCE_USER: &str = "user";

/// SQL WHERE clause fragment to filter out soft-deleted decisions.
pub const SQL_NOT_REMOVED: &str =
    "COALESCE(json_extract(ext_data, '$.removed'), 0) NOT IN (1, 'true')";

pub use code_pattern::{
    CodePatternData, PatternResult, cosine_similarity, query_code_pattern,
    query_code_pattern_with_embeddings,
};
pub use conventions::{
    AdoptionInfo, ConventionResult, EvidenceExample, QueryConventionData, query_convention,
};
pub use cross_reference::{
    CrossReferenceConfig, CrossReferenceResult, ReinforcedNode, cross_reference,
};
pub use decisions::{
    RecordDecisionData, RecordDecisionParams, RemoveDecisionData, RemoveDecisionParams,
    UpdateDecisionData, UpdateDecisionParams, compute_description_hash, record_decision,
    remove_decision, update_decision,
};
pub use dependencies::{
    BlastRadius, DEFAULT_TRANSITIVE_DEPTH, DependencyData, DependencyEntry, DependentEntry,
    ExternalDependency, MAX_TRANSITIVE_DEPTH, QueryDependenciesOptions, query_dependencies,
    query_dependencies_batch,
};
pub use detection::{DetectionReport, convention_to_node, persist_and_index, run_detection_cycle};
pub use diff_impact::{
    AdoptionSummary, AffectedSymbol, BlastRadiusSummary, ChangedFile, ConventionRisk, DependentRef,
    DiffImpactData, DiffImpactRequest, FileStatus, ImpactMetadata, map_diff_impact,
};
pub use error::GraphError;
pub use fts::{delete_fts_entry, insert_fts_entry, rebuild_fts_index, search_conventions};
pub use golden_files::{DEFAULT_GOLDEN_FILES_LIMIT, GoldenFile, get_golden_files};
pub use project_context::{
    ConfidenceSummary, DependencyInfo, DomainDependency, LanguageInfo, ModuleInfo,
    ProjectContextData, query_project_context,
};
pub use validate_approach::{
    Contradiction, DecisionEntry, DuplicatePattern, ObservationEntry, RuleViolation,
    ValidateApproachData, ValidateApproachParams, validate_approach,
};

/// Acquire the database connection lock, mapping poison errors to `GraphError`.
pub fn lock_conn(conn: &Arc<Mutex<Connection>>) -> Result<MutexGuard<'_, Connection>, GraphError> {
    conn.lock().map_err(|e| {
        GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
            "Failed to acquire connection lock: {e}"
        )))
    })
}

// ── Shared test helpers ──────────────────────────────────────

#[cfg(test)]
pub(crate) mod test_helpers {
    use std::sync::{Arc, Mutex};

    use rusqlite::{Connection, params};
    use seshat_core::ProjectFile;
    use seshat_storage::Database;

    /// Open an in-memory database and return its connection.
    ///
    /// The database has all migrations applied (nodes, files_ir, FTS5, etc.).
    pub fn test_conn() -> Arc<Mutex<Connection>> {
        let db = Database::open(":memory:").expect("in-memory DB");
        db.connection().clone()
    }

    /// Insert a serialized IR file into the database for a branch.
    pub fn insert_ir(conn: &Arc<Mutex<Connection>>, branch_id: &str, file: &ProjectFile) {
        let c = conn.lock().unwrap();
        let ir_data = seshat_storage::serialize_ir(file).expect("serialize IR");
        let file_path = file.path.to_string_lossy();
        c.execute(
            "INSERT INTO files_ir (branch_id, file_path, language, content_hash, ir_data)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                branch_id,
                file_path.as_ref(),
                file.language.as_str(),
                file.content_hash,
                ir_data,
            ],
        )
        .expect("insert IR");
    }

    /// Insert a convention (or decision/observation) node into the database.
    ///
    /// Flexible helper that supports different `nature` values (`"convention"`,
    /// `"decision"`, `"observation"`) and returns the row id.
    pub fn insert_convention_node(
        conn: &Arc<Mutex<Connection>>,
        branch_id: &str,
        description: &str,
        weight: &str,
        confidence: f64,
        nature: &str,
    ) -> i64 {
        let c = conn.lock().unwrap();
        let ext = serde_json::json!({
            "source": if nature == "decision" { "user" } else { "auto_detected" },
            "detector_name": "test",
            "trend": "stable",
            "evidence": [{
                "file": "src/main.rs",
                "line": 10,
                "end_line": 15,
                "snippet": "example snippet"
            }]
        });
        c.execute(
            "INSERT INTO nodes (branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data)
             VALUES (?1, ?2, ?3, ?4, 9, 10, ?5, ?6)",
            params![branch_id, nature, weight, confidence, description, ext.to_string()],
        )
        .unwrap();
        c.last_insert_rowid()
    }
}
