---
date: 2026-05-02
scope: Technical Debt Epic 7 + Post-Fix Review
source: code-review-epic7-deferred-2026-04-07.md
pr_count: 5
active_items: 14
resolved_items: 7
m2plus_items: 3
---

# PRD: Technical Debt — Paydown Plan

**Author:** Kostik
**Date:** 2026-05-02

---

## Part I: Tech Debt Audit — Full Status

### Source Document

`_bmad-output/implementation-artifacts/code-review-epic7-deferred-2026-04-07.md`
Contains 22 items grouped by layer: Embedding provider (D1-D4), Graph (D5-D9), MCP (D10-D12), Storage (D13-D15), Post-Fix Review (D16-D19), Epic 8 (D20-D22).

Audit performed 2026-05-02. Statuses updated against the current state of the code.

### Status Table

| ID | Layer | Brief Description | Status | Rationale |
|----|-------|-------------------|--------|-----------|
| D1 | Embedding | Endpoint not configurable | **OBSOLETE** | Ollama/OpenAI removed in Epic 8. Replaced with `BuiltinProvider` (fastembed-rs). No HTTP endpoints. |
| D2 | Embedding | No retry/backoff on HTTP | **OBSOLETE** | HTTP providers removed. `BuiltinProvider::embed()` works locally via ONNX. |
| D3 | Embedding | `tracing` not used | **FIXED** | `tracing::info!` now used in `BuiltinProvider::new()` at line 218. |
| D4 | Embedding | No health check for Ollama | **OBSOLETE** | Ollama removed. `BuiltinProvider::new()` validates the model on creation; no external service. |
| D5 | Graph | LIKE without stop-word filtering | **OPEN** | `extract_keywords` filters only `len() > 1`; stop words are not removed. |
| D6 | Graph | FTS5 vs LIKE inconsistency | **OPEN** | Conventions searched via FTS5 (AND logic), decisions/observations via LIKE (OR logic). |
| D7 | Graph | f32 accumulation precision loss | **OPEN** | `cosine_similarity` accumulates dot/mag in f32. For 384d+ vectors — precision loss. |
| D8 | Graph | `resolve_by_suffix` O(N*E) | **OPEN** | Linear scan of `known_paths` × `FILE_EXTENSIONS` for each import. |
| D9 | Graph | Workspace imports invisible | **OPEN** | `is_likely_internal()` does not recognize `seshat_core::module`, `seshat_graph::validate_approach`, etc. |
| D10 | MCP | Call logger silent degradation | **OPEN** | `unwrap_or(0)` / `unwrap_or("unknown")` on JSON key renames — silent data loss. |
| D11 | MCP | `internal_error` → `map_graph_error` | **FIXED** | `map_graph_error` used consistently in `project_context.rs`, `query_convention.rs` and others. |
| D12 | MCP | Double-trim description | **OPEN** | `.trim()` called in both MCP handler and graph layer. Duplication. |
| D13 | Storage | Redundant index | **OPEN** | `idx_code_embeddings_branch(branch_id)` — prefix of `idx_code_embeddings_branch_file(branch_id, file_path)`. |
| D14 | Storage | No timestamps on embeddings | **OPEN** | `code_embeddings` without `created_at`/`updated_at`. Cannot invalidate by TTL. |
| D15 | Storage | i64→usize unsafe cast | **OPEN** | `count as usize` without bounds check. |
| D16 | Graph | LIMIT 10000 truncates deps | **OPEN** | Warning added, but `truncated` flag not propagated to response. Consumer unaware of incompleteness. |
| D17 | Graph | OR-logic keyword search too broad | **OPEN** | `build_keyword_like` builds OR join; any word of 2+ chars yields a match. |
| D18 | MCP | Path normalization no `..` rejection | **OPEN** | `../../etc/passwd` not rejected. Low practical risk, but defence-in-depth. |
| D19 | CLI | Stale embeddings not cleaned | **OPEN** | Code explicitly documents the problem in a comment. Stale rows from deleted files accumulate. |
| D20 | CLI | Inline embedding generation | **DEFERRED (M2+)** | Requires scan pipeline reorganization. |
| D21 | CLI | Function body imports analysis | **CLOSED → D23** | `import_context` added (uses: ...) to embedding text. Closed as satisfactory compromise. New item D23 (M2+) created for deeper work. |
| D22 | Graph | sqlite-vec ANN search | **DEFERRED (M2+)** | Performance improvement. Not a blocker; scales to ~50k embeddings with brute-force. |

### Audit Summary

| Category | Count | Items |
|----------|-------|-------|
| **RESOLVED** (obsolete or fixed) | 6 | D1, D2, D3, D4, D11, D21 |
| **OPEN** (require implementation) | 14 | D5, D6, D7, D8, D9, D10, D12, D13, D14, D15, D16, D17, D18, D19 |
| **DEFERRED M2+** | 3 | D20, D22, D23 |

**The original file `code-review-epic7-deferred-2026-04-07.md` is considered fully processed and should be replaced by this document.**

