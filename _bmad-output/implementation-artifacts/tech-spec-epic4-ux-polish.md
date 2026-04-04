---
title: 'Epic 4 Post-Implementation UX Polish'
slug: 'epic4-ux-polish'
created: '2026-04-01'
status: 'done'
stepsCompleted: [1, 2, 3, 4]
tech_stack: ['Rust', 'indicatif', 'owo-colors', 'rayon', 'ignore crate', 'clap']
files_to_modify:
  - 'crates/seshat-cli/src/scan.rs'
  - 'crates/seshat-cli/src/args.rs'
  - 'crates/seshat-detectors/src/pipeline.rs'
  - 'crates/seshat-detectors/src/naming.rs'
  - 'crates/seshat-scanner/src/discovery.rs'
  - 'crates/seshat-scanner/src/orchestrator.rs'
  - 'crates/seshat-core/src/config.rs'
code_patterns:
  - 'ScanProgress enum + Fn callback pattern in orchestrator.rs'
  - 'rayon par_iter for parallel file processing in pipeline.rs'
  - 'indicatif ProgressBar with ProgressStyle in scan.rs'
  - 'WalkBuilder from ignore crate in discovery.rs'
  - 'CasePattern enum + matches_expected() in naming.rs'
test_patterns:
  - '#[cfg(test)] mod tests at bottom of each file'
  - 'tempfile::TempDir for filesystem tests'
  - 'make_rust_file() / make_ts_file() helpers in naming.rs'
  - 'setup_temp_project() helper in discovery.rs'
---

# Tech-Spec: Epic 4 Post-Implementation UX Polish

**Created:** 2026-04-01

## Overview

### Problem Statement

Post-implementation testing of `seshat scan` on real projects (Seshat itself ~200 files, walt-chat-backend ~1050 files) revealed three UX/logic issues that undermine the "first impression" goal of Epic 4:

1. **No progress visibility after scanning phase** -- After the scanning progress bar clears, `run_all_detectors()` and `collect_git_file_dates()` run silently for several seconds. The tool appears frozen.
2. **Single-word naming false positives** -- File names like `utils` and `styles` generate confusing convention entries ("uses single_lower_word (expected snake_case)") that clutter the report with noise.
3. **Git submodule files included in scan** -- Submodule directories are scanned as part of the root project, mixing conventions from unrelated codebases (e.g., Python backend + TypeScript frontend submodule).

Additionally, a **duplicate `collect_git_file_dates()` call** was discovered: it runs inside the orchestrator AND again in `scan.rs`.

### Solution

Three targeted fixes plus one optimization:

1. Add progress indicators for post-scan phases (detection progress bar, git dates spinner) and stop clearing the scanning bar.
2. Reclassify single-word file names using the expected pattern name in descriptions, and generalize descriptions for better aggregation.
3. Exclude git submodule paths from discovery by default, with `--include-submodules` opt-in flag.
4. Eliminate duplicate `collect_git_file_dates()` call by exposing git dates from the orchestrator's `ScanResult`.

### Scope

**In Scope:**
- Progress indicators for detection and git date collection phases
- Change `finish_and_clear()` to `finish_with_message()` on scanning progress bar
- Fix single-word naming description text in naming detector
- Generalize file naming descriptions for proper aggregation
- Exclude git submodules from discovery by default
- Add `--include-submodules` CLI flag and `ScanConfig` field
- Remove duplicate `collect_git_file_dates()` call from `scan.rs`
- Unit tests for all changes

**Out of Scope:**
- Full submodule support with separate knowledge graphs (Epic 6)
- Multi-line persistent progress display (nice-to-have, not required)
- Language grouping/prefixing in convention report output
- Changes to the report rendering module (`report/`)

## Context for Development

### Codebase Patterns

