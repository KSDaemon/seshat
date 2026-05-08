//! Convention detection pipeline — shared between the scan command and the
//! warm-tier watcher.
//!
//! This module exists to eliminate the copy-paste that previously lived in
//! both `seshat-cli/src/scan.rs` and `seshat-watcher/src/warm_tier.rs`.
//! Both callers now call into this single implementation.
//!
//! # Pipeline
//!
//! ```text
//! load files_ir from DB
//!   → run_all_detectors (rayon, CPU-bound)
//!   → aggregate_findings (confidence, trend, adoption)
//!   → persist_conventions (delete auto-detected → insert fresh nodes)
//!   → update_convention_compliance_counts
//!   → rebuild_fts_index
//! ```
//!
//! The entire persist step runs inside a single SQLite transaction so a
//! partial failure leaves the nodes table intact.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use seshat_core::{BranchId, DetectionConfig, KnowledgeNode, NodeId};
use seshat_detectors::{
    AggregatedConvention, ProjectContext, aggregate_findings, run_all_detectors,
};
use seshat_storage::{FileIRRepository, SqliteFileIRRepository};
use tracing::info;

use crate::error::GraphError;
use crate::{SOURCE_AUTO_DETECTED, compute_description_hash, rebuild_fts_index};

// ── Public API ────────────────────────────────────────────────────────────────

/// Result of a successful detection cycle.
#[derive(Debug, Clone, Copy)]
pub struct DetectionReport {
    /// Number of source files that were analysed.
    pub file_count: usize,
    /// Number of distinct convention nodes persisted.
    pub convention_count: usize,
}

/// Run the full convention-detection pipeline on the given connection.
///
/// # Arguments
///
/// * `conn` — shared database connection (holds all IR and nodes).
/// * `branch_id` — branch to operate on (currently always `"main"`).
/// * `detection_config` — thresholds, weights, and detector settings.
/// * `file_dates` — optional map of `file_path → last_commit_unix_ts`
///   used for trend computation.  Pass an empty map when git dates are
///   unavailable (e.g. warm-tier incremental runs).
/// * `source_map` — map of `file_path → source content` for files whose
///   source is available in memory.  Detectors use this to produce real
///   code snippets in evidence instead of empty strings.  Pass an empty
///   map when source is not available (e.g. warm-tier recalculation where
///   only changed files are watched; snippets for those files are extracted
///   in the hot-tier pass).
///
/// # Errors
///
/// Returns `GraphError` on any database or serialisation failure.
/// The persist step is transactional: a failure rolls back the entire
/// node replacement, leaving the previous state intact.
pub fn run_detection_cycle(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &BranchId,
    detection_config: &DetectionConfig,
    file_dates: &HashMap<String, Option<i64>>,
    source_map: &HashMap<std::path::PathBuf, String>,
) -> Result<DetectionReport, GraphError> {
    // 1. Load all parsed files from the DB (current IR schema version only).
    let file_ir_repo = SqliteFileIRRepository::new(conn.clone());
    let all_files = file_ir_repo
        .get_by_branch(branch_id)
        .map_err(GraphError::Storage)?;

    let file_count = all_files.len();

    if all_files.is_empty() {
        return Ok(DetectionReport {
            file_count: 0,
            convention_count: 0,
        });
    }

    // 2. Build the cross-cutting project context once.
    //    Holds the project-internal name set used by Phase 3 of the
    //    pipeline; previously rebuilt on every `run_all_detectors`
    //    call. Constructing once per scan keeps the warm-tier cycle
    //    cheap.
    let project_context = ProjectContext::from_files(&all_files);

    // 3. Run all detectors in parallel (rayon).
    // When source_map is non-empty (scan path), detectors use detect_with_source
    // and produce real code snippets in evidence.  When source_map is empty
    // (warm-tier watcher path), detectors fall back to IR-only detection.
    let detector_results = run_all_detectors(
        &all_files,
        source_map,
        detection_config,
        &project_context,
        None,
    );
    let findings: Vec<seshat_core::ConventionFinding> = detector_results
        .into_iter()
        .flat_map(|r| r.findings)
        .collect();

    // 3. Aggregate findings into convention nodes.
    let now = chrono::Utc::now().timestamp();
    let aggregated = aggregate_findings(&findings, detection_config, file_dates, now);
    let convention_count = aggregated.len();

    // 4. Persist: delete old auto-detected nodes + insert fresh ones, all in
    //    a single transaction so a partial failure leaves the table intact.
    persist_conventions(conn, branch_id, &aggregated)?;

    // 5. Update per-file compliance counts (outside the main transaction —
    //    idempotent and non-critical; warm tier will retry on next cycle).
    update_compliance_counts(conn, branch_id, &findings)?;

    // 6. Rebuild FTS5 index.
    rebuild_fts_index(conn).map_err(|e| {
        GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
            "rebuild FTS: {e}"
        )))
    })?;

    info!(
        files = file_count,
        conventions = convention_count,
        "Detection cycle complete"
    );

    Ok(DetectionReport {
        file_count,
        convention_count,
    })
}

