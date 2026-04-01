# Epic 4 CLI Scan UX Issues — Post-Implementation Research

**Date:** 2026-04-01
**Context:** Post-implementation review of Epic 4 (CLI Scan Report & First Impression, stories 4.1–4.4). Testing `seshat scan` against real projects revealed three categories of UX/logic issues.

**Test projects:**
- `seshat` itself (small, ~200 files, Rust)
- `walt-chat-backend` (large, ~1050 files, Python + TypeScript git submodule)

---

## Issue 1: No Progress Visibility After File Scanning Phase

### Symptom

When running `cargo run -- scan ~/Projects/Walt/walt-chat-backend` on a 1050-file project:

1. Spinner shows "Discovering files... 1050 found" — works fine.
2. Progress bar `Scanning ████░░░ 500/1050 [00:00:03]` appears briefly.
3. Progress bar **disappears** (cleared by `finish_and_clear()`).
4. **Silence for several seconds** — no visual feedback at all.
5. "Project Overview" section suddenly appears.

The user has no idea what's happening during step 4. It feels like the tool froze.

### Root Cause

After the scanning progress bar completes and clears, three heavy operations run with **zero progress indicators**:

| Phase | Location | What it does | Progress? |
|---|---|---|---|
| Post-scan persistence | `orchestrator.rs` steps 4–9 | Persist IR, rebuild module graph, analyze manifests, parse docs | None |
| Convention detection | `scan.rs:141` `run_all_detectors()` | Process all 1050 files through 8 detectors (rayon parallel) | None |
| Git history walk | `scan.rs:150` `collect_git_file_dates()` | Walk entire git commit history to collect per-file dates | None |

The scanning bar calls `pb.finish_and_clear()` at `scan.rs:128`, which **erases the bar entirely** — so the user doesn't even see it completed.

### Code Flow

```
scan.rs:98   scan_project_with_progress() called
               ├── Discovery spinner ✓ (visible)
               ├── Scanning progress bar ✓ (visible, but cleared on finish)
               └── Steps 4-9: persist, module graph, manifests... ✗ (no progress)

scan.rs:141  run_all_detectors()          ✗ (no progress — heaviest step)
scan.rs:150  collect_git_file_dates()     ✗ (no progress — walks git log)
scan.rs:163  aggregate_findings()         (fast, OK without progress)
scan.rs:170  print_report()               (instant)
```

### Proposed Fix

1. **Don't clear the scanning bar.** Change `finish_and_clear()` to `finish()` or `finish_with_message("Scanning... done")` so completion is visible.

