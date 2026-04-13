# Dev Spec: Phase 2 — Real Code Snippets + Pipeline Source Map

**Scope:** Restructure the scanning pipeline to pass file content through without re-reading, deliver real source code snippets in detector evidence, and eliminate redundant I/O in embedding generation.
**Risk:** Medium — touches core pipeline, IR types, detector trait, all 8 detectors, and embedding generation.
**Files touched:** ~20

---

## Context & Problem

### Root Cause 1: `CodeEvidence` has no `file` field

```rust
// crates/seshat-core/src/detector_result.rs:22 — CURRENT
pub struct CodeEvidence {
    pub line: usize,
    pub end_line: usize,
    pub snippet: String,
}
```

File path lives in `ConventionFinding.file_path` but is lost during aggregation into `AggregatedConvention`. Evidence snippets in the final DB have no per-snippet file attribution.

### Root Cause 2: All snippets are synthetic strings, not real code

Every detector constructs snippets via `format!()` from IR metadata:
- `format!("Custom error type: {error_type}")` — not source code
- `format!("fn {}", f.name)` — not the actual function signature
- `"module.exports = ..."` — hardcoded placeholder
- `format!("#[derive({})] on {}", derives.join(", "), type_name)` — reconstructed

None of the 8 detectors lift actual lines from source files.

### Root Cause 3: Source is dropped before detectors run

```
orchestrator.rs:220  read_to_string()  → source: String  (in memory)
orchestrator.rs:259  parse_file(&source)  → ProjectFile  (source still in scope)
orchestrator.rs:271  parsed_files.push(project_file)  ← source DROPPED here
orchestrator.rs:308  file_ir_repo.upsert(project_file)  → SQLite

scan.rs:440          get_by_branch()  → Vec<ProjectFile>  ← reload from SQLite
scan.rs:452          run_all_detectors(&all_files)  ← no source available
scan.rs:682          read_to_string() AGAIN  ← embeddings re-read each file
```

Source is available at line 259 but not carried forward. Both detectors and embeddings suffer from this.

---

## Architecture: `source_map` Through the Pipeline

The fix: after `parse_file()` returns, keep `source` alive in a `HashMap<PathBuf, String>` and carry it through `ScanResult` to all consumers (detectors + embeddings).

**Key constraint:** Only new/changed files have source in the map. Unchanged files (hash match → `continue`) are skipped — but their detectors already ran in a previous scan and their embeddings already exist in the DB. No re-processing needed for unchanged files.

---

## Step-by-Step Changes

### Step 1: Add `file` to `CodeEvidence`

**File:** `crates/seshat-core/src/detector_result.rs`

```rust
// BEFORE
pub struct CodeEvidence {
    pub line: usize,
    pub end_line: usize,
    pub snippet: String,
}

// AFTER
pub struct CodeEvidence {
    pub file: PathBuf,    // ← ADD: real path to the source file
    pub line: usize,
    pub end_line: usize,
    pub snippet: String,  // will become real source code in Step 5
}
```

**Fix all construction sites:** Every place that creates a `CodeEvidence` now needs a `file` field. The compiler will tell you every location after this change. For all existing synthetic constructions, temporarily set `file: file.path.clone()` — the real snippet extraction comes in Step 5.

---

### Step 2: Add `source_map` to `ScanResult`

**File:** `crates/seshat-scanner/src/orchestrator.rs`

**Change `ScanResult` struct** (around line 81):
```rust
pub struct ScanResult {
    // ... existing fields unchanged ...

    /// Source content for newly parsed/changed files only.
    /// Unchanged files are absent — their previous scan results remain valid.
    ///
    /// Memory note: this map holds source for changed files only, not the full repo.
    /// For typical repos (< 10k changed files per scan) this is negligible.
    /// For very large monorepos under extreme churn, limit parallel file parsing
    /// threads (suggested default: 10) so at most N sources are in-flight before
    /// insertion. This is a future optimization — not required for this phase.
    pub source_map: HashMap<PathBuf, String>,
}
```

**In the scan loop** (around line 217–278), after `parse_file()` call (line 259):
```rust
// CURRENT (line 259–271):
let mut project_file = parse_file(&df.path, &source, df.language);
// ...
parsed_files.push(project_file);
// source dropped here

// AFTER:
let mut project_file = parse_file(&df.path, &source, df.language);
// ...
parsed_files.push(project_file);
source_map.insert(df.path.clone(), source);  // ← keep source alive
```