/// Classify a convention by detector name and description into a category.
///
/// Used to distinguish different kinds of findings produced by the same detector
/// (e.g. `dependency_usage` emits both dependency declarations and wrapper patterns).
fn classify_finding_category(detector_name: &str, description: &str) -> String {
    if detector_name != "dependency_usage" {
        return String::new();
    }

    if description.starts_with("Wrapper module for ") {
        "wrapper_declaration".to_owned()
    } else if description.starts_with("Use ") && description.contains(" for ") {
        "wrapper_violation".to_owned()
    } else if (description.starts_with("Canonical ") && description.contains(" library: "))
        || (description.starts_with("Likely ") && description.contains(" library (heuristic): "))
    {
        "dependency".to_owned()
    } else {
        String::new()
    }
}

/// Convert an [`AggregatedConvention`] to a [`KnowledgeNode`] for storage.
///
/// The `ext_data` JSON contains:
/// - `source`: `"auto_detected"` (distinguishes from user decisions)
/// - `detector_name`: which detector produced this
/// - `trend`: rising / stable / declining / unknown
/// - `adoption_rate`: confidence as a float
/// - `evidence`: `[{file, line, end_line, snippet}]`
///
/// The `snippet` field is stored as a plain string.  Callers read it via
/// `extract_evidence` which calls `truncate_snippet` on the raw value, so
/// truncation state is always recomputed at read time — there is no need to
/// persist it.  (Earlier versions stored `{"content": ..., "truncated": false}`;
/// `extract_evidence` still handles that legacy format for existing DB rows.)
pub fn convention_to_node(
    convention: &AggregatedConvention,
    branch_id: &BranchId,
) -> KnowledgeNode {
    let evidence_json: Vec<serde_json::Value> = convention
        .evidence
        .iter()
        .map(|e| {
            serde_json::json!({
                "file": e.file.display().to_string(),
                "line": e.line,
                "end_line": e.end_line,
                "snippet": e.snippet,
                "snippet_start_line": e.snippet_start_line,
            })
        })
        .collect();

    let mut ext_data = convention
        .ext_data(None)
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    ext_data.insert(
        "source".to_owned(),
        serde_json::Value::String(SOURCE_AUTO_DETECTED.to_owned()),
    );
    ext_data.insert(
        "detector_name".to_owned(),
        serde_json::Value::String(convention.detector_name.clone()),
    );
    ext_data.insert(
        "evidence".to_owned(),
        serde_json::Value::Array(evidence_json),
    );

    ext_data.insert(
        "finding_category".to_owned(),
        serde_json::Value::String(classify_finding_category(
            &convention.detector_name,
            &convention.description,
        )),
    );

    KnowledgeNode {
        id: NodeId(0), // auto-assigned by DB
        branch_id: branch_id.clone(),
        nature: convention.nature,
        weight: convention.weight,
        confidence: convention.confidence,
        adoption_count: convention.adoption_count,
        total_count: convention.total_count,
        description: convention.description.clone(),
        ext_data: Some(serde_json::Value::Object(ext_data)),
    }
}