---

## Part II: Implementation Plan

14 active items are grouped into **5 sequential PRs**.

Implementation order:

```
PR 1 (Quick Wins) →  ─┬─→ PR 2 (Search Quality)
                       ├─→ PR 3 (Infrastructure Robustness)
                       └─→ PR 4 (Graph Performance)
                                           │
                                           ↓
                               PR 5 (KSD Code Review)
```

PR 2, 3, 4 can run in parallel after PR 1. PR 5 — final, after all others.

---

### PR 1: Quick Wins

**Branch:** `fix/debt-quick-wins`
**Items:** D7, D12, D13, D15
**Estimate:** ~30 min
**Description:** Four atomic fixes, each touching 1-3 lines in a single file. No dependencies between them.

---

#### D7: f64 accumulation in cosine_similarity

**File:** `crates/seshat-graph/src/code_pattern.rs:248-270`
**Summary:** Accumulators `dot`, `mag_a`, `mag_b` are declared as `f32`. For vectors of dimension 384 (all-MiniLM-L6-v2) or 768+, accumulated floating-point error may affect ranking of close cosine-distance results.

**Current code (248-261):**
```rust
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
```

**Target code:**
```rust
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
    // Result is in [-1.0, 1.0], safe to cast back to f32.
    if result.is_finite() { result as f32 } else { 0.0 }
}
```

**Changes:**
- `dot`, `mag_a`, `mag_b` → `f64`
- `x * y` → `(*x as f64) * (*y as f64)`
- `result` (f64) cast to `f32` on return
- Update doc comment on line 241: `f32 vectors` → describe f64 accumulation

**Risks:** None. Purely mathematical improvement. Return type unchanged (`f32`).

---

#### D12: Double-trim description

**Files:**
- `crates/seshat-mcp/src/tools/validate_approach.rs:80` — remove `.trim()`
- `crates/seshat-graph/src/validate_approach.rs:157` — keep `.trim()`

**Summary:** Trim should only happen in the graph layer (closer to data). The MCP handler is a thin wrapper that should not modify data.

**Current code MCP layer (79-81):**
```rust
let description = req.description.trim();
if description.is_empty() {
```

**Target code MCP layer:**
```rust
let description = &req.description;
if description.is_empty() {
```

**Changes:**
- Remove `.trim()` from MCP handler
- Empty string still rejected at MCP level (fast early-exit)
- Remove `.to_owned()` on line 95: `description: description.to_owned()` → `description: description.clone()`

**Risks:** None. Graph layer still does `.trim()` on line 157.

---

#### D13: Drop redundant idx_code_embeddings_branch index

**File:** New migration `V9__drop_redundant_branch_index.sql`

**Summary:** The `idx_code_embeddings_branch` index on `(branch_id)` is a prefix of the compound index `idx_code_embeddings_branch_file` on `(branch_id, file_path)`. SQLite uses the leftmost prefix of a compound index for queries on `branch_id` alone, making the single-column index redundant.

**V9 migration content:**
```sql
-- V9: Drop redundant single-column index.
-- The compound index idx_code_embeddings_branch_file(branch_id, file_path)
-- covers queries filtering by branch_id alone (leftmost prefix).
DROP INDEX IF EXISTS idx_code_embeddings_branch;
```

**Risks:**
- Ensure migrations are numbered sequentially. Current: V1-V8. V9 is free.
- Verify no application code references this index by name.
- DROP INDEX IF EXISTS — idempotent, safe.

---

#### D15: Safe i64→usize cast

**File:** `crates/seshat-storage/src/repository/embedding_repository.rs:180`

**Summary:** `COUNT(*)` returns `i64`. Direct cast `as usize` — undefined behavior for negative values (theoretically impossible for COUNT, but technically incorrect). On 32-bit platforms — data loss for >4B.

**Current code (174-181):**
```rust
let count: i64 = conn.query_row(
    "SELECT COUNT(*) FROM code_embeddings WHERE branch_id = ?1",
    params![branch_id],
    |row| row.get(0),
)?;

Ok(count as usize)
```

**Target code:**
```rust
let count: i64 = conn.query_row(
    "SELECT COUNT(*) FROM code_embeddings WHERE branch_id = ?1",
    params![branch_id],
    |row| row.get(0),
)?;

Ok(usize::try_from(count).unwrap_or(0))
```

**Risks:** `COUNT(*)` never returns a negative value. `try_from` — safety net. Panic only on SQLite bug, which is impossible.

---

### PR 2: Search Quality

**Branch:** `fix/debt-search-quality`
**Items:** D5, D6, D17
**Estimate:** ~2-3 hours
**Main file:** `crates/seshat-graph/src/validate_approach.rs`
**Depends on:** PR 1 (no changes in the same file, but for ordering)

---

#### D5: Stop-word filtering in extract_keywords

**File:** `crates/seshat-graph/src/validate_approach.rs:343-348`

