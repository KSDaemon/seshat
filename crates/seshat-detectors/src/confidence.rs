//! Frequency-based confidence scoring (ADR-7).
//!
//! Confidence is computed as `adoption_count / total_count` and mapped to a
//! [`KnowledgeWeight`] via configurable thresholds from [`DetectionConfig`].
//!
//! The [`aggregate_findings`] function groups raw per-file findings by
//! `(detector_name, description)`, computes adoption counts, and produces
//! [`AggregatedConvention`] values ready for storage.

use std::collections::HashMap;

use seshat_core::{ConventionFinding, DetectionConfig, KnowledgeNature, KnowledgeWeight};

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
pub fn aggregate_findings(
    findings: &[ConventionFinding],
    config: &DetectionConfig,
) -> Vec<AggregatedConvention> {
    /// Grouping key and accumulator.
    struct Bucket {
        nature: KnowledgeNature,
        adoption_count: u32,
        total_count: u32,
        evidence: Vec<seshat_core::CodeEvidence>,
    }

    let mut groups: HashMap<(String, String), Bucket> = HashMap::new();

    for finding in findings {
        let key = (finding.detector_name.clone(), finding.description.clone());
        let bucket = groups.entry(key).or_insert_with(|| Bucket {
            nature: finding.nature,
            adoption_count: 0,
            total_count: 0,
            evidence: Vec::new(),
        });

        bucket.total_count += 1;
        if finding.follows_convention {
            bucket.adoption_count += 1;
        }

        // Collect a bounded number of evidence snippets.
        if bucket.evidence.len() < config.max_snippet_lines {
            bucket.evidence.extend(finding.evidence.iter().cloned());
        }
    }

    groups
        .into_iter()
        .map(|((detector_name, description), bucket)| {
            let confidence = compute_confidence(bucket.adoption_count, bucket.total_count);
            let weight = weight_from_confidence(confidence, config);
            AggregatedConvention {
                detector_name,
                description,
                nature: bucket.nature,
                adoption_count: bucket.adoption_count,
                total_count: bucket.total_count,
                confidence,
                weight,
                evidence: bucket.evidence,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::{CodeEvidence, KnowledgeNature, KnowledgeWeight};
    use std::path::PathBuf;

    fn default_config() -> DetectionConfig {
        DetectionConfig::default()
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

    // --- aggregate_findings ---

    #[test]
    fn aggregate_empty_findings() {
        let result = aggregate_findings(&[], &default_config());
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
        let result = aggregate_findings(&findings, &default_config());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].adoption_count, 1);
        assert_eq!(result[0].total_count, 1);
        assert!((result[0].confidence - 1.0).abs() < f64::EPSILON);
        assert_eq!(result[0].weight, KnowledgeWeight::Strong);
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
        let result = aggregate_findings(&findings, &default_config());
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
}
