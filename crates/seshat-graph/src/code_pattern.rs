//! Code pattern search backed by the `symbol_definitions` SQL index.
//!
//! Provides `query_code_pattern()` which probes the V13 `symbol_definitions`
//! table by `symbol_name` with scored results, plus related conventions via
//! FTS5.  IR is still loaded (lazily) for call-site enrichment and the
//! optional embedding-similarity path, but the symbol-by-name match itself
//! no longer iterates over deserialized IR blobs.
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
use seshat_core::{CodeSnippet, LanguageIR, MAX_DEFINITION_SNIPPET_LINES, ProjectFile};
use seshat_embedding::EmbeddingProvider;
use seshat_storage::{EmbeddingRow, bytes_to_f32s, deserialize_ir};

use crate::conventions::{ConventionResult, QueryConventionData};
use crate::error::GraphError;
use crate::query_convention;

// ── Constants ────────────────────────────────────────────────

/// Maximum number of lines in a code pattern snippet before truncation.
///
/// Matches [`seshat_core::MAX_DEFINITION_SNIPPET_LINES`]; the SQL-indexed
/// `symbol_definitions.snippet` column is already truncated to this bound at
/// write time so the keyword path does not need to re-truncate.
const MAX_PATTERN_SNIPPET_LINES: usize = MAX_DEFINITION_SNIPPET_LINES;

// ── Response data types ──────────────────────────────────────

/// Result of loading IR files with truncation flag.
#[derive(Debug, Clone)]
pub(crate) struct LoadedIR {
    /// Deserialized IR files.
    pub files: Vec<ProjectFile>,
    /// Whether results were truncated (LIMIT reached).
    pub truncated: bool,
}

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
    /// Whether IR loading was truncated (LIMIT reached), meaning results
    /// may be incomplete for very large repositories.
    #[serde(default)]
    pub truncated: bool,
}

/// A single call-site example for a code pattern.
#[derive(Debug, Clone, Serialize)]
pub struct CallSiteResult {
    /// File where the call appears.
    pub file: String,
    /// 1-indexed line of the call expression opening.
    pub line: usize,
    /// 1-indexed line of the call expression closing (equals `line` for single-line calls).
    pub end_line: usize,
    /// Context snippet: a few lines before + the full call expression + a few lines after.
    pub snippet: String,
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
    /// Up to 5 call-site examples from across the codebase.
    ///
    /// Shows **where and how** this symbol is actually called, not just its
    /// definition.  Each entry includes a multi-line snippet with context
    /// before, the full call expression, and context after.
    pub call_sites: Vec<CallSiteResult>,
    /// Total number of call-site files matched (may be > `call_sites.len()`).
    pub call_site_count: usize,
}

// ── Public API ───────────────────────────────────────────────

/// Search code patterns matching the query (keyword only).
///
/// Probes `symbol_definitions` via SQL to find function / type / export rows
/// whose `symbol_name` matches the query, scored by exact > prefix > contains.
/// Also searches conventions via FTS5 for related conventions.
///
/// `kind` filters the SQL query to a single kind: `Some("function")`,
/// `Some("type")`, or `Some("export")`.  `None` (or `Some("all")`) returns all
/// kinds.
///
/// This is the no-embeddings entry point. For vector search support,
/// use [`query_code_pattern_with_embeddings`] instead.
///
/// Returns `Err(GraphError::InvalidInput)` for empty queries.
/// Returns empty arrays (not an error) when no results match.
pub fn query_code_pattern(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    query: &str,
    kind: Option<&str>,
) -> Result<CodePatternData, GraphError> {
    query_code_pattern_with_embeddings(conn, branch_id, query, kind, None)
}

