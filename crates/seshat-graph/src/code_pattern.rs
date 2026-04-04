//! Code pattern search over deserialized IR (functions, types, exports).
//!
//! Provides `query_code_pattern()` which searches `files_ir` blobs by name
//! matching with scored results, plus related conventions via FTS5.
//!
//! Scoring: exact match (1.0) > prefix match (0.7) > contains (0.4).
//! Results are sorted by score descending.

use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};
use serde::Serialize;
use seshat_core::{CodeSnippet, ProjectFile};
use seshat_storage::deserialize_ir;

use crate::conventions::{ConventionResult, QueryConventionData};
use crate::error::GraphError;
use crate::query_convention;

// ── Constants ────────────────────────────────────────────────

/// Maximum number of lines in a code pattern snippet before truncation.
const MAX_PATTERN_SNIPPET_LINES: usize = 10;

// ── Response data types ──────────────────────────────────────

/// Full response data for the `query_code_pattern` tool.
#[derive(Debug, Clone, Serialize)]
pub struct CodePatternData {
    /// Code patterns (functions, types, exports) matching the query.
    pub patterns: Vec<PatternResult>,
    /// Related conventions from FTS5 search.
    pub related_conventions: Vec<ConventionResult>,
    /// Metadata about the search.
    pub metadata: CodePatternMetadata,
}

/// A single code pattern result from IR search.
#[derive(Debug, Clone, Serialize)]
pub struct PatternResult {
    /// Name of the function, type, or export.
    pub name: String,
    /// Kind of the pattern: "function", "type", or "export".
    pub kind: String,
    /// File path where the pattern was found.
    pub file_path: String,
    /// Start line number.
    pub line: usize,
    /// End line number.
    pub end_line: usize,
    /// Whether the symbol is public.
    pub is_public: bool,
    /// Code snippet (may be truncated).
    pub snippet: CodeSnippet,
    /// Match score (1.0 = exact, 0.7 = prefix, 0.4 = contains).
    pub score: f64,
}

/// Metadata about the code pattern search.
#[derive(Debug, Clone, Serialize)]
pub struct CodePatternMetadata {
    /// The original query string.
    pub query: String,
    /// Number of pattern results found.
    pub pattern_count: usize,
    /// Number of related conventions found.
    pub convention_count: usize,
    /// Type of search performed.
    pub search_type: String,
    /// Suggested next steps for the AI agent.
    pub next_steps: Vec<String>,
}

// ── Public API ───────────────────────────────────────────────

/// Search deserialized IR for code patterns matching the query.
///
/// Searches function names, type names, and export names in all files for the
/// given branch. Also searches conventions via FTS5 for related conventions.
///
/// Returns `Err(GraphError::InvalidInput)` for empty queries.
/// Returns empty arrays (not an error) when no results match.
pub fn query_code_pattern(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    query: &str,
) -> Result<CodePatternData, GraphError> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err(GraphError::InvalidInput(
            "query must not be empty".to_owned(),
        ));
    }

    let query_lower = trimmed.to_lowercase();
    let query_tokens: Vec<&str> = query_lower.split_whitespace().collect();

    // Load and deserialize all IR for this branch.
    let files = load_branch_ir(conn, branch_id)?;

    // Search IR for matching patterns.
    let mut patterns = Vec::new();
    for file in &files {
        let file_path = file.path.to_string_lossy().to_string();
        search_functions(file, &file_path, &query_tokens, &mut patterns);
        search_types(file, &file_path, &query_tokens, &mut patterns);
        search_exports(file, &file_path, &query_tokens, &mut patterns);
    }

    // Sort by score descending, then by name for stability.
    patterns.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });

    // Search conventions via FTS5.
    let convention_data = query_convention(conn, branch_id, trimmed).unwrap_or_else(|e| {
        tracing::warn!("Convention search failed, returning empty: {e}");
        QueryConventionData {
            conventions: Vec::new(),
        }
    });

    let pattern_count = patterns.len();
    let convention_count = convention_data.conventions.len();

    let next_steps = build_next_steps(pattern_count, convention_count);

    Ok(CodePatternData {
        patterns,
        related_conventions: convention_data.conventions,
        metadata: CodePatternMetadata {
            query: trimmed.to_owned(),
            pattern_count,
            convention_count,
            search_type: "keyword".to_owned(),
            next_steps,
        },
    })
}

// ── Internal helpers ─────────────────────────────────────────

