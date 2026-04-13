//! Code pattern search over deserialized IR (functions, types, exports).
//!
//! Provides `query_code_pattern()` which searches `files_ir` blobs by name
//! matching with scored results, plus related conventions via FTS5.
//!
//! When an embedding provider is configured, `query_code_pattern_with_embeddings()`
//! additionally performs vector similarity search and merges results.
//!
//! Scoring: exact match (1.0) > prefix match (0.7) > contains (0.4).
//! Vector results use cosine similarity (0.0–1.0).
//! Results are sorted by score descending.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};
use serde::Serialize;
use seshat_core::{CodeSnippet, ProjectFile};
use seshat_embedding::EmbeddingProvider;
use seshat_storage::{EmbeddingRow, bytes_to_f32s, deserialize_ir};

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
    /// Internal search type used by the MCP handler (not serialized).
    #[serde(skip)]
    pub search_type: String,
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

// ── Public API ───────────────────────────────────────────────

/// Search deserialized IR for code patterns matching the query (keyword only).
///
/// Searches function names, type names, and export names in all files for the
/// given branch. Also searches conventions via FTS5 for related conventions.
///
/// This is the backward-compatible entry point. For vector search support,
/// use [`query_code_pattern_with_embeddings`] instead.
///
/// Returns `Err(GraphError::InvalidInput)` for empty queries.
/// Returns empty arrays (not an error) when no results match.
pub fn query_code_pattern(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    query: &str,
) -> Result<CodePatternData, GraphError> {
    query_code_pattern_with_embeddings(conn, branch_id, query, None)
}

/// Search deserialized IR for code patterns with optional vector similarity.
///
/// When `provider` is `Some`, embeds the query text and performs cosine
/// similarity search against stored code embeddings, then merges with
/// keyword (FTS5) results. When `provider` is `None`, behaves identically
/// to [`query_code_pattern`].
///
/// - `search_type` in metadata is `"keyword"` (FTS5 only) or `"semantic"`
///   (FTS5 + vector).
/// - Provider errors degrade gracefully to keyword-only search with a warning.
///
/// Returns `Err(GraphError::InvalidInput)` for empty queries.
/// Returns empty arrays (not an error) when no results match.
pub fn query_code_pattern_with_embeddings(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    query: &str,
    provider: Option<&dyn EmbeddingProvider>,
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

    // 1. Keyword search over IR.
    let mut keyword_patterns = Vec::new();
    for file in &files {
        let file_path = file.path.to_string_lossy().to_string();
        search_functions(file, &file_path, &query_tokens, &mut keyword_patterns);
        search_types(file, &file_path, &query_tokens, &mut keyword_patterns);
        search_exports(file, &file_path, &query_tokens, &mut keyword_patterns);
    }

    // 2. Vector search (if provider is available).
    let (vector_patterns, used_vector) = match provider {
        Some(prov) => match vector_search(conn, branch_id, trimmed, prov, &files) {
            Ok(results) => (results, true),
            Err(e) => {
                tracing::warn!("Vector search failed, falling back to keyword-only: {e}");
                (Vec::new(), false)
            }
        },
        None => (Vec::new(), false),
    };

    // 3. Merge keyword + vector results.
    let patterns = merge_results(keyword_patterns, vector_patterns);

    // Search conventions via FTS5.
    let convention_data = query_convention(conn, branch_id, trimmed).unwrap_or_else(|e| {
        tracing::warn!("Convention search failed, returning empty: {e}");
        QueryConventionData {
            conventions: Vec::new(),
        }
    });

    let search_type = if used_vector { "semantic" } else { "keyword" };

    Ok(CodePatternData {
        patterns,
        related_conventions: convention_data.conventions,
        search_type: search_type.to_owned(),
    })
}

// ── Internal helpers ─────────────────────────────────────────

/// Maximum number of IR files to load for a single query.
///
/// Safety limit to prevent OOM on very large repositories. When exceeded,
/// results are truncated and a warning is logged.
const MAX_IR_FILES: usize = 10_000;