Declare `source_map: HashMap<PathBuf, String>` at the top of the function, populate in the loop, include in the returned `ScanResult`.

**Do NOT include unchanged files in `source_map`.** The `continue` at line 238/247 (hash match) correctly skips them — leave that logic untouched.

---

### Step 3: Add `detect_with_source` to `ConventionDetector` trait

**File:** `crates/seshat-detectors/src/trait_def.rs`

```rust
pub trait ConventionDetector: Send + Sync {
    fn name(&self) -> &'static str;

    /// Detect conventions using IR only (used for unchanged files loaded from DB).
    fn detect(&self, file: &ProjectFile) -> Vec<ConventionFinding>;

    /// Detect conventions with access to raw source content.
    /// Required — no default fallback. All 8 detectors must implement this.
    /// Called for every new/changed file; `detect()` is called for unchanged files only.
    fn detect_with_source(
        &self,
        file: &ProjectFile,
        source: &str,
    ) -> Vec<ConventionFinding>;

    fn detect_cross_file(&self, _files: &[ProjectFile]) -> Vec<ConventionFinding> {
        Vec::new()
    }

    fn supported_languages(&self) -> &[Language];
}
```

**Breaking change — intentional.** This is a fully internal project with no external consumers. The compiler will list every detector that needs updating. No default impl, no backward-compat shim, no deprecated annotation — implement it correctly everywhere, once.

---

### Step 4: Update `run_all_detectors` to accept `source_map`

**File:** `crates/seshat-detectors/src/pipeline.rs`

**Change signature:**
```rust
// BEFORE
pub fn run_all_detectors(
    files: &[ProjectFile],
    config: &DetectionConfig,
    on_progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Vec<DetectorResults>

// AFTER
pub fn run_all_detectors(
    files: &[ProjectFile],
    source_map: &HashMap<PathBuf, String>,  // ← ADD (empty HashMap is fine)
    config: &DetectionConfig,
    on_progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Vec<DetectorResults>
```

**In the inner dispatch** (around line 155), change:
```rust
// BEFORE
detector.detect(file)

// AFTER
if let Some(source) = source_map.get(&file.path) {
    detector.detect_with_source(file, source)
} else {
    detector.detect(file)
}
```

**Call sites to update:**
- `crates/seshat-cli/src/scan.rs:452` — pass `&scan_result.source_map`
- `crates/seshat-graph/src/detection.rs:83` — pass `&HashMap::new()` (watcher path, see Step 8)

---

### Step 5a: Add `extract_snippet` helper

**File:** `crates/seshat-detectors/src/snippet.rs` (new file, pub within crate)

```rust
/// Extract lines [line..=end_line] from source (1-indexed, inclusive).
/// Returns up to `max_lines` lines joined by "\n".
/// Gracefully handles all edge cases — never panics.
pub fn extract_snippet(source: &str, line: usize, end_line: usize, max_lines: usize) -> String {
    if source.is_empty() || line == 0 || line > end_line {
        return String::new();
    }
    let start = line - 1; // convert to 0-indexed
    let end = end_line; // end_line is inclusive, lines().nth() is 0-indexed so end_line maps to index end_line-1; we take min with count
    let lines: Vec<&str> = source.lines().collect();
    let end_clamped = end.min(lines.len()); // clamp to actual file length
    if start >= end_clamped {
        return String::new();
    }
    let take = (end_clamped - start).min(max_lines);
    lines[start..start + take].join("\n")
}
```

**Expose in `lib.rs`:** `pub mod snippet;` + `pub use snippet::extract_snippet;`

**Test matrix** (in `snippet.rs` `#[cfg(test)]` block):

| Test name | line / end_line / max | Source | Expected |
|---|---|---|---|
| `normal_range` | 3 / 5 / 10 | 10-line file | lines 3-5 |
| `line_zero` | 0 / 0 / 10 | any | `""` |
| `end_beyond_eof` | 3 / 999 / 10 | 5-line file | lines 3-5 |
| `single_line` | 2 / 2 / 10 | 5-line file | line 2 only |
| `empty_source` | 1 / 1 / 10 | `""` | `""` |
| `line_gt_end` | 5 / 3 / 10 | 10-line file | `""` |
| `max_lines_truncation` | 1 / 20 / 5 | 20-line file | first 5 lines |
| `utf8_multibyte` | 1 / 2 / 10 | 2-line file with unicode | correct lines, no panic |