/// Search code patterns with optional vector similarity.
///
/// When `provider` is `Some`, embeds the query text and performs cosine
/// similarity search against stored code embeddings, then merges with
/// keyword results. When `provider` is `None`, behaves identically
/// to [`query_code_pattern`].
///
/// - `search_type` in metadata is `"keyword"` (SQL probe only) or `"semantic"`
///   (SQL + vector).
/// - Provider errors degrade gracefully to keyword-only search with a warning.
/// - `kind` filters keyword results to a single kind at the SQL layer; the
///   vector path applies the same filter post-merge for parity.
///
/// Returns `Err(GraphError::InvalidInput)` for empty queries.
/// Returns empty arrays (not an error) when no results match.
pub fn query_code_pattern_with_embeddings(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    query: &str,
    kind: Option<&str>,
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

    // Normalise the kind filter: an empty string or `"all"` means "no filter".
    let kind_filter = kind
        .map(str::trim)
        .filter(|k| !k.is_empty() && !k.eq_ignore_ascii_case("all"))
        .map(str::to_ascii_lowercase);

    // 1. Keyword search over the `symbol_definitions` SQL index — replaces
    //    the previous full-IR iteration.  Kind filter is pushed into SQL.
    let keyword_patterns =
        search_symbol_definitions(conn, branch_id, &query_tokens, kind_filter.as_deref())?;

    // 2. Vector + call-site enrichment both need full IR.  Load it once,
    //    lazily — if the keyword probe found nothing AND no embedding
    //    provider is configured, there is nothing left to enrich, so we
    //    skip the deserialization cost entirely.
    let need_ir = !keyword_patterns.is_empty() || provider.is_some();
    let (files, truncated): (Vec<ProjectFile>, bool) = if need_ir {
        let loaded = load_branch_ir(conn, branch_id)?;
        (loaded.files, loaded.truncated)
    } else {
        (Vec::new(), false)
    };

    // 3. Vector search (if provider is available).
    //
    // Implementation choice: the embedding fallback keeps its IR-derived
    // snippet lookup (`build_ir_lookup`) rather than re-pointing at
    // `symbol_definitions.snippet`.  The IR is already in memory for the
    // call-site enrichment step below, so reusing it costs zero extra SQL.
    // Both paths render snippets via `seshat_core::symbol_snippet`, so the
    // two views of "the snippet for this symbol" cannot drift.
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

    // 4. Merge keyword + vector results.
    let mut patterns = merge_results(keyword_patterns, vector_patterns);

    // 4a. Vector results bypass the SQL kind filter — re-apply it here so the
    // combined output respects the user-requested kind regardless of source.
    if let Some(ref k) = kind_filter {
        patterns.retain(|p| p.kind == *k);
    }

    // 5. Enrich patterns with call-site evidence from function_calls IR.
    enrich_with_call_sites(&mut patterns, &files);

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
        truncated,
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
) -> Result<LoadedIR, GraphError> {
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

    let truncated = files.len() > MAX_IR_FILES;

    if truncated {
        tracing::warn!(
            "Loaded {MAX_IR_FILES} IR files (limit reached) — results may be incomplete for large repositories"
        );
    }
    Ok(LoadedIR { files, truncated })
}

// ── Vector search helpers ────────────────────────────────────

/// IR snippet data for a single code item: `(line, end_line, is_public, snippet)`.
type IrSnippetData = (usize, usize, bool, CodeSnippet);

/// Lookup key for IR snippet data: `(file_path, item_name, item_kind)`.
type IrLookupKey = (String, String, String);

/// Compute cosine similarity between two f32 vectors.
///
/// Accumulates dot product and magnitudes in f64 to prevent precision
/// loss for high-dimensional vectors (384d+). The final result is cast
/// back to f32, with a non-finite guard for corrupted inputs.
///
/// Returns a value in `[-1.0, 1.0]` for normalised vectors, or `0.0` if
/// either vector has zero magnitude, mismatched lengths, or the result
/// is non-finite (NaN/Infinity).
///
/// No SQLite extension needed — pure Rust dot-product computation.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0_f64;
    let mut mag_a = 0.0_f64;
    let mut mag_b = 0.0_f64;

    for (x, y) in a.iter().zip(b.iter()) {
        dot += (*x as f64) * (*y as f64);
        mag_a += (*x as f64) * (*x as f64);
        mag_b += (*y as f64) * (*y as f64);
    }

    let denom = mag_a.sqrt() * mag_b.sqrt();
    if denom == 0.0 {
        return 0.0;
    }
    let result = dot / denom;
    // Guard against NaN/Infinity from corrupted embedding data,
    // then cast back to f32 (result ∈ [-1.0, 1.0] for valid inputs).
    if result.is_finite() {
        result as f32
    } else {
        0.0
    }
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
            call_sites: vec![],
            call_site_count: 0,
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
                let snippet_raw = seshat_core::function_definition_snippet(f);
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
                let snippet_raw = seshat_core::type_definition_snippet(t);
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
                let snippet_raw = seshat_core::export_definition_snippet(e);
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
///
/// Used by the vector-search path; the SQL keyword path receives snippets
/// pre-truncated by the storage writer (`extract_definitions`) and therefore
/// does not call this.
fn truncate_pattern_snippet(raw: &str) -> CodeSnippet {
    seshat_core::truncate_snippet_to(raw, MAX_PATTERN_SNIPPET_LINES)
}