**Summary:** `extract_keywords` splits the description by whitespace and filters tokens shorter than 2 chars. But common English words like "the", "for", "with" pass the filter and create a huge number of LIKE matches.

**Current code (343-348):**
```rust
fn extract_keywords(description: &str) -> Vec<String> {
    description
        .split_whitespace()
        .filter(|w| w.len() > 1)
        .map(|w| w.to_lowercase())
        .collect()
}
```

**Target code:**
```rust
/// Common English stop words that produce overly broad search results.
/// These are filtered out during keyword extraction to prevent matching
/// nearly every node in the graph.
const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
    "have", "has", "had", "do", "does", "did", "will", "would", "could",
    "should", "may", "might", "can", "shall", "to", "of", "in", "for",
    "on", "with", "at", "by", "from", "as", "into", "through", "during",
    "before", "after", "above", "below", "between", "under", "this",
    "that", "these", "those", "it", "its", "they", "them", "their",
    "he", "she", "his", "her", "we", "our", "you", "your", "and",
    "but", "or", "not", "no", "nor", "so", "if", "than", "then",
    "else", "when", "where", "which", "who", "whom", "whose", "how",
    "all", "each", "every", "both", "few", "more", "most", "other",
    "some", "such", "only", "own", "same", "just", "about", "also",
    "very", "too",
];

fn extract_keywords(description: &str) -> Vec<String> {
    description
        .split_whitespace()
        .filter(|w| w.len() > 1)
        .map(|w| w.to_lowercase())
        .filter(|w| !STOP_WORDS.contains(&w.as_str()))
        .collect()
}
```

**Changes:**
- Add `STOP_WORDS` constant (module level)
- Add `.filter(|w| !STOP_WORDS.contains(&w.as_str()))` to the `extract_keywords` chain
- Update doc comment: mention stop-word filtering
- Keep `len() > 1` threshold — short identifiers (`io`, `fs`, `db`, `id`) are still valid

**Risks:** If all keywords from the description are stop words, `extract_keywords` returns an empty vector, and `keyword_search_nodes` returns `Vec::new()` (already handled on line 386-388). This is correct behavior.

---

#### D6: Unify search — FTS5 for decisions/observations

**File:** `crates/seshat-graph/src/validate_approach.rs`

**Summary:** Currently conventions are searched via FTS5 (AND logic), while decisions and observations are searched via `keyword_search_nodes` → `build_keyword_like` (OR logic). This yields inconsistent behavior: the same description returns different result sets.

**Current code for decisions (`find_decisions` function, lines ~500-530):**
```rust
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
        "id, description, weight, confidence, source, nature, category",
        "AND nature = 'decision'",
        "decisions",
        |row| {
            Ok(DecisionEntry {
                id: row.get(0)?,
                description: row.get(1)?,
                weight: row.get(2)?,
                confidence: row.get::<_, f64>(3)? as f32,
                source: row.get(4)?,
                nature: row.get(5)?,
                category: row.get(6)?,
            })
        },
    )
}
```

**Target code:**
Use `query_convention` FTS5 with `nature = 'decision'` filter:

```rust
fn find_decisions(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    description: &str,
) -> Result<Vec<DecisionEntry>, GraphError> {
    // Reuse FTS5 search with nature filter for consistent AND-matching semantics.
    let all = query_convention(conn, branch_id, description).unwrap_or_else(|e| {
        tracing::warn!("Decision search failed: {e}");
        QueryConventionData { conventions: Vec::new() }
    });
    let decisions = all
        .conventions
        .into_iter()
        .filter(|c| c.nature == "decision")
        .map(|c| DecisionEntry {
            id: c.id,
            description: c.description,
            weight: c.weight,
            confidence: c.confidence,
            source: c.source,
            nature: c.nature,
            category: c.category,
        })
        .collect();
    Ok(decisions)
}
```

**Same for observations** (`find_observations` function): filter `nature == "observation"`.

**Changes:**
- `find_decisions` → remove `keyword_search_nodes`, reuse `query_convention` + filter
- `find_observations` → same
- Remove `keyword_search_nodes` calls with `"AND nature = 'decision'"` and `"AND nature = 'observation'"`
- Simplify code: single FTS5 query for all conventions/decisions/observations at line 165

**Risks:**
- `query_convention` returns `Vec<ConventionEntry>`, `find_decisions` returns `Vec<DecisionEntry>`. Fields need mapping.
- FTS5 uses AND logic (stricter). If decisions were previously found via OR and were useful, there will be fewer now. This is **desired behavior** — less noise, more precise results.

---

#### D17: Switch keyword_search_nodes to AND logic

**File:** `crates/seshat-graph/src/validate_approach.rs:357-364`

**Summary:** After D6, `keyword_search_nodes` remains only for contradictions (edges table, no FTS5). OR logic with `%keyword%` patterns is too broad. Switch to AND + add LIMIT.