/// Persist aggregated conventions and rebuild search indices without re-running
/// detection.
///
/// Use this when the caller has already run detection (e.g., the scan command
/// runs detection with a progress spinner) and only needs to persist the
/// results.  For a full end-to-end cycle use [`run_detection_cycle`] instead.
pub fn persist_and_index(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &BranchId,
    aggregated: &[AggregatedConvention],
    findings: &[seshat_core::ConventionFinding],
) -> Result<(), GraphError> {
    persist_conventions(conn, branch_id, aggregated)?;
    update_compliance_counts(conn, branch_id, findings)?;
    rebuild_fts_index(conn).map_err(|e| {
        GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
            "rebuild FTS: {e}"
        )))
    })?;
    Ok(())
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Atomically replace all auto-detected convention nodes for a branch.
///
/// Runs DELETE + INSERT inside a single `BEGIN … COMMIT` transaction.
/// On any error the transaction is rolled back and the previous node set
/// remains intact.
///
/// Decision-aware skip: any aggregated convention whose `description_hash`
/// matches a row in the project-wide `decisions` table (any state) is
/// skipped at INSERT time. The matching set is bulk-fetched in a single
/// chunked SELECT before the transaction begins to avoid the N+1 query
/// pattern this function used to have against the `nodes` table.
fn persist_conventions(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &BranchId,
    aggregated: &[AggregatedConvention],
) -> Result<(), GraphError> {
    let hashes: Vec<String> = aggregated
        .iter()
        .map(|c| compute_description_hash(&c.description))
        .collect();

    let guard = crate::lock_conn(conn)?;

    // BEGIN IMMEDIATE up-front to acquire the SQLite write lock right
    // away. The decided-hash fetch and the DELETE+INSERT loop now run
    // under a single transaction, closing the TOCTOU race where a
    // concurrent record_decision could land a new decision between the
    // fetch and the loop, leading us to insert a node whose decision
    // already exists.
    guard.execute_batch("BEGIN IMMEDIATE").map_err(|e| {
        GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
            "BEGIN IMMEDIATE: {e}"
        )))
    })?;

    let decided_hashes = match bulk_fetch_decided_hashes_locked(&guard, &hashes) {
        Ok(set) => set,
        Err(e) => {
            let _ = guard.execute_batch("ROLLBACK");
            return Err(e);
        }
    };

    let del = guard.execute(
        "DELETE FROM nodes
         WHERE branch_id = ?1
           AND json_extract(ext_data, '$.source') = 'auto_detected'",
        rusqlite::params![branch_id.0],
    );
    if let Err(e) = del {
        let _ = guard.execute_batch("ROLLBACK");
        return Err(GraphError::Storage(
            seshat_storage::StorageError::QueryError(format!("delete conventions: {e}")),
        ));
    }

    let mut inserted_count = 0usize;

    for (convention, description_hash) in aggregated.iter().zip(hashes.iter()) {
        if decided_hashes.contains(description_hash) {
            tracing::debug!(
                description = %convention.description,
                hash = %description_hash,
                "Skipping auto-detected convention: matching decision exists"
            );
            continue;
        }

        let node = convention_to_node(convention, branch_id);
        let ext = node.ext_data.as_ref().map(|v| v.to_string());

        let ins = guard.execute(
            "INSERT INTO nodes
              (branch_id, nature, weight, confidence,
               adoption_count, total_count, description, ext_data, description_hash)
              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                node.branch_id.0,
                node.nature.as_str(),
                node.weight.as_str(),
                node.confidence,
                node.adoption_count,
                node.total_count,
                node.description,
                ext,
                description_hash,
            ],
        );
        if let Err(e) = ins {
            let _ = guard.execute_batch("ROLLBACK");
            return Err(GraphError::Storage(
                seshat_storage::StorageError::QueryError(format!("insert convention: {e}")),
            ));
        }
        inserted_count += 1;
    }

    guard.execute_batch("COMMIT").map_err(|e| {
        GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
            "COMMIT: {e}"
        )))
    })?;

    info!(
        inserted = inserted_count,
        skipped = aggregated.len().saturating_sub(inserted_count),
        "Persisted convention nodes"
    );
    Ok(())
}

/// Bulk-fetch the subset of `description_hash` values that have a matching
/// row in the `decisions` table — operating on an already-locked
/// connection so the fetch and the consumer's INSERT loop share one
/// transaction (closes the TOCTOU race where a concurrent record_decision
/// could land between them).
///
/// Chunks the IN-clause at 500 hashes, matching SqliteDecisionRepository's
/// internal limit (HASH_BULK_CHUNK_SIZE).
fn bulk_fetch_decided_hashes_locked(
    conn: &Connection,
    hashes: &[String],
) -> Result<HashSet<String>, GraphError> {
    if hashes.is_empty() {
        return Ok(HashSet::new());
    }
    const CHUNK: usize = 500;
    let mut found: HashSet<String> = HashSet::with_capacity(hashes.len());

    for chunk in hashes.chunks(CHUNK) {
        let placeholders = (1..=chunk.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT description_hash FROM decisions WHERE description_hash IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "prepare bulk decided-hashes: {e}"
            )))
        })?;
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|h| h as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt
            .query_map(params.as_slice(), |row| row.get::<_, String>(0))
            .map_err(|e| {
                GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                    "query bulk decided-hashes: {e}"
                )))
            })?;
        for h in rows.flatten() {
            found.insert(h);
        }
    }
    Ok(found)
}

