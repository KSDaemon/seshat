//! Graduated approach validation against the knowledge graph.
//!
//! Provides `validate_approach()` which checks a proposed approach against
//! rules, contradictions, duplicates, conventions, decisions, and observations.
//! Returns a graduated response with verdict, evidence gating, and actionable
//! suggestions.
//!
//! Reuses `query_code_pattern` for duplicate detection and optionally
//! `query_dependencies` for enriching `used_by` counts.

use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};
use serde::Serialize;
use seshat_core::CodeSnippet;

use crate::code_pattern::query_code_pattern;
use crate::conventions::{ConventionResult, QueryConventionData};
use crate::dependencies::query_dependencies;
use crate::error::GraphError;
use crate::{SQL_NOT_REMOVED, query_convention};

// ── Constants ────────────────────────────────────────────────

/// Minimum score from `query_code_pattern` to consider a pattern a duplicate.
const DUPLICATE_SCORE_THRESHOLD: f64 = 0.6;

/// Confidence threshold below which conventions are considered stale/uncertain.
const LOW_CONFIDENCE_THRESHOLD: f64 = 0.5;

// ── Input parameters ─────────────────────────────────────────

/// Parameters for the `validate_approach` function.
#[derive(Debug, Clone)]
pub struct ValidateApproachParams {
    /// Description of the proposed approach.
    pub description: String,
    /// Optional file context for enriching results (e.g., used_by counts).
    pub file_context: Option<String>,
    /// Optional approach type for filtering (e.g., "refactor", "new_feature").
    pub approach_type: Option<String>,
}

// ── Response data types ──────────────────────────────────────

/// Full response data for the `validate_approach` tool.
#[derive(Debug, Clone, Serialize)]
pub struct ValidateApproachData {
    /// Rules that the approach violates (weight = Rule).
    pub rules: Vec<RuleViolation>,
    /// Contradictions found in the knowledge graph (Contradicts edges).
    pub contradictions: Vec<Contradiction>,
    /// Potential duplicate code patterns (from IR search, score > 0.6).
    pub duplicates: Vec<DuplicatePattern>,
    /// Matching conventions from FTS5 search.
    pub conventions: Vec<ConventionResult>,
    /// User-recorded decisions relevant to the approach.
    pub decisions: Vec<DecisionEntry>,
    /// Low-confidence observations.
    pub observations: Vec<ObservationEntry>,
    /// Overall verdict.
    pub verdict: String,
    /// Whether the approach is ready to proceed.
    pub ready: bool,
    /// Suggestions when not ready.
    pub what_would_help: Vec<String>,
    /// Deterministic summary counting each section.
    pub summary: String,
}

/// A rule violation (conventions with weight = "rule").
#[derive(Debug, Clone, Serialize)]
pub struct RuleViolation {
    /// Node ID of the rule.
    pub id: i64,
    /// Description of the rule.
    pub description: String,
    /// Evidence snippet from the codebase.
    pub evidence: CodeSnippet,
    /// Severity is always "must_fix" for rules.
    pub severity: String,
}

/// A contradiction found via Contradicts edges in the graph.
#[derive(Debug, Clone, Serialize)]
pub struct Contradiction {
    /// The source node ID.
    pub source_id: i64,
    /// The target node ID.
    pub target_id: i64,
    /// Description of the source node.
    pub source_description: String,
    /// Description of the target node.
    pub target_description: String,
    /// Edge weight.
    pub weight: f64,
}

/// A potential duplicate pattern found via IR search.
#[derive(Debug, Clone, Serialize)]
pub struct DuplicatePattern {
    /// Name of the function, type, or export.
    pub name: String,
    /// File path where the pattern was found.
    pub file_path: String,
    /// Start line number.
    pub line: usize,
    /// Code snippet.
    pub snippet: CodeSnippet,
    /// Number of files that depend on (use) this pattern.
    pub used_by: usize,
}

/// A user-recorded decision relevant to the approach.
#[derive(Debug, Clone, Serialize)]
pub struct DecisionEntry {
    /// Node ID.
    pub id: i64,
    /// Description of the decision.
    pub description: String,
    /// Weight of the decision.
    pub weight: String,
    /// Confidence score.
    pub confidence: f64,
}