---

### Step 5: Implement `detect_with_source` in all 8 detectors via `RawFinding` pattern

**No double pass.** Each detector splits its logic into:
1. `find_items()` — private method, pure IR traversal, returns `Vec<RawFinding>` with line numbers only, no snippets
2. `detect()` — calls `find_items()`, builds `ConventionFinding` with `snippet: String::new()` (IR-only path)
3. `detect_with_source()` — calls `find_items()`, builds `ConventionFinding` with `snippet = extract_snippet(...)` (real source path)

**`RawFinding` struct** — private, defined per-detector or shared in a `crates/seshat-detectors/src/raw_finding.rs`:

```rust
/// Intermediate finding from IR traversal — no snippet, just coordinates.
pub(crate) struct RawFinding {
    pub kind: &'static str,
    pub description: String,
    pub line: usize,
    pub end_line: usize,
}

impl RawFinding {
    pub fn into_evidence_ir(self, file_path: &PathBuf) -> CodeEvidence {
        CodeEvidence {
            file: file_path.clone(),
            line: self.line,
            end_line: self.end_line,
            snippet: String::new(),
        }
    }

    pub fn into_evidence_with_source(self, file_path: &PathBuf, source: &str) -> CodeEvidence {
        let snippet = if self.line > 0 {
            extract_snippet(source, self.line, self.end_line, 10)
        } else {
            self.description.clone() // file_structure: keep path-based description
        };
        CodeEvidence {
            file: file_path.clone(),
            line: self.line,
            end_line: self.end_line,
            snippet,
        }
    }
}
```

**Detector implementation pattern:**

```rust
impl SomeDetector {
    fn find_items(&self, file: &ProjectFile) -> Vec<RawFinding> {
        // All IR traversal logic lives here — no format!() snippets
        // Returns line numbers from IR (functions, types, imports, etc.)
    }
}

impl ConventionDetector for SomeDetector {
    fn detect(&self, file: &ProjectFile) -> Vec<ConventionFinding> {
        self.find_items(file)
            .into_iter()
            .map(|r| ConventionFinding {
                // ... build from r using into_evidence_ir()
            })
            .collect()
    }

    fn detect_with_source(&self, file: &ProjectFile, source: &str) -> Vec<ConventionFinding> {
        self.find_items(file)
            .into_iter()
            .map(|r| ConventionFinding {
                // ... build from r using into_evidence_with_source()
            })
            .collect()
    }
}
```

**Per-detector notes:**

| Detector | File | `find_items` returns | Special case |
|---|---|---|---|
| `error_handling` | `error_handling.rs` | line of type/enum/struct definition | — |
| `dependency_usage` | `dependency_usage.rs` | line of import/use statement | — |
| `naming_conventions` | `naming.rs` | line of function/type declaration | — |
| `export_patterns` | `export_patterns.rs` | line of export/pub statement | — |
| `import_organization` | `import_organization.rs` | first_line + last_line of import block | `max_lines: 20` for full block |
| `logging_observability` | `logging_observability.rs` | line of logging import/call | — |
| `test_patterns` | `test_patterns.rs` | line of test function | — |
| `file_structure` | `file_structure.rs` | `line: 0, end_line: 0` | `into_evidence_with_source` falls back to description; `detect_with_source` = `detect` logic |

**`file_structure` special case:** No line numbers available (evidence is path-based). `find_items` returns `line: 0, end_line: 0`. `into_evidence_with_source` detects `line == 0` and uses `self.description` as the snippet. The `file` field is still set correctly to `file.path.clone()`.

**`import_organization` special case:** Evidence spans the full import block. Pass `max_lines: 20` to `extract_snippet` to capture the entire block rather than just 10 lines.

---

### Step 6: Fix `convention_to_node` serialization

**File:** `crates/seshat-graph/src/detection.rs` (around lines 129–179)