/// Load and deserialize all IR files for a branch.
pub(crate) fn load_branch_ir(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
) -> Result<Vec<ProjectFile>, GraphError> {
    let conn_guard = crate::lock_conn(conn)?;

    let mut stmt = conn_guard
        .prepare("SELECT ir_data FROM files_ir WHERE branch_id = ?1")
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Failed to prepare IR query: {e}"
            )))
        })?;

    let rows = stmt
        .query_map(params![branch_id], |row| {
            let ir_data: Vec<u8> = row.get(0)?;
            Ok(ir_data)
        })
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Failed to query files_ir: {e}"
            )))
        })?;

    let mut files = Vec::new();
    for row in rows {
        match row {
            Ok(ir_data) => match deserialize_ir(&ir_data) {
                Ok(project_file) => files.push(project_file),
                Err(e) => {
                    tracing::warn!("Skipping file with stale/corrupt IR: {e}");
                }
            },
            Err(e) => {
                tracing::warn!("Skipping IR row due to read error: {e}");
            }
        }
    }

    Ok(files)
}

/// Score a candidate name against query tokens.
///
/// Returns the best score across all tokens:
/// - 1.0 for exact match (case-insensitive)
/// - 0.7 for prefix match
/// - 0.4 for substring (contains) match
/// - 0.0 for no match
fn score_name(name: &str, query_tokens: &[&str]) -> f64 {
    let name_lower = name.to_lowercase();
    let mut best_score = 0.0_f64;

    for &token in query_tokens {
        let score = if name_lower == token {
            1.0
        } else if name_lower.starts_with(token) {
            0.7
        } else if name_lower.contains(token) {
            0.4
        } else {
            0.0
        };
        best_score = best_score.max(score);
    }

    best_score
}

/// Truncate a snippet to the code pattern limit (10 lines).
fn truncate_pattern_snippet(raw: &str) -> CodeSnippet {
    let lines: Vec<&str> = raw.lines().collect();
    if lines.len() > MAX_PATTERN_SNIPPET_LINES {
        CodeSnippet {
            content: lines[..MAX_PATTERN_SNIPPET_LINES].join("\n"),
            truncated: true,
        }
    } else {
        CodeSnippet {
            content: raw.to_owned(),
            truncated: false,
        }
    }
}

/// Build a synthetic snippet from a function's metadata.
fn function_snippet(f: &seshat_core::Function, file_path: &str) -> String {
    let vis = if f.is_public { "pub " } else { "" };
    let async_kw = if f.is_async { "async " } else { "" };
    let params = f.parameters.join(", ");
    format!(
        "// {file_path}:{}\n{vis}{async_kw}fn {}({params})",
        f.line, f.name
    )
}

/// Build a synthetic snippet from a type's metadata.
fn type_snippet(t: &seshat_core::TypeDef, file_path: &str) -> String {
    let vis = if t.is_public { "pub " } else { "" };
    let kind = format!("{:?}", t.kind).to_lowercase();
    format!("// {file_path}:{}\n{vis}{kind} {}", t.line, t.name)
}

/// Build a synthetic snippet from an export's metadata.
fn export_snippet(e: &seshat_core::Export, file_path: &str) -> String {
    let default = if e.is_default { "default " } else { "" };
    let type_only = if e.is_type_only { "type " } else { "" };
    format!(
        "// {file_path}:{}\nexport {default}{type_only}{}",
        e.line, e.name
    )
}

/// Search functions in a file and add matching results.
fn search_functions(
    file: &ProjectFile,
    file_path: &str,
    query_tokens: &[&str],
    results: &mut Vec<PatternResult>,
) {
    for f in &file.functions {
        let score = score_name(&f.name, query_tokens);
        if score > 0.0 {
            let snippet_raw = function_snippet(f, file_path);
            results.push(PatternResult {
                name: f.name.clone(),
                kind: "function".to_owned(),
                file_path: file_path.to_owned(),
                line: f.line,
                end_line: f.end_line,
                is_public: f.is_public,
                snippet: truncate_pattern_snippet(&snippet_raw),
                score,
            });
        }
    }
}

/// Search types in a file and add matching results.
fn search_types(
    file: &ProjectFile,
    file_path: &str,
    query_tokens: &[&str],
    results: &mut Vec<PatternResult>,
) {
    for t in &file.types {
        let score = score_name(&t.name, query_tokens);
        if score > 0.0 {
            let snippet_raw = type_snippet(t, file_path);
            results.push(PatternResult {
                name: t.name.clone(),
                kind: "type".to_owned(),
                file_path: file_path.to_owned(),
                line: t.line,
                end_line: t.line, // TypeDef has no end_line, use line
                is_public: t.is_public,
                snippet: truncate_pattern_snippet(&snippet_raw),
                score,
            });
        }
    }
}