/// A low-confidence observation.
#[derive(Debug, Clone, Serialize)]
pub struct ObservationEntry {
    /// Node ID.
    pub id: i64,
    /// Description of the observation.
    pub description: String,
    /// Confidence score.
    pub confidence: f64,
}

// ── Public API ───────────────────────────────────────────────

/// Validate a proposed approach against the knowledge graph.
///
/// Checks rules, contradictions, duplicates, conventions, decisions, and
/// observations. Returns a graduated response with verdict and evidence gating.
///
/// Returns `Err(GraphError::InvalidInput)` for empty descriptions.
pub fn validate_approach(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    params: ValidateApproachParams,
) -> Result<ValidateApproachData, GraphError> {
    let description = params.description.trim();
    if description.is_empty() {
        return Err(GraphError::InvalidInput(
            "description must not be empty".to_owned(),
        ));
    }

    // 1+4. Single FTS5 search, then partition into rules vs conventions.
    let all_conventions = query_convention(conn, branch_id, description).unwrap_or_else(|e| {
        tracing::warn!("Convention search failed in validate_approach: {e}");
        QueryConventionData {
            conventions: Vec::new(),
        }
    });
    let (rule_convs, conventions): (Vec<_>, Vec<_>) = all_conventions
        .conventions
        .into_iter()
        .partition(|c| c.weight == "rule");
    let rules = rules_from_conventions(rule_convs);

    // 2. Contradictions: edges with type = "contradicts"
    let contradictions = find_contradictions(conn, branch_id, description)?;

    // 3. Duplicates: reuse query_code_pattern for IR search, filter by score threshold
    let duplicates = find_duplicates(conn, branch_id, description, params.file_context.as_deref())?;

    // 5. Decisions: user-recorded decisions matching via FTS5
    let decisions = find_decisions(conn, branch_id, description)?;

    // 6. Observations: low-confidence items
    let observations = find_observations(conn, branch_id, description)?;

    // Verdict logic
    let verdict = compute_verdict(&rules, &contradictions, &conventions);

    // Evidence gating
    let has_stale_conventions = conventions
        .iter()
        .any(|c| c.confidence < LOW_CONFIDENCE_THRESHOLD);
    let ready = verdict != "rules_violated" && !has_stale_conventions;

    // what_would_help
    let what_would_help = build_what_would_help(
        &verdict,
        &rules,
        &contradictions,
        &conventions,
        has_stale_conventions,
    );

    // Summary
    let summary = build_summary(
        rules.len(),
        contradictions.len(),
        duplicates.len(),
        conventions.len(),
        decisions.len(),
        observations.len(),
        &verdict,
    );

    Ok(ValidateApproachData {
        rules,
        contradictions,
        duplicates,
        conventions,
        decisions,
        observations,
        verdict,
        ready,
        what_would_help,
        summary,
    })
}

// ── Internal helpers ─────────────────────────────────────────

/// Convert pre-filtered rule conventions into `RuleViolation` structs.
fn rules_from_conventions(rule_convs: Vec<ConventionResult>) -> Vec<RuleViolation> {
    rule_convs
        .into_iter()
        .map(|c| {
            let evidence = c
                .examples
                .first()
                .map(|ex| CodeSnippet {
                    content: ex.snippet.content.clone(),
                    truncated: ex.snippet.truncated,
                })
                .unwrap_or_else(|| CodeSnippet {
                    content: String::new(),
                    truncated: false,
                });

            RuleViolation {
                id: c.id,
                description: c.description,
                evidence,
                severity: "must_fix".to_owned(),
            }
        })
        .collect()
}

