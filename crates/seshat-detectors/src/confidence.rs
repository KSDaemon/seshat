//! Frequency-based confidence scoring (ADR-7).
//!
//! Confidence is computed as `adoption_count / total_count` and mapped to a
//! [`KnowledgeWeight`] via configurable thresholds from [`DetectionConfig`].
//!
//! The [`aggregate_findings`] function groups raw per-file findings by
//! `(detector_name, description)`, computes adoption counts, and produces
//! [`AggregatedConvention`] values ready for storage.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use seshat_core::{
    AnchorKind, CodeEvidence, ConventionFinding, DetectionConfig, KnowledgeNature, KnowledgeWeight,
    Trend,
};

/// Maximum file paths listed inline in the composite snippet that
/// replaces N file-level evidence rows for a single convention. The
/// snippet appends "... and N more (truncated)" when this cap is
/// exceeded.
///
/// Bounded by TUI usability: the snippet panel is a fixed-height pane
/// that does not scroll independently of the wizard, so dumping 600+
/// rows hides the convention header from view. 20 is enough to give a
/// representative sample across project subtrees while leaving the
/// header visible.
const MAX_FILES_LISTED_IN_COMPOSITE: usize = 20;

/// An aggregated convention produced from multiple per-file findings.
///
/// Grouping key is `(detector_name, description)`.
#[derive(Debug, Clone)]
pub struct AggregatedConvention {
    /// Name of the detector that produced this convention.
    pub detector_name: String,
    /// Human-readable description of the convention.
    pub description: String,
    /// The nature of the finding (Convention, Observation, etc.).
    pub nature: KnowledgeNature,
    /// Number of files that follow this convention.
    pub adoption_count: u32,
    /// Total number of files analyzed for this convention.
    pub total_count: u32,
    /// Computed confidence score (`adoption_count / total_count`).
    pub confidence: f64,
    /// Weight derived from confidence thresholds.
    pub weight: KnowledgeWeight,
    /// Representative evidence from individual findings.
    pub evidence: Vec<seshat_core::CodeEvidence>,
    /// Trend indicator computed from file commit dates.
    pub trend: Trend,
}

impl AggregatedConvention {
    /// Build `ext_data` JSON for a [`seshat_core::KnowledgeNode`].
    ///
    /// Produces a JSON object with at least `{"trend": "<variant>"}` and
    /// `{"adoption_rate": <float>}`. If `existing_ext` is provided, the trend
    /// and adoption_rate fields are merged into it (existing fields preserved).
    pub fn ext_data(&self, existing_ext: Option<&serde_json::Value>) -> Option<serde_json::Value> {
        let mut obj = match existing_ext.and_then(|v| v.as_object()) {
            Some(existing) => existing.clone(),
            None => serde_json::Map::new(),
        };
        obj.insert(
            "trend".to_owned(),
            serde_json::Value::String(self.trend.as_str().to_owned()),
        );
        obj.insert(
            "adoption_rate".to_owned(),
            serde_json::json!(self.confidence),
        );
        Some(serde_json::Value::Object(obj))
    }
}

/// Compute confidence as `adoption_count / total_count`.
///
/// Returns `0.0` when `total_count` is zero (no data means no confidence).
pub fn compute_confidence(adoption_count: u32, total_count: u32) -> f64 {
    if total_count == 0 {
        return 0.0;
    }
    f64::from(adoption_count) / f64::from(total_count)
}

/// Map a confidence score to a [`KnowledgeWeight`] using the thresholds in
/// [`DetectionConfig`].
///
/// | Confidence         | Weight   |
/// |--------------------|----------|
/// | > strong (0.85)    | Strong   |
/// | > moderate (0.50)  | Moderate |
/// | > weak (0.20)      | Weak     |
/// | <= weak            | Info     |
pub fn weight_from_confidence(confidence: f64, config: &DetectionConfig) -> KnowledgeWeight {
    if confidence > config.confidence_strong {
        KnowledgeWeight::Strong
    } else if confidence > config.confidence_moderate {
        KnowledgeWeight::Moderate
    } else if confidence > config.confidence_weak {
        KnowledgeWeight::Weak
    } else {
        KnowledgeWeight::Info
    }
}

/// Compute the trend for a convention from the commit dates of its associated
/// files.
///
/// Uses the **P90 percentile** of valid (non-`None`) dates: the 90th-percentile
/// date represents when the convention was most recently applied across the
/// codebase. The age of that date relative to `now` determines the trend:
///
/// - P90 age < `trend_rising_days` → [`Trend::Rising`]
/// - P90 age < `trend_stable_days` → [`Trend::Stable`]
/// - P90 age >= `trend_stable_days` → [`Trend::Declining`]
/// - No valid dates → [`Trend::Unknown`]
///
/// # Arguments
///
/// * `file_dates` - Unix timestamps (seconds) of last commit per file.
///   `None` entries (files without git history) are ignored.
/// * `config` - Detection config with `trend_rising_days` and `trend_stable_days`.
/// * `now` - Current Unix timestamp in seconds.
#[tracing::instrument(skip_all, fields(n_dates = file_dates.len()))]
pub fn compute_trend(file_dates: &[Option<i64>], config: &DetectionConfig, now: i64) -> Trend {
    // Collect valid (non-None) timestamps.
    let mut valid_dates: Vec<i64> = file_dates.iter().filter_map(|d| *d).collect();

    if valid_dates.is_empty() {
        return Trend::Unknown;
    }

    // Sort ascending to compute percentile.
    valid_dates.sort_unstable();

    // P90: index = ceil(N * 0.9) - 1, clamped to valid range.
    let n = valid_dates.len();
    let p90_index = ((n as f64 * 0.9).ceil() as usize)
        .saturating_sub(1)
        .min(n - 1);
    let p90_timestamp = valid_dates[p90_index];

    // Compute age in days.
    let age_seconds = now.saturating_sub(p90_timestamp).max(0);
    let age_days = (age_seconds / 86_400) as u32;

    if age_days < config.trend_rising_days {
        Trend::Rising
    } else if age_days < config.trend_stable_days {
        Trend::Stable
    } else {
        Trend::Declining
    }
}