**Current code `build_keyword_like` (357-364):**
```rust
fn build_keyword_like(keywords: &[String], param_offset: usize) -> (String, Vec<String>) {
    let clauses: Vec<String> = keywords
        .iter()
        .enumerate()
        .map(|(i, _)| format!("LOWER(description) LIKE ?{}", param_offset + i))
        .collect();
    let params: Vec<String> = keywords.iter().map(|k| format!("%{k}%")).collect();
    (clauses.join(" OR "), params)
}
```

**Target code:**
```rust
/// Maximum number of keywords used in LIKE-based edge searches.
/// Limits SQL complexity for long descriptions (>50 words).
const MAX_LIKE_KEYWORDS: usize = 5;

fn build_keyword_like(keywords: &[String], param_offset: usize) -> (String, Vec<String>) {
    // Take the longest keywords first — they are the most discriminative.
    let mut sorted: Vec<&String> = keywords.iter().collect();
    sorted.sort_by(|a, b| b.len().cmp(&a.len()));
    sorted.truncate(MAX_LIKE_KEYWORDS);

    let clauses: Vec<String> = sorted
        .iter()
        .enumerate()
        .map(|(i, _)| format!("LOWER(description) LIKE ?{}", param_offset + i))
        .collect();
    let params: Vec<String> = sorted.iter().map(|k| format!("%{k}%")).collect();
    (clauses.join(" AND "), params)
}
```

**In `keyword_search_nodes` (lines 392-393):**
Add `LIMIT 50` to SQL:
```rust
let sql = format!(
    "SELECT {columns} FROM nodes WHERE branch_id = ?1 AND ({like_where}) {extra_where} AND {SQL_NOT_REMOVED} LIMIT 50"
);
```

**Changes:**
- `clauses.join(" OR ")` → `clauses.join(" AND ")`
- Add `MAX_LIKE_KEYWORDS = 5`: take the 5 longest keywords (most discriminative)
- Add `LIMIT 50` to SQL query
- Update doc comments for both functions

**Risks:**
- If keywords empty — handled at line 386-388
- AND logic with >5 keywords may yield 0 results for very specific queries. This is better than OR noise.
- `LIMIT 50` — contradictions rarely exceed 50. Edge case protection.

---

### PR 3: Infrastructure Robustness

**Branch:** `fix/debt-infra-robustness`
**Items:** D10, D14, D16, D18, D19
**Estimate:** ~2-3 hours
**Affects:** `call_logger`, `code_embeddings`, `load_branch_ir`, `query_dependencies`, `scan.rs`

---

#### D10: Shared key constants + debug logging in call_logger

**Files:**
- New: `crates/seshat-mcp/src/call_logger_keys.rs`
- `crates/seshat-mcp/src/call_logger.rs` — replace magic strings

**Summary:** All call_logger result functions use `unwrap_or(0)` / `unwrap_or("unknown")` for missing JSON keys. When a field is renamed in the envelope (e.g., `languages` → `langs`), the logger silently writes 0. No compile-time contract between handlers and logger.

**New file `call_logger_keys.rs`:**
```rust
/// Shared JSON key constants for call-logger result summary extraction.
///
/// These keys correspond to field names in the data envelope produced by
/// each MCP tool handler. Changing a field name in a handler MUST update
/// the corresponding constant here, otherwise the call logger will silently
/// report zeroed-out values.
///
/// # Naming convention
///
/// Each constant is prefixed with the tool name: `PROJECT_CTX_*`,
/// `QUERY_CONV_*`, `CODE_PATTERN_*`, `DEPS_*`, `VALIDATE_*`.

pub mod project_context {
    pub const LANGUAGES: &str = "languages";
    pub const CONVENTIONS_COUNT: &str = "conventions_count";
    pub const GOLDEN_FILES: &str = "golden_files";
}

pub mod query_convention {
    pub const CONVENTIONS: &str = "conventions";
}

pub mod code_pattern {
    pub const PATTERNS: &str = "patterns";
    pub const RELATED_CONVENTIONS: &str = "related_conventions";
}

pub mod dependencies {
    pub const DEPENDENTS: &str = "dependents";
    pub const DEPENDENCIES: &str = "dependencies";
    pub const BLAST_RADIUS: &str = "blast_radius";
}

pub mod validate_approach {
    pub const VERDICT: &str = "verdict";
    pub const RULES: &str = "rules";
    pub const DUPLICATES: &str = "duplicates";
    pub const CONVENTIONS: &str = "conventions";
    pub const READY: &str = "ready";
}
```

**Update `call_logger.rs`:**
Each result function replaces string literals with constants and adds debug logging when an expected key is missing:

Example for `project_context_result`:
```rust
pub fn project_context_result(response_data: &serde_json::Value) -> serde_json::Value {
    use crate::call_logger_keys::project_context as keys;

    let language_count = response_data
        .get(keys::LANGUAGES)
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or_else(|| {
            tracing::debug!("missing key '{}' in project_context response", keys::LANGUAGES);
            0
        });

    let convention_count = response_data
        .get(keys::CONVENTIONS_COUNT)
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| {
            tracing::debug!("missing key '{}' in project_context response", keys::CONVENTIONS_COUNT);
            0
        });

    let golden_file_count = response_data
        .get(keys::GOLDEN_FILES)
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or_else(|| {
            tracing::debug!("missing key '{}' in project_context response", keys::GOLDEN_FILES);
            0
        });

    serde_json::json!({
        "language_count": language_count,
        "convention_count": convention_count,
        "golden_file_count": golden_file_count,
    })
}
```

**Same for:** `query_convention_result`, `code_pattern_result`, `dependencies_result`, `validate_approach_result`.

**Add to `call_logger.rs` at the top:**
```rust
mod call_logger_keys;
use call_logger_keys as keys;
```

Or via `pub mod` in `lib.rs`:
```rust
pub mod call_logger_keys;
```

**Risks:**
- When a field name changes in a handler — the constant in `call_logger_keys.rs` must be updated. This is desired behavior (explicit contract). Ideally, a test verifying field names, but out of scope.
- `tracing::debug!` not visible in production by default, no spam.

---

#### D14: Timestamps on code_embeddings table

**File:** New migration `V10__add_embeddings_timestamp.sql`

**Summary:** `code_embeddings` has no timestamp columns. Cannot implement TTL-based invalidation or detect stale embeddings after embedding model change.

**V10 migration content:**
```sql
-- V10: Add updated_at timestamp to code_embeddings.
-- Enables TTL-based invalidation and detection of stale embeddings
-- after model changes.
ALTER TABLE code_embeddings ADD COLUMN updated_at TEXT NOT NULL DEFAULT (datetime('now'));
```

**Update `embedding_repository.rs`:**

In `upsert_batch` method (function doing `INSERT ... ON CONFLICT DO UPDATE`) — add `updated_at`:

```rust
INSERT INTO code_embeddings (branch_id, file_path, item_name, item_kind, embedding, updated_at)
VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))
ON CONFLICT(branch_id, file_path, item_name, item_kind) DO UPDATE SET
    embedding = excluded.embedding,
    updated_at = datetime('now')
```

**Risks:**
- `ALTER TABLE ... ADD COLUMN ... DEFAULT` — SQLite supports since v3.25.0 (2018). Safe.
- Existing rows get `updated_at = datetime('now')` at migration time — this is correct (consider them created "now").
- Proper TTL implementation requires separate functionality later — currently just adding the column.

---

#### D16: Truncated flag propagation

**Files:**
- `crates/seshat-graph/src/code_pattern.rs` — `load_branch_ir`, return structures
- `crates/seshat-mcp/src/tools/project_context.rs`, `code_pattern.rs`, `query_dependencies.rs`

**Summary:** `load_branch_ir` has LIMIT 10000. Warning is logged, but the caller doesn't know results are truncated. An AI agent calling `query_dependencies` gets incomplete data with no indication.

**Step 1: Change `load_branch_ir` signature**

Current:
```rust
pub(crate) fn load_branch_ir(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
) -> Result<Vec<ProjectFile>, GraphError>
```

Target:
```rust
/// Result of loading branch IR, including truncation flag.
pub(crate) struct LoadedIR {
    pub files: Vec<ProjectFile>,
    /// True if the LIMIT was reached and results may be incomplete.
    pub truncated: bool,
}

pub(crate) fn load_branch_ir(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
) -> Result<LoadedIR, GraphError>
```

**Step 2: Add truncated to response structures**

`CodePatternData` (add field):
```rust
pub struct CodePatternData {
    pub patterns: Vec<CodePatternItem>,
    pub related_conventions: Vec<RelatedConvention>,
    pub truncated: bool,  // NEW
}
```

`DependenciesData` (add field):
```rust
pub struct DependenciesData {
    pub dependents: Vec<DependentEntry>,
    pub dependencies: Vec<DependencyEntry>,
    pub blast_radius: String,
    pub truncated: bool,  // NEW
}
```

**Step 3: Propagate truncated through call chain**

- `query_code_pattern` → calls `load_branch_ir`, propagates `truncated` to `CodePatternData`
- `query_dependencies` → same to `DependenciesData`
- `query_project_context` → may also use `load_branch_ir`, add `truncated` if used

**Step 4: MCP layer — propagate to JSON**

In each handler after receiving data — add field to JSON response.

Example for `query_dependencies.rs`:
```rust
let envelope = DataEnvelope::new(tool, repo_name, data);
// envelope already serializes all DependenciesData fields, including truncated
```

**Risks:**
- Changing `load_branch_ir` signature — pub function within crate. Must update all call sites.
- `truncated: bool` added to JSON response — doesn't break existing consumers (additive field).
- For `query_project_context` — check if `load_branch_ir` is used there. If not, skip truncated.

---

#### D18: Reject `..` path traversal

**File:** `crates/seshat-mcp/src/tools/query_dependencies.rs:66-69`