**Current (broken):**
```rust
serde_json::json!({
    "file":     e.snippet.lines().next().unwrap_or(""),  // ← BUG: uses snippet as file path
    "line":     e.line,
    "end_line": e.end_line,
    "snippet":  { "content": e.snippet, "truncated": false },
})
```

**Fixed:**
```rust
serde_json::json!({
    "file":     e.file.display().to_string(),  // ← real path from CodeEvidence.file
    "line":     e.line,
    "end_line": e.end_line,
    "snippet":  { "content": e.snippet, "truncated": false },
})
```

This is the serialization into `ext_data` in the `nodes` table. The fix is one line.

---

### Step 7: Update `scan.rs` to pass `source_map` to detectors

**File:** `crates/seshat-cli/src/scan.rs`

**Change at line 452:**
```rust
// BEFORE
let detector_results = run_all_detectors(&all_files, &detection_config, Some(&progress_cb));

// AFTER
let detector_results = run_all_detectors(
    &all_files,
    &scan_result.source_map,  // ← pass source map from scan phase
    &detection_config,
    Some(&progress_cb),
);
```

Note: `all_files` still comes from `get_by_branch()` (DB reload) to include unchanged files for cross-file detection. `source_map` provides source only for new/changed files — detectors fall back to IR-only for unchanged ones.

---

### Step 8: Optimize `generate_embeddings` — eliminate re-reads and skip unchanged files

**File:** `crates/seshat-cli/src/scan.rs`

**Change function signature:**
```rust
// BEFORE
fn generate_embeddings(
    db: &Database,
    embedding_config: &EmbeddingConfig,
    all_files: &[ProjectFile],
    branch_id: &str,
    show: bool,
) -> Result<(), CliError>

// AFTER
fn generate_embeddings(
    db: &Database,
    embedding_config: &EmbeddingConfig,
    all_files: &[ProjectFile],
    source_map: &HashMap<PathBuf, String>,  // ← ADD
    branch_id: &str,
    show: bool,
) -> Result<(), CliError>
```

**Change the file loop** (around line 670–690):
```rust
// BEFORE
for file in all_files {
    // Process all files, re-read source from disk
    let source_lines: Option<Vec<String>> = std::fs::read_to_string(&file.path)
        .ok()
        .map(|s| s.lines().map(str::to_owned).collect());
    // ... process ...
}

// AFTER
for file in all_files {
    // Skip files not in source_map — they are unchanged, embeddings already current in DB
    let source = match source_map.get(&file.path) {
        Some(s) => s,
        None => continue,  // ← unchanged file: skip re-embedding
    };
    let source_lines: Vec<String> = source.lines().map(str::to_owned).collect();
    // ... rest of processing unchanged, just use source_lines directly ...
}
```

**Update call site** (around line 487):
```rust
// BEFORE
generate_embeddings(&db, embedding_config, &all_files, "main", show)?;

// AFTER
generate_embeddings(&db, embedding_config, &all_files, &scan_result.source_map, "main", show)?;
```

**Result:**
- Zero `read_to_string` calls in embedding generation for any file
- Unchanged files are skipped entirely — their embeddings remain in `code_embeddings` table as-is
- Changed/new files get fresh embeddings from the source already in memory

---

### Step 9: Watcher path (`run_detection_cycle`)

**File:** `crates/seshat-graph/src/detection.rs`

The watcher path always reloads from SQLite (line 68–71) and does not have access to `source_map`. For now, pass an empty map — detectors fall back to IR-only (`detect()`) for watcher-triggered rescans.

```rust
// In run_detection_cycle (line 83):
let detector_results = run_all_detectors(
    &all_files,
    &HashMap::new(),  // ← empty: watcher uses IR-only detection (fallback)
    detection_config,
    None,
);
```

**Future work (not in this spec):** When the watcher detects a file change, it could read that file's content and build a mini `source_map` with just the changed files before calling `run_detection_cycle`. This would give real snippets for watcher-triggered rescans too. Defer to a future PR.

---

## Data Flow After Changes

