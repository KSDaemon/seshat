# Dev Spec: Phase 1 — MCP Response Cleanup (Noise Reduction)

**Scope:** Remove noisy, redundant, and ambiguous fields from all MCP tool responses.
**Risk:** Low — output-only changes, no logic changes, no data model changes.
**Estimated files touched:** ~6

---

## Context & Problem

Current MCP tool responses contain significant noise that wastes LLM tokens and reduces signal quality:

1. **Duplicate fields** — counts, verdicts, and identifiers appear in both `data` and `metadata`
2. **Internal plumbing fields** — `duration_ms`, `search_type`, `scope`, `branch` — not actionable by agents
3. **Ambiguous numeric fields** — `confidence: 1`, `adoption.rate: 1` — no units, no scale context
4. **Useless IDs** — `id: 10592` on conventions — nowhere accepted as input in any tool
5. **`next_steps` placement** — most actionable field, buried last in response

---

## Changes by File

### 1. `crates/seshat-mcp/src/envelope.rs`

**Remove from `ResponseEnvelope<T>`:**
- `duration_ms: u64` field and its serialization
- `scope: String` field and its serialization
- `branch: String` field and its serialization

The envelope after cleanup:
```rust
pub struct ResponseEnvelope<T: Serialize> {
    pub status: String,
    pub tool: String,
    pub repo: String,
    // scope, branch, duration_ms — REMOVED
    pub data: T,
    pub metadata: ResponseMetadata,
}
```

**Move `next_steps` to top of serialized output:**
Ensure `next_steps` serializes before other metadata fields. Use `#[serde(rename)]` or reorder struct fields — serde serializes in declaration order for JSON.

---

### 2. `crates/seshat-mcp/src/tools/query_convention.rs`

**Remove from `metadata` extras:**
- `results_count` — agent can count `data.conventions.length()` itself
- `search_type` — always `"fts5"`, not actionable

**Remove from each convention object (in graph layer response):**
- `id` field — not accepted as input by any tool

**Change numeric fields on each convention:**

From:
```json
{
  "confidence": 1,
  "adoption": {
    "count": 13,
    "total": 13,
    "rate": 1
  }
}
```

To:
```json
{
  "confidence_pct": 100,
  "adoption": {
    "count": 13,
    "total": 13,
    "rate_pct": 100
  }
}
```

Implementation: change serialization in `crates/seshat-graph/src/conventions.rs` in the `ConventionResult` struct or its `Serialize` impl. Multiply `confidence` (f64 0.0–1.0) by 100 and round to nearest integer before serializing. Same for `adoption.rate`.

**Where to make the change:** Find `ConventionResult` struct serialization in `crates/seshat-graph/src/conventions.rs`. Add a custom serializer or transform the value before building the JSON response in `query_convention::handle()`.

---

### 3. `crates/seshat-mcp/src/tools/query_code_pattern.rs`

**Remove from `metadata` extras:**
- `pattern_count` — duplicates `data.patterns.length()`
- `convention_count` — duplicates `data.related_conventions.length()`
- `search_type` — not actionable

**Remove nested `data.metadata` object entirely:**
Currently `data` contains a nested `metadata: { pattern_count, convention_count, search_type }` — this is a duplicate of the top-level metadata extras. Remove `data.metadata` completely.

Find the response struct in `crates/seshat-graph/src/code_pattern.rs` (the `QueryCodePatternData` or equivalent) and remove the nested metadata field.

---

### 4. `crates/seshat-mcp/src/tools/query_dependencies.rs`

**Remove from `metadata` extras (all are exact duplicates of `data` fields):**
- `target` — exact duplicate of `data.target`
- `dependent_count` — exact duplicate of `data.dependents.length()`
- `dependency_count` — exact duplicate of `data.dependencies.length()`
- `blast_radius` — exact duplicate of `data.blast_radius`

After cleanup, `metadata` for this tool should contain only `next_steps`.

**Add threshold context to `blast_radius_count`:**
Currently `blast_radius_count` is a raw integer with no documented thresholds. Add a companion field or embed in description. Simplest: remove `blast_radius_count` from response entirely (the categorical `blast_radius: "low|medium|high"` is sufficient for agent reasoning).

---

### 5. `crates/seshat-mcp/src/tools/validate_approach.rs`

**Remove from `metadata` extras (all duplicates):**
- `verdict` — exact duplicate of `data.verdict`
- `ready` — exact duplicate of `data.ready`
- `rule_count` — duplicates `data.rules.length()`
- `duplicate_count` — duplicates `data.duplicates.length()`
- `convention_count` — duplicates `data.conventions.length()`

After cleanup, `metadata` contains only `next_steps`.

---

### 6. `crates/seshat-mcp/src/tools/project_context.rs`

Minimal changes needed here — metadata is already relatively clean.

**Remove from envelope:** `scope`, `branch`, `duration_ms` (handled by envelope change in file #1).

No other changes needed for this tool.

---

## `next_steps` Placement

After the envelope change, ensure `next_steps` appears at the top of `metadata` in the serialized JSON. In Rust serde, struct fields serialize in declaration order.

In `ResponseMetadata`:
```rust
pub struct ResponseMetadata {
    pub next_steps: Vec<String>,  // FIRST field — serializes first
    // all other fields after
}
```

Verify this is already the case or reorder if needed.

---

## Testing

After changes, verify with the existing MCP integration test suite:
```bash
cargo test -p seshat-mcp
```

Manually verify by running a test call against the running server:
```bash
# Start server
cargo run --bin seshat serve

# Test query_convention
echo '{"topic": "error handling"}' | seshat-mcp-client query_convention
```

Expected: response has no `duration_ms`, no `scope`, no `branch`, `confidence_pct: 100` instead of `confidence: 1`, no duplicate fields in metadata.

---

## Acceptance Criteria

- [ ] `duration_ms`, `scope`, `branch` absent from ALL tool responses
- [ ] `search_type` absent from `query_convention` and `query_code_pattern`
- [ ] `id` field absent from convention objects in `query_convention`
- [ ] `confidence_pct` (integer 0–100) replaces `confidence` (float 0.0–1.0)
- [ ] `adoption.rate_pct` (integer 0–100) replaces `adoption.rate` (float 0.0–1.0)
- [ ] No duplicate fields between `data` and `metadata` in any tool
- [ ] Nested `data.metadata` removed from `query_code_pattern`
- [ ] `next_steps` is the first field in `metadata` in serialized JSON
- [ ] All existing MCP tests pass
- [ ] No breaking changes to error response format (`ErrorEnvelope`)
