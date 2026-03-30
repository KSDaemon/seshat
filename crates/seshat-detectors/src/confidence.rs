//! Frequency-based confidence scoring (ADR-7).
//!
//! Confidence is computed as `adoption_count / total_count` and mapped to a
//! [`KnowledgeWeight`] via configurable thresholds from [`DetectionConfig`].
//!
//! The [`aggregate_findings`] function groups raw per-file findings by
//! `(detector_name, description)`, computes adoption counts, and produces
//! [`AggregatedConvention`] values ready for storage.

use std::collections::HashMap;

use seshat_core::{ConventionFinding, DetectionConfig, KnowledgeNature, KnowledgeWeight, Trend};

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
    struct Bucket {
        nature: KnowledgeNature,
        adoption_count: u32,
        total_count: u32,
        evidence: Vec<seshat_core::CodeEvidence>,
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
            evidence: Vec::new(),
            dates: Vec::new(),
        });

        bucket.total_count += 1;
        if finding.follows_convention {
            bucket.adoption_count += 1;
        }

        // Collect a bounded number of evidence snippets.
        if bucket.evidence.len() < config.max_snippet_lines {
            bucket.evidence.extend(finding.evidence.iter().cloned());
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
            AggregatedConvention {
                detector_name,
                description,
                nature: bucket.nature,
                adoption_count: bucket.adoption_count,
                total_count: bucket.total_count,
                confidence,
                weight,
                evidence: bucket.evidence,
                trend,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::{CodeEvidence, KnowledgeNature, KnowledgeWeight, Trend};
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
                line: 1,
                end_line: 1,
                snippet: "fn my_func()".to_owned(),
            }],
            follows_convention: true,
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
            },
            ConventionFinding {
                file_path: PathBuf::from("b.rs"),
                detector_name: "det_a".to_owned(),
                nature: KnowledgeNature::Convention,
                description: "pattern X".to_owned(),
                evidence: Vec::new(),
                follows_convention: false,
            },
            ConventionFinding {
                file_path: PathBuf::from("c.rs"),
                detector_name: "det_b".to_owned(),
                nature: KnowledgeNature::Observation,
                description: "pattern Y".to_owned(),
                evidence: Vec::new(),
                follows_convention: true,
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
            },
            ConventionFinding {
                file_path: PathBuf::from("old.rs"),
                detector_name: "det_b".to_owned(),
                nature: KnowledgeNature::Convention,
                description: "pattern Y".to_owned(),
                evidence: Vec::new(),
                follows_convention: true,
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
}