/// Load and deserialize all IR files for a branch.
pub(crate) fn load_branch_ir(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
) -> Result<Vec<ProjectFile>, GraphError> {
    let conn_guard = crate::lock_conn(conn)?;

    let mut stmt = conn_guard
        .prepare("SELECT ir_data FROM files_ir WHERE branch_id = ?1 LIMIT ?2")
        .map_err(|e| GraphError::query(format!("Failed to prepare IR query: {e}")))?;

    let rows = stmt
        .query_map(params![branch_id, MAX_IR_FILES as i64], |row| {
            let ir_data: Vec<u8> = row.get(0)?;
            Ok(ir_data)
        })
        .map_err(|e| GraphError::query(format!("Failed to query files_ir: {e}")))?;

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

    if files.len() >= MAX_IR_FILES {
        tracing::warn!(
            "Loaded {MAX_IR_FILES} IR files (limit reached) — results may be incomplete for large repositories"
        );
    }

    Ok(files)
}

// ── Vector search helpers ────────────────────────────────────

/// IR snippet data for a single code item: `(line, end_line, is_public, snippet)`.
type IrSnippetData = (usize, usize, bool, CodeSnippet);

/// Lookup key for IR snippet data: `(file_path, item_name, item_kind)`.
type IrLookupKey = (String, String, String);

/// Compute cosine similarity between two f32 vectors.
///
/// Returns a value in `[-1.0, 1.0]` for normalised vectors, or `0.0` if
/// either vector has zero magnitude, mismatched lengths, or the result
/// is non-finite (NaN/Infinity from corrupted inputs).
///
/// No SQLite extension needed — pure Rust dot-product computation.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0_f32;
    let mut mag_a = 0.0_f32;
    let mut mag_b = 0.0_f32;

    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        mag_a += x * x;
        mag_b += y * y;
    }

    let denom = mag_a.sqrt() * mag_b.sqrt();
    if denom == 0.0 {
        return 0.0;
    }
    let result = dot / denom;
    // Guard against NaN/Infinity from corrupted embedding data.
    if result.is_finite() { result } else { 0.0 }
}

/// Maximum number of embeddings to load for vector search.
const MAX_EMBEDDINGS: usize = 50_000;

/// Load embeddings for a branch from the `code_embeddings` table.
fn load_branch_embeddings(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
) -> Result<Vec<EmbeddingRow>, GraphError> {
    let conn_guard = crate::lock_conn(conn)?;

    let mut stmt = conn_guard
        .prepare(
            "SELECT branch_id, file_path, item_name, item_kind, embedding
             FROM code_embeddings WHERE branch_id = ?1 LIMIT ?2",
        )
        .map_err(|e| GraphError::query(format!("Failed to prepare embeddings query: {e}")))?;

    let rows = stmt
        .query_map(params![branch_id, MAX_EMBEDDINGS as i64], |row| {
            let blob: Vec<u8> = row.get(4)?;
            Ok(EmbeddingRow {
                branch_id: row.get(0)?,
                file_path: row.get(1)?,
                item_name: row.get(2)?,
                item_kind: row.get(3)?,
                embedding: bytes_to_f32s(&blob),
            })
        })
        .map_err(|e| GraphError::query(format!("Failed to query code_embeddings: {e}")))?;

    let mut result = Vec::new();
    for row in rows {
        match row {
            Ok(emb) => result.push(emb),
            Err(e) => {
                tracing::warn!("Skipping embedding row due to read error: {e}");
            }
        }
    }

    Ok(result)
}