```
orchestrator.rs:220  read_to_string()  → source: String
orchestrator.rs:259  parse_file(&source)  → ProjectFile
orchestrator.rs:271  source_map.insert(path, source)  ← KEEP SOURCE
orchestrator.rs:308  file_ir_repo.upsert(project_file)  → SQLite (unchanged)

ScanResult {
    source_map: HashMap<PathBuf, String>,  // only new/changed files
    ...
}

scan.rs:440  get_by_branch()  → all_files (all files, inc. unchanged)

scan.rs:452  run_all_detectors(&all_files, &source_map, ...)
             ↓ per file:
             if source_map.has(file.path) → detect_with_source(file, source)
               → evidence.file = real PathBuf
               → evidence.snippet = real source lines
             else → detect(file)  [unchanged files, IR-only]

scan.rs:487  generate_embeddings(&all_files, &source_map, ...)
             ↓ per file:
             if not in source_map → continue  [skip unchanged]
             else → use source from map  [no disk read]
```

---

## Evidence in DB After Changes

**Before (broken):**
```json
{
  "file": "Custom error type: ConfigError",
  "line": 196,
  "end_line": 196,
  "snippet": { "content": "Custom error type: ConfigError", "truncated": false }
}
```

**After (correct):**
```json
{
  "file": "crates/seshat-core/src/config.rs",
  "line": 196,
  "end_line": 196,
  "snippet": { "content": "#[derive(Debug, thiserror::Error)]\npub enum ConfigError {\n    #[error(\"missing field: {0}\")]\n    MissingField(String),\n    ...\n}", "truncated": false }
}
```

---

## Testing

### Unit: `extract_snippet` (in `crates/seshat-detectors/src/snippet.rs`)

All 8 cases must pass — see Step 5a test matrix. Run with:
```bash
cargo test -p seshat-detectors snippet
```

### Unit: per-detector `detect_with_source`

For each of the 8 detectors, add a test that:
1. Loads a small fixture source string (inline `&str` — no file I/O needed)
2. Builds a minimal `ProjectFile` from that fixture via the existing IR parser or manually constructed IR
3. Calls `detect_with_source(file, source)`
4. Asserts:
   - `evidence[0].file` == `file.path`
   - `evidence[0].snippet` does **not** contain `"Custom "`, `"fn "` (synthetic format patterns) — it contains actual source characters
   - `evidence[0].line > 0` (except `file_structure`: assert `line == 0` and `snippet` non-empty)

```bash
cargo test -p seshat-detectors
```

### Unit: per-detector `detect` (IR-only path, regression)

Existing tests must continue to pass — `detect()` still returns valid findings with `snippet: ""`. No regression on unchanged-file path.

### Unit: `convention_to_node` serialization

After Step 6, add/update a test in `crates/seshat-graph/src/detection.rs` asserting that serialized evidence JSON has:
- `"file"` field = a non-empty path string (not a snippet substring)
- `"snippet"."content"` = a string that may be empty but is not a file path

### Integration

```bash
# Full pipeline
cargo test --workspace

# Manual end-to-end
cargo run --bin seshat scan
```

Then verify via `query_convention`:
1. Call with topic `"error handling"`
2. `examples[0].file` → real path like `"crates/seshat-detectors/src/error_handling.rs"`
3. `examples[0].snippet.content` → real Rust code, not `"Custom error type: ..."`
4. Call with topic `"naming"` → `examples[0].snippet.content` contains actual function signature
5. Call with topic `"imports"` → multi-line import block in snippet

---

## Acceptance Criteria

- [ ] `CodeEvidence` has `file: PathBuf` field
- [ ] `ScanResult` has `source_map: HashMap<PathBuf, String>`
- [ ] Source is not re-read from disk anywhere in the scan→detect→embed pipeline
- [ ] All 8 detectors implement `detect_with_source()` with real snippet extraction
- [ ] `convention_to_node()` uses `e.file.display().to_string()` for the file field
- [ ] `query_convention` examples contain real file paths (not synthetic strings)
- [ ] `query_convention` examples contain real source code lines (not `format!()` strings)
- [ ] Embedding generation skips unchanged files (`source_map` lookup)
- [ ] No `read_to_string` calls in `generate_embeddings` loop
- [ ] `file_structure` detector evidence: `file` is set, snippet stays path-based (line 0 is valid)
- [ ] Watcher path compiles with `&HashMap::new()` passed to `run_all_detectors`
- [ ] All existing tests pass: `cargo test --workspace`
- [ ] Full scan on seshat repo completes without errors