2. **Add progress callback to `run_all_detectors()`** — this is the longest post-scan step. Options:
   - **(a) Full progress bar** — add `on_progress: impl Fn(usize, usize)` parameter to `run_all_detectors()`, report `(done, total)` file counts. Show `"Analyzing conventions... 500/1050 files"`. Most informative.
   - **(b) Simple spinner** — just show `"Analyzing conventions..."` with a braille spinner. Simpler but less informative.
   - **(c) Multi-phase spinners** — separate spinners for each phase: "Persisting results...", "Analyzing conventions...", "Collecting git history...". Most granular.

   Recommendation: **(a)** for detection (it's the longest), simple spinners for git dates and persistence.

3. **Add spinner for git date collection.** `collect_git_file_dates()` can take seconds on large repos with deep history. A simple `"Collecting git history..."` spinner suffices.

### Key Files

- `crates/seshat-cli/src/scan.rs:82-132` — progress bar setup and callback
- `crates/seshat-scanner/src/orchestrator.rs:119-225` — scan_project_with_progress pipeline
- `crates/seshat-detectors/src/pipeline.rs` — run_all_detectors (no progress callback currently)

---

## Issue 2: Naming Conventions — Single-Word False Positives

### Symptom

In the conventions report, entries like these appear:

```
File naming: 'utils' uses single_lower_word (expected snake_case for Python)   100%
File naming: 'styles' uses single_lower_word (expected kebab-case for TypeScript)   100%
```

This is confusing because:
- A single lowercase word (`utils`) **cannot contradict** snake_case, kebab-case, or camelCase — it's valid in all three conventions.
- The description text says "uses single_lower_word (expected snake_case)" which **reads as a mismatch** even though the finding is actually marked as conforming (`follows_convention: true`, 100% confidence).
- In a multi-language project, it's unclear which language a convention applies to from the aggregated view.

### Root Cause

The code **correctly classifies** single-word names as conforming. At `naming.rs:574-591`, `matches_expected()` returns `true` for `(SingleLowerWord, SnakeCase)`, `(SingleLowerWord, CamelCase)`, and `(SingleLowerWord, KebabCase)`.

However, the **description string** at `naming.rs:459-465` always includes the raw classified pattern name:

```rust
let description = format!(
    "File naming: '{}' uses {} (expected {} for {})",
    stem,
    pattern.as_str(),      // "single_lower_word" — the confusing part
    expected.as_str(),     // "snake_case"
    lang,                  // "Python"
);
```

Since aggregation groups by `(detector_name, description)` at `confidence.rs:182`, every unique file name with a different stem creates its own convention group. So `'utils'` and `'styles'` become separate convention entries, each at 100%, cluttering the report with noise.

### Additionally: Missing Language Context in Aggregated View

When conventions from Python and TypeScript are interleaved in the list, the language is embedded in the description text (e.g., "expected snake_case for Python") but not visually prominent. For multi-language projects, it's hard to scan and filter by language.

### Proposed Fix Options

**For single-word false positives:**

| Option | Description | Pros | Cons |
|---|---|---|---|
| **(a) Use expected pattern name** | When `matches_expected` returns true due to single-word compatibility, use the expected pattern name in the description: `"File naming: 'utils' uses snake_case (Python)"` | Still counted in stats; no confusing mismatch text | Slightly inaccurate — it's not *really* snake_case, it's just compatible |
| **(b) Skip single-word findings entirely** | Don't emit file naming findings for single-word stems | Eliminates noise completely | Reduces adoption count; may skew percentages |
| **(c) Combine: reclassify + aggregate** | Use expected pattern name AND aggregate all single-word file names into one finding per language instead of per-file-name | Clean aggregation; no clutter | More complex change |

Recommendation: **(a)** is the simplest fix. When `classify_case()` returns `SingleLowerWord` or `SingleUpperWord` and `matches_expected()` returns `true`, substitute the expected pattern name in the description. This way `"File naming: 'utils' uses snake_case (Python) 100%"` is accurate (a single lowercase word IS valid snake_case) and doesn't confuse anyone.

**For language context:**

- Consider prefixing or grouping conventions by language in the report output, especially for multi-language projects.

### Key Files

- `crates/seshat-detectors/src/naming.rs:442-479` — `detect_file_naming()` description generation
- `crates/seshat-detectors/src/naming.rs:574-591` — `matches_expected()` single-word handling
- `crates/seshat-detectors/src/naming.rs:94-147` — `classify_case()` pattern classification
- `crates/seshat-detectors/src/confidence.rs:162-226` — `aggregate_findings()` grouping by description

---

## Issue 3: Git Submodule Files Included in Scan

### Symptom

`walt-chat-backend` is a Python project with a TypeScript frontend attached as a git submodule. Running `seshat scan` on it discovers and scans **both** the Python project AND the TypeScript submodule together, treating them as one project.

The report shows:
- Language breakdown: Python 24%, TypeScript 72% (submodule dominates)
- Convention findings mix Python and TypeScript naming rules
- Dependency counts merge pip and npm packages

This makes the analysis misleading — the user scanned a Python backend but sees a TypeScript-dominated report.

### Root Cause

`discover_files()` in the scanner walks the directory tree recursively and has no awareness of `.gitmodules` or `.git` directories within subdirectories that signal submodule boundaries. It discovers all parseable source files regardless of submodule membership.

### Proposed Fix Options

| Option | Description |
|---|---|
| **(a) Exclude submodules by default** | During discovery, detect `.gitmodules` in the root, parse it to find submodule paths, and exclude those directories. Users could opt-in via `--include-submodules` flag or config. Simplest, matches most expectations. |
| **(b) Scan submodules separately** | Detect submodules, treat each as a separate sub-project with its own language/convention grouping. Show them in the report under a "Submodules" section (partially designed in PRD US-002). |
| **(c) Include but tag** | Scan everything but tag findings with submodule origin. Group conventions per sub-project in the report. |

Recommendation: **(a)** for now — exclude by default, opt-in via flag. This is the most common developer expectation. Option (b) is the long-term ideal but requires more design work.

### Key Files

- `crates/seshat-scanner/src/discovery.rs` — `discover_files()` directory walker
- `crates/seshat-cli/src/args.rs` — CLI argument definitions (for `--include-submodules` flag)

---

## Summary of Action Items

| # | Issue | Priority | Effort | Fix |
|---|---|---|---|---|
| 1 | No progress after scanning phase | **High** | Medium | Add progress callbacks for detection + spinners for git dates |
| 2 | Single-word naming false positives | **High** | Small | Reclassify description text when single-word matches expected |
| 3 | Git submodule inclusion | **Medium** | Medium | Exclude submodule paths in discovery by default |

All three issues affect the "first impression" UX that Epic 4 was designed to nail. Issues 1 and 2 are the most impactful — a tool that appears frozen and reports confusing conventions will lose developer trust immediately.