/// Perform vector similarity search.
///
/// 1. Embed the query text using the provider.
/// 2. Load all stored embeddings for the branch.
/// 3. Compute cosine similarity between the query embedding and each stored embedding.
/// 4. Build `PatternResult`s for items with positive similarity, using IR for snippet data.
///
/// Returns a `Vec<PatternResult>` scored by cosine similarity (mapped to 0.0–1.0 range).
fn vector_search(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    query: &str,
    provider: &dyn EmbeddingProvider,
    files: &[ProjectFile],
) -> Result<Vec<PatternResult>, GraphError> {
    // Embed the query text.
    let query_text = query.to_owned();
    let query_embeddings = provider
        .embed(&[query_text])
        .map_err(|e| GraphError::query(format!("Embedding provider error: {e}")))?;

    if query_embeddings.is_empty() || query_embeddings[0].is_empty() {
        return Ok(Vec::new());
    }

    let query_vec = &query_embeddings[0];

    // Load stored embeddings for this branch.
    let embeddings = load_branch_embeddings(conn, branch_id)?;
    if embeddings.is_empty() {
        return Ok(Vec::new());
    }

    // Build a lookup map from (file_path, item_name, item_kind) → IR snippet data.
    let ir_lookup = build_ir_lookup(files);

    // Compute cosine similarity for each embedding and build results.
    let mut results = Vec::new();
    for emb_row in &embeddings {
        let sim = cosine_similarity(query_vec, &emb_row.embedding);

        // Only include results with positive similarity.
        if sim <= 0.0 {
            continue;
        }

        // Clamp to [0.0, 1.0] for scoring.
        let score = (sim as f64).clamp(0.0, 1.0);

        let key = (
            emb_row.file_path.clone(),
            emb_row.item_name.clone(),
            emb_row.item_kind.clone(),
        );

        let (line, end_line, is_public, snippet) =
            ir_lookup.get(&key).cloned().unwrap_or_else(|| {
                (
                    0,
                    0,
                    false,
                    CodeSnippet {
                        content: String::new(),
                        truncated: false,
                    },
                )
            });

        results.push(PatternResult {
            name: emb_row.item_name.clone(),
            kind: emb_row.item_kind.clone(),
            file_path: emb_row.file_path.clone(),
            line,
            end_line,
            is_public,
            snippet,
            score,
        });
    }

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(results)
}

/// Build a lookup map from `(file_path, item_name, item_kind)` to IR snippet data.
///
/// Uses owned `String` keys so the map is self-contained and outlives
/// temporary `Cow<str>` values from `to_string_lossy()`.
///
/// When a file contains duplicate names (e.g., overloaded functions or
/// re-exports), the first occurrence is kept.
fn build_ir_lookup(files: &[ProjectFile]) -> HashMap<IrLookupKey, IrSnippetData> {
    let mut map = HashMap::new();

    for file in files {
        let file_path = file.path.to_string_lossy().to_string();

        for f in &file.functions {
            let key = (file_path.clone(), f.name.clone(), "function".to_owned());
            map.entry(key).or_insert_with(|| {
                let snippet_raw = function_snippet(f, &file_path);
                (
                    f.line,
                    f.end_line,
                    f.is_public,
                    truncate_pattern_snippet(&snippet_raw),
                )
            });
        }
        for t in &file.types {
            let key = (file_path.clone(), t.name.clone(), "type".to_owned());
            map.entry(key).or_insert_with(|| {
                let snippet_raw = type_snippet(t, &file_path);
                (
                    t.line,
                    t.line,
                    t.is_public,
                    truncate_pattern_snippet(&snippet_raw),
                )
            });
        }
        for e in &file.exports {
            let key = (file_path.clone(), e.name.clone(), "export".to_owned());
            map.entry(key).or_insert_with(|| {
                let snippet_raw = export_snippet(e, &file_path);
                (e.line, e.line, true, truncate_pattern_snippet(&snippet_raw))
            });
        }
    }

    map
}