- **Progress callback pattern**: `ScanProgress` enum with 4 variants in `orchestrator.rs`, passed as `Fn(&ScanProgress)` closure from `scan.rs`. Extend this pattern for detection.
- **Rayon parallel iteration**: `pipeline.rs` uses `files.par_iter()` for Phase 1 detection. Thread-safe counter via `AtomicUsize` is the standard approach for progress in rayon.
- **indicatif usage**: Spinner for discovery (braille chars), progress bar for scanning (`█░` chars). `ProgressDrawTarget::hidden()` when quiet mode.
- **WalkBuilder**: `ignore` crate's `WalkBuilder` has `.git_submodules(bool)` method. Currently not called (defaults to true = descend into submodules).
- **Naming detector**: Per-file findings for file naming (unlike function/type naming which aggregate by percentage). `CasePattern::SingleLowerWord` correctly matches expected patterns via `matches_expected()`, but the description text exposes the raw pattern name.
- **ScanResult struct**: Currently does NOT include git file dates. Orchestrator calls `collect_git_file_dates()` internally and uses the result for DB persistence (passing dates to `file_ir_repo.upsert()`), but does NOT expose the dates in the returned `ScanResult` struct. The second call in `scan.rs` re-does the work.

### Files to Reference

| File | Purpose |
| ---- | ------- |
| `crates/seshat-cli/src/scan.rs` | Scan command orchestration, progress bars, calls detectors |
| `crates/seshat-cli/src/args.rs` | CLI argument definitions (clap derive) |
| `crates/seshat-detectors/src/pipeline.rs` | `run_all_detectors()`, rayon par_iter |
| `crates/seshat-detectors/src/naming.rs` | File naming detection, `classify_case()`, `matches_expected()` |
| `crates/seshat-scanner/src/discovery.rs` | `discover_files()`, WalkBuilder setup |
| `crates/seshat-core/src/config.rs` | `ScanConfig` struct definition |
| `crates/seshat-scanner/src/orchestrator.rs` | `scan_project_with_progress()`, `ScanResult` struct |
| `docs/research/epic4-cli-scan-ux-issues-2026-04-01.md` | Full issue analysis with root causes |

### Technical Decisions

1. **Detection progress: full progress bar (option a)** -- `run_all_detectors()` gets an `on_progress: Option<&(dyn Fn(usize, usize) + Sync)>` parameter. Rayon increments `AtomicUsize` per file in Phase 1 (parallel per-file), callback reports `(done, total)`. Phase 2 (cross-file detection) is fast and sequential -- no additional progress needed. Total is `files.len()` for Phase 1 only. This avoids adding `indicatif` as a dependency to `seshat-detectors`. Note: `ProgressBar` from indicatif is `Send + Sync`, so the callback closure in `scan.rs` can capture it directly without `RefCell`.
2. **Git dates: simple spinner** -- Short-lived, unpredictable duration. Spinner is sufficient.
3. **Scanning bar: keep visible on completion** -- Replace `finish_and_clear()` with `finish()`. The progress bar template must include `{msg}` placeholder for `finish_with_message()` to work; alternatively, use `finish()` which keeps the bar as-is showing 100%.
4. **Naming fix: option (a)** -- When `matches_expected()` returns true due to single-word compatibility, use the expected pattern name in the description. Additionally, generalize file naming descriptions to remove the specific stem for better aggregation.
5. **Submodules: exclude by default using `WalkBuilder::git_submodules(false)`** -- The `ignore` crate's `WalkBuilder` has a built-in `.git_submodules(bool)` method that controls whether to descend into git submodules. Call `builder.git_submodules(config.include_submodules)` in `discover_files()`. For the info message, separately detect submodule paths by parsing `.gitmodules` (simple line parsing). `--include-submodules` flag for opt-in.
6. **Duplicate git dates: expose from ScanResult** -- Add `pub file_dates: HashMap<PathBuf, i64>` to `ScanResult`, populate from orchestrator's existing call, remove the second call from `scan.rs`. Note: `scan.rs` still needs to convert from `HashMap<PathBuf, i64>` to `HashMap<String, Option<i64>>` for `aggregate_findings()` -- the conversion code remains but sources from `scan_result.file_dates`.

## Implementation Plan

### Tasks

- [ ] Task 1: Add progress callback to `run_all_detectors()`
  - File: `crates/seshat-detectors/src/pipeline.rs`
  - Action: Add `on_progress: Option<&(dyn Fn(usize, usize) + Sync)>` parameter to `run_all_detectors()` and `run_detectors()`. In the rayon `par_iter` block (lines 71-80), add `AtomicUsize` counter, increment after each file, call `on_progress(done, total)` if Some where `total = files.len()`. Phase 2 (cross-file detection, lines 85-114) is sequential and fast -- no additional progress needed; the progress bar will already be at 100% when Phase 2 starts. Update `run_all_detectors()` to pass through callback.
  - Notes: `Fn` must be `Sync` for rayon. Use `AtomicUsize::fetch_add(1, Ordering::Relaxed)`. Call frequency: every file (1050 calls for large project is fine -- indicatif throttles internally). `ProgressBar` from indicatif is `Send + Sync`, so the closure in `scan.rs` captures it directly (no `RefCell` needed).