/// Maximum number of call-site examples returned per pattern result.
const MAX_CALL_SITES_PER_PATTERN: usize = 5;

/// Populate `call_sites` and `call_site_count` on each [`PatternResult`].
///
/// For every pattern result, scans all files' `function_calls` IR looking for
/// entries whose `callee` matches the pattern name.  Matching uses a
/// boundary-aware suffix check so that:
///
/// - `"scan_project"` matches callee `"scan_project"` (exact)
/// - `"scan_project"` matches callee `"scanner::scan_project"` (qualified)
/// - `"scan_project"` does NOT match callee `"rescan_project"` (different name)
///
/// Results are sorted deterministically by file path.  Up to
/// [`MAX_CALL_SITES_PER_PATTERN`] examples are stored; `call_site_count` holds
/// the total count (may be larger).
fn enrich_with_call_sites(patterns: &mut [PatternResult], files: &[ProjectFile]) {
    // Sort files once by path for deterministic output across all patterns.
    let mut sorted_files: Vec<&ProjectFile> = files.iter().collect();
    sorted_files.sort_by(|a, b| a.path.cmp(&b.path));

    for pattern in patterns.iter_mut() {
        let name = &pattern.name;
        let mut sites: Vec<CallSiteResult> = Vec::new();
        let mut total_count = 0usize;

        for file in &sorted_files {
            let calls: &[seshat_core::FunctionCall] = match file.language_ir {
                LanguageIR::Rust(ref ir) => &ir.function_calls,
                LanguageIR::TypeScript(ref ir) => &ir.function_calls,
                LanguageIR::JavaScript(ref ir) => &ir.function_calls,
                LanguageIR::Python(ref ir) => &ir.function_calls,
            };
            for fc in calls {
                if callee_matches_name(&fc.callee, name) {
                    total_count += 1;
                    if sites.len() < MAX_CALL_SITES_PER_PATTERN {
                        sites.push(CallSiteResult {
                            file: file.path.to_string_lossy().to_string(),
                            line: fc.line,
                            end_line: fc.end_line,
                            snippet: fc.snippet.clone(),
                        });
                    }
                }
            }
        }

        pattern.call_sites = sites;
        pattern.call_site_count = total_count;
    }
}

/// Return `true` if `callee` (as written in source) refers to a symbol named
/// `name`.
///
/// Handles:
/// - exact match: `"scan_project"` == `"scan_project"`
/// - path-qualified: `"crate::scanner::scan_project"` ends with `"::scan_project"`
/// - method call: `"db.execute"` ends with `".execute"`
///
/// The `::` and `.` separators themselves already prevent accidental partial-word
/// matches (e.g. `"rescan_project"` cannot end with `"::scan_project"`).
fn callee_matches_name(callee: &str, name: &str) -> bool {
    if callee == name {
        return true;
    }
    // Check for `::name` suffix (qualified path) or `.name` suffix (method).
    for sep in &["::", "."] {
        let needle = format!("{sep}{name}");
        if callee.ends_with(needle.as_str()) {
            return true;
        }
    }
    false
}

// ── SQL keyword search ─────────────────────────────────────