/// Merge keyword and vector search results.
///
/// For items that appear in both result sets (same file_path + name + kind),
/// takes the maximum score. For items in only one set, keeps them as-is.
/// Final results are sorted by score descending, then by name for stability.
fn merge_results(
    keyword_results: Vec<PatternResult>,
    vector_results: Vec<PatternResult>,
) -> Vec<PatternResult> {
    if vector_results.is_empty() {
        // Fast path: just sort keyword results.
        let mut results = keyword_results;
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.name.cmp(&b.name))
        });
        return results;
    }

    // Use a map keyed by (file_path, name, kind) to deduplicate.
    let mut merged: HashMap<(String, String, String), PatternResult> = HashMap::new();

    for result in keyword_results {
        let key = (
            result.file_path.clone(),
            result.name.clone(),
            result.kind.clone(),
        );
        merged.insert(key, result);
    }

    for result in vector_results {
        let key = (
            result.file_path.clone(),
            result.name.clone(),
            result.kind.clone(),
        );
        merged
            .entry(key)
            .and_modify(|existing| {
                // When vector score is higher, replace the entire result
                // (keyword snippet may be synthetic while vector snippet has
                // richer context from IR lookup).
                if result.score > existing.score {
                    *existing = result.clone();
                }
            })
            .or_insert(result);
    }

    let mut results: Vec<PatternResult> = merged.into_values().collect();
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });

    results
}

// ── Keyword search helpers ──────────────────────────────────

/// Normalize a name by converting to lowercase and replacing common separators
/// (`-`, `.`) with underscores for consistent matching.
///
/// E.g., `"error-handler"`, `"error.handler"`, and `"error_handler"` all
/// normalize to `"error_handler"`.
fn normalize_name(name: &str) -> String {
    name.to_lowercase().replace(['-', '.'], "_")
}

