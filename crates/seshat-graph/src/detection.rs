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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use seshat_core::{BranchId, DetectionConfig, KnowledgeNode, NodeId};
use seshat_detectors::{aggregate_findings, run_all_detectors, AggregatedConvention};
use seshat_storage::{FileIRRepository, SqliteFileIRRepository};
use tracing::info;

use crate::error::GraphError;
use crate::{rebuild_fts_index, SOURCE_AUTO_DETECTED};

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

    // 2. Run all detectors in parallel (rayon).
    // Watcher path: no source in memory — pass empty map so detectors fall back
    // to IR-only detection (empty snippets). Future improvement: build a mini
    // source_map from just the changed files before calling this.
    let detector_results = run_all_detectors(&all_files, &HashMap::new(), detection_config, None);
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

/// Convert an [`AggregatedConvention`] to a [`KnowledgeNode`] for storage.
///
/// The `ext_data` JSON contains:
/// - `source`: `"auto_detected"` (distinguishes from user decisions)
/// - `detector_name`: which detector produced this
/// - `trend`: rising / stable / declining / unknown
/// - `adoption_rate`: confidence as a float
/// - `evidence`: `[{file, line, end_line, snippet}]`
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
                "snippet": { "content": e.snippet, "truncated": false },
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
fn persist_conventions(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &BranchId,
    aggregated: &[AggregatedConvention],
) -> Result<(), GraphError> {
    let guard = crate::lock_conn(conn)?;

    guard.execute_batch("BEGIN").map_err(|e| {
        GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
            "BEGIN: {e}"
        )))
    })?;

    // Delete all auto-detected nodes for this branch.
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

    // Insert fresh nodes.
    for convention in aggregated {
        let node = convention_to_node(convention, branch_id);
        let ext = node.ext_data.as_ref().map(|v| v.to_string());
        let ins = guard.execute(
            "INSERT INTO nodes
             (branch_id, nature, weight, confidence,
              adoption_count, total_count, description, ext_data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                node.branch_id.0,
                node.nature.as_str(),
                node.weight.as_str(),
                node.confidence,
                node.adoption_count,
                node.total_count,
                node.description,
                ext,
            ],
        );
        if let Err(e) = ins {
            let _ = guard.execute_batch("ROLLBACK");
            return Err(GraphError::Storage(
                seshat_storage::StorageError::QueryError(format!("insert convention: {e}")),
            ));
        }
    }

    guard.execute_batch("COMMIT").map_err(|e| {
        GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
            "COMMIT: {e}"
        )))
    })?;

    info!(count = aggregated.len(), "Persisted convention nodes");
    Ok(())
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
    use seshat_core::Language;
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
        let result = run_detection_cycle(&conn, &branch, &config, &HashMap::new());
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
        let result = run_detection_cycle(&conn, &branch, &config, &HashMap::new());
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
        // snippet must be nested object with "content" key
        assert_eq!(
            ev["snippet"]["content"].as_str().unwrap(),
            "pub fn real_code() {}"
        );
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
        use seshat_detectors::run_all_detectors;
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
        let detector_results = run_all_detectors(&files, &source_map, &config, None);
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
                        ev["snippet"]["content"]
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
        let snippets_with_content: Vec<&str> = evidence
            .iter()
            .filter_map(|ev| ev["snippet"]["content"].as_str())
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
}