/// Find contradictions from the edges table.
fn find_contradictions(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    description: &str,
) -> Result<Vec<Contradiction>, GraphError> {
    let conn_guard = crate::lock_conn(conn)?;

    // Find nodes that match the description terms, then check for Contradicts edges.
    let node_ids = find_matching_node_ids(&conn_guard, branch_id, description)?;

    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Prepare once, reuse across all node_ids.
    let mut stmt = conn_guard
        .prepare(
            "SELECT e.source_id, e.target_id, e.weight,
                    s.description, t.description
             FROM edges e
             JOIN nodes s ON s.id = e.source_id
             JOIN nodes t ON t.id = e.target_id
             WHERE e.edge_type = 'contradicts'
               AND e.branch_id = ?1
               AND (e.source_id = ?2 OR e.target_id = ?2)",
        )
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Failed to prepare contradiction query: {e}"
            )))
        })?;

    let mut contradictions = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for node_id in &node_ids {
        let rows = stmt
            .query_map(params![branch_id, node_id], |row| {
                Ok(Contradiction {
                    source_id: row.get(0)?,
                    target_id: row.get(1)?,
                    weight: row.get(2)?,
                    source_description: row.get(3)?,
                    target_description: row.get(4)?,
                })
            })
            .map_err(|e| {
                GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                    "Failed to query contradictions: {e}"
                )))
            })?;

        for row in rows {
            match row {
                Ok(contradiction) => {
                    // Deduplicate: edge could appear from both source and target sides.
                    let key = (contradiction.source_id, contradiction.target_id);
                    if seen.insert(key) {
                        contradictions.push(contradiction);
                    }
                }
                Err(e) => {
                    tracing::warn!("Skipping contradiction row: {e}");
                }
            }
        }
    }

    Ok(contradictions)
}

/// Extract significant keywords (len > 2, lowercased) from a description.
fn extract_keywords(description: &str) -> Vec<String> {
    description
        .split_whitespace()
        .filter(|w| w.len() > 2)
        .map(|w| w.to_lowercase())
        .collect()
}

/// Build parameterized LIKE clauses and corresponding bind values.
///
/// Returns `(where_fragment, params)` where `where_fragment` is e.g.
/// `(LOWER(description) LIKE ?2 OR LOWER(description) LIKE ?3)` and `params`
/// are the `%keyword%` patterns. `param_offset` is the first `?N` index to use
/// (e.g. 2 when `?1` is already taken by `branch_id`).
fn build_keyword_like(keywords: &[String], param_offset: usize) -> (String, Vec<String>) {
    let clauses: Vec<String> = keywords
        .iter()
        .enumerate()
        .map(|(i, _)| format!("LOWER(description) LIKE ?{}", param_offset + i))
        .collect();
    let params: Vec<String> = keywords.iter().map(|k| format!("%{k}%")).collect();
    (clauses.join(" OR "), params)
}

/// Execute a keyword-based LIKE search on the `nodes` table.
///
/// `columns` — the SELECT columns (e.g. `"id"` or `"id, description, weight, confidence"`).
/// `extra_where` — additional AND clause (e.g. `"AND nature = 'decision'"`) or empty string.
///
/// Uses parameterized queries to prevent SQL injection.
fn keyword_search_nodes<T, F>(
    conn_guard: &rusqlite::Connection,
    branch_id: &str,
    description: &str,
    columns: &str,
    extra_where: &str,
    context: &str,
    row_mapper: F,
) -> Result<Vec<T>, GraphError>
where
    F: Fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
{
    let keywords = extract_keywords(description);
    if keywords.is_empty() {
        return Ok(Vec::new());
    }

    let (like_where, like_params) = build_keyword_like(&keywords, 2);

    let sql = format!(
        "SELECT {columns} FROM nodes WHERE branch_id = ?1 AND ({like_where}) {extra_where} AND {SQL_NOT_REMOVED}"
    );

    let mut stmt = conn_guard.prepare(&sql).map_err(|e| {
        GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
            "Failed to prepare {context} query: {e}"
        )))
    })?;

    // Build dynamic params: [branch_id, "%kw1%", "%kw2%", ...]
    let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> =
        vec![Box::new(branch_id.to_owned())];
    for p in &like_params {
        bind_values.push(Box::new(p.clone()));
    }
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        bind_values.iter().map(|b| b.as_ref()).collect();

    let rows = stmt
        .query_map(param_refs.as_slice(), &row_mapper)
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Failed to query {context}: {e}"
            )))
        })?;

    let mut results = Vec::new();
    for row in rows {
        match row {
            Ok(item) => results.push(item),
            Err(e) => tracing::warn!("Skipping {context} row: {e}"),
        }
    }

    Ok(results)
}