/// Score a candidate name against query tokens.
///
/// Returns the best score across all tokens:
/// - 1.0 for exact match (case-insensitive, separator-normalized)
/// - 0.7 for prefix match
/// - 0.4 for substring (contains) match
/// - 0.0 for no match
fn score_name(name: &str, query_tokens: &[&str]) -> f64 {
    let name_norm = normalize_name(name);
    let mut best_score = 0.0_f64;

    for &token in query_tokens {
        let token_norm = normalize_name(token);
        let score = if name_norm == token_norm {
            1.0
        } else if name_norm.starts_with(&token_norm) {
            0.7
        } else if name_norm.contains(&token_norm) {
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
    seshat_core::truncate_snippet_to(raw, MAX_PATTERN_SNIPPET_LINES)
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

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use seshat_core::{
        Export, Function, Language, LanguageIR, ProjectFile, RustIR, TypeDef, TypeDefKind,
    };
    use seshat_embedding::{EmbeddingError, EmbeddingProvider};
    use seshat_storage::f32s_to_bytes;

    use crate::test_helpers::{insert_ir, test_conn};

    // ── Mock embedding provider ──────────────────────────────────

    /// Mock provider that returns deterministic embeddings for testing.
    /// Embeds each text as a vector where the first element is the string length / 100.0
    /// and the rest are zeros. This gives us predictable cosine similarity scores.
    #[derive(Debug)]
    struct MockEmbeddingProvider {
        dim: usize,
        error: Option<String>,
    }

    impl MockEmbeddingProvider {
        fn new(dim: usize) -> Self {
            Self { dim, error: None }
        }

        fn with_error(dim: usize, msg: &str) -> Self {
            Self {
                dim,
                error: Some(msg.to_owned()),
            }
        }
    }

    impl EmbeddingProvider for MockEmbeddingProvider {
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
            if let Some(ref msg) = self.error {
                return Err(EmbeddingError::ProviderError(msg.clone()));
            }
            Ok(texts
                .iter()
                .map(|t| {
                    let mut vec = vec![0.0_f32; self.dim];
                    // Use text length as first component for deterministic similarity.
                    vec[0] = t.len() as f32 / 100.0;
                    // Use a second component based on first char for differentiation.
                    if let Some(c) = t.chars().next() {
                        if self.dim > 1 {
                            vec[1] = (c as u32) as f32 / 1000.0;
                        }
                    }
                    vec
                })
                .collect())
        }

        fn dimension(&self) -> usize {
            self.dim
        }
    }

    /// Helper: insert an embedding directly into the database.
    fn insert_embedding(
        conn: &Arc<Mutex<Connection>>,
        branch_id: &str,
        file_path: &str,
        item_name: &str,
        item_kind: &str,
        embedding: &[f32],
    ) {
        let c = conn.lock().unwrap();
        let blob = f32s_to_bytes(embedding);
        c.execute(
            "INSERT INTO code_embeddings (branch_id, file_path, item_name, item_kind, embedding)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![branch_id, file_path, item_name, item_kind, blob],
        )
        .expect("insert embedding");
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
                    doc_comment: None,
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
                    doc_comment: None,
                    end_line: 80,
                    parameters: vec!["raw".to_owned()],
                },
                Function {
                    name: "handle_request".to_owned(),
                    is_public: true,
                    is_async: true,
                    line: 82,
                    doc_comment: None,
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
                    doc_comment: None,
                },
                TypeDef {
                    name: "ConventionResult".to_owned(),
                    kind: TypeDefKind::Struct,
                    is_public: true,
                    line: 8,
                    doc_comment: None,
                },
            ],
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
            file_doc: None,
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
        assert_eq!(result.search_type, "keyword");
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
    fn search_type_is_keyword_without_provider() {
        let conn = test_conn();
        let file = sample_project_file("src/conventions.rs");
        insert_ir(&conn, "main", &file);

        let result = query_code_pattern(&conn, "main", "query").unwrap();
        assert_eq!(result.search_type, "keyword");
        assert!(!result.patterns.is_empty());
    }

    // ── Cosine similarity tests ──────────────────────────────────

    #[test]
    fn cosine_similarity_identical_vectors() {
        let a = vec![1.0_f32, 0.0, 0.0];
        let b = vec![1.0_f32, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 1.0).abs() < 1e-6, "Expected 1.0, got {sim}");
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors() {
        let a = vec![1.0_f32, 0.0, 0.0];
        let b = vec![0.0_f32, 1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6, "Expected 0.0, got {sim}");
    }

    #[test]
    fn cosine_similarity_opposite_vectors() {
        let a = vec![1.0_f32, 0.0];
        let b = vec![-1.0_f32, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - (-1.0)).abs() < 1e-6, "Expected -1.0, got {sim}");
    }

    #[test]
    fn cosine_similarity_zero_vector() {
        let a = vec![0.0_f32, 0.0, 0.0];
        let b = vec![1.0_f32, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 0.0).abs() < 1e-6, "Expected 0.0, got {sim}");
    }

    #[test]
    fn cosine_similarity_different_lengths() {
        let a = vec![1.0_f32, 2.0];
        let b = vec![1.0_f32, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b);
        assert!(
            (sim - 0.0).abs() < 1e-6,
            "Expected 0.0 for mismatched lengths"
        );
    }

    #[test]
    fn cosine_similarity_empty_vectors() {
        let sim = cosine_similarity(&[], &[]);
        assert!((sim - 0.0).abs() < 1e-6, "Expected 0.0 for empty");
    }

    #[test]
    fn cosine_similarity_similar_vectors() {
        // Two very similar (but not identical) vectors should have high similarity.
        let a = vec![1.0_f32, 2.0, 3.0];
        let b = vec![1.0_f32, 2.0, 3.1];
        let sim = cosine_similarity(&a, &b);
        assert!(sim > 0.99, "Expected > 0.99, got {sim}");
    }

    // ── Vector search integration tests ──────────────────────────

    #[test]
    fn vector_search_with_embeddings_returns_semantic_search_type() {
        let conn = test_conn();
        let file = sample_project_file("src/conventions.rs");
        insert_ir(&conn, "main", &file);

        let provider = MockEmbeddingProvider::new(4);

        // Insert embeddings for the IR items using the same format as scan.
        // Embed "function query_convention in src/conventions.rs" etc.
        // Use the mock provider's algorithm to generate matching embeddings.
        let texts: Vec<String> = vec![
            "function query_convention in src/conventions.rs".to_owned(),
            "function enrich_convention in src/conventions.rs".to_owned(),
            "function handle_request in src/conventions.rs".to_owned(),
        ];
        let embeddings = provider.embed(&texts).unwrap();

        insert_embedding(
            &conn,
            "main",
            "src/conventions.rs",
            "query_convention",
            "function",
            &embeddings[0],
        );
        insert_embedding(
            &conn,
            "main",
            "src/conventions.rs",
            "enrich_convention",
            "function",
            &embeddings[1],
        );
        insert_embedding(
            &conn,
            "main",
            "src/conventions.rs",
            "handle_request",
            "function",
            &embeddings[2],
        );

        let result =
            query_code_pattern_with_embeddings(&conn, "main", "query_convention", Some(&provider))
                .unwrap();

        assert_eq!(result.search_type, "semantic");
        assert!(!result.patterns.is_empty());
    }

    #[test]
    fn vector_search_ranking_scores_higher_for_similar() {
        let conn = test_conn();
        let file = sample_project_file("src/conventions.rs");
        insert_ir(&conn, "main", &file);

        let provider = MockEmbeddingProvider::new(4);

        // Insert embeddings: one that's very similar to query, one that's different.
        // "query_convention" will produce embedding based on text length.
        // Store one embedding that matches the query embedding well,
        // and another that doesn't.
        let query_text = "query_convention".to_owned();
        let query_emb = provider.embed(&[query_text]).unwrap();
        let similar_emb = query_emb[0].clone(); // Identical to query → cosine = 1.0
        let different_emb = vec![0.0_f32, 0.0, 0.0, 1.0]; // Orthogonal

        insert_embedding(
            &conn,
            "main",
            "src/conventions.rs",
            "query_convention",
            "function",
            &similar_emb,
        );
        insert_embedding(
            &conn,
            "main",
            "src/conventions.rs",
            "handle_request",
            "function",
            &different_emb,
        );

        let result =
            query_code_pattern_with_embeddings(&conn, "main", "query_convention", Some(&provider))
                .unwrap();

        // query_convention should appear with high score (keyword exact=1.0 merged with vector=1.0).
        let qc = result
            .patterns
            .iter()
            .find(|p| p.name == "query_convention" && p.kind == "function");
        assert!(qc.is_some());
        assert!(
            qc.unwrap().score >= 0.9,
            "Expected high score, got {}",
            qc.unwrap().score
        );
    }

    #[test]
    fn graceful_degradation_on_provider_error() {
        let conn = test_conn();
        let file = sample_project_file("src/conventions.rs");
        insert_ir(&conn, "main", &file);

        // Provider that always errors.
        let provider = MockEmbeddingProvider::with_error(4, "connection refused");

        // Should still return keyword results, just with "keyword" search type.
        let result =
            query_code_pattern_with_embeddings(&conn, "main", "query_convention", Some(&provider))
                .unwrap();

        // Provider error → falls back to keyword only.
        assert_eq!(result.search_type, "keyword");
        // Keyword search still works.
        assert!(!result.patterns.is_empty());
        let exact = result
            .patterns
            .iter()
            .find(|p| p.name == "query_convention" && p.kind == "function");
        assert!(exact.is_some());
    }

    #[test]
    fn merged_result_ordering() {
        let conn = test_conn();
        let file = sample_project_file("src/conventions.rs");
        insert_ir(&conn, "main", &file);

        let provider = MockEmbeddingProvider::new(4);

        // Insert embeddings: handle_request gets a very similar embedding to query,
        // while query_convention gets a less similar one.
        // This way vector search boosts handle_request above its keyword score.
        let query_text = "handle".to_owned();
        let query_emb = provider.embed(&[query_text]).unwrap();

        // handle_request: identical embedding to query → cosine = 1.0
        let handle_emb = query_emb[0].clone();
        // query_convention: orthogonal → cosine ~0
        let query_conv_emb = vec![0.0_f32, 0.0, 0.0, 1.0];

        insert_embedding(
            &conn,
            "main",
            "src/conventions.rs",
            "handle_request",
            "function",
            &handle_emb,
        );
        insert_embedding(
            &conn,
            "main",
            "src/conventions.rs",
            "query_convention",
            "function",
            &query_conv_emb,
        );

        let result =
            query_code_pattern_with_embeddings(&conn, "main", "handle", Some(&provider)).unwrap();

        assert_eq!(result.search_type, "semantic");

        // handle_request should be top result (keyword prefix=0.7, vector=1.0 → merged=1.0).
        let first = &result.patterns[0];
        assert_eq!(first.name, "handle_request");

        // All results sorted by score descending.
        for window in result.patterns.windows(2) {
            assert!(
                window[0].score >= window[1].score,
                "Merged results not sorted: {} ({}) >= {} ({})",
                window[0].name,
                window[0].score,
                window[1].name,
                window[1].score,
            );
        }
    }

    #[test]
    fn no_embeddings_stored_still_works_with_provider() {
        let conn = test_conn();
        let file = sample_project_file("src/conventions.rs");
        insert_ir(&conn, "main", &file);

        let provider = MockEmbeddingProvider::new(4);

        // No embeddings inserted → vector search returns empty, falls back to keyword.
        let result =
            query_code_pattern_with_embeddings(&conn, "main", "query_convention", Some(&provider))
                .unwrap();

        // Still semantic because provider was available and didn't error.
        assert_eq!(result.search_type, "semantic");
        // Keyword results still present.
        assert!(!result.patterns.is_empty());
    }

    #[test]
    fn without_provider_returns_keyword_search_type() {
        let conn = test_conn();
        let file = sample_project_file("src/conventions.rs");
        insert_ir(&conn, "main", &file);

        let result =
            query_code_pattern_with_embeddings(&conn, "main", "query_convention", None).unwrap();

        assert_eq!(result.search_type, "keyword");
        assert!(!result.patterns.is_empty());
    }

    #[test]
    fn merge_results_deduplicates_by_key() {
        // Two results with same (file_path, name, kind) should be merged, keeping max score.
        let keyword = vec![PatternResult {
            name: "foo".to_owned(),
            kind: "function".to_owned(),
            file_path: "src/a.rs".to_owned(),
            line: 10,
            end_line: 20,
            is_public: true,
            snippet: CodeSnippet {
                content: "fn foo()".to_owned(),
                truncated: false,
            },
            score: 0.7,
        }];
        let vector = vec![PatternResult {
            name: "foo".to_owned(),
            kind: "function".to_owned(),
            file_path: "src/a.rs".to_owned(),
            line: 10,
            end_line: 20,
            is_public: true,
            snippet: CodeSnippet {
                content: "fn foo()".to_owned(),
                truncated: false,
            },
            score: 0.9,
        }];

        let merged = merge_results(keyword, vector);
        assert_eq!(merged.len(), 1);
        assert!((merged[0].score - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn merge_results_includes_unique_from_both() {
        let keyword = vec![PatternResult {
            name: "keyword_only".to_owned(),
            kind: "function".to_owned(),
            file_path: "src/a.rs".to_owned(),
            line: 10,
            end_line: 20,
            is_public: true,
            snippet: CodeSnippet {
                content: "fn keyword_only()".to_owned(),
                truncated: false,
            },
            score: 0.7,
        }];
        let vector = vec![PatternResult {
            name: "vector_only".to_owned(),
            kind: "function".to_owned(),
            file_path: "src/b.rs".to_owned(),
            line: 5,
            end_line: 15,
            is_public: false,
            snippet: CodeSnippet {
                content: "fn vector_only()".to_owned(),
                truncated: false,
            },
            score: 0.8,
        }];

        let merged = merge_results(keyword, vector);
        assert_eq!(merged.len(), 2);
        // vector_only has higher score, should be first.
        assert_eq!(merged[0].name, "vector_only");
        assert_eq!(merged[1].name, "keyword_only");
    }
}