/// Compute and write per-file convention-compliance counts.
///
/// Counts `ConventionFinding`s where `follows_convention == true` per file
/// path and writes those counts into `files_ir.convention_compliance_count`.
fn update_compliance_counts(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &BranchId,
    findings: &[seshat_core::ConventionFinding],
) -> Result<(), GraphError> {
    let mut counts: HashMap<String, u32> = HashMap::new();
    for finding in findings {
        if finding.follows_convention {
            let key = finding.file_path.to_string_lossy().to_string();
            *counts.entry(key).or_insert(0) += 1;
        }
    }

    let file_ir_repo = SqliteFileIRRepository::new(conn.clone());
    file_ir_repo
        .update_convention_compliance_counts(branch_id, &counts)
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "update compliance counts: {e}"
            )))
        })?;

    info!(
        files_with_conventions = counts.len(),
        "Updated per-file convention compliance counts"
    );
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::test_helpers::make_project_file;
    use seshat_core::{AnchorKind, Language};
    use seshat_storage::Database;

    fn open_db() -> (Database, Arc<Mutex<Connection>>) {
        let db = Database::open(":memory:").expect("in-memory DB");
        let conn = db.connection().clone();
        (db, conn)
    }

    #[test]
    fn run_detection_cycle_empty_db_returns_zero() {
        let (_db, conn) = open_db();
        let branch = BranchId::from("main");
        let config = DetectionConfig::default();
        let result = run_detection_cycle(&conn, &branch, &config, &HashMap::new(), &HashMap::new());
        assert!(result.is_ok());
        let r = result.unwrap();
        assert_eq!(r.file_count, 0);
        assert_eq!(r.convention_count, 0);
    }

    #[test]
    fn run_detection_cycle_with_files_runs_without_error() {
        let (db, conn) = open_db();
        let branch = BranchId::from("main");

        // Seed a file via the proper upsert path.
        let file = make_project_file(Language::Rust);
        SqliteFileIRRepository::new(conn.clone())
            .upsert(&branch, &file, None)
            .expect("upsert");

        let config = DetectionConfig::default();
        let result = run_detection_cycle(&conn, &branch, &config, &HashMap::new(), &HashMap::new());
        assert!(
            result.is_ok(),
            "detection cycle should not fail: {result:?}"
        );
        let r = result.unwrap();
        assert_eq!(r.file_count, 1);
        drop(db); // keep db alive until here
    }

    #[test]
    fn convention_to_node_sets_source_auto_detected() {
        use seshat_core::{KnowledgeNature, KnowledgeWeight, Trend};
        use seshat_detectors::AggregatedConvention;

        let convention = AggregatedConvention {
            description: "test convention".to_string(),
            detector_name: "test_detector".to_string(),
            nature: KnowledgeNature::Convention,
            weight: KnowledgeWeight::Strong,
            confidence: 0.85,
            adoption_count: 8,
            total_count: 10,
            trend: Trend::Stable,
            evidence: vec![],
        };

        let branch = BranchId::from("main");
        let node = convention_to_node(&convention, &branch);

        let ext = node.ext_data.as_ref().unwrap();
        assert_eq!(ext["source"].as_str().unwrap(), SOURCE_AUTO_DETECTED);
        assert_eq!(ext["detector_name"].as_str().unwrap(), "test_detector");
        assert_eq!(node.confidence, 0.85);
        assert_eq!(node.description, "test convention");
    }

    #[test]
    fn convention_to_node_evidence_uses_file_not_snippet() {
        use seshat_core::{CodeEvidence, KnowledgeNature, KnowledgeWeight, Trend};
        use seshat_detectors::AggregatedConvention;
        use std::path::PathBuf;

        let convention = AggregatedConvention {
            description: "test".to_string(),
            detector_name: "test_detector".to_string(),
            nature: KnowledgeNature::Convention,
            weight: KnowledgeWeight::Strong,
            confidence: 0.9,
            adoption_count: 5,
            total_count: 10,
            trend: Trend::Stable,
            evidence: vec![CodeEvidence {
                file: PathBuf::from("crates/seshat-core/src/lib.rs"),
                line: 42,
                end_line: 44,
                snippet: "pub fn real_code() {}".to_string(),
                snippet_start_line: 0,
                anchor: AnchorKind::CallSite,
            }],
        };

        let branch = BranchId::from("main");
        let node = convention_to_node(&convention, &branch);
        let ext = node.ext_data.as_ref().unwrap();
        let evidence = ext["evidence"].as_array().unwrap();
        assert_eq!(evidence.len(), 1);

        let ev = &evidence[0];
        // "file" must be the real path, NOT the snippet content
        assert_eq!(
            ev["file"].as_str().unwrap(),
            "crates/seshat-core/src/lib.rs"
        );
        // snippet is stored as a plain string (truncation is recomputed at read time)
        assert_eq!(ev["snippet"].as_str().unwrap(), "pub fn real_code() {}");
        // line numbers preserved
        assert_eq!(ev["line"].as_u64().unwrap(), 42);
        assert_eq!(ev["end_line"].as_u64().unwrap(), 44);
    }

    /// Integration regression test: persist_and_index with real source produces
    /// non-empty snippets containing actual source keywords in evidence JSON.
    ///
    /// This test pins the end-to-end contract from scan → detect → persist:
    /// convention nodes stored in the DB must have evidence with real code
    /// snippets, not empty strings or synthetic format!() placeholders.
    #[test]
    fn persist_and_index_with_source_produces_real_snippets() {
        use seshat_core::ir::{DeriveUsage, LanguageIR, RustIR};
        use seshat_core::{DependencyUsage, Import, TypeDef, TypeDefKind};
        use seshat_detectors::{ProjectContext, run_all_detectors};
        use seshat_storage::{
            FileIRRepository, NodeRepository, SqliteFileIRRepository, SqliteNodeRepository,
        };
        use std::path::PathBuf;

        let (_db, conn) = open_db();
        let branch = BranchId::from("main");
        let config = DetectionConfig::default();

        // Build a minimal Rust file with thiserror — should trigger ErrorHandlingDetector.
        let file_path = PathBuf::from("src/errors.rs");
        let source = "use thiserror::Error;\n\n#[derive(Debug, Error)]\npub enum AppError {\n    #[error(\"not found\")]\n    NotFound,\n}\n";

        let project_file = seshat_core::ProjectFile {
            path: file_path.clone(),
            language: seshat_core::Language::Rust,
            content_hash: "abc".to_string(),
            imports: vec![Import {
                module: "thiserror".to_string(),
                names: vec!["Error".to_string()],
                is_type_only: false,
                line: 1,
            }],
            exports: vec![],
            functions: vec![],
            types: vec![TypeDef {
                name: "AppError".to_string(),
                kind: TypeDefKind::Enum,
                is_public: true,
                line: 3,
                doc_comment: None,
            }],
            dependencies_used: vec![DependencyUsage {
                package: "thiserror".to_string(),
                import_path: "thiserror".to_string(),
                line: 1,
            }],
            language_ir: LanguageIR::Rust(RustIR {
                error_types: vec!["AppError".to_string()],
                derive_macros: vec![DeriveUsage {
                    type_name: "AppError".to_string(),
                    derives: vec!["Debug".to_string(), "Error".to_string()],
                    line: 3,
                }],
                ..RustIR::default()
            }),
            file_doc: None,
        };

        // Upsert the file IR into the DB.
        SqliteFileIRRepository::new(conn.clone())
            .upsert(&branch, &project_file, None)
            .expect("upsert file IR");

        // Build source_map with the real source — simulates what orchestrator produces.
        let mut source_map = HashMap::new();
        source_map.insert(file_path.clone(), source.to_string());

        // Run detectors with full source_map.
        let files = vec![project_file];
        let project_context = ProjectContext::from_files(&files);
        let detector_results =
            run_all_detectors(&files, &source_map, &config, &project_context, None);
        let findings: Vec<seshat_core::ConventionFinding> = detector_results
            .into_iter()
            .flat_map(|r| r.findings)
            .collect();

        let now = chrono::Utc::now().timestamp();
        let file_dates = HashMap::new();
        let aggregated = seshat_detectors::aggregate_findings(&findings, &config, &file_dates, now);

        // Must have detected at least one convention.
        assert!(
            !aggregated.is_empty(),
            "should detect at least one convention from thiserror usage"
        );

        // Persist to DB.
        persist_and_index(&conn, &branch, &aggregated, &findings)
            .expect("persist_and_index should succeed");

        // Read back from DB and inspect evidence snippets.
        let node_repo = SqliteNodeRepository::new(conn.clone());
        let nodes = node_repo.find_by_branch(&branch).expect("find nodes");

        let auto_detected: Vec<_> = nodes
            .iter()
            .filter(|n| {
                n.ext_data
                    .as_ref()
                    .and_then(|e| e["source"].as_str())
                    .map(|s| s == "auto_detected")
                    .unwrap_or(false)
            })
            .collect();

        assert!(
            !auto_detected.is_empty(),
            "should have at least one auto-detected convention node"
        );

        // The error_handling node must have evidence with a real code snippet
        // (not a path-based snippet like "Path: src/errors.rs" from file_structure).
        let node_with_snippet = auto_detected.iter().find(|n| {
            let is_error_handling = n
                .ext_data
                .as_ref()
                .and_then(|e| e["detector_name"].as_str())
                .map(|d| d == "error_handling")
                .unwrap_or(false);
            if !is_error_handling {
                return false;
            }
            n.ext_data
                .as_ref()
                .and_then(|e| e["evidence"].as_array())
                .map(|evs| {
                    evs.iter().any(|ev| {
                        // snippet is now a plain string
                        ev["snippet"]
                            .as_str()
                            .map(|s| !s.is_empty())
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
        });

        assert!(
            node_with_snippet.is_some(),
            "at least one auto-detected node must have evidence with a real snippet. \
             Nodes: {:#?}",
            auto_detected
                .iter()
                .map(|n| &n.ext_data)
                .collect::<Vec<_>>()
        );

        // The snippet must contain actual source keywords — not synthetic strings.
        let ext = node_with_snippet.unwrap().ext_data.as_ref().unwrap();
        let evidence = ext["evidence"].as_array().unwrap();
        // snippet is now stored as a plain string (not {"content": ..., "truncated": false})
        let snippets_with_content: Vec<&str> = evidence
            .iter()
            .filter_map(|ev| ev["snippet"].as_str())
            .filter(|s| !s.is_empty())
            .collect();

        let has_thiserror = snippets_with_content
            .iter()
            .any(|s| s.contains("thiserror") || s.contains("AppError") || s.contains("Error"));

        assert!(
            has_thiserror,
            "at least one snippet must contain real source keywords \
             ('thiserror', 'AppError', or 'Error'). Snippets: {snippets_with_content:?}"
        );
    }

    /// Regression: the `user_rejected` exception in the DELETE clause was
    /// removed in US-008 — rejections live in the `decisions` table now and
    /// are honoured at INSERT time, not by leaving stale nodes behind.
    /// Any pre-existing `auto_detected` row, regardless of `ext_data`, is
    /// wiped at the start of every persist.
    #[test]
    fn persist_conventions_no_longer_preserves_user_rejected_nodes() {
        use seshat_storage::{NodeRepository, SqliteNodeRepository};

        let (_db, conn) = open_db();
        let branch = BranchId::from("main");

        let guard = crate::lock_conn(&conn).unwrap();
        guard
            .execute(
                "INSERT INTO nodes (branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data)
                 VALUES ('main', 'convention', 'strong', 0.9, 10, 10, 'previously rejected convention',
                         json('{\"source\": \"auto_detected\", \"user_rejected\": 1, \"removed\": 1, \"removed_reason\": \"Rejected via TUI\"}'))",
                [],
            )
            .unwrap();
        drop(guard);

        // Persist with no aggregated input — pure DELETE.
        persist_conventions(&conn, &branch, &[]).unwrap();

        let node_repo = SqliteNodeRepository::new(conn.clone());
        let nodes = node_repo.find_by_branch(&branch).unwrap();

        let still_exists = nodes
            .iter()
            .any(|n| n.description == "previously rejected convention");
        assert!(
            !still_exists,
            "auto_detected node with user_rejected=1 must NOT survive persist_conventions \
             (the exception was removed in US-008 — rejections now live in the decisions table)"
        );
    }

    #[test]
    fn persist_conventions_deletes_normal_auto_detected() {
        use seshat_core::{KnowledgeNature, KnowledgeWeight, Trend};
        use seshat_detectors::AggregatedConvention;
        use seshat_storage::{NodeRepository, SqliteNodeRepository};

        let (_db, conn) = open_db();
        let branch = BranchId::from("main");

        let guard = crate::lock_conn(&conn).unwrap();
        guard
            .execute(
                "INSERT INTO nodes (branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data)
                 VALUES ('main', 'convention', 'strong', 0.8, 8, 10, 'old convention',
                         json('{\"source\": \"auto_detected\"}'))",
                [],
            )
            .unwrap();
        drop(guard);

        let aggregated = vec![AggregatedConvention {
            description: "new convention".to_string(),
            detector_name: "test".to_string(),
            nature: KnowledgeNature::Convention,
            weight: KnowledgeWeight::Strong,
            confidence: 0.7,
            adoption_count: 7,
            total_count: 10,
            trend: Trend::Stable,
            evidence: vec![],
        }];

        persist_conventions(&conn, &branch, &aggregated).unwrap();

        let node_repo = SqliteNodeRepository::new(conn.clone());
        let nodes = node_repo.find_by_branch(&branch).unwrap();

        let old_deleted = nodes.iter().any(|n| n.description == "old convention");
        assert!(!old_deleted, "old auto_detected node should be deleted");
    }

    /// AC for US-008: 100 conventions, 50 with matching decisions in any
    /// state — only the 50 undecided ones are inserted, and every inserted
    /// node carries a non-empty `description_hash`.
    #[test]
    fn persist_conventions_skips_inserts_with_matching_decision_in_any_state() {
        use seshat_core::{KnowledgeNature, KnowledgeWeight, Trend};
        use seshat_detectors::AggregatedConvention;
        use seshat_storage::{
            Decision, DecisionNature, DecisionRepository, DecisionState, DecisionWeight,
            NodeRepository, SqliteDecisionRepository, SqliteNodeRepository,
        };

        let (_db, conn) = open_db();
        let branch = BranchId::from("main");

        let aggregated: Vec<AggregatedConvention> = (0..100)
            .map(|i| AggregatedConvention {
                description: format!("convention #{i}"),
                detector_name: "test".to_string(),
                nature: KnowledgeNature::Convention,
                weight: KnowledgeWeight::Strong,
                confidence: 0.7,
                adoption_count: 7,
                total_count: 10,
                trend: Trend::Stable,
                evidence: vec![],
            })
            .collect();

        // Seed 50 decisions covering convention #0 .. #49, spread across all
        // four states so the "skip regardless of state" contract is exercised.
        let decision_repo = SqliteDecisionRepository::new(conn.clone());
        let states = [
            DecisionState::Approved,
            DecisionState::Rejected,
            DecisionState::Partial,
            DecisionState::Recorded,
        ];
        for i in 0..50usize {
            let description = format!("convention #{i}");
            let hash = compute_description_hash(&description);
            decision_repo
                .upsert(&Decision {
                    description_hash: hash,
                    description,
                    state: states[i % states.len()],
                    nature: DecisionNature::Convention,
                    weight: DecisionWeight::Rule,
                    category: None,
                    reason: None,
                    examples: vec![],
                    decided_on_branch: branch.clone(),
                    decided_at: 1_700_000_000,
                    updated_at: 1_700_000_000,
                })
                .unwrap();
        }

        persist_conventions(&conn, &branch, &aggregated).unwrap();

        let node_repo = SqliteNodeRepository::new(conn.clone());
        let nodes = node_repo.find_by_branch(&branch).unwrap();
        assert_eq!(
            nodes.len(),
            50,
            "expected exactly 50 inserted (those without a decision row); got {}",
            nodes.len()
        );

        // The inserted ones must be exactly indices 50..=99 (the undecided ones).
        let mut indices: Vec<u32> = nodes
            .iter()
            .map(|n| {
                n.description
                    .trim_start_matches("convention #")
                    .parse::<u32>()
                    .expect("parseable index")
            })
            .collect();
        indices.sort_unstable();
        let expected: Vec<u32> = (50..100).collect();
        assert_eq!(
            indices, expected,
            "inserted set must be exactly the undecided indices 50..=99"
        );

        // AC: auto-detected nodes are inserted with `description_hash` populated.
        let guard = crate::lock_conn(&conn).unwrap();
        let count_with_hash: i64 = guard
            .query_row(
                "SELECT COUNT(*) FROM nodes
                 WHERE branch_id = ?1
                   AND description_hash IS NOT NULL
                   AND description_hash != ''",
                rusqlite::params![branch.0],
                |row| row.get(0),
            )
            .unwrap();
        drop(guard);
        assert_eq!(
            count_with_hash, 50,
            "all inserted auto-detected nodes must have description_hash populated"
        );
    }

    // SQL captured by `trace_capture` for the bulk-fetch regression test.
    // Per-thread RefCell so parallel cargo test runs don't interfere; cleared
    // at the top of the test that uses it.
    thread_local! {
        static CAPTURED_SQL: std::cell::RefCell<Vec<String>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    fn trace_capture(event: rusqlite::trace::TraceEvent<'_>) {
        if let rusqlite::trace::TraceEvent::Stmt(_, sql) = event {
            CAPTURED_SQL.with(|cell| cell.borrow_mut().push(sql.to_string()));
        }
    }

    /// AC regression: the bulk-decision lookup must issue exactly **one**
    /// SELECT against the `decisions` table for ≤500 conventions, not one
    /// SELECT per convention. Verified via SQLite's `trace_v2` callback.
    #[test]
    fn persist_conventions_bulk_decision_lookup_uses_single_select() {
        use rusqlite::trace::TraceEventCodes;
        use seshat_core::{KnowledgeNature, KnowledgeWeight, Trend};
        use seshat_detectors::AggregatedConvention;

        let (_db, conn) = open_db();
        let branch = BranchId::from("main");

        let aggregated: Vec<AggregatedConvention> = (0..100)
            .map(|i| AggregatedConvention {
                description: format!("convention #{i}"),
                detector_name: "test".to_string(),
                nature: KnowledgeNature::Convention,
                weight: KnowledgeWeight::Strong,
                confidence: 0.7,
                adoption_count: 7,
                total_count: 10,
                trend: Trend::Stable,
                evidence: vec![],
            })
            .collect();

        CAPTURED_SQL.with(|cell| cell.borrow_mut().clear());

        {
            let guard = crate::lock_conn(&conn).unwrap();
            guard.trace_v2(TraceEventCodes::SQLITE_TRACE_STMT, Some(trace_capture));
        }

        persist_conventions(&conn, &branch, &aggregated).unwrap();

        // Disable tracing before the test ends so other tests sharing the
        // thread aren't polluted by stale callbacks.
        {
            let guard = crate::lock_conn(&conn).unwrap();
            guard.trace_v2(TraceEventCodes::SQLITE_TRACE_STMT, None);
        }

        let captured = CAPTURED_SQL.with(|cell| cell.borrow().clone());

        // Filter for SELECTs that target the `decisions` table specifically.
        // Match against the canonical lower-case form to avoid false
        // positives from SQLite's internal upper-cased keywords. Use a
        // word-boundary check so a future query against, say,
        // `decisions_archive` doesn't sneak past.
        let decisions_selects: Vec<&String> = captured
            .iter()
            .filter(|sql| {
                let lower = sql.to_lowercase();
                lower.contains("select ")
                    && (lower.contains("from decisions ")
                        || lower.contains("from decisions\n")
                        || lower.trim_end().ends_with("from decisions")
                        || lower.contains("from decisions where "))
            })
            .collect();

        assert_eq!(
            decisions_selects.len(),
            1,
            "expected exactly 1 SELECT against decisions for 100 conventions \
             (single chunk under HASH_BULK_CHUNK_SIZE=500); got {}: {:#?}",
            decisions_selects.len(),
            decisions_selects
        );

        // Stronger guard: the captured SELECT must look like a chunked
        // IN(...) batch, not a per-row WHERE description_hash = ?N.
        // A regression that issues 100 single-row SELECTs would still
        // pass the count==1 check on a different test fixture; counting
        // bound parameters here catches it on this one.
        let captured_sql = decisions_selects[0];
        let bind_count = captured_sql.matches('?').count();
        assert!(
            bind_count >= 100,
            "bulk SELECT must bind one parameter per convention (≥100 for \
             this fixture); got {bind_count} in: {captured_sql}"
        );

        // Sanity: nothing was inserted into decisions, so the SELECT path
        // must run regardless of whether matches exist (otherwise the
        // skip logic wouldn't be exercised).
        assert!(
            captured.iter().any(|sql| sql.contains("INSERT INTO nodes")),
            "expected the persist path to actually insert nodes after the SELECT"
        );
    }

    /// Integration regression: end-to-end flow through `run_detection_cycle`.
    /// Seeds a `decisions` row, runs detection, and asserts the auto-detected
    /// node for that hash is NOT recreated even though the detector would
    /// otherwise emit it. This pins the merge-aware skip behaviour all the
    /// way from detection through persistence.
    #[test]
    fn run_detection_cycle_skips_conventions_with_matching_decision_in_decisions_table() {
        use seshat_core::{
            ProjectFile,
            ir::{LanguageIR, RustIR},
        };
        use seshat_storage::{
            Decision, DecisionNature, DecisionRepository, DecisionState, DecisionWeight,
            FileIRRepository, NodeRepository, SqliteDecisionRepository, SqliteFileIRRepository,
            SqliteNodeRepository,
        };
        use std::path::PathBuf;

        let (_db, conn) = open_db();
        let branch = BranchId::from("main");

        // Seed a Rust file IR so detectors have something to chew on.
        let project_file = ProjectFile {
            path: PathBuf::from("src/main.rs"),
            language: seshat_core::Language::Rust,
            content_hash: "abc123".to_string(),
            imports: vec![],
            exports: vec![],
            functions: vec![],
            types: vec![],
            dependencies_used: vec![],
            language_ir: LanguageIR::Rust(RustIR::default()),
            file_doc: None,
        };
        SqliteFileIRRepository::new(conn.clone())
            .upsert(&branch, &project_file, None)
            .expect("upsert file");

        // First run: no decisions, capture whatever the detector emits.
        let config = DetectionConfig::default();
        let file_dates = HashMap::new();
        let source_map: HashMap<std::path::PathBuf, String> = HashMap::new();
        run_detection_cycle(&conn, &branch, &config, &file_dates, &source_map)
            .expect("first cycle");

        let node_repo = SqliteNodeRepository::new(conn.clone());
        let nodes_before = node_repo.find_by_branch(&branch).unwrap();
        if nodes_before.is_empty() {
            // No detector matched the trivial fixture — nothing to assert.
            // The unit-level skip test already covers the persist contract.
            return;
        }

        // Pick the first detected convention and seed a `rejected` decision
        // for its description hash. After re-running detection, that node
        // must NOT be re-inserted.
        let target = &nodes_before[0];
        let target_hash = compute_description_hash(&target.description);
        let target_description = target.description.clone();

        SqliteDecisionRepository::new(conn.clone())
            .upsert(&Decision {
                description_hash: target_hash.clone(),
                description: target_description.clone(),
                state: DecisionState::Rejected,
                nature: DecisionNature::Convention,
                weight: DecisionWeight::Rule,
                category: None,
                reason: Some("test-only".to_string()),
                examples: vec![],
                decided_on_branch: branch.clone(),
                decided_at: 1_700_000_000,
                updated_at: 1_700_000_000,
            })
            .expect("seed decision");

        run_detection_cycle(&conn, &branch, &config, &file_dates, &source_map)
            .expect("second cycle");

        let nodes_after = node_repo.find_by_branch(&branch).unwrap();
        let still_present = nodes_after
            .iter()
            .any(|n| n.description == target_description);
        assert!(
            !still_present,
            "convention with a `rejected` decision row must NOT be re-emitted as auto-detected. \
             target description: {target_description:?}"
        );
    }
}