/// Search exports in a file and add matching results.
fn search_exports(
    file: &ProjectFile,
    file_path: &str,
    query_tokens: &[&str],
    results: &mut Vec<PatternResult>,
) {
    for e in &file.exports {
        let score = score_name(&e.name, query_tokens);
        if score > 0.0 {
            let snippet_raw = export_snippet(e, file_path);
            results.push(PatternResult {
                name: e.name.clone(),
                kind: "export".to_owned(),
                file_path: file_path.to_owned(),
                line: e.line,
                end_line: e.line, // Export has no end_line, use line
                is_public: true,  // Exports are inherently public
                snippet: truncate_pattern_snippet(&snippet_raw),
                score,
            });
        }
    }
}

/// Build contextual next_steps suggestions.
fn build_next_steps(pattern_count: usize, convention_count: usize) -> Vec<String> {
    let mut steps = Vec::new();

    if pattern_count > 0 {
        steps.push(
            "Call query_dependencies on matching files to understand blast radius".to_owned(),
        );
        steps.push("Call validate_approach to check for convention violations".to_owned());
    } else {
        steps.push("Try broader search terms or check if the codebase has been scanned".to_owned());
    }

    if convention_count > 0 {
        steps.push("Review related conventions before implementing new code".to_owned());
    }

    steps
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

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

    /// Helper: create a sample ProjectFile with functions, types, and exports.
    fn sample_project_file(path: &str) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::Rust,
            content_hash: "abc123".to_owned(),
            imports: Vec::new(),
            exports: vec![
                Export {
                    name: "query_convention".to_owned(),
                    is_default: false,
                    is_type_only: false,
                    line: 1,
                },
                Export {
                    name: "QueryConventionData".to_owned(),
                    is_default: false,
                    is_type_only: true,
                    line: 2,
                },
            ],
            functions: vec![
                Function {
                    name: "query_convention".to_owned(),
                    is_public: true,
                    is_async: false,
                    line: 10,
                    end_line: 50,
                    parameters: vec![
                        "conn".to_owned(),
                        "branch_id".to_owned(),
                        "topic".to_owned(),
                    ],
                },
                Function {
                    name: "enrich_convention".to_owned(),
                    is_public: false,
                    is_async: false,
                    line: 52,
                    end_line: 80,
                    parameters: vec!["raw".to_owned()],
                },
                Function {
                    name: "handle_request".to_owned(),
                    is_public: true,
                    is_async: true,
                    line: 82,
                    end_line: 100,
                    parameters: vec!["req".to_owned()],
                },
            ],
            types: vec![
                TypeDef {
                    name: "QueryConventionData".to_owned(),
                    kind: TypeDefKind::Struct,
                    is_public: true,
                    line: 5,
                },
                TypeDef {
                    name: "ConventionResult".to_owned(),
                    kind: TypeDefKind::Struct,
                    is_public: true,
                    line: 8,
                },
            ],
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
        }
    }

    #[test]
    fn exact_name_match_scores_highest() {
        let conn = test_conn();
        let file = sample_project_file("src/conventions.rs");
        insert_ir(&conn, "main", &file);

        let result = query_code_pattern(&conn, "main", "query_convention").unwrap();
        assert!(!result.patterns.is_empty());

        // The exact match should be first and have score 1.0.
        let first = &result.patterns[0];
        assert_eq!(first.name, "query_convention");
        assert!((first.score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn prefix_match_scores_07() {
        let conn = test_conn();
        let file = sample_project_file("src/conventions.rs");
        insert_ir(&conn, "main", &file);

        let result = query_code_pattern(&conn, "main", "query").unwrap();
        assert!(!result.patterns.is_empty());

        // "query_convention" should match as prefix with score 0.7.
        let query_match = result
            .patterns
            .iter()
            .find(|p| p.name == "query_convention" && p.kind == "function");
        assert!(query_match.is_some());
        assert!((query_match.unwrap().score - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn substring_match_scores_04() {
        let conn = test_conn();
        let file = sample_project_file("src/conventions.rs");
        insert_ir(&conn, "main", &file);

        let result = query_code_pattern(&conn, "main", "convention").unwrap();
        assert!(!result.patterns.is_empty());

        // "query_convention" should match as substring with score 0.4.
        // "enrich_convention" should also match as substring.
        let query_match = result
            .patterns
            .iter()
            .find(|p| p.name == "query_convention" && p.kind == "function");
        assert!(query_match.is_some());
        assert!((query_match.unwrap().score - 0.4).abs() < f64::EPSILON);

        let enrich_match = result
            .patterns
            .iter()
            .find(|p| p.name == "enrich_convention");
        assert!(enrich_match.is_some());
        assert!((enrich_match.unwrap().score - 0.4).abs() < f64::EPSILON);
    }

    #[test]
    fn type_search_returns_types() {
        let conn = test_conn();
        let file = sample_project_file("src/conventions.rs");
        insert_ir(&conn, "main", &file);

        let result = query_code_pattern(&conn, "main", "QueryConventionData").unwrap();

        // Should find both the type and the export with that name.
        let type_match = result
            .patterns
            .iter()
            .find(|p| p.name == "QueryConventionData" && p.kind == "type");
        assert!(type_match.is_some());
        assert!((type_match.unwrap().score - 1.0).abs() < f64::EPSILON);
        assert!(type_match.unwrap().is_public);
    }

    #[test]
    fn convention_results_included() {
        let conn = test_conn();

        // Insert a convention node and rebuild FTS.
        {
            let c = conn.lock().unwrap();
            c.execute(
                "INSERT INTO nodes (branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data)
                 VALUES ('main', 'convention', 'strong', 0.9, 9, 10, 'Uses query pattern for data access', ?1)",
                params![serde_json::json!({
                    "source": "auto_detected",
                    "detector_name": "pattern_usage",
                    "trend": "stable",
                    "evidence": []
                }).to_string()],
            )
            .unwrap();
        }
        crate::fts::rebuild_fts_index(&conn).unwrap();

        // Insert IR too.
        let file = sample_project_file("src/conventions.rs");
        insert_ir(&conn, "main", &file);

        let result = query_code_pattern(&conn, "main", "query").unwrap();
        assert!(result.metadata.convention_count > 0);
        assert!(!result.related_conventions.is_empty());
    }

    #[test]
    fn empty_query_returns_error() {
        let conn = test_conn();

        let result = query_code_pattern(&conn, "main", "");
        assert!(result.is_err());
        match result {
            Err(GraphError::InvalidInput(msg)) => {
                assert!(msg.contains("empty"));
            }
            other => panic!("Expected InvalidInput, got: {other:?}"),
        }

        // Also whitespace-only.
        let result = query_code_pattern(&conn, "main", "   ");
        assert!(result.is_err());
    }

    #[test]
    fn no_results_returns_empty_arrays() {
        let conn = test_conn();
        let file = sample_project_file("src/conventions.rs");
        insert_ir(&conn, "main", &file);

        let result = query_code_pattern(&conn, "main", "nonexistent_xyz_999").unwrap();
        assert!(result.patterns.is_empty());
        assert_eq!(result.metadata.pattern_count, 0);
        assert_eq!(result.metadata.search_type, "keyword");
    }

    #[test]
    fn results_sorted_by_score_descending() {
        let conn = test_conn();
        let file = sample_project_file("src/conventions.rs");
        insert_ir(&conn, "main", &file);

        // "query" matches: "query_convention" (prefix=0.7), "handle_request" (no match)
        // plus types/exports that contain "query"
        let result = query_code_pattern(&conn, "main", "query").unwrap();

        // All results should be sorted by score descending.
        for window in result.patterns.windows(2) {
            assert!(
                window[0].score >= window[1].score,
                "Results not sorted by score: {} ({}) >= {} ({})",
                window[0].name,
                window[0].score,
                window[1].name,
                window[1].score,
            );
        }
    }

    #[test]
    fn snippet_truncation_works() {
        let long_snippet = (1..=15)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = truncate_pattern_snippet(&long_snippet);
        assert!(result.truncated);
        assert_eq!(result.content.lines().count(), MAX_PATTERN_SNIPPET_LINES);

        let short_snippet = "line 1\nline 2\nline 3";
        let result = truncate_pattern_snippet(short_snippet);
        assert!(!result.truncated);
    }

    #[test]
    fn score_name_function_works() {
        assert!((score_name("query_convention", &["query_convention"]) - 1.0).abs() < f64::EPSILON);
        assert!((score_name("query_convention", &["query"]) - 0.7).abs() < f64::EPSILON);
        assert!((score_name("query_convention", &["convention"]) - 0.4).abs() < f64::EPSILON);
        assert!((score_name("query_convention", &["nonexistent"]) - 0.0).abs() < f64::EPSILON);
        // Case insensitive.
        assert!(
            (score_name("QueryConventionData", &["queryconventiondata"]) - 1.0).abs()
                < f64::EPSILON
        );
        assert!((score_name("QueryConventionData", &["query"]) - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn metadata_has_correct_fields() {
        let conn = test_conn();
        let file = sample_project_file("src/conventions.rs");
        insert_ir(&conn, "main", &file);

        let result = query_code_pattern(&conn, "main", "query").unwrap();
        assert_eq!(result.metadata.query, "query");
        assert_eq!(result.metadata.search_type, "keyword");
        assert!(result.metadata.pattern_count > 0);
        assert!(!result.metadata.next_steps.is_empty());
    }
}