/// Escape a string for use as a SQLite `LIKE` pattern with `ESCAPE '\\'`.
///
/// `LIKE` treats `_` as "any single character" and `%` as "zero or more
/// characters".  Without escaping, a query for `do_thing` would also match
/// `doathing`.  The escape character is `\\` (configured via `ESCAPE '\\'` in
/// the SQL itself), so `\\` itself is doubled.
fn escape_like_pattern(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Probe `symbol_definitions` for rows matching any of `query_tokens`.
///
/// Replaces the previous full-IR iteration: instead of deserializing every
/// `files_ir.ir_data` blob and scoring each `Function` / `TypeDef` / `Export`
/// in memory, this issues one parameterised `LIKE` query per token against
/// the `(branch_id, symbol_name)` index introduced by V13.
///
/// Returned rows are scored in Rust with the same exact > prefix > contains
/// semantics as the old in-memory matcher (`score_name`); per
/// `(file_path, name, kind)` we keep the best score across all tokens.
///
/// `kind_filter`, when `Some`, is pushed down as a SQL `WHERE` clause —
/// satisfies the "no post-filter" acceptance criterion for kind selection.
fn search_symbol_definitions(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    query_tokens: &[&str],
    kind_filter: Option<&str>,
) -> Result<Vec<PatternResult>, GraphError> {
    if query_tokens.is_empty() {
        return Ok(Vec::new());
    }

    let conn_guard = crate::lock_conn(conn)?;

    // Two prepared statements: with and without the kind filter.  We pick one
    // up front rather than threading a dynamic SQL string through the loop.
    let sql_with_kind = "SELECT symbol_name, file_path, line, end_line, kind, is_public, snippet
         FROM symbol_definitions
         WHERE branch_id = ?1 AND kind = ?2 AND LOWER(symbol_name) LIKE ?3 ESCAPE '\\'";
    let sql_no_kind = "SELECT symbol_name, file_path, line, end_line, kind, is_public, snippet
         FROM symbol_definitions
         WHERE branch_id = ?1 AND LOWER(symbol_name) LIKE ?2 ESCAPE '\\'";

    let mut stmt = conn_guard
        .prepare(if kind_filter.is_some() {
            sql_with_kind
        } else {
            sql_no_kind
        })
        .map_err(|e| {
            GraphError::query(format!("Failed to prepare symbol_definitions query: {e}"))
        })?;

    let mut merged: HashMap<IrLookupKey, PatternResult> = HashMap::new();

    for &token in query_tokens {
        let token_norm = normalize_name(token);
        // Skip empty normalised tokens (e.g. whitespace-only inputs) — they
        // would degenerate into `LIKE '%%'` and pull every row.
        if token_norm.is_empty() {
            continue;
        }
        let like_pattern = format!("%{}%", escape_like_pattern(&token_norm));

        let row_iter = if let Some(kind) = kind_filter {
            stmt.query_map(params![branch_id, kind, like_pattern], map_symbol_row)
        } else {
            stmt.query_map(params![branch_id, like_pattern], map_symbol_row)
        }
        .map_err(|e| GraphError::query(format!("Failed to query symbol_definitions: {e}")))?;

        for row in row_iter {
            let SymbolDefinitionDbRow {
                name,
                file_path,
                line,
                end_line,
                kind,
                is_public,
                snippet,
            } = match row {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("Skipping symbol_definitions row: {e}");
                    continue;
                }
            };
            let score = score_name(&name, &[token]);
            if score <= 0.0 {
                // Defensive: a contains-LIKE row that fails Rust-side scoring
                // implies a normalisation mismatch (e.g. unicode case-folding
                // diverging between SQLite and Rust).  Skip rather than emit
                // a score-0 result.
                continue;
            }
            let key = (file_path.clone(), name.clone(), kind.clone());
            merged
                .entry(key)
                .and_modify(|existing: &mut PatternResult| {
                    if score > existing.score {
                        existing.score = score;
                    }
                })
                .or_insert_with(|| PatternResult {
                    name,
                    kind,
                    file_path,
                    line,
                    end_line,
                    is_public,
                    snippet: CodeSnippet {
                        content: snippet,
                        // Snippet was already truncated to
                        // MAX_DEFINITION_SNIPPET_LINES at write time; the
                        // synthetic format is single-line so this is
                        // effectively always `false` today.
                        truncated: false,
                    },
                    score,
                    call_sites: Vec::new(),
                    call_site_count: 0,
                });
        }
    }

    Ok(merged.into_values().collect())
}

