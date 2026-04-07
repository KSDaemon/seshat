# Code Review — Epic 7: Deferred Findings

**Date:** 2026-04-07
**Branch:** `feat/advanced-mcp-tools`
**Scope:** Items surfaced during code review that are pre-existing issues or out-of-scope enhancements, not caused by the current change set.

---

## Embedding Provider (seshat-embedding)

### D1: Endpoint not configurable (Ollama and OpenAI)
- **Location:** `crates/seshat-embedding/src/lib.rs:207, 300`
- **Description:** Ollama hardcodes `localhost:11434`, OpenAI hardcodes `api.openai.com`. `with_endpoint` exists but is `#[cfg(test)]` only. Users behind Docker, Azure OpenAI, vLLM, or proxies have no config path.
- **Recommendation:** Add `endpoint` / `base_url` field to `EmbeddingConfig` and make `with_endpoint` public.

### D2: No retry/backoff on transient HTTP failures
- **Location:** `crates/seshat-embedding/src/lib.rs` (both providers)
- **Description:** Single attempt per request. 429/503 → immediate failure. For embedding workloads with hundreds of batches, this is fragile.
- **Recommendation:** Add configurable retry with exponential backoff (max 3 retries, respect Retry-After header).

### D3: `tracing` dependency declared but never used
- **Location:** `crates/seshat-embedding/Cargo.toml:13`
- **Description:** Zero `tracing::` calls in 825-line file. No logging on HTTP failures or provider selection.
- **Recommendation:** Add `tracing::info!` for provider creation, `tracing::warn!` for HTTP errors.

### D4: No health check for Ollama — 30s timeout per batch
- **Location:** `crates/seshat-embedding/src/lib.rs:180-185`
- **Description:** When Ollama isn't running, each batch waits 30s before timeout. 100 batches × 30s = ~50 min hang.
- **Recommendation:** Add lightweight health check in `create_provider()` with short timeout (2s).

## Graph Layer (seshat-graph)

### D5: `keyword_search_nodes` LIKE without stop-word filtering
- **Location:** `crates/seshat-graph/src/validate_approach.rs:320-341`
- **Description:** Common words like "use", "the", "new" match enormous numbers of nodes. LIKE with leading `%` prevents index usage. OR-join of clauses can return the entire nodes table. No result limit.
- **Recommendation:** Add stop-word list, consider FTS5 for decisions/observations too, add LIMIT.

### D6: FTS5 vs LIKE search inconsistency
- **Location:** `crates/seshat-graph/src/validate_approach.rs`
- **Description:** Rules/conventions searched via FTS5 (AND-matching), decisions/observations via LIKE (OR-matching). Same description returns different node sets through these two paths.
- **Recommendation:** Unify search mechanism — FTS5 for all, or document the intentional difference.

### D7: f32 accumulation in cosine_similarity — precision loss
- **Location:** `crates/seshat-graph/src/code_pattern.rs:240-251`
- **Description:** Dot product and magnitude use f32 accumulators. For 384-1536 dim vectors, accumulated floating-point error can affect ranking of close scores.
- **Recommendation:** Use f64 accumulators, or accept the precision tradeoff and document it.

### D8: `resolve_by_suffix` O(N*E) complexity
- **Location:** `crates/seshat-graph/src/dependencies.rs:310-341`
- **Description:** Iterates ALL known_paths × FILE_EXTENSIONS for each import. 50k paths × 6 extensions × 100 imports = 30M string ops per query.
- **Recommendation:** Build reverse suffix index (HashMap) at load time for O(1) lookups.

### D9: Workspace imports invisible in dependency graph
- **Location:** `crates/seshat-graph/src/dependencies.rs:244`
- **Description:** `is_likely_internal()` doesn't recognize workspace-relative imports like `my_crate::module`. These are treated as external and excluded from the dependency graph.
- **Recommendation:** Cross-reference module prefix against workspace crate names from Cargo.toml.

## MCP Layer (seshat-mcp)

### D10: Call logger silent degradation on key renames
- **Location:** `crates/seshat-mcp/src/call_logger.rs:105-197`
- **Description:** All logger extraction functions use `unwrap_or(0)` / `unwrap_or("unknown")`. If metadata keys are renamed, logger silently reports zeroed-out values. No compile-time contract between tool handlers and logger.
- **Recommendation:** Add `tracing::debug!` when expected keys are missing. Consider shared constants for key names.

### D11: `internal_error` → `map_graph_error` refactor mixed into feature PR
- **Location:** `crates/seshat-mcp/src/tools/project_context.rs`, `query_convention.rs`
- **Description:** Error mapping function was renamed as part of this feature branch. The behavior change should be reviewed independently.
- **Recommendation:** Already shipped — verify `map_graph_error` has equivalent or better semantics.

### D12: Double-trim description in MCP + graph layers
- **Location:** `crates/seshat-mcp/src/tools/validate_approach.rs:78,91` + `crates/seshat-graph/src/validate_approach.rs:151`
- **Description:** Description trimmed at MCP layer, trimmed again in graph layer. Harmless but violates thin-handler contract.
- **Recommendation:** Choose one layer to own validation. Graph layer is the safer choice.

## Storage Layer (seshat-storage)

### D13: Redundant `idx_code_embeddings_branch` index
- **Location:** `crates/seshat-storage/migrations/V6__code_embeddings.sql:15-16`
- **Description:** Single-column index on `(branch_id)` is a prefix of the compound index `(branch_id, file_path)`. SQLite can use the compound index for branch-only queries.
- **Recommendation:** Drop the single-column index in a future migration.

### D14: No timestamps on `code_embeddings` table
- **Location:** `crates/seshat-storage/migrations/V6__code_embeddings.sql`
- **Description:** No `created_at` / `updated_at` columns. Impossible to implement TTL-based invalidation or detect stale embeddings after model changes.
- **Recommendation:** Add `updated_at DEFAULT CURRENT_TIMESTAMP` in a future migration.

### D15: `count_by_branch` i64→usize cast
- **Location:** `crates/seshat-storage/src/repository/embedding_repository.rs:180`
- **Description:** `count as usize` is lossy on 32-bit targets. Unlikely to matter in practice but technically incorrect.
- **Recommendation:** Use `usize::try_from(count).unwrap_or(0)`.