/// Group findings by `(detector_name, description)` and compute adoption
/// metrics.
///
/// Each unique `(detector_name, description)` pair becomes one
/// [`AggregatedConvention`]. `adoption_count` is the number of findings where
/// `follows_convention == true`; `total_count` is the total number of
/// findings in the group.
///
/// Evidence is collected from all findings in the group, capped at
/// `config.max_snippet_lines` representative samples.
///
/// The `file_dates` parameter maps file paths (as strings) to their optional
/// last-commit Unix timestamps. When provided, a [`Trend`] is computed per
/// convention group from the commit dates of the files in that group. Pass an
/// empty map if git dates are unavailable — all trends will be
/// [`Trend::Unknown`].
///
/// `now` is the current Unix timestamp in seconds, used for trend age
/// computation. Pass `0` if trends are not needed.
#[tracing::instrument(skip_all, fields(n_findings = findings.len()))]
pub fn aggregate_findings(
    findings: &[ConventionFinding],
    config: &DetectionConfig,
    file_dates: &HashMap<String, Option<i64>>,
    now: i64,
) -> Vec<AggregatedConvention> {
    /// Grouping key and accumulator.
    ///
    /// Two evidence accumulators tracked separately so file-level and
    /// source-anchored findings get distinct UX:
    ///
    /// - `anchored_evidence` collects CallSite / Declaration / ImportLine
    ///   rows. Capped at `config.max_snippet_lines` and deduplicated by
    ///   `(file, line, end_line)` via the parallel `seen_anchored`
    ///   HashSet so the dedup is O(1) per insert.
    ///
    /// - `file_level_files` collects file paths from FileLevel rows
    ///   (line == 0 synthetic descriptors like
    ///   "config_service [snake_case]"). NOT capped — the composite
    ///   snippet listing the file paths is generated at the end and the
    ///   cap is applied only when rendering. Conventions like "Test file
    ///   placement: separate tests/ directory" naturally produce one
    ///   FileLevel evidence per project file (98+ on a real workspace);
    ///   collapsing to one composite row removes the per-file repetition.
    struct Bucket {
        nature: KnowledgeNature,
        adoption_count: u32,
        total_count: u32,
        anchored_evidence: Vec<CodeEvidence>,
        seen_anchored: HashSet<(PathBuf, usize, usize)>,
        /// FileLevel rows paired with the originating finding's
        /// `follows_convention` flag. The flag lets the composite
        /// renderer phrase the header correctly when a bucket holds a
        /// mix of conforming and non-conforming files (e.g. file-naming
        /// after Fix B emits both under one description).
        file_level_rows: Vec<(CodeEvidence, bool)>,
        file_level_seen: HashSet<PathBuf>,
        /// Commit dates for files in this convention group.
        dates: Vec<Option<i64>>,
    }

    let mut groups: HashMap<(String, String), Bucket> = HashMap::new();

    for finding in findings {
        let key = (finding.detector_name.clone(), finding.description.clone());
        let bucket = groups.entry(key).or_insert_with(|| Bucket {
            nature: finding.nature,
            adoption_count: 0,
            total_count: 0,
            anchored_evidence: Vec::new(),
            seen_anchored: HashSet::new(),
            file_level_rows: Vec::new(),
            file_level_seen: HashSet::new(),
            dates: Vec::new(),
        });

        bucket.total_count += 1;
        if finding.follows_convention {
            bucket.adoption_count += 1;
        }

        // Route evidence rows by anchor kind. FileLevel rows go into a
        // separate list that is NOT capped — every project file
        // contributing to the convention is recorded so the composite
        // row at the end can list them all.
        for ev in finding.evidence.iter() {
            match ev.anchor {
                AnchorKind::FileLevel => {
                    if bucket.file_level_seen.insert(ev.file.clone()) {
                        bucket
                            .file_level_rows
                            .push((ev.clone(), finding.follows_convention));
                    }
                }
                _ => {
                    if bucket.anchored_evidence.len() >= config.max_snippet_lines {
                        continue;
                    }
                    let dedup_key = (ev.file.clone(), ev.line, ev.end_line);
                    if bucket.seen_anchored.insert(dedup_key) {
                        bucket.anchored_evidence.push(ev.clone());
                    }
                }
            }
        }

        // Collect the commit date for this file.
        let file_key = finding.file_path.to_string_lossy();
        let date = file_dates.get(file_key.as_ref()).copied().unwrap_or(None);
        bucket.dates.push(date);
    }

    groups
        .into_iter()
        .map(|((detector_name, description), bucket)| {
            let confidence = compute_confidence(bucket.adoption_count, bucket.total_count);
            let weight = weight_from_confidence(confidence, config);
            let trend = compute_trend(&bucket.dates, config, now);
            // Build the final evidence vector. Anchored rows always come
            // first (call sites carry the most useful detail). File-level
            // rows are collapsed into a single composite row that lists
            // every contributing file inline — replaces the previous
            // N-rows-of-empty-snippets pattern that polluted the review
            // TUI for conventions like "Test file placement: separate
            // tests/ directory" (one row per file, 98+ on a real
            // workspace).
            let mut evidence = bucket.anchored_evidence;
            // Pass-through for single-file file-level findings ONLY
            // when the row carries a useful per-file descriptor like
            // "config_service [snake_case]" — that descriptor renders
            // as the snippet in the Example tab. When the snippet is
            // empty (e.g. "Testing framework (from config file): pytest"
            // emitted with `snippet: String::new()`), collapse into a
            // synthetic composite so the user sees the file path inline
            // instead of the TUI's "(no snippet available)" placeholder.
            match bucket.file_level_rows.len() {
                0 => {}
                1 => {
                    let (only, follows) = bucket.file_level_rows.into_iter().next().unwrap();
                    // Whitespace-only snippets render as a blank pane in
                    // the TUI — visually identical to the "(no snippet
                    // available)" placeholder. Collapse them into the
                    // composite so the file path itself is visible.
                    if only.snippet.trim().is_empty() {
                        evidence.push(build_file_level_composite(
                            &[(only, follows)],
                            bucket.adoption_count,
                            bucket.total_count,
                        ));
                    } else {
                        evidence.push(only);
                    }
                }
                _ => evidence.push(build_file_level_composite(
                    &bucket.file_level_rows,
                    bucket.adoption_count,
                    bucket.total_count,
                )),
            }
            AggregatedConvention {
                detector_name,
                description,
                nature: bucket.nature,
                adoption_count: bucket.adoption_count,
                total_count: bucket.total_count,
                confidence,
                weight,
                evidence,
                trend,
            }
        })
        .collect()
}

/// Build a single composite [`CodeEvidence`] that enumerates a
/// representative sample of files contributing FileLevel evidence to a
/// convention.
///
/// When the row count exceeds [`MAX_FILES_LISTED_IN_COMPOSITE`], the
/// sample is chosen via [`select_diverse_sample`] — group by the first
/// path segment that varies across the corpus, then round-robin pick
/// across groups so the sample spans different parts of the project
/// rather than the alphabetically-first N rows.
///
/// Each row is rendered as `path  (descriptor)` when the original
/// FileLevel evidence carried a per-file descriptor in `snippet` (e.g.
/// `"config_service [snake_case]"`), or just `path` otherwise. Rows
/// whose originating finding had `follows_convention=false` carry a
/// trailing `[non-conforming]` marker so the user can tell which files
/// drag the adoption ratio down without cross-referencing the snippet.
///
/// The header verb is chosen from (`adoption_count`, `total_count`):
/// - all follow → "match"
/// - none follow → "violate"
/// - mixed → "reference" with a follow/violate split breakdown
///
/// Snippet shape (all-conforming case):
///   // 707 files match this convention (showing 20):
///   //   crates/seshat-cli/src/config.rs   (config [snake_case])
///   //   ...
///   // ... and 687 more (truncated)
fn build_file_level_composite(
    rows: &[(CodeEvidence, bool)],
    adoption_count: u32,
    total_count: u32,
) -> CodeEvidence {
    let total = rows.len();
    let omitted = rows
        .iter()
        .filter(|(r, _)| is_uninformative_file(&r.file))
        .count();
    // select_diverse_sample only sees the evidence rows — re-bind the
    // follows flag after selection by file path. Two rows for the same
    // file cannot exist in the same bucket (file_level_seen dedupes
    // at insertion time), so the lookup is unambiguous.
    let evidence_only: Vec<CodeEvidence> = rows.iter().map(|(ev, _)| ev.clone()).collect();
    let selected = select_diverse_sample(&evidence_only, MAX_FILES_LISTED_IN_COMPOSITE);
    let follows_by_path: HashMap<PathBuf, bool> = rows
        .iter()
        .map(|(ev, follows)| (ev.file.clone(), *follows))
        .collect();
    let shown = selected.len();
    let informative_pool = total.saturating_sub(omitted);
    let all_markers = informative_pool == 0 && omitted > 0;
    let informative_total = informative_pool.max(shown);

    // Header verb depends on the bucket's adoption ratio so the inline
    // summary stays consistent with the `Found in: X/N (Y%)` line
    // rendered above it in the review TUI.
    let verb_phrase = composite_header_verb(adoption_count, total_count, total);

    let mut lines = Vec::with_capacity(shown + 2);
    let header = if total == 1 {
        format!("// 1 file {verb_phrase}:")
    } else if all_markers {
        if total == shown {
            format!("// {total} files {verb_phrase} (all are __init__.py markers):")
        } else {
            format!(
                "// {total} files {verb_phrase} (showing {shown}; all are __init__.py markers):"
            )
        }
    } else if omitted > 0 && informative_total != total {
        format!(
            "// {total} files {verb_phrase} (showing {shown} informative; {omitted} __init__.py markers omitted):"
        )
    } else if total == shown {
        format!("// {total} files {verb_phrase}:")
    } else {
        format!("// {total} files {verb_phrase} (showing {shown}):")
    };
    lines.push(header);

    for row in &selected {
        let follows = follows_by_path.get(&row.file).copied().unwrap_or(true);
        let non_conforming_marker = if follows { "" } else { "   [non-conforming]" };
        let line = match composite_descriptor(&row.snippet) {
            Some(descriptor) => format!(
                "//   {}   ({}){}",
                row.file.display(),
                descriptor,
                non_conforming_marker,
            ),
            None => format!("//   {}{}", row.file.display(), non_conforming_marker),
        };
        lines.push(line);
    }
    // In the all-markers fallback, truncation is measured against the
    // RAW total since every row is a marker; otherwise against the
    // informative pool (which excludes markers from "X more").
    let truncation_total = if all_markers {
        total
    } else {
        informative_total
    };
    if truncation_total > shown {
        lines.push(format!(
            "// ... and {} more (truncated)",
            truncation_total - shown,
        ));
    }
    CodeEvidence {
        // Synthetic composite: no single file owns this row.
        file: PathBuf::new(),
        line: 0,
        end_line: 0,
        snippet: lines.join("\n"),
        snippet_start_line: 0,
        anchor: AnchorKind::FileLevel,
    }
}