**Summary:** Path normalization removes `./` and replaces `\` with `/`, but doesn't check `..`. Path `../../etc/passwd` passes validation, producing a confusing "not found" error.

**After current normalization (lines 66-69) add:**

```rust
// Normalize the path: trim whitespace, strip leading `./`, replace backslashes.
let mut path = req.path.trim().replace('\\', "/");
while path.starts_with("./") {
    path = path[2..].to_owned();
}

// Reject paths containing `..` — they can't resolve to valid IR entries
// and create confusing error messages.
if path.contains("..") {
    let err = ErrorEnvelope::new(
        tool,
        repo_name,
        ErrorCode::InvalidInput,
        "Paths containing '..' are not allowed",
        "Use a resolved path like 'src/handler.rs' without parent directory references",
    );
    return serde_json::to_string(&err).unwrap_or_else(|_| {
        r#"{"status":"error","tool":"query_dependencies","repo":"","error":{"code":"INTERNAL_ERROR","message":"Failed to serialize error","suggestion":"Report this issue"}}"#.to_owned()
    });
}
```

**Risks:**
- `..` in the middle of a path (`src/../lib/foo.rs`) — also rejected (`contains("..")`).
- Legitimate paths never contain `..` (Seshat works with normalized relative paths).
- Absolute paths already rejected (lines 86-97).

---

#### D19: Stale embedding cleanup after successful scan

**File:** `crates/seshat-cli/src/scan.rs:828-899`

**Summary:** Code explicitly documents the problem in a comment (lines 828-833). With the upsert approach, stale rows from deleted/renamed files accumulate.

**Plan:**
1. At the start of `generate_embeddings`, collect `current_keys: HashSet<(String, String, String)>` — all (file_path, item_name, item_kind) present in `items`
2. After successful upsert of all batches — query `stored_keys` from DB: `SELECT file_path, item_name, item_kind FROM code_embeddings WHERE branch_id = ?`
3. Compute `stale = stored_keys - current_keys`
4. Delete stale in batches of 100: `DELETE FROM code_embeddings WHERE branch_id = ? AND file_path = ? AND item_name = ? AND item_kind = ?`
5. Log count of removed stale rows: `tracing::info!("Pruned {} stale embedding rows", stale_count)`
6. Update comment: describe new cleanup flow

**Additionally:** Add `delete_stale` method to `SqliteEmbeddingRepository` trait:
```rust
fn delete_stale(&self, branch_id: &str, stale_keys: &[(String, String, String)]) -> Result<usize, StorageError>;
```

**Risks:**
- If `delete_stale` fails after successful upsert — some stale rows remain. Not critical (next scan will clean up). Upsert data already in DB.
- `stale_keys` may be large for major refactors. Batches of 100 with separate transactions, or a single large DELETE with WHERE ... IN.

---

### PR 4: Graph Performance

**Branch:** `fix/debt-graph-performance`
**Items:** D8, D9
**Estimate:** ~2-3 hours
**Main file:** `crates/seshat-graph/src/dependencies.rs`

---

#### D8: Reverse suffix index for resolve_by_suffix

**File:** `crates/seshat-graph/src/dependencies.rs:330-361`

**Summary:** `resolve_by_suffix` — linear scan of `known_paths` × `FILE_EXTENSIONS` for each import. For large repos (50k+ files × 6 extensions × 100 imports) = 30M string ops per query. Solution: build a reverse suffix index (HashMap) once per `resolve_imports_for_file` call.

**SuffixIndex design:**

```rust
use std::collections::HashMap;

/// Reverse suffix index for O(1) import resolution.
///
/// Maps path suffixes (e.g. "utils.rs", "models/user.rs") to the full
/// normalized path. Built once from `known_paths` and reused for all
/// imports in a single `resolve_imports_for_file` call.
struct SuffixIndex {
    /// suffix → full path
    map: HashMap<String, String>,
}

impl SuffixIndex {
    /// Build a suffix index from the set of known file paths.
    ///
    /// For each path, inserts entries for the last component, last two
    /// components, etc., up to the full path. This handles both shallow
    /// imports (`utils`) and nested imports (`models::user` or `models/user`).
    fn build(known_paths: &HashSet<String>) -> Self {
        let mut map = HashMap::new();
        for path in known_paths {
            let normalized = path.replace('\\', "/");
            let components: Vec<&str> = normalized.split('/').collect();

            // Insert suffixes of increasing depth: "file.rs", "dir/file.rs", ...
            for depth in 1..=components.len() {
                let suffix = components[components.len() - depth..].join("/");
                // Only store the first match for each suffix (stable).
                map.entry(suffix).or_insert_with(|| path.clone());
            }
        }
        Self { map }
    }