/// One row read from `symbol_definitions` — owned strings so the rusqlite
/// row borrow doesn't escape the closure.
struct SymbolDefinitionDbRow {
    name: String,
    file_path: String,
    line: usize,
    end_line: usize,
    kind: String,
    is_public: bool,
    snippet: String,
}

fn map_symbol_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SymbolDefinitionDbRow> {
    let line_i64: i64 = row.get(2)?;
    let end_line_i64: i64 = row.get(3)?;
    let is_public_i64: i64 = row.get(5)?;
    Ok(SymbolDefinitionDbRow {
        name: row.get(0)?,
        file_path: row.get(1)?,
        line: usize::try_from(line_i64).unwrap_or(0),
        end_line: usize::try_from(end_line_i64).unwrap_or(0),
        kind: row.get(4)?,
        is_public: is_public_i64 != 0,
        snippet: row.get(6)?,
    })
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use seshat_core::{
        Export, Function, FunctionCall, JavaScriptIR, Language, LanguageIR, ModuleSystem,
        ProjectFile, RustIR, TypeDef, TypeDefKind, TypeScriptIR,
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
                    end_line: 1,
                },
                Export {
                    name: "QueryConventionData".to_owned(),
                    is_default: false,
                    is_type_only: true,
                    line: 2,
                    end_line: 2,
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
                    end_line: 5,
                    doc_comment: None,
                },
                TypeDef {
                    name: "ConventionResult".to_owned(),
                    kind: TypeDefKind::Struct,
                    is_public: true,
                    line: 8,
                    end_line: 8,
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

        let result = query_code_pattern(&conn, "main", "query_convention", None).unwrap();
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

        let result = query_code_pattern(&conn, "main", "query", None).unwrap();
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

        let result = query_code_pattern(&conn, "main", "convention", None).unwrap();
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

        let result = query_code_pattern(&conn, "main", "QueryConventionData", None).unwrap();

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

        let result = query_code_pattern(&conn, "main", "query", None).unwrap();
        assert!(!result.related_conventions.is_empty());
    }

    #[test]
    fn empty_query_returns_error() {
        let conn = test_conn();

        let result = query_code_pattern(&conn, "main", "", None);
        assert!(result.is_err());
        match result {
            Err(GraphError::InvalidInput(msg)) => {
                assert!(msg.contains("empty"));
            }
            other => panic!("Expected InvalidInput, got: {other:?}"),
        }

        // Also whitespace-only.
        let result = query_code_pattern(&conn, "main", "   ", None);
        assert!(result.is_err());
    }

    #[test]
    fn no_results_returns_empty_arrays() {
        let conn = test_conn();
        let file = sample_project_file("src/conventions.rs");
        insert_ir(&conn, "main", &file);

        let result = query_code_pattern(&conn, "main", "nonexistent_xyz_999", None).unwrap();
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
        let result = query_code_pattern(&conn, "main", "query", None).unwrap();

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
    fn snippet_content_does_not_contain_file_path_comment() {
        let conn = test_conn();
        let file = sample_project_file("src/conventions.rs");
        insert_ir(&conn, "main", &file);

        // Function snippet should not contain file path.
        let result = query_code_pattern(&conn, "main", "query_convention", None).unwrap();
        let func = result
            .patterns
            .iter()
            .find(|p| p.name == "query_convention" && p.kind == "function")
            .expect("query_convention function not found");
        assert!(
            !func.snippet.content.contains("src/conventions.rs"),
            "function snippet content should not contain file path, got: {:?}",
            func.snippet.content
        );
        assert!(
            !func.snippet.content.starts_with("//"),
            "function snippet content should not start with comment, got: {:?}",
            func.snippet.content
        );

        // Type snippet should not contain file path.
        let result = query_code_pattern(&conn, "main", "QueryConventionData", None).unwrap();
        let type_match = result
            .patterns
            .iter()
            .find(|p| p.name == "QueryConventionData" && p.kind == "type")
            .expect("QueryConventionData type not found");
        assert!(
            !type_match.snippet.content.contains("src/conventions.rs"),
            "type snippet content should not contain file path, got: {:?}",
            type_match.snippet.content
        );

        // Export snippet should not contain file path.
        let export_match = result
            .patterns
            .iter()
            .find(|p| p.name == "QueryConventionData" && p.kind == "export")
            .expect("QueryConventionData export not found");
        assert!(
            !export_match.snippet.content.contains("src/conventions.rs"),
            "export snippet content should not contain file path, got: {:?}",
            export_match.snippet.content
        );
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

        let result = query_code_pattern(&conn, "main", "query", None).unwrap();
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

        let result = query_code_pattern_with_embeddings(
            &conn,
            "main",
            "query_convention",
            None,
            Some(&provider),
        )
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

        let result = query_code_pattern_with_embeddings(
            &conn,
            "main",
            "query_convention",
            None,
            Some(&provider),
        )
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
        let result = query_code_pattern_with_embeddings(
            &conn,
            "main",
            "query_convention",
            None,
            Some(&provider),
        )
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
            query_code_pattern_with_embeddings(&conn, "main", "handle", None, Some(&provider))
                .unwrap();

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
        let result = query_code_pattern_with_embeddings(
            &conn,
            "main",
            "query_convention",
            None,
            Some(&provider),
        )
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
            query_code_pattern_with_embeddings(&conn, "main", "query_convention", None, None)
                .unwrap();

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
            call_sites: vec![],
            call_site_count: 0,
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
            call_sites: vec![],
            call_site_count: 0,
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
            call_sites: vec![],
            call_site_count: 0,
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
            call_sites: vec![],
            call_site_count: 0,
        }];

        let merged = merge_results(keyword, vector);
        assert_eq!(merged.len(), 2);
        // vector_only has higher score, should be first.
        assert_eq!(merged[0].name, "vector_only");
        assert_eq!(merged[1].name, "keyword_only");
    }

    // -----------------------------------------------------------------------
    // Call-site tests: TypeScript IR (v7)
    // -----------------------------------------------------------------------

    fn make_ts_project_file(path: &str, function_calls: Vec<FunctionCall>) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::TypeScript,
            content_hash: "abc".to_owned(),
            imports: vec![],
            exports: vec![],
            functions: vec![Function {
                name: "useEffect".to_owned(),
                is_public: true,
                is_async: false,
                line: 10,
                end_line: 10,
                parameters: vec![],
                doc_comment: None,
            }],
            types: vec![],
            dependencies_used: vec![],
            language_ir: LanguageIR::TypeScript(TypeScriptIR {
                has_barrel_exports: false,
                type_only_imports: vec![],
                decorators: vec![],
                default_export: false,
                function_calls,
            }),
            file_doc: None,
        }
    }

    #[test]
    fn call_sites_populated_from_typescript_ir() {
        let conn = test_conn();

        // A file that defines "useEffect" and calls it.
        let pf = make_ts_project_file(
            "src/component.tsx",
            vec![FunctionCall {
                callee: "useEffect".to_owned(),
                line: 10,
                end_line: 10,
                snippet: "  useEffect(fn, [dep]);".to_owned(),
            }],
        );

        insert_ir(&conn, "main", &pf);

        let data = query_code_pattern(&conn, "main", "useEffect", None).unwrap();
        let results = data.patterns;

        assert!(
            !results.is_empty(),
            "expected pattern results for 'useEffect'"
        );
        let r = &results[0];
        assert!(
            r.call_site_count > 0,
            "call_site_count must be > 0; got {}",
            r.call_site_count
        );
        assert!(
            !r.call_sites.is_empty(),
            "call_sites must not be empty; got {:?}",
            r.call_sites
        );
        assert!(
            r.call_sites[0].snippet.contains("useEffect"),
            "snippet must contain 'useEffect'; got {:?}",
            r.call_sites[0].snippet
        );
    }

    #[test]
    fn call_sites_populated_from_javascript_ir() {
        let conn = test_conn();

        let pf = ProjectFile {
            path: PathBuf::from("src/utils.js"),
            language: Language::JavaScript,
            content_hash: "js_abc".to_owned(),
            imports: vec![],
            exports: vec![],
            functions: vec![Function {
                name: "fetchData".to_owned(),
                is_public: true,
                is_async: true,
                line: 5,
                end_line: 10,
                parameters: vec![],
                doc_comment: None,
            }],
            types: vec![],
            dependencies_used: vec![],
            language_ir: LanguageIR::JavaScript(JavaScriptIR {
                module_system: ModuleSystem::ESM,
                has_module_exports: false,
                require_calls: vec![],
                function_calls: vec![FunctionCall {
                    callee: "fetchData".to_owned(),
                    line: 20,
                    end_line: 20,
                    snippet: "  const data = fetchData(url);".to_owned(),
                }],
            }),
            file_doc: None,
        };

        insert_ir(&conn, "main", &pf);

        let data = query_code_pattern(&conn, "main", "fetchData", None).unwrap();
        let results = data.patterns;

        assert!(!results.is_empty(), "expected results for 'fetchData'");
        let r = &results[0];
        assert!(
            r.call_site_count > 0,
            "JS call_site_count must be > 0; got {}",
            r.call_site_count
        );
        assert!(
            r.call_sites[0].snippet.contains("fetchData"),
            "snippet must contain 'fetchData'; got {:?}",
            r.call_sites[0].snippet
        );
    }

    // -----------------------------------------------------------------------
    // US-009: SQL-backed keyword search assertions
    // -----------------------------------------------------------------------

    #[test]
    fn kind_filter_pushed_into_sql_drops_other_kinds() {
        // The sample fixture exports the name "QueryConventionData" as both a
        // type and an export.  Filtering by `kind = "type"` must remove the
        // export entry (and any other-kind matches) entirely — that is the
        // observable consequence of pushing the filter into the SQL `WHERE`
        // clause rather than post-filtering.
        let conn = test_conn();
        let file = sample_project_file("src/conventions.rs");
        insert_ir(&conn, "main", &file);

        let result =
            query_code_pattern(&conn, "main", "QueryConventionData", Some("type")).unwrap();

        assert!(!result.patterns.is_empty(), "expected at least one match");
        for p in &result.patterns {
            assert_eq!(
                p.kind, "type",
                "kind filter leaked a non-type result: {p:?}"
            );
        }

        // Whitespace and "all" both mean "no filter".
        let all = query_code_pattern(&conn, "main", "QueryConventionData", Some("all")).unwrap();
        let kinds: std::collections::HashSet<&str> =
            all.patterns.iter().map(|p| p.kind.as_str()).collect();
        assert!(
            kinds.contains("type") && kinds.contains("export"),
            "'all' kind should return both type and export; got {kinds:?}"
        );

        let whitespace =
            query_code_pattern(&conn, "main", "QueryConventionData", Some("  ")).unwrap();
        let kinds_ws: std::collections::HashSet<&str> = whitespace
            .patterns
            .iter()
            .map(|p| p.kind.as_str())
            .collect();
        assert!(
            kinds_ws.contains("type") && kinds_ws.contains("export"),
            "whitespace kind should behave like no filter; got {kinds_ws:?}"
        );
    }

    #[test]
    fn sql_like_does_not_match_wildcard_underscore() {
        // `LIKE` treats `_` as "any single character"; we escape it so that a
        // query for `do_thing` does NOT match `doXthing`.  Regression test
        // for the LIKE-pattern escaping in `search_symbol_definitions`.
        use seshat_core::{
            Function, Language, LanguageIR, RustIR, test_helpers::make_project_file,
        };

        let conn = test_conn();
        let mut file = make_project_file(Language::Rust);
        file.path = "src/lib.rs".into();
        file.language_ir = LanguageIR::Rust(RustIR::default());
        file.functions = vec![
            Function {
                name: "do_thing".to_owned(),
                is_public: true,
                is_async: false,
                line: 1,
                end_line: 1,
                parameters: vec![],
                doc_comment: None,
            },
            Function {
                name: "doXthing".to_owned(),
                is_public: true,
                is_async: false,
                line: 5,
                end_line: 5,
                parameters: vec![],
                doc_comment: None,
            },
        ];
        insert_ir(&conn, "main", &file);

        let result = query_code_pattern(&conn, "main", "do_thing", None).unwrap();
        let names: Vec<&str> = result.patterns.iter().map(|p| p.name.as_str()).collect();
        assert!(
            names.contains(&"do_thing"),
            "expected do_thing in results, got {names:?}"
        );
        assert!(
            !names.contains(&"doXthing"),
            "doXthing must NOT match do_thing (LIKE underscore wildcard regression); got {names:?}"
        );
    }

    /// US-009: 1000-definition fixture sanity guard.
    ///
    /// Asserts that the SQL probe stays bounded as the symbol-index grows.
    /// We aim well below the PRD's manual-bench 50ms target so this remains
    /// stable on slow CI runners (no IR is loaded — only the
    /// `symbol_definitions` probe runs because the query has zero matches
    /// AND no embedding provider).  See `lazy IR load` in
    /// `query_code_pattern_with_embeddings`.
    #[test]
    fn lookup_time_bounded_with_1000_definitions() {
        use std::time::Instant;

        use seshat_core::BranchId;
        use seshat_storage::{
            SqliteSymbolIndexRepository, SymbolDefinitionRow, SymbolImportRow,
            SymbolIndexRepository, SymbolKind,
        };

        let conn = test_conn();
        let repo = SqliteSymbolIndexRepository::new(conn.clone());
        let branch = BranchId::from("main");

        // Insert 1000 definitions across 50 files, each with 20 symbols.
        for file_ix in 0..50 {
            let file_path = format!("src/mod_{file_ix:03}.rs");
            let mut defs = Vec::with_capacity(20);
            for sym_ix in 0..20 {
                defs.push(SymbolDefinitionRow {
                    symbol_name: format!("Symbol_{file_ix:03}_{sym_ix:03}"),
                    file_path: file_path.clone(),
                    line: 1,
                    end_line: 1,
                    kind: if sym_ix % 3 == 0 {
                        SymbolKind::Function
                    } else if sym_ix % 3 == 1 {
                        SymbolKind::Type
                    } else {
                        SymbolKind::Export
                    },
                    is_public: sym_ix % 2 == 0,
                    snippet: "stub".to_owned(),
                });
            }
            let imports: Vec<SymbolImportRow> = Vec::new();
            repo.replace_file(&branch, &file_path, &defs, &imports)
                .unwrap();
        }

        // 1) Exact-name lookup: pulls one row, runs no IR load.
        let started = Instant::now();
        let result = query_code_pattern(&conn, "main", "Symbol_025_010", None).unwrap();
        let elapsed = started.elapsed();
        assert!(
            result
                .patterns
                .iter()
                .any(|p| p.name == "Symbol_025_010" && (p.score - 1.0).abs() < f64::EPSILON),
            "expected exact match for Symbol_025_010"
        );
        // Generous sanity guard — slow CI runners can swing wildly, so we
        // pick a budget that still catches order-of-magnitude regressions.
        assert!(
            elapsed.as_millis() < 200,
            "1000-definition exact lookup took {elapsed:?}, expected < 200ms"
        );

        // 2) Kind filter + prefix lookup: confirms the SQL `WHERE kind = ?`
        // limits work to a single kind.
        let started = Instant::now();
        let result = query_code_pattern(&conn, "main", "Symbol_010", Some("function")).unwrap();
        let elapsed = started.elapsed();
        for p in &result.patterns {
            assert_eq!(p.kind, "function");
        }
        assert!(
            elapsed.as_millis() < 200,
            "1000-definition kind-filtered lookup took {elapsed:?}, expected < 200ms"
        );

        // 3) No match: smallest possible work — confirms the empty-result
        // path also short-circuits without loading IR.
        let started = Instant::now();
        let result = query_code_pattern(&conn, "main", "no_such_symbol_xyz_999", None).unwrap();
        let elapsed = started.elapsed();
        assert!(result.patterns.is_empty());
        assert!(
            elapsed.as_millis() < 200,
            "1000-definition no-match lookup took {elapsed:?}, expected < 200ms"
        );
    }
}