/// Pick a verb phrase for the composite header based on how many of the
/// bucket's files actually follow the convention.
///
/// - `adoption == total` → `"match this convention"`
/// - `adoption == 0`     → `"violate this convention"`
/// - mixed               → `"reference this convention (X follow / Y violate)"`
///   so the user can tell at a glance how many of the listed files are
///   the ones dragging the adoption ratio down
///
/// `subject_count` drives singular ("matches" / "violates" / "references")
/// vs plural conjugation. A mixed bucket has `total_count >= 2` by
/// definition; the `debug_assert!` pins that invariant so callers can't
/// silently slip in a 1-row mixed header.
///
/// `total_count == 0` should be impossible (the bucket only exists because
/// at least one finding contributed to it); the function still picks a
/// well-formed singular/plural "match" so the function is total.
fn composite_header_verb(adoption_count: u32, total_count: u32, subject_count: usize) -> String {
    debug_assert!(
        adoption_count <= total_count,
        "adoption_count ({adoption_count}) > total_count ({total_count}) — bucket math invariant violated",
    );

    let singular = subject_count == 1;
    let match_verb = if singular { "matches" } else { "match" };
    let violate_verb = if singular { "violates" } else { "violate" };
    let reference_verb = if singular { "references" } else { "reference" };

    if total_count == 0 || adoption_count == total_count {
        format!("{match_verb} this convention")
    } else if adoption_count == 0 {
        format!("{violate_verb} this convention")
    } else {
        // Mixed buckets have >= 2 findings by definition (one follower +
        // one violator at minimum), so subject_count==1 here would mean
        // an upstream invariant break. Pin it.
        debug_assert!(
            total_count >= 2,
            "mixed bucket needs total_count >= 2; got {total_count}",
        );
        // Saturating subtraction belt-and-suspenders: the debug_assert
        // above catches adoption > total in debug builds; saturating_sub
        // prevents a release-mode u32 wraparound if we ever miss one.
        let violators = total_count.saturating_sub(adoption_count);
        format!("{reference_verb} this convention ({adoption_count} follow / {violators} violate)")
    }
}

/// Files whose path looks low-signal in a per-file evidence sample.
///
/// Currently flags Python's `__init__.py` package markers — they're in
/// every Python directory, are commonly empty, and crowd out
/// substantive files when round-robin sampling picks one per group.
/// On a 274-file `tests/` convention this previously surfaced 11 of 20
/// sample slots filled with `__init__.py` rows, hiding the actual test
/// modules.
///
/// The composite renderer falls back to the unfiltered set when *every*
/// row is uninformative, so package-only conventions still render.
fn is_uninformative_file(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|n| n.to_str()),
        Some("__init__.py")
    )
}

/// Reduce a per-file evidence snippet to a single short label suitable
/// for the composite row. Returns `None` when the original snippet is
/// empty so the renderer knows to skip the trailing `(…)` block.
///
/// Why this exists: some detectors (notably `import_organization`)
/// produce multi-line snippets — e.g. an `// Order: …` header followed
/// by per-group import listings. Joining those lines with spaces
/// produces a single 200+ char string that is truncated at the right
/// edge of the TUI snippet pane, leaving the user staring at "// Order:
/// stdlib → external → inter…" with the actually useful detail off
/// screen. Picking just the first line keeps the descriptor compact
/// (typically 50–80 chars) and self-contained — by convention the first
/// line of every multi-line snippet is the headline summary.
///
/// Leading `// ` is stripped so the descriptor reads naturally inside
/// the parentheses (`(Order: stdlib → external)` rather than `(// Order:
/// stdlib → external)`).
fn composite_descriptor(snippet: &str) -> Option<String> {
    let first_line = snippet.lines().next()?.trim();
    if first_line.is_empty() {
        return None;
    }
    let trimmed = first_line.strip_prefix("// ").unwrap_or(first_line);
    Some(trimmed.to_owned())
}

/// Select up to `cap` evidence rows that show diversity across the
/// project's path structure.
///
/// Strategy:
/// 0. Drop low-signal marker files (currently `__init__.py`) when the
///    pool has any informative rows left after filtering. Falls back
///    to the unfiltered set when *every* row is a marker.
/// 1. Compute the longest common path-component prefix across the
///    remaining rows. Components shared by every file (e.g. an
///    absolute project root like `/Users/me/Projects/foo/`) carry
///    no signal.
/// 2. Group rows by the first path component AFTER that prefix — i.e.
///    the first segment that *varies*. This typically lands on the
///    top-level project subtree (`crates/`, `src/`, `tests/`,
///    `scripts/`, …).
/// 3. Round-robin pick across the (sorted) groups: take the first row
///    of each group, then the second, and so on, stopping when `cap`
///    rows are selected or all groups are exhausted.
///
/// The output preserves a stable, alphabetically-grouped order so the
/// sample is reproducible run-to-run.
fn select_diverse_sample(rows: &[CodeEvidence], cap: usize) -> Vec<&CodeEvidence> {
    let informative: Vec<&CodeEvidence> = rows
        .iter()
        .filter(|r| !is_uninformative_file(&r.file))
        .collect();
    if informative.is_empty() {
        // Every row is a marker — fall back so the composite still has
        // something to render.
        let all: Vec<&CodeEvidence> = rows.iter().collect();
        return select_from_pool(&all, cap);
    }
    select_from_pool(&informative, cap)
}

/// Internal sampler: takes an already-filtered pool of evidence
/// references and runs the prefix/group/round-robin pipeline. Split
/// from `select_diverse_sample` so the public entry point can adjust
/// the pool (drop markers, future filters) without duplicating the
/// sampling logic.
fn select_from_pool<'a>(pool: &[&'a CodeEvidence], cap: usize) -> Vec<&'a CodeEvidence> {
    if pool.len() <= cap {
        return pool.to_vec();
    }

    let prefix_len = longest_common_prefix_len(pool);

    // Group keys are borrowed from each row's `file` (lifetime tied to
    // the row reference, which is `'a`), so the `BTreeMap` key type is
    // `&'a str` rather than `String`. On a 700-row pool this avoids
    // 700 small heap allocations just to compute a sort key. Rows whose
    // path is shorter than the common prefix get the placeholder
    // `"<root>"` (a `'static str`, also a valid `&'a str`).
    let mut groups: BTreeMap<&'a str, Vec<&'a CodeEvidence>> = BTreeMap::new();
    for row in pool {
        let key = group_key_after_prefix(&row.file, prefix_len).unwrap_or("<root>");
        groups.entry(key).or_default().push(*row);
    }

    // Round-robin across groups. `BTreeMap` iteration is sorted by key,
    // giving a deterministic order. Iterate the values DIRECTLY rather
    // than collecting an extra `Vec<&Vec<...>>` plus a parallel
    // `Vec<usize>` of cursors — pop from the front of each group in
    // place using a single pass per round.
    //
    // Each round we walk every group exactly once. If a group still
    // has rows, we take its first row and advance it; if every group
    // is exhausted, we stop. The map ordering is preserved across
    // rounds because `iter_mut()` over a `BTreeMap` is sorted-by-key.
    let mut selected: Vec<&'a CodeEvidence> = Vec::with_capacity(cap);
    // Track per-group cursors as a `Vec<usize>` indexed by sorted-key
    // position. This is the only auxiliary buffer we need; previously
    // we also maintained `Vec<&Vec<&CodeEvidence>>` purely to enable
    // index-based access — direct iteration removes that.
    let mut indices: Vec<usize> = vec![0; groups.len()];
    loop {
        let mut progressed = false;
        for (g_idx, group) in groups.values().enumerate() {
            if selected.len() >= cap {
                return selected;
            }
            if indices[g_idx] < group.len() {
                selected.push(group[indices[g_idx]]);
                indices[g_idx] += 1;
                progressed = true;
            }
        }
        if !progressed {
            return selected;
        }
    }
}

/// Number of path components (excluding the file name) that are equal
/// across all rows. Used as the depth at which to start grouping for
/// diverse sampling.
fn longest_common_prefix_len(rows: &[&CodeEvidence]) -> usize {
    let mut iter = rows.iter();
    let Some(first) = iter.next() else {
        return 0;
    };
    let first_components: Vec<_> = first.file.components().collect();
    // Never include the file name itself in the "common prefix" — we
    // group by *directory* segments, not by individual files.
    let mut prefix_len = first_components.len().saturating_sub(1);
    for row in iter {
        let common = first_components
            .iter()
            .zip(row.file.components())
            .take_while(|(a, b)| **a == *b)
            .count();
        prefix_len = prefix_len.min(common);
        if prefix_len == 0 {
            break;
        }
    }
    prefix_len
}