    /// Look up a module path suffix. Returns the full path if found.
    fn resolve(&self, module: &str, extra_ext: Option<&str>) -> Option<&str> {
        let suffix = module_to_path_suffix(module);
        if let Some(found) = self.map.get(&suffix) {
            return Some(found.as_str());
        }
        if let Some(ext) = extra_ext {
            let with_ext = format!("{suffix}{ext}");
            if let Some(found) = self.map.get(&with_ext) {
                return Some(found.as_str());
            }
        }
        None
    }
}
```

**Integration into `resolve_imports_for_file`:**

1. Build `SuffixIndex` once before the import loop:
```rust
let suffix_index = SuffixIndex::build(known_paths);
```

2. Pass `&suffix_index` to `resolve_import` and `resolve_by_suffix`

3. `resolve_by_suffix` with index:
```rust
fn resolve_by_suffix(module: &str, index: &SuffixIndex) -> Option<String> {
    let suffix = module_to_path_suffix(module);

    // Try exact suffix match.
    if let Some(found) = index.map.get(&suffix) {
        return Some(found.clone());
    }

    // Try with each file extension.
    for ext in FILE_EXTENSIONS {
        let with_ext = format!("{suffix}{ext}");
        if let Some(found) = index.map.get(&with_ext) {
            return Some(found.clone());
        }
    }

    None
}
```

**Changes:**
- New module: `crates/seshat-graph/src/suffix_index.rs` or inline in `dependencies.rs`
- `resolve_by_suffix` takes `&SuffixIndex` instead of `&HashSet<String>`
- `resolve_import`, `resolve_relative_import` — update signatures
- `resolve_imports_for_file` — build index once before loop
- Add `#[cfg(test)] mod tests` with tests for:
  - Simple suffix match
  - Nested suffix match: `models/user.rs` ← query "user"
  - With extension: `utils.rs` ← query "utils" + ".rs"
  - Backward compatibility: same tests as old `resolve_by_suffix`

**Risks:**
- Memory: SuffixIndex for 50k paths × avg depth 3 = ~150k entries. ~10-15 MB. Acceptable.
- Index construction per `resolve_imports_for_file` call — O(N*D). Still faster than O(N*E) per import.
- Future: cache SuffixIndex between calls (lazy static), but not needed now.

---

#### D9: Workspace crate names as internal imports

**Files:**
- `crates/seshat-graph/src/dependencies.rs:264-272` — `is_likely_internal`
- `crates/seshat-graph/src/dependencies.rs:281-301` — `resolve_import`

**Summary:** Imports of workspace crates (e.g., `use seshat_graph::validate_approach`) are not recognized as internal. `is_likely_internal("seshat_graph")` → `false`. `resolve_import` returns `None`. As a result, cross-crate dependencies don't appear in the dependency graph.

**Design:**

Recognize workspace crate names based on:
- `Cargo.toml` `[workspace.members]` — `crates/seshat-graph` → `seshat_graph`
- Normalization: replace `-` with `_`

**New constant (computed or lazy_static):**
```rust
/// Workspace crate names (normalized: `-` → `_`).
/// Used to recognize cross-crate imports within the workspace.
const WORKSPACE_CRATES: &[&str] = &[
    "seshat_core",
    "seshat_scanner",
    "seshat_detectors",
    "seshat_storage",
    "seshat_graph",
    "seshat_mcp",
    "seshat_embedding",
    "seshat_watcher",
    "seshat_cli",
    "seshat_bin",
];
```

**Update `is_likely_internal`:**
```rust
fn is_likely_internal(module: &str) -> bool {
    // Extract the first segment before `::`.
    let first_seg = module.split("::").next().unwrap_or(module);

    module.starts_with('.')
        || module == "crate" || module.starts_with("crate::")
        || module == "super" || module.starts_with("super::")
        || module == "self" || module.starts_with("self::")
        || module.starts_with("src/")
        || module.starts_with("src.")
        || WORKSPACE_CRATES.contains(&first_seg)
}
```

**Update `resolve_import`:**
```rust
fn resolve_import(
    module: &str,
    importing_dir: &Path,
    known_paths: &HashSet<String>,
    suffix_index: &SuffixIndex,  // NEW parameter from D8
) -> Option<String> {
    if module.starts_with('.') {
        resolve_relative_import(module, importing_dir, known_paths)
    } else if module.starts_with("crate")
        || module.starts_with("super")
        || module.starts_with("self")
    {
        resolve_by_suffix(module, suffix_index)
    } else if module.starts_with("src/") || module.starts_with("src.") {
        resolve_by_suffix(module, suffix_index)
    } else {
        // Check workspace crate: e.g. "seshat_graph::validate_approach"
        let first_seg = module.split("::").next().unwrap_or(module);
        if WORKSPACE_CRATES.contains(&first_seg) {
            // Resolve the full module path within the workspace crate.
            resolve_by_suffix(module, suffix_index)
        } else {
            None
        }
    }
}
```

**Risks:**
- `WORKSPACE_CRATES` — hardcoded. When adding a new crate to workspace, this constant must be updated. Future: parse `Cargo.toml` at compile time via `include_str!` + `toml`, but manual for now.
- Crate names overlapping with external crates? Unlikely (all start with `seshat_`). Even if they do — false internal recognition. Not a problem in practice.
- Test: after changes, `use seshat_graph::validate_approach` should appear in DependencyEntry as a resolved internal import.