/// Find matching node IDs by checking if description keywords appear in node descriptions.
fn find_matching_node_ids(
    conn_guard: &rusqlite::Connection,
    branch_id: &str,
    description: &str,
) -> Result<Vec<i64>, GraphError> {
    keyword_search_nodes(
        conn_guard,
        branch_id,
        description,
        "id",
        "",
        "matching nodes",
        |row| row.get::<_, i64>(0),
    )
}

/// Find potential duplicates using `query_code_pattern`.
fn find_duplicates(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    description: &str,
    file_context: Option<&str>,
) -> Result<Vec<DuplicatePattern>, GraphError> {
    // Use the full description as the query for code pattern search.
    let pattern_data = match query_code_pattern(conn, branch_id, description) {
        Ok(data) => data,
        Err(e) => {
            tracing::warn!("Code pattern search failed in validate_approach: {e}");
            return Ok(Vec::new());
        }
    };

    // Filter by score threshold and convert to DuplicatePattern.
    let mut duplicates: Vec<DuplicatePattern> = pattern_data
        .patterns
        .into_iter()
        .filter(|p| p.score >= DUPLICATE_SCORE_THRESHOLD)
        .map(|p| DuplicatePattern {
            name: p.name.clone(),
            file_path: p.file_path.clone(),
            line: p.line,
            snippet: p.snippet,
            used_by: 0,
        })
        .collect();

    // Enrich used_by counts when caller provides file context (signals
    // they care about dependency information for the duplicates).
    if file_context.is_some() {
        enrich_used_by(conn, branch_id, &mut duplicates);
    }

    Ok(duplicates)
}

/// Enrich `used_by` counts for duplicate patterns by querying dependencies.
fn enrich_used_by(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    duplicates: &mut [DuplicatePattern],
) {
    for dup in duplicates.iter_mut() {
        match query_dependencies(conn, branch_id, &dup.file_path) {
            Ok(dep_data) => {
                dup.used_by = dep_data.dependents.len();
            }
            Err(e) => {
                tracing::debug!("Could not get dependency info for {}: {e}", dup.file_path);
            }
        }
    }
}

/// Find user-recorded decisions matching via keyword LIKE search.
fn find_decisions(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    description: &str,
) -> Result<Vec<DecisionEntry>, GraphError> {
    let conn_guard = crate::lock_conn(conn)?;
    keyword_search_nodes(
        &conn_guard,
        branch_id,
        description,
        "id, description, weight, confidence",
        "AND nature = 'decision'",
        "decisions",
        |row| {
            Ok(DecisionEntry {
                id: row.get(0)?,
                description: row.get(1)?,
                weight: row.get(2)?,
                confidence: row.get(3)?,
            })
        },
    )
}

/// Find low-confidence observations matching via keyword LIKE search.
fn find_observations(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    description: &str,
) -> Result<Vec<ObservationEntry>, GraphError> {
    let conn_guard = crate::lock_conn(conn)?;
    keyword_search_nodes(
        &conn_guard,
        branch_id,
        description,
        "id, description, confidence",
        "AND nature = 'observation'",
        "observations",
        |row| {
            Ok(ObservationEntry {
                id: row.get(0)?,
                description: row.get(1)?,
                confidence: row.get(2)?,
            })
        },
    )
}

/// Compute the verdict based on findings.
///
/// - `rules_violated`: any rules found
/// - `warnings_found`: contradictions or high-weight (strong) conventions
/// - `info_only`: some findings but nothing critical
/// - `approved`: nothing matches
fn compute_verdict(
    rules: &[RuleViolation],
    contradictions: &[Contradiction],
    conventions: &[ConventionResult],
) -> String {
    if !rules.is_empty() {
        return "rules_violated".to_owned();
    }

    let has_strong_conventions = conventions.iter().any(|c| c.weight == "strong");
    if !contradictions.is_empty() || has_strong_conventions {
        return "warnings_found".to_owned();
    }

    if !conventions.is_empty() {
        return "info_only".to_owned();
    }

    "approved".to_owned()
}