/// First path component after the common prefix, used as the bucket
/// key for diverse sampling. Returns `None` when the path is shorter
/// than the prefix (e.g. a file at the project root) or when the
/// component cannot be represented as UTF-8.
///
/// Returns a `&str` borrowed from `path`, NOT an owned `String` —
/// `select_from_pool` calls this once per pool row and the previous
/// `String`-returning version allocated a fresh heap buffer for every
/// row just to feed a `BTreeMap` sort key.
fn group_key_after_prefix(path: &Path, prefix_len: usize) -> Option<&str> {
    path.components()
        .nth(prefix_len)
        .and_then(|c| c.as_os_str().to_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::{
        AnchorKind, CodeEvidence, FindingKind, KnowledgeNature, KnowledgeWeight, Trend,
    };
    use std::path::PathBuf;

    fn default_config() -> DetectionConfig {
        DetectionConfig::default()
    }

    /// Helper: empty file_dates map for tests that don't need trend.
    fn no_dates() -> HashMap<String, Option<i64>> {
        HashMap::new()
    }

    // --- compute_confidence ---

    #[test]
    fn confidence_zero_total_returns_zero() {
        assert!((compute_confidence(0, 0)).abs() < f64::EPSILON);
    }

    #[test]
    fn confidence_all_adopted() {
        assert!((compute_confidence(10, 10) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn confidence_none_adopted() {
        assert!((compute_confidence(0, 10)).abs() < f64::EPSILON);
    }

    #[test]
    fn confidence_partial() {
        assert!((compute_confidence(3, 4) - 0.75).abs() < f64::EPSILON);
    }

    // --- weight_from_confidence boundary tests ---

    #[test]
    fn weight_strong_above_threshold() {
        let config = default_config();
        // > 0.85 -> Strong
        assert_eq!(
            weight_from_confidence(0.86, &config),
            KnowledgeWeight::Strong
        );
    }

    #[test]
    fn weight_at_strong_boundary_is_moderate() {
        let config = default_config();
        // == 0.85 is NOT > 0.85, so Moderate
        assert_eq!(
            weight_from_confidence(0.85, &config),
            KnowledgeWeight::Moderate
        );
    }

    #[test]
    fn weight_moderate_above_threshold() {
        let config = default_config();
        assert_eq!(
            weight_from_confidence(0.60, &config),
            KnowledgeWeight::Moderate
        );
    }

    #[test]
    fn weight_at_moderate_boundary_is_weak() {
        let config = default_config();
        // == 0.50 is NOT > 0.50, so Weak
        assert_eq!(weight_from_confidence(0.50, &config), KnowledgeWeight::Weak);
    }

    #[test]
    fn weight_weak_above_threshold() {
        let config = default_config();
        assert_eq!(weight_from_confidence(0.30, &config), KnowledgeWeight::Weak);
    }

    #[test]
    fn weight_at_weak_boundary_is_info() {
        let config = default_config();
        // == 0.20 is NOT > 0.20, so Info
        assert_eq!(weight_from_confidence(0.20, &config), KnowledgeWeight::Info);
    }

    #[test]
    fn weight_below_weak_threshold_is_info() {
        let config = default_config();
        assert_eq!(weight_from_confidence(0.10, &config), KnowledgeWeight::Info);
    }

    #[test]
    fn weight_zero_is_info() {
        let config = default_config();
        assert_eq!(weight_from_confidence(0.0, &config), KnowledgeWeight::Info);
    }

    #[test]
    fn weight_one_is_strong() {
        let config = default_config();
        assert_eq!(
            weight_from_confidence(1.0, &config),
            KnowledgeWeight::Strong
        );
    }

    // --- compute_trend ---

    #[test]
    fn trend_empty_dates_returns_unknown() {
        let config = default_config();
        assert_eq!(compute_trend(&[], &config, 1_000_000), Trend::Unknown);
    }

    #[test]
    fn trend_all_none_returns_unknown() {
        let config = default_config();
        assert_eq!(
            compute_trend(&[None, None, None], &config, 1_000_000),
            Trend::Unknown
        );
    }

    #[test]
    fn trend_single_date_rising() {
        let config = default_config();
        // Single date 10 days ago → Rising (< 90 days).
        let now = 1_000_000;
        let date = now - 10 * 86_400;
        assert_eq!(compute_trend(&[Some(date)], &config, now), Trend::Rising);
    }

    #[test]
    fn trend_89_days_ago_is_rising() {
        let config = default_config();
        let now = 1_000_000_000;
        let date = now - 89 * 86_400;
        assert_eq!(compute_trend(&[Some(date)], &config, now), Trend::Rising);
    }

    #[test]
    fn trend_90_days_ago_is_stable() {
        let config = default_config();
        // P90 = exactly 90 days ago. 90 is NOT < 90, so Stable check: 90 < 365 → Stable.
        let now = 1_000_000_000;
        let date = now - 90 * 86_400;
        assert_eq!(compute_trend(&[Some(date)], &config, now), Trend::Stable);
    }

    #[test]
    fn trend_364_days_ago_is_stable() {
        let config = default_config();
        let now = 1_000_000_000;
        let date = now - 364 * 86_400;
        assert_eq!(compute_trend(&[Some(date)], &config, now), Trend::Stable);
    }

    #[test]
    fn trend_365_days_ago_is_declining() {
        let config = default_config();
        // 365 days is NOT < 365, so Declining.
        let now = 1_000_000_000;
        let date = now - 365 * 86_400;
        assert_eq!(compute_trend(&[Some(date)], &config, now), Trend::Declining);
    }

    #[test]
    fn trend_366_days_ago_is_declining() {
        let config = default_config();
        let now = 1_000_000_000;
        let date = now - 366 * 86_400;
        assert_eq!(compute_trend(&[Some(date)], &config, now), Trend::Declining);
    }

    #[test]
    fn trend_p90_with_multiple_dates() {
        let config = default_config();
        let now = 1_000_000_000;
        // 10 dates: 9 very old (500 days) and 1 very recent (5 days).
        // P90 index = ceil(10 * 0.9) - 1 = 9 - 1 = 8 → sorted[8] = old date.
        // So the P90 is the old date, meaning Declining.
        let old = now - 500 * 86_400;
        let recent = now - 5 * 86_400;
        let dates: Vec<Option<i64>> = vec![
            Some(old),
            Some(old),
            Some(old),
            Some(old),
            Some(old),
            Some(old),
            Some(old),
            Some(old),
            Some(old),
            Some(recent),
        ];
        assert_eq!(compute_trend(&dates, &config, now), Trend::Declining);
    }

    #[test]
    fn trend_p90_mostly_recent() {
        let config = default_config();
        let now = 1_000_000_000;
        // 10 dates: 9 recent (5 days) and 1 old (500 days).
        // Sorted ascending: [old, recent x9]. P90 index = 8 → sorted[8] = recent.
        let old = now - 500 * 86_400;
        let recent = now - 5 * 86_400;
        let dates: Vec<Option<i64>> = vec![
            Some(recent),
            Some(recent),
            Some(recent),
            Some(recent),
            Some(recent),
            Some(recent),
            Some(recent),
            Some(recent),
            Some(recent),
            Some(old),
        ];
        assert_eq!(compute_trend(&dates, &config, now), Trend::Rising);
    }

    #[test]
    fn trend_ignores_none_dates() {
        let config = default_config();
        let now = 1_000_000_000;
        // Mix of None and recent dates. P90 should only consider valid dates.
        let recent = now - 30 * 86_400;
        let dates: Vec<Option<i64>> = vec![None, None, Some(recent), None, Some(recent)];
        assert_eq!(compute_trend(&dates, &config, now), Trend::Rising);
    }

    // --- aggregate_findings ---

    #[test]
    fn aggregate_empty_findings() {
        let result = aggregate_findings(&[], &default_config(), &no_dates(), 0);
        assert!(result.is_empty());
    }

    #[test]
    fn aggregate_single_finding() {
        let findings = vec![ConventionFinding {
            file_path: PathBuf::from("a.rs"),
            detector_name: "test_detector".to_owned(),
            nature: KnowledgeNature::Convention,
            description: "uses snake_case".to_owned(),
            evidence: vec![CodeEvidence {
                file: PathBuf::from("a.rs"),
                line: 1,
                end_line: 1,
                snippet: "fn my_func()".to_owned(),
                snippet_start_line: 0,
                anchor: AnchorKind::CallSite,
            }],
            follows_convention: true,
            kind: FindingKind::Other,
        }];
        let result = aggregate_findings(&findings, &default_config(), &no_dates(), 0);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].adoption_count, 1);
        assert_eq!(result[0].total_count, 1);
        assert!((result[0].confidence - 1.0).abs() < f64::EPSILON);
        assert_eq!(result[0].weight, KnowledgeWeight::Strong);
        assert_eq!(result[0].trend, Trend::Unknown);
    }

    #[test]
    fn aggregate_groups_by_detector_and_description() {
        let findings = vec![
            ConventionFinding {
                file_path: PathBuf::from("a.rs"),
                detector_name: "det_a".to_owned(),
                nature: KnowledgeNature::Convention,
                description: "pattern X".to_owned(),
                evidence: Vec::new(),
                follows_convention: true,
                kind: FindingKind::Other,
            },
            ConventionFinding {
                file_path: PathBuf::from("b.rs"),
                detector_name: "det_a".to_owned(),
                nature: KnowledgeNature::Convention,
                description: "pattern X".to_owned(),
                evidence: Vec::new(),
                follows_convention: false,
                kind: FindingKind::Other,
            },
            ConventionFinding {
                file_path: PathBuf::from("c.rs"),
                detector_name: "det_b".to_owned(),
                nature: KnowledgeNature::Observation,
                description: "pattern Y".to_owned(),
                evidence: Vec::new(),
                follows_convention: true,
                kind: FindingKind::Other,
            },
        ];
        let result = aggregate_findings(&findings, &default_config(), &no_dates(), 0);
        assert_eq!(result.len(), 2);

        // Find the det_a group.
        let det_a = result.iter().find(|a| a.detector_name == "det_a").unwrap();
        assert_eq!(det_a.adoption_count, 1);
        assert_eq!(det_a.total_count, 2);
        assert!((det_a.confidence - 0.5).abs() < f64::EPSILON);
        assert_eq!(det_a.weight, KnowledgeWeight::Weak);

        // Find the det_b group.
        let det_b = result.iter().find(|a| a.detector_name == "det_b").unwrap();
        assert_eq!(det_b.adoption_count, 1);
        assert_eq!(det_b.total_count, 1);
        assert!((det_b.confidence - 1.0).abs() < f64::EPSILON);
        assert_eq!(det_b.weight, KnowledgeWeight::Strong);
    }

    #[test]
    fn aggregate_computes_trend_from_file_dates() {
        let now = 1_000_000_000_i64;
        let recent = now - 30 * 86_400;
        let old = now - 400 * 86_400;

        let mut dates = HashMap::new();
        dates.insert("recent.rs".to_owned(), Some(recent));
        dates.insert("old.rs".to_owned(), Some(old));

        let findings = vec![
            ConventionFinding {
                file_path: PathBuf::from("recent.rs"),
                detector_name: "det_a".to_owned(),
                nature: KnowledgeNature::Convention,
                description: "pattern X".to_owned(),
                evidence: Vec::new(),
                follows_convention: true,
                kind: FindingKind::Other,
            },
            ConventionFinding {
                file_path: PathBuf::from("old.rs"),
                detector_name: "det_b".to_owned(),
                nature: KnowledgeNature::Convention,
                description: "pattern Y".to_owned(),
                evidence: Vec::new(),
                follows_convention: true,
                kind: FindingKind::Other,
            },
        ];

        let result = aggregate_findings(&findings, &default_config(), &dates, now);
        assert_eq!(result.len(), 2);

        let det_a = result.iter().find(|a| a.detector_name == "det_a").unwrap();
        assert_eq!(det_a.trend, Trend::Rising); // 30 days ago → Rising

        let det_b = result.iter().find(|a| a.detector_name == "det_b").unwrap();
        assert_eq!(det_b.trend, Trend::Declining); // 400 days ago → Declining
    }

    // --- ext_data helper ---

    #[test]
    fn ext_data_includes_trend_and_adoption_rate() {
        let agg = AggregatedConvention {
            detector_name: "test".to_owned(),
            description: "desc".to_owned(),
            nature: KnowledgeNature::Convention,
            adoption_count: 8,
            total_count: 10,
            confidence: 0.8,
            weight: KnowledgeWeight::Moderate,
            evidence: Vec::new(),
            trend: Trend::Rising,
        };
        let ext = agg.ext_data(None).unwrap();
        assert_eq!(ext["trend"], "rising");
        assert!((ext["adoption_rate"].as_f64().unwrap() - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn ext_data_merges_with_existing() {
        let agg = AggregatedConvention {
            detector_name: "test".to_owned(),
            description: "desc".to_owned(),
            nature: KnowledgeNature::Convention,
            adoption_count: 5,
            total_count: 10,
            confidence: 0.5,
            weight: KnowledgeWeight::Weak,
            evidence: Vec::new(),
            trend: Trend::Stable,
        };
        let existing = serde_json::json!({"reasoning": "some reason"});
        let ext = agg.ext_data(Some(&existing)).unwrap();
        assert_eq!(ext["trend"], "stable");
        assert_eq!(ext["reasoning"], "some reason");
        assert!((ext["adoption_rate"].as_f64().unwrap() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn aggregate_preserves_line0_snippet() {
        // Regression: line:0 evidence with a descriptive snippet (e.g. file naming)
        // must survive aggregate_findings unchanged.
        use seshat_core::ConventionFinding;
        use std::path::PathBuf;

        let file_path = PathBuf::from("src/config_service.rs");
        let finding = ConventionFinding {
            file_path: file_path.clone(),
            detector_name: "naming_conventions".to_owned(),
            nature: KnowledgeNature::Convention,
            description: "File naming: snake_case convention (Rust)".to_owned(),
            evidence: vec![CodeEvidence {
                file: file_path.clone(),
                line: 0,
                end_line: 0,
                snippet: "config_service [snake_case]".to_owned(),
                snippet_start_line: 0,
                anchor: AnchorKind::FileLevel,
            }],
            follows_convention: true,
            kind: FindingKind::Other,
        };

        let config = default_config();
        let dates = std::collections::HashMap::new();
        let aggregated = aggregate_findings(&[finding], &config, &dates, 0);

        assert_eq!(aggregated.len(), 1);
        let ev = &aggregated[0].evidence[0];
        assert_eq!(
            ev.snippet, "config_service [snake_case]",
            "aggregate_findings must preserve line:0 snippet"
        );
    }

    /// Two findings carrying identical evidence rows (same file + same line
    /// span) must collapse into a single evidence in the aggregated bucket.
    /// Without dedup the TUI would display visually-identical examples.
    #[test]
    fn aggregate_dedups_evidence_by_file_line_endline() {
        let dup_evidence = CodeEvidence {
            file: PathBuf::from("a.rs"),
            line: 14,
            end_line: 14,
            snippet: String::new(),
            snippet_start_line: 0,
            anchor: AnchorKind::CallSite,
        };
        let findings = vec![
            ConventionFinding {
                file_path: PathBuf::from("a.rs"),
                detector_name: "det".to_owned(),
                nature: KnowledgeNature::Convention,
                description: "X".to_owned(),
                evidence: vec![dup_evidence.clone()],
                follows_convention: true,
                kind: FindingKind::Other,
            },
            ConventionFinding {
                file_path: PathBuf::from("a.rs"),
                detector_name: "det".to_owned(),
                nature: KnowledgeNature::Convention,
                description: "X".to_owned(),
                evidence: vec![dup_evidence.clone()],
                follows_convention: true,
                kind: FindingKind::Other,
            },
        ];
        let result = aggregate_findings(&findings, &default_config(), &no_dates(), 0);
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].evidence.len(),
            1,
            "duplicate evidence must collapse, got {} entries",
            result[0].evidence.len(),
        );
    }

    /// Distinct evidence rows from multiple findings — different lines or
    /// different files — must all survive aggregation.
    #[test]
    fn aggregate_keeps_distinct_evidence() {
        let findings = vec![
            ConventionFinding {
                file_path: PathBuf::from("a.rs"),
                detector_name: "det".to_owned(),
                nature: KnowledgeNature::Convention,
                description: "X".to_owned(),
                evidence: vec![CodeEvidence {
                    file: PathBuf::from("a.rs"),
                    line: 10,
                    end_line: 10,
                    snippet: String::new(),
                    snippet_start_line: 0,
                    anchor: AnchorKind::CallSite,
                }],
                follows_convention: true,
                kind: FindingKind::Other,
            },
            ConventionFinding {
                file_path: PathBuf::from("b.rs"),
                detector_name: "det".to_owned(),
                nature: KnowledgeNature::Convention,
                description: "X".to_owned(),
                evidence: vec![CodeEvidence {
                    file: PathBuf::from("b.rs"),
                    line: 20,
                    end_line: 22,
                    snippet: String::new(),
                    snippet_start_line: 0,
                    anchor: AnchorKind::CallSite,
                }],
                follows_convention: true,
                kind: FindingKind::Other,
            },
        ];
        let result = aggregate_findings(&findings, &default_config(), &no_dates(), 0);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].evidence.len(), 2);
    }

    /// File-level findings (`AnchorKind::FileLevel`) from many files
    /// collapse into ONE composite evidence row whose snippet
    /// enumerates every contributing file. Replaces the previous
    /// "98 examples × empty snippets" UX for conventions like
    /// "Test file placement" or "File naming: snake_case".
    #[test]
    fn aggregate_collapses_multi_file_file_level_evidence_into_one_composite_row() {
        let make_finding = |path: &str, descriptor: &str| ConventionFinding {
            file_path: PathBuf::from(path),
            detector_name: "naming".to_owned(),
            nature: KnowledgeNature::Convention,
            description: "File naming: snake_case".to_owned(),
            evidence: vec![CodeEvidence {
                file: PathBuf::from(path),
                line: 0,
                end_line: 0,
                snippet: descriptor.to_owned(),
                snippet_start_line: 0,
                anchor: AnchorKind::FileLevel,
            }],
            follows_convention: true,
            kind: FindingKind::Naming,
        };
        let findings = vec![
            make_finding("src/config.rs", "config [snake_case]"),
            make_finding("src/db.rs", "db [snake_case]"),
            make_finding("src/error.rs", "error [snake_case]"),
        ];
        let result = aggregate_findings(&findings, &default_config(), &no_dates(), 0);
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].evidence.len(),
            1,
            "3 file-level rows must collapse to 1 composite",
        );
        let snippet = &result[0].evidence[0].snippet;
        assert!(snippet.contains("3 files match"));
        assert!(snippet.contains("src/config.rs"));
        assert!(snippet.contains("src/db.rs"));
        assert!(snippet.contains("src/error.rs"));
        assert!(snippet.contains("(config [snake_case])"));
    }

    /// A single FileLevel row with an EMPTY snippet must collapse into
    /// the composite rather than passing through verbatim — otherwise
    /// the TUI renders an "Example (1/1): (path:0)" tab with the
    /// useless "(no snippet available)" placeholder.
    ///
    /// Concrete trigger: `test_patterns` emits "Testing framework (from
    /// config file): pytest" with `evidence: [{file, line:0, snippet:""}]`.
    /// One file, one row, zero descriptor — the composite path-inline
    /// rendering is the only way for the reviewer to see *which* file
    /// triggered the convention.
    #[test]
    fn aggregate_collapses_empty_snippet_singleton_to_composite() {
        let finding = ConventionFinding {
            file_path: PathBuf::from("/proj/tests/conftest.py"),
            detector_name: "test_patterns".to_owned(),
            nature: KnowledgeNature::Observation,
            description: "Testing framework (from config file): pytest".to_owned(),
            evidence: vec![CodeEvidence {
                file: PathBuf::from("/proj/tests/conftest.py"),
                line: 0,
                end_line: 0,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::FileLevel,
            }],
            follows_convention: true,
            kind: FindingKind::Testing,
        };
        let result = aggregate_findings(&[finding], &default_config(), &no_dates(), 0);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].evidence.len(), 1);
        let snippet = &result[0].evidence[0].snippet;
        assert!(
            snippet.contains("1 file matches this convention"),
            "single empty-snippet row must collapse into composite, got: {snippet:?}",
        );
        assert!(
            snippet.contains("/proj/tests/conftest.py"),
            "composite must include the file path so the reviewer sees which file matched, got: {snippet:?}",
        );
        // The composite row's own `file` is empty + `line` == 0 so the
        // TUI renders it under the `── Summary ` heading instead of
        // the `── Example (path:line) ──` one.
        assert!(result[0].evidence[0].file.as_os_str().is_empty());
        assert_eq!(result[0].evidence[0].line, 0);
    }

    /// Regression: snippets that contain ONLY whitespace (spaces,
    /// tabs, newlines) render as a blank example pane in the TUI —
    /// visually identical to the "(no snippet available)" placeholder.
    /// The singleton-collapse must use `trim().is_empty()` rather than
    /// `is_empty()` so these defensively collapse into the composite
    /// where the file path is at least visible.
    #[test]
    fn aggregate_collapses_whitespace_only_singleton_to_composite() {
        for ws in ["   ", "\n\n", "\t", " \n\t "] {
            let finding = ConventionFinding {
                file_path: PathBuf::from("/proj/tests/conftest.py"),
                detector_name: "test_patterns".to_owned(),
                nature: KnowledgeNature::Observation,
                description: "Testing framework (from config file): pytest".to_owned(),
                evidence: vec![CodeEvidence {
                    file: PathBuf::from("/proj/tests/conftest.py"),
                    line: 0,
                    end_line: 0,
                    snippet: ws.to_owned(),
                    snippet_start_line: 0,
                    anchor: AnchorKind::FileLevel,
                }],
                follows_convention: true,
                kind: FindingKind::Testing,
            };
            let result = aggregate_findings(&[finding], &default_config(), &no_dates(), 0);
            let snippet = &result[0].evidence[0].snippet;
            assert!(
                snippet.contains("1 file matches this convention"),
                "whitespace-only snippet {ws:?} must collapse into composite; got: {snippet:?}",
            );
            assert!(
                snippet.contains("/proj/tests/conftest.py"),
                "composite must include file path; got: {snippet:?}",
            );
        }
    }

    /// Documents the invariant the integration test relies on: when a
    /// single convention bucket accumulates BOTH anchored and
    /// file-level rows, `aggregate_findings` lays them out as
    /// `[anchored..., one composite tail]` — never interleaved, never
    /// the other way around. The TUI relies on this layout to render
    /// the example pane (anchored rows) and the summary footer (the
    /// tail row).
    #[test]
    fn aggregate_lays_out_mixed_evidence_as_anchored_prefix_then_filelevel_tail() {
        let anchored = ConventionFinding {
            file_path: PathBuf::from("/proj/src/a.rs"),
            detector_name: "logging".to_owned(),
            nature: KnowledgeNature::Convention,
            description: "Canonical logging library: tracing".to_owned(),
            evidence: vec![CodeEvidence {
                file: PathBuf::from("/proj/src/a.rs"),
                line: 12,
                end_line: 12,
                snippet: "tracing::info!(\"hi\");".to_owned(),
                snippet_start_line: 12,
                anchor: AnchorKind::CallSite,
            }],
            follows_convention: true,
            kind: FindingKind::Canonical,
        };
        let file_level = ConventionFinding {
            file_path: PathBuf::from("/proj/src/b.rs"),
            detector_name: "logging".to_owned(),
            nature: KnowledgeNature::Convention,
            description: "Canonical logging library: tracing".to_owned(),
            evidence: vec![CodeEvidence {
                file: PathBuf::from("/proj/src/b.rs"),
                line: 0,
                end_line: 0,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::FileLevel,
            }],
            follows_convention: true,
            kind: FindingKind::Canonical,
        };
        let result = aggregate_findings(&[anchored, file_level], &default_config(), &no_dates(), 0);
        assert_eq!(result.len(), 1);
        let evidence = &result[0].evidence;
        assert!(
            evidence.len() >= 2,
            "expected at least one anchored row plus the file-level tail, got: {evidence:?}",
        );
        // Anchored rows come FIRST (line > 0).
        assert!(
            evidence[0].line > 0,
            "first row must be anchored, got: {:?}",
            evidence[0],
        );
        // Last row is the file-level tail (line == 0).
        let last = evidence.last().unwrap();
        assert_eq!(last.line, 0, "last row must be file-level, got: {last:?}");
        // Exactly ONE tail.
        let tail_count = evidence.iter().filter(|e| e.line == 0).count();
        assert_eq!(
            tail_count, 1,
            "expected exactly one file-level tail, got: {evidence:?}",
        );
    }

    /// A single FileLevel row WITH a useful descriptor (e.g. naming's
    /// `"config_service [snake_case]"`) still passes through verbatim
    /// — the descriptor IS the snippet, no need for a composite
    /// wrapper.
    #[test]
    fn aggregate_preserves_singleton_with_useful_descriptor() {
        let finding = ConventionFinding {
            file_path: PathBuf::from("src/config.rs"),
            detector_name: "naming".to_owned(),
            nature: KnowledgeNature::Convention,
            description: "File naming: snake_case".to_owned(),
            evidence: vec![CodeEvidence {
                file: PathBuf::from("src/config.rs"),
                line: 0,
                end_line: 0,
                snippet: "config [snake_case]".to_owned(),
                snippet_start_line: 0,
                anchor: AnchorKind::FileLevel,
            }],
            follows_convention: true,
            kind: FindingKind::Naming,
        };
        let result = aggregate_findings(&[finding], &default_config(), &no_dates(), 0);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].evidence.len(), 1);
        let ev = &result[0].evidence[0];
        // Descriptor passed through, no composite "files match" header.
        assert_eq!(ev.snippet, "config [snake_case]");
        assert!(!ev.file.as_os_str().is_empty(), "file must be preserved");
    }

    /// Anchored evidence (CallSite, Declaration, ImportLine) is NOT
    /// collapsed; only file-level rows are folded into the composite.
    /// Mixed buckets keep anchored rows verbatim and append the
    /// composite at the end.
    #[test]
    fn aggregate_does_not_collapse_anchored_evidence() {
        let findings = vec![
            ConventionFinding {
                file_path: PathBuf::from("a.rs"),
                detector_name: "det".to_owned(),
                nature: KnowledgeNature::Convention,
                description: "X".to_owned(),
                evidence: vec![CodeEvidence {
                    file: PathBuf::from("a.rs"),
                    line: 5,
                    end_line: 7,
                    snippet: "fn foo() {}".to_owned(),
                    snippet_start_line: 0,
                    anchor: AnchorKind::CallSite,
                }],
                follows_convention: true,
                kind: FindingKind::Other,
            },
            ConventionFinding {
                file_path: PathBuf::from("b.rs"),
                detector_name: "det".to_owned(),
                nature: KnowledgeNature::Convention,
                description: "X".to_owned(),
                evidence: vec![CodeEvidence {
                    file: PathBuf::from("b.rs"),
                    line: 12,
                    end_line: 14,
                    snippet: "fn bar() {}".to_owned(),
                    snippet_start_line: 0,
                    anchor: AnchorKind::Declaration,
                }],
                follows_convention: true,
                kind: FindingKind::Other,
            },
        ];
        let result = aggregate_findings(&findings, &default_config(), &no_dates(), 0);
        assert_eq!(result.len(), 1);
        // Two anchored rows survive verbatim; no composite is added.
        assert_eq!(result[0].evidence.len(), 2);
        assert!(
            result[0]
                .evidence
                .iter()
                .all(|e| !e.snippet.contains("files match"))
        );
    }

    // --- build_file_level_composite: smart sampling ---

    fn make_file_level_evidence(path: &str) -> CodeEvidence {
        CodeEvidence {
            file: PathBuf::from(path),
            line: 0,
            end_line: 0,
            snippet: String::new(),
            snippet_start_line: 0,
            anchor: AnchorKind::FileLevel,
        }
    }

    /// Test helper: wrap a slice of FileLevel evidence as all-conforming
    /// (the most common test fixture state) and call the composite
    /// renderer with matching adoption counts. Centralises the boring
    /// `(ev, true)` tagging across every smart-sampling test.
    fn build_composite_all_conforming(rows: &[CodeEvidence]) -> CodeEvidence {
        let pairs: Vec<(CodeEvidence, bool)> = rows.iter().map(|r| (r.clone(), true)).collect();
        let n = rows.len() as u32;
        build_file_level_composite(&pairs, n, n)
    }

    /// When the row count is at or below the cap, every row appears in
    /// the composite snippet — no truncation, header omits "showing N".
    #[test]
    fn composite_lists_every_row_when_under_cap() {
        let rows: Vec<CodeEvidence> = (0..5)
            .map(|i| make_file_level_evidence(&format!("/proj/src/m{i}.rs")))
            .collect();
        let composite = build_composite_all_conforming(&rows);
        assert!(composite.snippet.contains("5 files match this convention:"));
        assert!(!composite.snippet.contains("showing"));
        assert!(!composite.snippet.contains("truncated"));
        for i in 0..5 {
            assert!(
                composite.snippet.contains(&format!("/proj/src/m{i}.rs")),
                "row {i} must appear in composite",
            );
        }
    }

    /// When rows exceed the cap, the header advertises the sample size
    /// and a "... and N more (truncated)" tail line is appended.
    #[test]
    fn composite_truncates_with_summary_when_over_cap() {
        let rows: Vec<CodeEvidence> = (0..50)
            .map(|i| make_file_level_evidence(&format!("/proj/src/m{i:02}.rs")))
            .collect();
        let composite = build_composite_all_conforming(&rows);
        assert!(
            composite.snippet.contains(&format!(
                "50 files match this convention (showing {MAX_FILES_LISTED_IN_COMPOSITE})"
            )),
            "header must announce sample size, got: {}",
            composite.snippet,
        );
        assert!(
            composite.snippet.contains(&format!(
                "and {} more (truncated)",
                50 - MAX_FILES_LISTED_IN_COMPOSITE
            )),
            "tail must announce truncation count",
        );
    }

    /// Sampling round-robins across top-level subtrees so the snippet
    /// shows files from EVERY part of the project, not just the first
    /// alphabetical bucket. This is the core UX win over a simple
    /// `take(cap)` — without it, a 700-file Python project's composite
    /// would be 20 paths from `atlas/` and zero from `tests/`,
    /// `scripts/`, etc.
    #[test]
    fn composite_round_robins_across_top_level_subtrees() {
        // 30 files in `crates_a/`, 30 in `crates_b/`, 30 in `tests/` —
        // 90 total, sampled to 20. Each subtree must contribute roughly
        // a third (within ±2) so no group is starved.
        let mut rows = Vec::new();
        for i in 0..30 {
            rows.push(make_file_level_evidence(&format!(
                "/proj/crates_a/src/m{i:02}.rs"
            )));
        }
        for i in 0..30 {
            rows.push(make_file_level_evidence(&format!(
                "/proj/crates_b/src/m{i:02}.rs"
            )));
        }
        for i in 0..30 {
            rows.push(make_file_level_evidence(&format!("/proj/tests/m{i:02}.rs")));
        }

        let composite = build_composite_all_conforming(&rows);

        let count_substr = |needle: &str| -> usize { composite.snippet.matches(needle).count() };
        let from_a = count_substr("/proj/crates_a/");
        let from_b = count_substr("/proj/crates_b/");
        let from_tests = count_substr("/proj/tests/");

        assert!(
            from_a >= 6 && from_b >= 6 && from_tests >= 6,
            "each subtree must contribute at least 6 of 20 (round-robin), got a={from_a} b={from_b} tests={from_tests}",
        );
        assert_eq!(
            from_a + from_b + from_tests,
            MAX_FILES_LISTED_IN_COMPOSITE,
            "total selected must equal the cap",
        );
    }

    /// Multi-line evidence snippets (e.g. import_organization's
    /// `// Order: …\n// stdlib imports:\n…`) are reduced to their first
    /// line in the composite descriptor, with the leading `// ` comment
    /// marker stripped. Otherwise joining lines with spaces produces a
    /// 200+ char descriptor that is truncated mid-token at the right
    /// edge of the TUI snippet pane.
    #[test]
    fn composite_descriptor_takes_first_line_only() {
        assert_eq!(
            composite_descriptor(
                "// Order: stdlib → external → internal\n\n// stdlib imports:\n  std::path"
            ),
            Some("Order: stdlib → external → internal".to_owned()),
        );
    }

    /// Single-line snippets (e.g. naming detector's `"config_service
    /// [snake_case]"`) pass through unchanged — no `// ` prefix to
    /// strip, no newline to split.
    #[test]
    fn composite_descriptor_passes_through_single_line() {
        assert_eq!(
            composite_descriptor("config_service [snake_case]"),
            Some("config_service [snake_case]".to_owned()),
        );
    }

    /// Empty or whitespace-only first line → no descriptor block in
    /// the composite row (renderer falls back to bare path).
    #[test]
    fn composite_descriptor_returns_none_for_empty_or_whitespace() {
        assert_eq!(composite_descriptor(""), None);
        assert_eq!(composite_descriptor("   \n// foo"), None);
    }

    /// The composite renderer must use `composite_descriptor` so a
    /// multi-line `import_organization` snippet collapses to just the
    /// `Order: …` headline inside the parens — not the whole 4-line
    /// summary smashed onto one line.
    #[test]
    fn composite_renders_multi_line_import_snippet_as_first_line() {
        let row = CodeEvidence {
            file: PathBuf::from("/proj/src/a.rs"),
            line: 1,
            end_line: 5,
            snippet: "// Order: stdlib → external\n\n// stdlib imports:\n  std::io\n\n// external imports:\n  serde".to_owned(),
            snippet_start_line: 0,
            anchor: AnchorKind::FileLevel,
        };
        let composite = build_composite_all_conforming(&[row]);
        assert!(
            composite.snippet.contains("(Order: stdlib → external)"),
            "composite must show only the Order headline in parens, got: {}",
            composite.snippet,
        );
        assert!(
            !composite.snippet.contains("stdlib imports"),
            "composite must NOT include the per-group import details (those overflow the TUI pane), got: {}",
            composite.snippet,
        );
    }

    /// `select_diverse_sample` returns the input verbatim when
    /// rows.len() <= cap — no work, no allocation surprises.
    #[test]
    fn select_diverse_sample_returns_input_when_under_cap() {
        let rows = vec![
            make_file_level_evidence("/proj/a.rs"),
            make_file_level_evidence("/proj/b.rs"),
        ];
        let selected = select_diverse_sample(&rows, 20);
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].file, rows[0].file);
        assert_eq!(selected[1].file, rows[1].file);
    }

    /// Python `__init__.py` package markers are filtered out of the
    /// sample whenever there is at least one substantive file to show.
    /// Without this, a 274-file `tests/` convention surfaces ~11 of 20
    /// sample slots filled with empty `__init__.py` rows, hiding the
    /// real test modules.
    #[test]
    fn select_diverse_sample_drops_python_init_markers() {
        let mut rows = Vec::new();
        // 5 real test modules.
        for i in 0..5 {
            rows.push(make_file_level_evidence(&format!(
                "/proj/tests/test_m{i}.py"
            )));
        }
        // 10 empty package markers across various subdirs.
        for sub in &["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"] {
            rows.push(make_file_level_evidence(&format!(
                "/proj/tests/{sub}/__init__.py"
            )));
        }
        let selected = select_diverse_sample(&rows, 20);
        assert!(
            selected.iter().all(|ev| !ev.file.ends_with("__init__.py")),
            "no __init__.py file should appear in the sample when other files exist",
        );
        assert_eq!(
            selected.len(),
            5,
            "all 5 real modules must be shown; the 10 markers are skipped",
        );
    }

    /// When *every* row is a marker file, the sampler falls back to
    /// the unfiltered set so the composite still has rows to render —
    /// rather than producing an empty snippet.
    #[test]
    fn select_diverse_sample_falls_back_when_all_rows_are_markers() {
        let rows: Vec<CodeEvidence> = (0..3)
            .map(|i| make_file_level_evidence(&format!("/proj/pkg{i}/__init__.py")))
            .collect();
        let selected = select_diverse_sample(&rows, 20);
        assert_eq!(
            selected.len(),
            3,
            "fallback must include all 3 marker rows when there's nothing else",
        );
    }

    /// Regression: when EVERY row is a marker, the sampler's fallback
    /// shows the markers themselves. The header must NOT claim those
    /// shown rows are "informative" (that would lie) and MUST disclose
    /// that all rows are markers (otherwise the user reads "30 files"
    /// thinking there are 30 substantive examples). The truncation
    /// tail must also count against the raw total, not the (zero)
    /// informative pool.
    #[test]
    fn composite_header_does_not_lie_about_informative_when_all_markers() {
        // 30 marker-only rows, cap is 20 so 10 must be reported as
        // truncated.
        let rows: Vec<CodeEvidence> = (0..30)
            .map(|i| make_file_level_evidence(&format!("/proj/pkg{i:02}/__init__.py")))
            .collect();
        let composite = build_composite_all_conforming(&rows);
        assert!(
            !composite.snippet.contains("informative"),
            "all-markers fallback must not claim shown rows are informative; got: {}",
            composite.snippet,
        );
        assert!(
            !composite.snippet.contains("__init__.py markers omitted"),
            "no markers were omitted in the fallback path; got: {}",
            composite.snippet,
        );
        // Header must disclose the all-markers status so the user
        // doesn't read "30 files match" as 30 substantive matches.
        assert!(
            composite.snippet.contains(
                "30 files match this convention (showing 20; all are __init__.py markers):"
            ),
            "fallback header should disclose the marker-only status; got: {}",
            composite.snippet,
        );
        assert!(
            composite.snippet.contains("... and 10 more (truncated)"),
            "fallback truncation tail must count against the raw total (30 − 20); got: {}",
            composite.snippet,
        );
    }

    /// All-markers fallback when the sample fits in the cap: header
    /// still discloses the marker-only status and skips the truncation
    /// tail (everything fits).
    #[test]
    fn composite_all_markers_under_cap_discloses_marker_status() {
        // 5 marker-only rows, cap 20 → all shown, no truncation.
        let rows: Vec<CodeEvidence> = (0..5)
            .map(|i| make_file_level_evidence(&format!("/proj/pkg{i}/__init__.py")))
            .collect();
        let composite = build_composite_all_conforming(&rows);
        assert!(
            composite
                .snippet
                .contains("5 files match this convention (all are __init__.py markers):"),
            "under-cap all-markers header must disclose marker-only status; got: {}",
            composite.snippet,
        );
        assert!(
            !composite.snippet.contains("more (truncated)"),
            "no truncation tail when all rows fit in the cap; got: {}",
            composite.snippet,
        );
    }

    /// Composite header should announce *both* totals when markers are
    /// hidden, so the user understands why "showing 5" is less than
    /// "274 files match".
    #[test]
    fn composite_header_calls_out_omitted_init_py_markers() {
        let mut rows = Vec::new();
        for i in 0..5 {
            rows.push(make_file_level_evidence(&format!(
                "/proj/tests/test_m{i}.py"
            )));
        }
        for sub in &["a", "b", "c", "d", "e"] {
            rows.push(make_file_level_evidence(&format!(
                "/proj/tests/{sub}/__init__.py"
            )));
        }
        let composite = build_composite_all_conforming(&rows);
        assert!(
            composite
                .snippet
                .contains("10 files match this convention (showing 5 informative; 5 __init__.py markers omitted)"),
            "header must call out omitted markers, got: {}",
            composite.snippet,
        );
        assert!(
            !composite.snippet.contains("more (truncated)"),
            "no truncation tail when all informative rows are shown, got: {}",
            composite.snippet,
        );
    }

    // --- composite_header_verb: adoption-aware wording ---

    #[test]
    fn composite_header_verb_all_conforming_plural() {
        assert_eq!(
            composite_header_verb(5, 5, 5),
            "match this convention".to_owned()
        );
    }

    #[test]
    fn composite_header_verb_all_conforming_singular() {
        assert_eq!(
            composite_header_verb(1, 1, 1),
            "matches this convention".to_owned()
        );
    }

    #[test]
    fn composite_header_verb_all_violating_plural() {
        assert_eq!(
            composite_header_verb(0, 5, 5),
            "violate this convention".to_owned()
        );
    }

    #[test]
    fn composite_header_verb_all_violating_singular() {
        assert_eq!(
            composite_header_verb(0, 1, 1),
            "violates this convention".to_owned()
        );
    }

    #[test]
    fn composite_header_verb_mixed_shows_follow_violate_split() {
        // 3 of 5 follow, 2 violate.
        assert_eq!(
            composite_header_verb(3, 5, 5),
            "reference this convention (3 follow / 2 violate)".to_owned()
        );
    }

    /// Composite header for a violation-only bucket says "violate", not
    /// "match", so the wording stops contradicting the `Found in: 0/N`
    /// adoption line shown directly above it in the review TUI.
    #[test]
    fn composite_header_says_violate_when_no_one_follows() {
        let rows = vec![
            (make_file_level_evidence("/proj/a.ts"), false),
            (make_file_level_evidence("/proj/b.ts"), false),
            (make_file_level_evidence("/proj/c.ts"), false),
        ];
        let composite = build_file_level_composite(&rows, 0, 3);
        assert!(
            composite
                .snippet
                .contains("3 files violate this convention:"),
            "header should say 'violate' for 0% adoption bucket, got: {}",
            composite.snippet,
        );
        assert!(
            !composite.snippet.contains("match this convention"),
            "should not also say 'match', got: {}",
            composite.snippet,
        );
    }

    /// Mixed bucket: 2 of 5 files follow, 3 violate. The header surfaces
    /// the split inline so the user doesn't need to cross-reference the
    /// `Found in:` adoption ratio just to know which listed files are
    /// the offenders.
    #[test]
    fn composite_header_shows_split_for_mixed_bucket() {
        let rows = vec![
            (make_file_level_evidence("/proj/a.ts"), true),
            (make_file_level_evidence("/proj/b.ts"), true),
            (make_file_level_evidence("/proj/c.ts"), false),
            (make_file_level_evidence("/proj/d.ts"), false),
            (make_file_level_evidence("/proj/e.ts"), false),
        ];
        let composite = build_file_level_composite(&rows, 2, 5);
        assert!(
            composite
                .snippet
                .contains("5 files reference this convention (2 follow / 3 violate):"),
            "mixed header should split follow/violate counts, got: {}",
            composite.snippet,
        );
    }

    /// Per-file non-conforming marker appears next to violator paths in
    /// the listing so the user can scan the list without losing track of
    /// which files are the offenders.
    #[test]
    fn composite_listing_marks_non_conforming_rows() {
        let rows = vec![
            (make_file_level_evidence("/proj/conforming.ts"), true),
            (make_file_level_evidence("/proj/offender.ts"), false),
        ];
        let composite = build_file_level_composite(&rows, 1, 2);
        // Conforming row carries no marker.
        let conforming_line = composite
            .snippet
            .lines()
            .find(|l| l.contains("/proj/conforming.ts"))
            .expect("conforming row must be in the listing");
        assert!(
            !conforming_line.contains("[non-conforming]"),
            "conforming row must not carry the non-conforming marker; got: {conforming_line}"
        );
        // Non-conforming row carries the marker.
        let offender_line = composite
            .snippet
            .lines()
            .find(|l| l.contains("/proj/offender.ts"))
            .expect("offender row must be in the listing");
        assert!(
            offender_line.contains("[non-conforming]"),
            "non-conforming row must carry the marker; got: {offender_line}"
        );
    }

    /// Common-prefix detection skips the project root so grouping
    /// happens at the first *varying* directory level.
    #[test]
    fn longest_common_prefix_excludes_filename_and_stops_at_divergence() {
        let rows = [
            make_file_level_evidence("/proj/a/x.rs"),
            make_file_level_evidence("/proj/b/y.rs"),
        ];
        // Components: ["/", "proj", "a", "x.rs"] vs ["/", "proj", "b", "y.rs"]
        // First two match; "a" vs "b" diverge → prefix_len = 2.
        let refs: Vec<&CodeEvidence> = rows.iter().collect();
        assert_eq!(longest_common_prefix_len(&refs), 2);
    }

    /// When all files live under a deep common prefix, the prefix
    /// should still stop one level before the file name — otherwise
    /// every row collapses into the `"<root>"` bucket.
    #[test]
    fn longest_common_prefix_caps_at_directory_depth() {
        let rows = vec![
            make_file_level_evidence("/proj/src/a.rs"),
            make_file_level_evidence("/proj/src/b.rs"),
            make_file_level_evidence("/proj/src/c.rs"),
        ];
        // All in /proj/src/ → prefix would be 3 components, but we cap
        // at len-1 = 3 for the first row. min across rows = 3, which
        // points to the file name index. Group key uses components()
        // .nth(3) → file name, so each file lands in its own bucket.
        // That's fine: all 3 fit under the cap and round-robin returns
        // them all.
        let selected = select_diverse_sample(&rows, 20);
        assert_eq!(selected.len(), 3);
    }
}