---

### PR 5: KSD Code Review — Final

**Branch:** `fix/debt-ksd-code-review`
**Items:** KSD Code Review for idiomatic Rust compliance, feature deduplication, and convention adherence
**Estimate:** ~1-2 hours
**Order:** After all PRs 1-4 merged to main

**Summary:** After implementing all 4 tech debt PRs, run a full KSD Code Review of the changes. Verify:

1. **Idiomatic Rust:**
   - `clippy::all` with no warnings
   - Use `&str` instead of `String` where possible
   - Proper use of `Result`, `Option`, `?` operator
   - No `unwrap()` without justification
   - No `unsafe` blocks

2. **Feature deduplication:**
   - No duplication between `call_logger_keys.rs` and existing envelope constants
   - `SuffixIndex` doesn't duplicate logic from `resolve_relative_import`
   - `extract_keywords` + `build_keyword_like` don't conflict with FTS5 query building
   - Truncated flag propagation — not duplicated across three handlers (extract to shared function?)

3. **Project conventions:**
   - All public functions documented (`///` doc comments)
   - Error handling: `GraphError`, `StorageError`, `CliError` — consistent
   - `tracing` — all warn/info/debug in place
   - Tests: new functions covered by tests (especially D8, D19)
   - Migrations: naming convention (V9, V10, ...), idempotency

4. **Adversarial review (Blind Hunter):**
   - Edge cases: what if `stale_keys` empty in D19? SuffixIndex with 0 known_paths?
   - Concurrency: call_logger and multiple simultaneous MCP requests?
   - Memory: SuffixIndex size for 100k+ files?

5. **Acceptance audit:**
   - All 14 items implemented?
   - Original file `code-review-epic7-deferred-2026-04-07.md` updated/replaced?

**Expected result:**
- KSD Code Review report with categorization:
  - `CRITICAL` → fix immediately (blocks merge)
  - `WARNING` → should fix (doesn't block, but important)
  - `INFO` → nice to have
- Fix CRITICAL (and WARNING where possible) in this same PR
- Final `cargo test && cargo clippy --all-targets` must pass

---

## Part III: Roadmap M2+

3 items deferred for the future. Brief architectural sketches.

### D20: Inline embedding generation during scan

**Description:** Currently `generate_embeddings()` is a separate function that rereads source from `source_map` after scan completes. During the parse+IR phase, source is already in memory. Idea: generate embedding text immediately during parsing, passing `EmbeddingProvider` through the scan pipeline.

**Complexity:** Requires reorganizing scan pipeline architecture. `EmbeddingProvider` must be available during file traversal, not after.

### D22: sqlite-vec ANN search

**Description:** Currently vector search is brute-force cosine similarity (O(N)). Degrades for >50k embeddings. Integration of `sqlite-vec` extension for HNSW index.

**Complexity:** Dependency on C extension. Requires cross-platform build compatibility assessment. Switching threshold (N > threshold) — configurable.

### D23: Per-function import usage analysis (successor to D21)

**Description:** Currently `import_context` adds all file imports to the function embedding text. More precise approach: analyze function body and determine which imports are actually used (by searching for name mentions in body_snippet).

**Complexity:** Requires AST body analysis or regex-based heuristic. May improve code embedding quality by reducing noise.

---

## Acceptance Criteria (common to all PRs)

- [ ] `cargo test --all-targets` — all tests pass
- [ ] `cargo clippy --all-targets -- -D warnings` — no warnings
- [ ] `cargo fmt --check` — formatting consistent
- [ ] Original file `code-review-epic7-deferred-2026-04-07.md` replaced with a reference to this document
- [ ] All 14 active items implemented
- [ ] KSD Code Review passed with no CRITICAL findings

---

## PR Summary Table

| # | Branch | Items | Estimate | Depends on | Key files |
|---|--------|-------|----------|------------|-----------|
| 1 | `fix/debt-quick-wins` | D7, D12, D13, D15 | 30 min | — | code_pattern.rs, validate_approach.rs (MCP), V9 migration, embedding_repository.rs |
| 2 | `fix/debt-search-quality` | D5, D6, D17 | 2-3 h | PR 1 | validate_approach.rs (graph) |
| 3 | `fix/debt-infra-robustness` | D10, D14, D16, D18, D19 | 2-3 h | PR 1 | call_logger.rs, call_logger_keys.rs, V10 migration, code_pattern.rs (3 structs), query_dependencies.rs (MCP), scan.rs |
| 4 | `fix/debt-graph-performance` | D8, D9 | 2-3 h | PR 1 | dependencies.rs |
| 5 | `fix/debt-ksd-code-review` | KSD Review | 1-2 h | PR 2,3,4 merged | All changed files |

**Total estimate:** ~8-11 hours (sequential) or ~4-5 hours (parallel PR 2,3,4).