/// Build actionable suggestions when the approach is not ready.
fn build_what_would_help(
    verdict: &str,
    rules: &[RuleViolation],
    contradictions: &[Contradiction],
    conventions: &[ConventionResult],
    has_stale_conventions: bool,
) -> Vec<String> {
    let mut suggestions = Vec::new();

    if verdict == "rules_violated" {
        suggestions.push(format!(
            "Fix {} rule violation(s) before proceeding",
            rules.len()
        ));
        for rule in rules {
            suggestions.push(format!("  - {}", rule.description));
        }
    }

    if !contradictions.is_empty() {
        suggestions.push(format!(
            "Resolve {} contradiction(s) in the knowledge graph",
            contradictions.len()
        ));
    }

    if has_stale_conventions {
        let stale_count = conventions
            .iter()
            .filter(|c| c.confidence < LOW_CONFIDENCE_THRESHOLD)
            .count();
        suggestions.push(format!(
            "Review {} convention(s) with low confidence (<{LOW_CONFIDENCE_THRESHOLD}) — they may be outdated",
            stale_count
        ));
    }

    suggestions
}

/// Build a deterministic summary counting each section.
fn build_summary(
    rules: usize,
    contradictions: usize,
    duplicates: usize,
    conventions: usize,
    decisions: usize,
    observations: usize,
    verdict: &str,
) -> String {
    format!(
        "Verdict: {verdict}. Found {rules} rule(s), {contradictions} contradiction(s), \
         {duplicates} duplicate(s), {conventions} convention(s), {decisions} decision(s), \
         {observations} observation(s)."
    )
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use rusqlite::params;
    use seshat_core::{
        Export, Function, Language, LanguageIR, ProjectFile, RustIR, TypeDef, TypeDefKind,
    };
    use seshat_storage::serialize_ir;

    use crate::test_helpers::test_conn;

    /// Helper: insert an IR file into the database for a branch.
    fn insert_ir(conn: &Arc<Mutex<Connection>>, branch_id: &str, file: &ProjectFile) {
        let c = conn.lock().unwrap();
        let ir_data = serialize_ir(file).expect("serialize IR");
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

    /// Helper: create a sample ProjectFile.
    fn sample_project_file(path: &str) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::Rust,
            content_hash: "abc123".to_owned(),
            imports: Vec::new(),
            exports: vec![Export {
                name: "handle_error".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
            }],
            functions: vec![Function {
                name: "handle_error".to_owned(),
                is_public: true,
                is_async: false,
                line: 10,
                end_line: 50,
                parameters: vec!["err".to_owned()],
            }],
            types: vec![TypeDef {
                name: "ErrorHandler".to_owned(),
                kind: TypeDefKind::Struct,
                is_public: true,
                line: 5,
            }],
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
        }
    }

    /// Helper: insert a convention node.
    fn insert_convention(
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

    /// Helper: insert a contradicts edge between two nodes.
    fn insert_contradiction_edge(
        conn: &Arc<Mutex<Connection>>,
        branch_id: &str,
        source_id: i64,
        target_id: i64,
    ) {
        let c = conn.lock().unwrap();
        c.execute(
            "INSERT INTO edges (source_id, target_id, edge_type, branch_id, weight)
             VALUES (?1, ?2, 'contradicts', ?3, 1.0)",
            params![source_id, target_id, branch_id],
        )
        .unwrap();
    }

    #[test]
    fn approach_matching_rule_returns_rules_violated() {
        let conn = test_conn();

        // Insert a rule-weight convention. Use terms that will match the query via FTS5.
        insert_convention(
            &conn,
            "main",
            "Always use thiserror for error types",
            "rule",
            1.0,
            "convention",
        );
        crate::fts::rebuild_fts_index(&conn).unwrap();

        // Insert IR so code pattern search works.
        let file = sample_project_file("src/errors.rs");
        insert_ir(&conn, "main", &file);

        // Use terms that overlap with the rule description so FTS5 can find it.
        // FTS5 uses AND semantics — all tokens must be present.
        let params = ValidateApproachParams {
            description: "thiserror error types".to_owned(),
            file_context: None,
            approach_type: None,
        };

        let result = validate_approach(&conn, "main", params).unwrap();

        assert_eq!(result.verdict, "rules_violated");
        assert!(!result.ready);
        assert!(!result.rules.is_empty());
        assert_eq!(result.rules[0].severity, "must_fix");
        assert!(!result.what_would_help.is_empty());
    }

    #[test]
    fn approach_with_duplicates_populates_duplicates() {
        let conn = test_conn();

        // Insert an IR file with a function named "handle_error".
        let file = sample_project_file("src/errors.rs");
        insert_ir(&conn, "main", &file);

        let params = ValidateApproachParams {
            description: "handle_error".to_owned(),
            file_context: Some("src/errors.rs".to_owned()),
            approach_type: None,
        };

        let result = validate_approach(&conn, "main", params).unwrap();

        // Should find "handle_error" as a duplicate (exact match score = 1.0 > 0.6).
        assert!(!result.duplicates.is_empty());
        assert!(result.duplicates.iter().any(|d| d.name == "handle_error"));
    }

    #[test]
    fn clean_approach_returns_approved_and_ready() {
        let conn = test_conn();

        // Insert IR so queries don't fail.
        let file = sample_project_file("src/utils.rs");
        insert_ir(&conn, "main", &file);

        let params = ValidateApproachParams {
            description: "add new widget component zzz_unique".to_owned(),
            file_context: None,
            approach_type: None,
        };

        let result = validate_approach(&conn, "main", params).unwrap();

        assert_eq!(result.verdict, "approved");
        assert!(result.ready);
        assert!(result.rules.is_empty());
        assert!(result.contradictions.is_empty());
        assert!(result.what_would_help.is_empty());
    }

    #[test]
    fn evidence_gating_with_stale_conventions() {
        let conn = test_conn();

        // Insert a convention with low confidence. Use distinctive terms.
        insert_convention(
            &conn,
            "main",
            "camelCase variable naming",
            "moderate",
            0.3, // Below LOW_CONFIDENCE_THRESHOLD (0.5)
            "convention",
        );
        crate::fts::rebuild_fts_index(&conn).unwrap();

        // Insert IR.
        let file = sample_project_file("src/naming.rs");
        insert_ir(&conn, "main", &file);

        // FTS5 AND semantics: all tokens must match.
        let params = ValidateApproachParams {
            description: "camelCase variable naming".to_owned(),
            file_context: None,
            approach_type: None,
        };

        let result = validate_approach(&conn, "main", params).unwrap();

        // Should not be ready because of low-confidence convention.
        assert!(!result.ready);
        assert!(
            result
                .what_would_help
                .iter()
                .any(|s| s.contains("low confidence"))
        );
    }

    #[test]
    fn what_would_help_populated_when_not_ready() {
        let conn = test_conn();

        insert_convention(
            &conn,
            "main",
            "validate input parameters",
            "rule",
            1.0,
            "convention",
        );
        crate::fts::rebuild_fts_index(&conn).unwrap();

        let file = sample_project_file("src/validation.rs");
        insert_ir(&conn, "main", &file);

        // Use terms matching the rule description for FTS5 to find it.
        let params = ValidateApproachParams {
            description: "validate input parameters".to_owned(),
            file_context: None,
            approach_type: None,
        };

        let result = validate_approach(&conn, "main", params).unwrap();

        assert_eq!(result.verdict, "rules_violated");
        assert!(!result.ready);
        assert!(!result.what_would_help.is_empty());
        assert!(
            result
                .what_would_help
                .iter()
                .any(|s| s.contains("rule violation"))
        );
    }

    #[test]
    fn empty_description_returns_error() {
        let conn = test_conn();

        let params = ValidateApproachParams {
            description: "".to_owned(),
            file_context: None,
            approach_type: None,
        };

        let result = validate_approach(&conn, "main", params);
        assert!(result.is_err());
        match result {
            Err(GraphError::InvalidInput(msg)) => {
                assert!(msg.contains("empty"));
            }
            other => panic!("Expected InvalidInput, got: {other:?}"),
        }
    }

    #[test]
    fn contradictions_detected_from_edges() {
        let conn = test_conn();

        // Insert two nodes that contradict each other.
        let node_a = insert_convention(
            &conn,
            "main",
            "Use REST for API design patterns",
            "strong",
            0.9,
            "convention",
        );
        let node_b = insert_convention(
            &conn,
            "main",
            "Use GraphQL for API design patterns",
            "strong",
            0.8,
            "convention",
        );
        insert_contradiction_edge(&conn, "main", node_a, node_b);
        crate::fts::rebuild_fts_index(&conn).unwrap();

        let file = sample_project_file("src/api.rs");
        insert_ir(&conn, "main", &file);

        let params = ValidateApproachParams {
            description: "API design patterns for new service".to_owned(),
            file_context: None,
            approach_type: None,
        };

        let result = validate_approach(&conn, "main", params).unwrap();

        assert!(!result.contradictions.is_empty());
        assert_eq!(result.verdict, "warnings_found");
    }

    #[test]
    fn decisions_found_when_matching() {
        let conn = test_conn();

        insert_convention(
            &conn,
            "main",
            "Use SQLite for storage backend",
            "strong",
            1.0,
            "decision",
        );
        crate::fts::rebuild_fts_index(&conn).unwrap();

        let file = sample_project_file("src/storage.rs");
        insert_ir(&conn, "main", &file);

        let params = ValidateApproachParams {
            description: "Switch storage backend to PostgreSQL".to_owned(),
            file_context: None,
            approach_type: None,
        };

        let result = validate_approach(&conn, "main", params).unwrap();

        assert!(!result.decisions.is_empty());
        assert!(
            result
                .decisions
                .iter()
                .any(|d| d.description.contains("SQLite"))
        );
    }

    #[test]
    fn observations_found_when_matching() {
        let conn = test_conn();

        insert_convention(
            &conn,
            "main",
            "Some files use logging pattern with tracing crate",
            "weak",
            0.3,
            "observation",
        );
        crate::fts::rebuild_fts_index(&conn).unwrap();

        let file = sample_project_file("src/logging.rs");
        insert_ir(&conn, "main", &file);

        let params = ValidateApproachParams {
            description: "Add logging with tracing crate".to_owned(),
            file_context: None,
            approach_type: None,
        };

        let result = validate_approach(&conn, "main", params).unwrap();

        assert!(!result.observations.is_empty());
        assert!(
            result
                .observations
                .iter()
                .any(|o| o.description.contains("tracing"))
        );
    }

    #[test]
    fn summary_counts_all_sections() {
        let summary = build_summary(2, 1, 3, 4, 1, 2, "rules_violated");
        assert!(summary.contains("2 rule(s)"));
        assert!(summary.contains("1 contradiction(s)"));
        assert!(summary.contains("3 duplicate(s)"));
        assert!(summary.contains("4 convention(s)"));
        assert!(summary.contains("1 decision(s)"));
        assert!(summary.contains("2 observation(s)"));
        assert!(summary.contains("rules_violated"));
    }

    #[test]
    fn verdict_logic_approved_when_empty() {
        let verdict = compute_verdict(&[], &[], &[]);
        assert_eq!(verdict, "approved");
    }

    #[test]
    fn verdict_logic_info_only_with_moderate_conventions() {
        // A convention with weight "moderate" should give info_only.
        let conv = ConventionResult {
            id: 1,
            nature: "convention".to_owned(),
            weight: "moderate".to_owned(),
            confidence: 0.7,
            adoption: crate::conventions::AdoptionInfo {
                count: 7,
                total: 10,
                rate: 0.7,
            },
            trend: "stable".to_owned(),
            description: "Test convention".to_owned(),
            source: "auto_detected".to_owned(),
            user_confirmed: false,
            examples: vec![],
        };

        let verdict = compute_verdict(&[], &[], &[conv]);
        assert_eq!(verdict, "info_only");
    }
}