- [ ] Task 2: Expose git file dates from `ScanResult`
  - File: `crates/seshat-scanner/src/orchestrator.rs`
  - Action: Add `pub file_dates: HashMap<PathBuf, i64>` field to `ScanResult` struct. The orchestrator already calls `collect_git_file_dates()` (around line 148) and stores the result in `git_file_dates` for DB persistence. Clone/move the dates into the `ScanResult` struct being returned. Update the `ScanResult` construction site (where the struct is built and returned) to include the new field. Also update any test code that constructs `ScanResult` directly (the struct is NOT `#[non_exhaustive]`, so all construction sites must include the new field).
  - Notes: ScanResult already has `manifest_analyses`, `files_discovered`, etc. This follows the same pattern. The type is `HashMap<PathBuf, i64>` (not `Option<i64>`) because the orchestrator only stores files that have dates. `scan.rs` will still need to convert to `HashMap<String, Option<i64>>` for `aggregate_findings()` -- the conversion code stays but sources from `scan_result.file_dates`.

- [ ] Task 3: Wire progress indicators in scan.rs
  - File: `crates/seshat-cli/src/scan.rs`
  - Action:
    1. Change `pb.finish_and_clear()` (line 128) to `pb.finish()` so the bar stays visible showing 100%. (The current template has no `{msg}` placeholder, so `finish_with_message()` won't display text. Either add `{msg}` to the template or use `finish()`.)
    2. Before `run_all_detectors()` call at line 141: create detection progress bar with template `"  Analyzing conventions {bar:40.cyan/dim} {pos}/{len}"`. Create a closure that captures the bar and calls `pb.set_position(done)`. Pass `Some(&closure)` as the `on_progress` parameter to `run_all_detectors(&all_files, &detection_config, Some(&progress_closure))`. The `ProgressBar` is `Send + Sync` so the closure can be `Fn + Sync`. Finish the detection bar after `run_all_detectors()` returns.
    3. After detection, before building file_dates_map: create spinner `"  Collecting git history..."` (same braille style as discovery spinner). The conversion from `scan_result.file_dates` is fast, but makes the flow consistent.
    4. Remove the duplicate `collect_git_file_dates(&root)` call (line 150). Replace with `scan_result.file_dates` and convert: `let file_dates_map: HashMap<String, Option<i64>> = all_files.iter().map(|f| { let date = scan_result.file_dates.get(f.path.as_path()).copied(); (f.path.to_string_lossy().to_string(), Some(date).flatten()) }).collect();`
    5. Finish all progress bars/spinners before `build_report_data()`.
  - Notes: Detection progress bar should respect `verbosity` (hidden when quiet via `ProgressDrawTarget::hidden()`). Spinner for git dates uses same braille pattern as discovery spinner. All bars/spinners use the same verbosity guard pattern already established.

- [ ] Task 4: Fix single-word naming descriptions
  - File: `crates/seshat-detectors/src/naming.rs`
  - Action:
    1. In `detect_file_naming()` (starting at line 442), split the description logic based on conformance:
       - **Conforming files** (`follows_convention = true`): use description `"File naming: {display_pattern} convention ({lang})"` where `display_pattern` is:
         - If `pattern` is `SingleLowerWord` or `SingleUpperWord`: use `expected.as_str()` (e.g., "snake_case")
         - Otherwise: use `pattern.as_str()` (e.g., "snake_case")
         - This means ALL conforming files for the same expected pattern + language produce the SAME description string, causing them to aggregate into one convention entry.
       - **Non-conforming files** (`follows_convention = false`): keep the specific description `"File naming: '{stem}' uses {pattern} (expected {expected} for {lang})"` for diagnostic value. Each non-conforming file gets its own finding (current behavior preserved).
    2. This creates an intentional asymmetry: conforming files aggregate, non-conforming files remain specific. This is correct because conforming files are "noise" individually but meaningful as a group statistic, while non-conforming files are actionable individually.
  - Notes: The aggregation key in `confidence.rs:182` is `(detector_name, description)`. This change only affects the file naming description -- function, type, parameter, and constant naming descriptions already use aggregated percentage format and are not affected.

- [ ] Task 5: Add `include_submodules` to ScanConfig
  - File: `crates/seshat-core/src/config.rs`
  - Action: Add `pub include_submodules: bool` field to `ScanConfig` with `#[serde(default)]` (defaults to false).
  - Notes: Default false = exclude submodules. Backward compatible via `serde(default)`.

- [ ] Task 6: Add `--include-submodules` CLI flag
  - File: `crates/seshat-cli/src/args.rs`
  - Action: Add `#[arg(long)] include_submodules: bool` to the `Scan` variant. Pass it through to `ScanConfig` in `scan.rs` before calling `scan_project_with_progress()`.
  - File: `crates/seshat-cli/src/scan.rs`
  - Action: Accept `include_submodules` from args, set `config.scan.include_submodules = include_submodules` before scan.

- [ ] Task 7: Exclude submodule paths in discovery
  - File: `crates/seshat-scanner/src/discovery.rs`
  - Action:
    1. In `discover_files()`, add `builder.git_submodules(config.include_submodules)` to the `WalkBuilder` setup (after line 59). The `ignore` crate's `WalkBuilder` has built-in support for this -- when `false`, it will not descend into directories that are git submodules. This is the primary exclusion mechanism.
    2. Add function `detect_submodule_paths(root: &Path) -> Vec<String>` -- parse `.gitmodules` file if it exists, extract `path = ...` values. Use simple `str::contains` + `str::split` parsing (no regex crate needed): read file, for each line, if line trimmed starts with `path`, split on `=`, trim the value. Return list of relative path strings.
    3. Change return type of `discover_files()` to return a `DiscoveryResult` struct: `pub struct DiscoveryResult { pub files: Vec<DiscoveredFile>, pub excluded_submodules: Vec<String> }`. When `!config.include_submodules`, populate `excluded_submodules` from `detect_submodule_paths()`. When `include_submodules` is true, set to empty vec.
    4. Update all call sites of `discover_files()` (orchestrator.rs and any tests) to use the new return type.
  - Notes: No regex crate needed. `.gitmodules` parsing edge cases (quoted paths, comments) are rare; simple parsing covers 99% of real projects. Full parsing deferred to Epic 6.

- [ ] Task 8: Show info message for excluded submodules
  - File: `crates/seshat-cli/src/scan.rs`
  - Action: After `scan_project_with_progress()` returns, check `scan_result` for excluded submodule info (from the `DiscoveryResult` -- the orchestrator must propagate `excluded_submodules` through to `ScanResult` or return it separately). If submodules were excluded and `!excluded_submodules.is_empty()`, print `format_info(&format!("Excluded {} submodule(s): {}. Use --include-submodules to include.", count, paths_joined), color)` using `format_info()` from `format.rs` with `color_enabled()` for the color parameter.
  - Notes: Only show when `verbosity.show_warnings()` returns true. The orchestrator needs to propagate `excluded_submodules: Vec<String>` from `DiscoveryResult` -- either add it to `ScanResult` or have the orchestrator return it separately. Adding to `ScanResult` is cleaner.

- [ ] Task 9: Update tests
  - Files: `crates/seshat-detectors/src/pipeline.rs`, `crates/seshat-detectors/src/naming.rs`, `crates/seshat-scanner/src/discovery.rs`, `crates/seshat-scanner/src/orchestrator.rs`
  - Action:
    1. `pipeline.rs`: Update ALL existing `run_all_detectors()` and `run_detectors()` calls in tests to pass `None` for new progress parameter. Add test: progress callback receives correct `(done, total)` values when called from rayon -- verify `done` increments from 1 to `files.len()` and `total == files.len()`.
    2. `naming.rs`: Add test: single-word file name (e.g., "utils") produces description `"File naming: snake_case convention (Python)"` (not "single_lower_word"). Add test: non-conforming file (e.g., "MyFile.py") keeps specific description with stem. Add test: multiple conforming Python files aggregate into one finding.
    3. `discovery.rs`: Update existing tests to use new `DiscoveryResult` return type (access `.files`). Add test: project with `.gitmodules` containing `path = frontend` and `include_submodules=false` -- verify `excluded_submodules` contains "frontend" and no files from `frontend/` in `.files`. Add test: `include_submodules=true` includes submodule files and `excluded_submodules` is empty. Add test: no `.gitmodules` file = empty `excluded_submodules`.
    4. `orchestrator.rs`: Update any direct `ScanResult` construction in tests to include `file_dates` field.

### Acceptance Criteria

- [ ] AC 1: Given a project with 1050 files, when `seshat scan` runs, then the scanning progress bar remains visible after completion (shows "done" instead of disappearing).
- [ ] AC 2: Given a project with 1050 files, when detection phase runs, then a progress bar shows `"Analyzing conventions... {done}/{total}"` updating in real-time.
- [ ] AC 3: Given a project in a git repository, when git date collection runs, then a spinner shows `"Collecting git history..."` until complete.
- [ ] AC 4: Given `--quiet` flag, when scan runs, then all progress bars and spinners are hidden.
- [ ] AC 5: Given a file named `utils.py`, when the naming detector runs, then the finding description reads `"File naming: snake_case convention (Python)"` (not `"single_lower_word"`).
- [ ] AC 6: Given 20 Python files all using snake_case naming (including single-word names), when conventions are aggregated, then they group into ONE convention entry (not 20 separate entries per file name).
- [ ] AC 7: Given a project with a git submodule at `frontend/`, when `seshat scan` runs without flags, then files inside `frontend/` are excluded from discovery.
- [ ] AC 8: Given a project with a git submodule, when `seshat scan --include-submodules` runs, then submodule files ARE included in discovery.
- [ ] AC 9: Given a project with excluded submodules, when scan runs, then an info message shows `"Excluded 1 submodule(s): frontend/"`.
- [ ] AC 10: Given a project without `.gitmodules`, when scan runs, then no submodule exclusion logic runs and no messages are shown.
- [ ] AC 11: Given the scan pipeline, when `collect_git_file_dates()` runs, then it is called exactly ONCE (from orchestrator), not twice.
- [ ] AC 12: `cargo test --workspace` passes.
- [ ] AC 13: `cargo clippy --all-targets --all-features -- -D warnings` passes.

## Additional Context

### Dependencies

- No new external crate dependencies needed.
- `std::sync::atomic::AtomicUsize` for thread-safe progress counter (stdlib).
- `.gitmodules` parsing uses `str::contains()` + `str::split()` -- no regex crate needed.
- `WalkBuilder::git_submodules()` is already available in the `ignore` crate (existing dependency of `seshat-scanner`).

### Testing Strategy

**Unit tests:**
- `pipeline.rs`: Progress callback receives correct values during rayon execution
- `naming.rs`: Single-word description reclassification, aggregation grouping
- `discovery.rs`: Submodule exclusion with/without `.gitmodules`, include flag

**Integration test (manual):**
- Run `seshat scan` on a project with submodules -- verify exclusion
- Run `seshat scan` on a large project -- verify all progress phases visible
- Run `seshat scan --quiet` -- verify no progress output

### Notes

- **Risk: rayon + AtomicUsize progress granularity** -- rayon may process files in batches, so progress updates may jump. This is acceptable; indicatif handles this gracefully.
- **Risk: Phase 2 (cross-file detection) gap** -- The progress bar covers Phase 1 (per-file parallel detection) only. Phase 2 runs sequentially after the bar reaches 100%. Phase 2 is fast (only `DependencyUsageDetector` overrides `detect_cross_file()` currently), so the gap is negligible. If Phase 2 becomes slow in future, a separate spinner can be added.
- **Risk: `.gitmodules` parsing edge cases** -- Nested submodules, quoted paths, inline comments. Simple line parsing covers 99% of real projects. Edge cases can be handled in Epic 6.
- **Risk: `ScanConfig` override precedence** -- `--include-submodules` CLI flag always overrides the config file value. If a user sets `include_submodules = true` in `seshat.toml`, passing no CLI flag will override it to `false`. This is standard CLI-over-config behavior but may surprise users. Acceptable for now.
- **Future: Multi-line persistent progress** -- The current approach uses sequential spinners/bars. A more polished approach would use `indicatif::MultiProgress` for persistent multi-line display. Deferred to future polish iteration.
- **Duplicate git dates call** -- This was introduced when scan.rs was written before orchestrator exposed the data. Simple fix: add field to ScanResult.
- **Breaking changes**: `run_all_detectors()` signature change (all callers must add `None`), `discover_files()` return type change (callers access `.files`), `ScanResult` new field (all construction sites must be updated). All changes are workspace-internal -- no external API.
