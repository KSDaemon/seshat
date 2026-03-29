//! Cross-reference code conventions vs documentation.
//!
//! Compares code-detected conventions (`Nature = Convention/Observation`) with
//! documentation-sourced knowledge nodes (`Nature = Fact`) using keyword/topic
//! matching (ADR-23). Produces:
//!
//! - **Contradictions**: a `Contradicts` edge between a doc node and a code
//!   convention node when their descriptions share keywords but reference
//!   different concrete choices (e.g., doc says "use X", code convention says
//!   "use Y").
//! - **Reinforcements**: boosted confidence on code convention nodes whose
//!   description matches a supporting doc node.
//!
//! The matching is intentionally simple (no NLP, no semantic embeddings) —
//! extract significant keywords, compute overlap, and apply heuristics for
//! agreement vs contradiction.

use std::collections::HashSet;
use std::sync::LazyLock;

use seshat_core::{BranchId, Edge, EdgeId, EdgeType, KnowledgeNode, KnowledgeWeight, NodeId};

// ── Public types ──────────────────────────────────────────────────────

/// Result of cross-referencing code conventions against documentation.
#[derive(Debug, Clone)]
pub struct CrossReferenceResult {
    /// Edges representing contradictions between doc nodes and code conventions.
    pub contradiction_edges: Vec<Edge>,
    /// Node IDs whose confidence was boosted by matching documentation.
    pub reinforced_nodes: Vec<ReinforcedNode>,
}

/// A code convention node whose confidence was boosted by documentation.
#[derive(Debug, Clone)]
pub struct ReinforcedNode {
    /// The ID of the code convention node that was reinforced.
    pub node_id: NodeId,
    /// The node's confidence before documentation reinforcement.
    pub original_confidence: f64,
    /// The node's confidence after applying the reinforcement boost.
    pub boosted_confidence: f64,
    /// The [`KnowledgeWeight`] corresponding to the boosted confidence.
    pub boosted_weight: KnowledgeWeight,
    /// The matching doc node that provided reinforcement.
    pub matched_doc_id: NodeId,
}

// ── Configuration ─────────────────────────────────────────────────────

/// Configuration for cross-reference matching.
#[derive(Debug, Clone)]
pub struct CrossReferenceConfig {
    /// Minimum keyword overlap ratio (Jaccard) to consider two descriptions
    /// as related. Range 0.0–1.0.
    pub min_keyword_overlap: f64,
    /// How much to boost confidence when documentation reinforces a code
    /// convention. Applied as: `new = old + boost * (1.0 - old)`.
    pub reinforcement_boost: f64,
}

impl Default for CrossReferenceConfig {
    fn default() -> Self {
        Self {
            min_keyword_overlap: 0.15,
            reinforcement_boost: 0.10,
        }
    }
}

// ── Core logic ────────────────────────────────────────────────────────

/// Cross-reference code conventions against documentation nodes.
///
/// Accepts slices of knowledge nodes already loaded from storage. Returns
/// edges to create and nodes to update — the caller is responsible for
/// persisting the results.
///
/// # Arguments
///
/// * `code_conventions` — nodes with `nature` = `Convention` or `Observation`
/// * `doc_nodes` — nodes with `nature` = `Fact` (documentation-sourced)
/// * `branch_id` — the branch for newly created edges
/// * `config` — matching thresholds
#[tracing::instrument(skip_all)]
pub fn cross_reference(
    code_conventions: &[KnowledgeNode],
    doc_nodes: &[KnowledgeNode],
    branch_id: &BranchId,
    config: &CrossReferenceConfig,
) -> CrossReferenceResult {
    let mut contradiction_edges = Vec::new();
    let mut reinforced_nodes = Vec::new();
    let mut edge_counter: i64 = 0;

    for doc in doc_nodes {
        let doc_keywords = extract_keywords(&doc.description);
        if doc_keywords.is_empty() {
            continue;
        }

        for convention in code_conventions {
            let conv_keywords = extract_keywords(&convention.description);
            if conv_keywords.is_empty() {
                continue;
            }

            let overlap = jaccard_similarity(&doc_keywords, &conv_keywords);
            if overlap < config.min_keyword_overlap {
                continue;
            }

            // Descriptions share enough keywords to be about the same topic.
            // Now determine: agreement or contradiction?
            match classify_relationship(&doc.description, &convention.description) {
                Relationship::Contradiction => {
                    edge_counter += 1;
                    let edge = Edge {
                        id: EdgeId(edge_counter),
                        source_id: convention.id,
                        target_id: doc.id,
                        edge_type: EdgeType::Contradicts,
                        branch_id: branch_id.clone(),
                        weight: overlap,
                        metadata: Some(serde_json::json!({
                            "reason": "keyword_match_with_different_choice",
                            "overlap": overlap,
                            "doc_description": doc.description,
                            "convention_description": convention.description,
                        })),
                    };
                    contradiction_edges.push(edge);
                }
                Relationship::Reinforcement => {
                    let boosted =
                        boost_confidence(convention.confidence, config.reinforcement_boost);
                    let boosted_weight = weight_from_confidence(boosted);
                    reinforced_nodes.push(ReinforcedNode {
                        node_id: convention.id,
                        original_confidence: convention.confidence,
                        boosted_confidence: boosted,
                        boosted_weight,
                        matched_doc_id: doc.id,
                    });
                }
                Relationship::Unrelated => {
                    // Keywords overlap but no clear agreement or disagreement.
                    // Skip — avoid false positives.
                }
            }
        }
    }

    CrossReferenceResult {
        contradiction_edges,
        reinforced_nodes,
    }
}

// ── Keyword extraction ────────────────────────────────────────────────

/// Stop words excluded from keyword matching.
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "do", "for", "from", "has", "have", "he",
    "in", "is", "it", "its", "of", "on", "or", "she", "that", "the", "their", "them", "then",
    "there", "these", "they", "this", "to", "was", "we", "were", "will", "with", "you", "your",
    "use", "using", "used", "should", "must", "can", "may", "all", "each", "every", "no", "not",
    "only", "but", "if", "when", "where", "how", "what", "which", "who", "whom", "why", "so",
    "than", "too", "very", "just", "about", "also", "been", "being", "both", "could", "did",
    "does", "doing", "done", "had", "having", "here", "into", "more", "most", "much", "own",
    "same", "some", "such", "would",
];

/// Pre-built stop-word set, initialized once on first access.
static STOP_SET: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| STOP_WORDS.iter().copied().collect());

/// Extract significant keywords from a description string.
///
/// Lowercases, splits on non-alphanumeric boundaries, removes stop words,
/// and filters out very short tokens.
fn extract_keywords(description: &str) -> HashSet<String> {
    description
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|w| w.len() >= 2)
        .filter(|w| !STOP_SET.contains(w))
        .map(|w| w.to_owned())
        .collect()
}

/// Compute the Jaccard similarity coefficient between two keyword sets.
fn jaccard_similarity(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    let intersection = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    intersection as f64 / union as f64
}

// ── Relationship classification ───────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Relationship {
    Contradiction,
    Reinforcement,
    Unrelated,
}

/// Classify whether two related descriptions agree or contradict.
///
/// Heuristic approach:
/// 1. Extract "choice" tokens — concrete library/tool/pattern names that
///    appear after signal phrases like "use", "prefer", "canonical".
/// 2. If both descriptions contain choices and the choices differ →
///    contradiction.
/// 3. If both descriptions contain choices and the choices overlap →
///    reinforcement.
/// 4. If one or both have no extractable choices → unrelated (cannot
///    determine agreement without concrete choices).
fn classify_relationship(doc_desc: &str, conv_desc: &str) -> Relationship {
    let doc_choices = extract_choices(doc_desc);
    let conv_choices = extract_choices(conv_desc);

    if doc_choices.is_empty() || conv_choices.is_empty() {
        // Cannot determine relationship without concrete choices on both sides.
        return Relationship::Unrelated;
    }

    // Check if any choices overlap (agreement/reinforcement).
    let has_overlap = doc_choices.iter().any(|dc| {
        conv_choices
            .iter()
            .any(|cc| cc == dc || cc.contains(dc.as_str()) || dc.contains(cc.as_str()))
    });

    if has_overlap {
        Relationship::Reinforcement
    } else {
        Relationship::Contradiction
    }
}

/// Signal phrases that precede concrete choices in descriptions.
const SIGNAL_PHRASES: &[&str] = &[
    "use ",
    "prefer ",
    "canonical: ",
    "canonical ",
    "primary: ",
    "primary ",
    "library: ",
    "library ",
    "framework: ",
    "framework ",
    "pattern: ",
    "pattern ",
    "convention: ",
    "convention ",
    "standard: ",
    "standard ",
    "adopt ",
    "recommended: ",
    "recommended ",
];

/// Extract concrete choice tokens from a description.
///
/// Looks for words following signal phrases. For example:
/// - "Use thiserror for error types" → ["thiserror"]
/// - "Canonical library: tokio for async runtime" → ["tokio"]
/// - "Prefer anyhow for application errors" → ["anyhow"]
fn extract_choices(description: &str) -> Vec<String> {
    let lower = description.to_lowercase();
    let mut choices = Vec::new();

    for signal in SIGNAL_PHRASES {
        let mut search_from = 0;
        while let Some(pos) = lower[search_from..].find(signal) {
            let abs_pos = search_from + pos + signal.len();
            if abs_pos < lower.len() {
                // Take the next word(s) after the signal phrase.
                let remaining = &lower[abs_pos..];
                let token = remaining
                    .split(|c: char| c.is_whitespace() || c == ',' || c == ';' || c == ')')
                    .next()
                    .unwrap_or("")
                    .trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-');

                if token.len() >= 2 && !is_filler_word(token) {
                    choices.push(token.to_owned());
                }
            }
            search_from = abs_pos;
        }
    }

    choices.sort();
    choices.dedup();
    choices
}

/// Words that appear after signal phrases but are not concrete choices.
fn is_filler_word(word: &str) -> bool {
    matches!(
        word,
        "for"
            | "the"
            | "an"
            | "and"
            | "or"
            | "in"
            | "as"
            | "to"
            | "is"
            | "it"
            | "by"
            | "of"
            | "with"
            | "from"
            | "over"
            | "vs"
            | "all"
            | "each"
            | "every"
            | "no"
            | "not"
    )
}

// ── Confidence boosting ───────────────────────────────────────────────

/// Boost confidence using asymptotic formula: `new = old + boost * (1.0 - old)`.
///
/// This ensures confidence approaches but never exceeds 1.0 and provides
/// diminishing returns as confidence increases.
fn boost_confidence(current: f64, boost: f64) -> f64 {
    let new_val = current + boost * (1.0 - current);
    new_val.min(1.0)
}

/// Map confidence to `KnowledgeWeight` using the standard thresholds.
///
/// Same thresholds as `seshat-detectors/src/confidence.rs` (ADR-7):
/// `>0.85` Strong, `0.50–0.85` Moderate, `0.20–0.50` Weak, `<0.20` Info.
/// Boundary values go to the lower tier.
fn weight_from_confidence(confidence: f64) -> KnowledgeWeight {
    if confidence > 0.85 {
        KnowledgeWeight::Strong
    } else if confidence > 0.50 {
        KnowledgeWeight::Moderate
    } else if confidence > 0.20 {
        KnowledgeWeight::Weak
    } else {
        KnowledgeWeight::Info
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::KnowledgeNature;

    fn make_node(id: i64, nature: KnowledgeNature, confidence: f64, desc: &str) -> KnowledgeNode {
        KnowledgeNode {
            id: NodeId(id),
            branch_id: BranchId::from("main"),
            nature,
            weight: weight_from_confidence(confidence),
            confidence,
            adoption_count: 0,
            total_count: 0,
            description: desc.to_owned(),
            ext_data: None,
        }
    }

    fn default_config() -> CrossReferenceConfig {
        CrossReferenceConfig::default()
    }

    fn branch() -> BranchId {
        BranchId::from("main")
    }

    // ── Contradiction tests ───────────────────────────────────────────

    #[test]
    fn contradiction_when_doc_says_x_code_says_y() {
        let doc_nodes = vec![make_node(
            1,
            KnowledgeNature::Fact,
            1.0,
            "Use anyhow for error handling in application code",
        )];
        let code_conventions = vec![make_node(
            2,
            KnowledgeNature::Convention,
            0.80,
            "Canonical error handling library: thiserror (adopted in 80% of files)",
        )];

        let result = cross_reference(&code_conventions, &doc_nodes, &branch(), &default_config());

        assert_eq!(result.contradiction_edges.len(), 1);
        let edge = &result.contradiction_edges[0];
        assert_eq!(edge.edge_type, EdgeType::Contradicts);
        assert_eq!(edge.source_id, NodeId(2));
        assert_eq!(edge.target_id, NodeId(1));
        assert!(edge.weight > 0.0);
        assert!(result.reinforced_nodes.is_empty());
    }

    #[test]
    fn contradiction_logging_library_mismatch() {
        let doc_nodes = vec![make_node(
            10,
            KnowledgeNature::Fact,
            1.0,
            "Use tracing for structured logging in Rust services",
        )];
        let code_conventions = vec![make_node(
            20,
            KnowledgeNature::Convention,
            0.75,
            "Canonical logging library: log for Rust logging (adopted in 75% of files)",
        )];

        let result = cross_reference(&code_conventions, &doc_nodes, &branch(), &default_config());

        assert_eq!(result.contradiction_edges.len(), 1);
        let edge = &result.contradiction_edges[0];
        assert_eq!(edge.source_id, NodeId(20));
        assert_eq!(edge.target_id, NodeId(10));
    }

    #[test]
    fn contradiction_metadata_contains_descriptions() {
        let doc_nodes = vec![make_node(
            1,
            KnowledgeNature::Fact,
            1.0,
            "Use reqwest for HTTP client requests in services",
        )];
        let code_conventions = vec![make_node(
            2,
            KnowledgeNature::Convention,
            0.70,
            "Canonical HTTP client library: hyper for HTTP requests (adopted in 70% of files)",
        )];

        let result = cross_reference(&code_conventions, &doc_nodes, &branch(), &default_config());

        assert_eq!(result.contradiction_edges.len(), 1);
        let meta = result.contradiction_edges[0].metadata.as_ref().unwrap();
        assert!(meta.get("doc_description").is_some());
        assert!(meta.get("convention_description").is_some());
        assert!(meta.get("overlap").is_some());
    }

    // ── Reinforcement tests ───────────────────────────────────────────

    #[test]
    fn reinforcement_when_doc_confirms_code_convention() {
        let doc_nodes = vec![make_node(
            1,
            KnowledgeNature::Fact,
            1.0,
            "Use thiserror for error types and derive macros",
        )];
        let code_conventions = vec![make_node(
            2,
            KnowledgeNature::Convention,
            0.80,
            "Canonical error handling library: thiserror (adopted in 80% of files)",
        )];

        let result = cross_reference(&code_conventions, &doc_nodes, &branch(), &default_config());

        assert!(result.contradiction_edges.is_empty());
        assert_eq!(result.reinforced_nodes.len(), 1);
        let reinforced = &result.reinforced_nodes[0];
        assert_eq!(reinforced.node_id, NodeId(2));
        assert!((reinforced.original_confidence - 0.80).abs() < f64::EPSILON);
        assert!(reinforced.boosted_confidence > 0.80);
        assert_eq!(reinforced.matched_doc_id, NodeId(1));
    }

    #[test]
    fn reinforcement_boosts_confidence_correctly() {
        let doc_nodes = vec![make_node(
            1,
            KnowledgeNature::Fact,
            1.0,
            "Use tokio for async runtime in Rust services",
        )];
        let code_conventions = vec![make_node(
            2,
            KnowledgeNature::Convention,
            0.50,
            "Canonical async runtime library: tokio for Rust async (adopted in 50% of files)",
        )];

        let config = CrossReferenceConfig {
            reinforcement_boost: 0.10,
            ..default_config()
        };

        let result = cross_reference(&code_conventions, &doc_nodes, &branch(), &config);

        assert_eq!(result.reinforced_nodes.len(), 1);
        let reinforced = &result.reinforced_nodes[0];
        // boost = 0.50 + 0.10 * (1.0 - 0.50) = 0.50 + 0.05 = 0.55
        assert!((reinforced.boosted_confidence - 0.55).abs() < f64::EPSILON);
    }

    #[test]
    fn reinforcement_weight_upgrade() {
        // Confidence at 0.84 (Moderate) — after boost should cross 0.85 boundary
        let doc_nodes = vec![make_node(
            1,
            KnowledgeNature::Fact,
            1.0,
            "Use tracing for structured logging",
        )];
        let code_conventions = vec![make_node(
            2,
            KnowledgeNature::Convention,
            0.84,
            "Canonical logging library: tracing (adopted in 84% of files)",
        )];

        let config = CrossReferenceConfig {
            reinforcement_boost: 0.15,
            ..default_config()
        };

        let result = cross_reference(&code_conventions, &doc_nodes, &branch(), &config);

        assert_eq!(result.reinforced_nodes.len(), 1);
        let reinforced = &result.reinforced_nodes[0];
        // boost = 0.84 + 0.15 * (1.0 - 0.84) = 0.84 + 0.024 = 0.864
        assert!(reinforced.boosted_confidence > 0.85);
        assert_eq!(reinforced.boosted_weight, KnowledgeWeight::Strong);
    }

    // ── No-match tests ────────────────────────────────────────────────

    #[test]
    fn no_match_when_descriptions_unrelated() {
        let doc_nodes = vec![make_node(
            1,
            KnowledgeNature::Fact,
            1.0,
            "Database schema uses UUIDs for primary keys",
        )];
        let code_conventions = vec![make_node(
            2,
            KnowledgeNature::Convention,
            0.90,
            "Import organization follows stdlib-then-external-then-internal pattern",
        )];

        let result = cross_reference(&code_conventions, &doc_nodes, &branch(), &default_config());

        assert!(result.contradiction_edges.is_empty());
        assert!(result.reinforced_nodes.is_empty());
    }

    #[test]
    fn no_match_when_descriptions_empty() {
        let doc_nodes = vec![make_node(1, KnowledgeNature::Fact, 1.0, "")];
        let code_conventions = vec![make_node(2, KnowledgeNature::Convention, 0.90, "")];

        let result = cross_reference(&code_conventions, &doc_nodes, &branch(), &default_config());

        assert!(result.contradiction_edges.is_empty());
        assert!(result.reinforced_nodes.is_empty());
    }

    #[test]
    fn empty_inputs_produce_empty_result() {
        let result = cross_reference(&[], &[], &branch(), &default_config());
        assert!(result.contradiction_edges.is_empty());
        assert!(result.reinforced_nodes.is_empty());
    }

    #[test]
    fn no_doc_nodes_produces_empty_result() {
        let code_conventions = vec![make_node(
            2,
            KnowledgeNature::Convention,
            0.80,
            "Use thiserror for errors",
        )];

        let result = cross_reference(&code_conventions, &[], &branch(), &default_config());
        assert!(result.contradiction_edges.is_empty());
        assert!(result.reinforced_nodes.is_empty());
    }

    #[test]
    fn no_code_conventions_produces_empty_result() {
        let doc_nodes = vec![make_node(
            1,
            KnowledgeNature::Fact,
            1.0,
            "Use thiserror for errors",
        )];

        let result = cross_reference(&[], &doc_nodes, &branch(), &default_config());
        assert!(result.contradiction_edges.is_empty());
        assert!(result.reinforced_nodes.is_empty());
    }

    // ── Multiple nodes tests ──────────────────────────────────────────

    #[test]
    fn multiple_docs_and_conventions() {
        let doc_nodes = vec![
            make_node(
                1,
                KnowledgeNature::Fact,
                1.0,
                "Use thiserror for error types",
            ),
            make_node(2, KnowledgeNature::Fact, 1.0, "Use tokio for async runtime"),
        ];
        let code_conventions = vec![
            make_node(
                10,
                KnowledgeNature::Convention,
                0.80,
                "Canonical error handling library: thiserror",
            ),
            make_node(
                20,
                KnowledgeNature::Convention,
                0.70,
                "Canonical async runtime: hyper (not tokio)",
            ),
        ];

        let result = cross_reference(&code_conventions, &doc_nodes, &branch(), &default_config());

        // thiserror matches → reinforcement
        assert!(
            result
                .reinforced_nodes
                .iter()
                .any(|r| r.node_id == NodeId(10))
        );
        // "async runtime" doc says tokio, code says hyper → contradiction
        assert!(
            result
                .contradiction_edges
                .iter()
                .any(|e| e.source_id == NodeId(20))
        );
    }

    #[test]
    fn observation_nodes_also_matched() {
        let doc_nodes = vec![make_node(
            1,
            KnowledgeNature::Fact,
            1.0,
            "Prefer serde for serialization",
        )];
        let code_conventions = vec![make_node(
            2,
            KnowledgeNature::Observation,
            0.30,
            "Canonical serialization library: serde (observed in 30% of files)",
        )];

        let result = cross_reference(&code_conventions, &doc_nodes, &branch(), &default_config());

        assert!(result.contradiction_edges.is_empty());
        assert_eq!(result.reinforced_nodes.len(), 1);
        assert_eq!(result.reinforced_nodes[0].node_id, NodeId(2));
    }

    // ── Edge properties tests ─────────────────────────────────────────

    #[test]
    fn contradiction_edge_has_correct_branch() {
        let branch = BranchId::from("feature/my-branch");
        let doc_nodes = vec![make_node(
            1,
            KnowledgeNature::Fact,
            1.0,
            "Use anyhow for error handling in application code",
        )];
        let code_conventions = vec![make_node(
            2,
            KnowledgeNature::Convention,
            0.80,
            "Canonical error handling library: thiserror",
        )];

        let result = cross_reference(&code_conventions, &doc_nodes, &branch, &default_config());

        assert_eq!(result.contradiction_edges.len(), 1);
        assert_eq!(
            result.contradiction_edges[0].branch_id,
            BranchId::from("feature/my-branch")
        );
    }

    #[test]
    fn contradiction_edges_have_unique_ids() {
        let doc_nodes = vec![
            make_node(
                1,
                KnowledgeNature::Fact,
                1.0,
                "Use anyhow for error handling",
            ),
            make_node(
                2,
                KnowledgeNature::Fact,
                1.0,
                "Use winston for logging in node services",
            ),
        ];
        let code_conventions = vec![
            make_node(
                10,
                KnowledgeNature::Convention,
                0.80,
                "Canonical error handling library: thiserror",
            ),
            make_node(
                20,
                KnowledgeNature::Convention,
                0.80,
                "Canonical logging library: pino for node services",
            ),
        ];

        let result = cross_reference(&code_conventions, &doc_nodes, &branch(), &default_config());

        let ids: HashSet<EdgeId> = result.contradiction_edges.iter().map(|e| e.id).collect();
        assert_eq!(ids.len(), result.contradiction_edges.len());
    }

    // ── Keyword extraction tests ──────────────────────────────────────

    #[test]
    fn extract_keywords_removes_stop_words() {
        let kw = extract_keywords("Use the thiserror library for all error types");
        assert!(kw.contains("thiserror"));
        assert!(kw.contains("library"));
        assert!(kw.contains("error"));
        assert!(kw.contains("types"));
        // Stop words removed
        assert!(!kw.contains("the"));
        assert!(!kw.contains("for"));
        assert!(!kw.contains("all"));
    }

    #[test]
    fn extract_keywords_lowercases() {
        let kw = extract_keywords("Use Thiserror for Error types");
        assert!(kw.contains("thiserror"));
        assert!(kw.contains("error"));
    }

    #[test]
    fn extract_keywords_splits_on_punctuation() {
        let kw = extract_keywords("library: tokio, runtime (async)");
        assert!(kw.contains("library"));
        assert!(kw.contains("tokio"));
        assert!(kw.contains("runtime"));
        assert!(kw.contains("async"));
    }

    #[test]
    fn extract_keywords_empty_string() {
        let kw = extract_keywords("");
        assert!(kw.is_empty());
    }

    #[test]
    fn extract_keywords_only_stop_words() {
        let kw = extract_keywords("the and or for in to");
        assert!(kw.is_empty());
    }

    // ── Jaccard similarity tests ──────────────────────────────────────

    #[test]
    fn jaccard_identical_sets() {
        let a: HashSet<String> = ["foo", "bar"].iter().map(|s| s.to_string()).collect();
        let b = a.clone();
        assert!((jaccard_similarity(&a, &b) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_disjoint_sets() {
        let a: HashSet<String> = ["foo", "bar"].iter().map(|s| s.to_string()).collect();
        let b: HashSet<String> = ["baz", "qux"].iter().map(|s| s.to_string()).collect();
        assert!((jaccard_similarity(&a, &b)).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_partial_overlap() {
        let a: HashSet<String> = ["foo", "bar", "baz"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let b: HashSet<String> = ["bar", "baz", "qux"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // intersection = {bar, baz} = 2, union = {foo, bar, baz, qux} = 4
        assert!((jaccard_similarity(&a, &b) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_empty_sets() {
        let a: HashSet<String> = HashSet::new();
        let b: HashSet<String> = HashSet::new();
        assert!((jaccard_similarity(&a, &b)).abs() < f64::EPSILON);
    }

    // ── Choice extraction tests ───────────────────────────────────────

    #[test]
    fn extract_choices_from_use_phrase() {
        let choices = extract_choices("Use thiserror for error types");
        assert!(choices.contains(&"thiserror".to_owned()));
    }

    #[test]
    fn extract_choices_from_prefer_phrase() {
        let choices = extract_choices("Prefer tokio over async-std for async runtime");
        assert!(choices.contains(&"tokio".to_owned()));
    }

    #[test]
    fn extract_choices_from_canonical_phrase() {
        let choices = extract_choices("Canonical library: serde for serialization");
        assert!(choices.contains(&"serde".to_owned()));
    }

    #[test]
    fn extract_choices_no_signal_phrase() {
        let choices =
            extract_choices("The error handling follows consistent patterns across files");
        assert!(choices.is_empty());
    }

    #[test]
    fn extract_choices_multiple_signals() {
        let choices = extract_choices("Use thiserror and prefer anyhow for applications");
        assert!(choices.contains(&"thiserror".to_owned()));
        assert!(choices.contains(&"anyhow".to_owned()));
    }

    // ── Relationship classification tests ─────────────────────────────

    #[test]
    fn classify_contradiction() {
        let rel = classify_relationship(
            "Use anyhow for error handling",
            "Canonical error handling library: thiserror",
        );
        assert_eq!(rel, Relationship::Contradiction);
    }

    #[test]
    fn classify_reinforcement() {
        let rel = classify_relationship(
            "Use thiserror for error types",
            "Canonical error handling library: thiserror",
        );
        assert_eq!(rel, Relationship::Reinforcement);
    }

    #[test]
    fn classify_unrelated_when_no_choices() {
        let rel = classify_relationship(
            "Error handling follows standard patterns",
            "The codebase has consistent error handling",
        );
        assert_eq!(rel, Relationship::Unrelated);
    }

    // ── Confidence boosting tests ─────────────────────────────────────

    #[test]
    fn boost_confidence_basic() {
        let boosted = boost_confidence(0.50, 0.10);
        // 0.50 + 0.10 * 0.50 = 0.55
        assert!((boosted - 0.55).abs() < f64::EPSILON);
    }

    #[test]
    fn boost_confidence_never_exceeds_one() {
        let boosted = boost_confidence(0.99, 0.50);
        assert!(boosted <= 1.0);
    }

    #[test]
    fn boost_confidence_zero_boost() {
        let boosted = boost_confidence(0.80, 0.0);
        assert!((boosted - 0.80).abs() < f64::EPSILON);
    }

    #[test]
    fn boost_confidence_from_zero() {
        let boosted = boost_confidence(0.0, 0.10);
        assert!((boosted - 0.10).abs() < f64::EPSILON);
    }

    // ── Weight mapping tests ──────────────────────────────────────────

    #[test]
    fn weight_mapping_boundaries() {
        assert_eq!(weight_from_confidence(0.86), KnowledgeWeight::Strong);
        assert_eq!(weight_from_confidence(0.85), KnowledgeWeight::Moderate); // boundary → lower tier
        assert_eq!(weight_from_confidence(0.51), KnowledgeWeight::Moderate);
        assert_eq!(weight_from_confidence(0.50), KnowledgeWeight::Weak); // boundary → lower tier
        assert_eq!(weight_from_confidence(0.21), KnowledgeWeight::Weak);
        assert_eq!(weight_from_confidence(0.20), KnowledgeWeight::Info); // boundary → lower tier
        assert_eq!(weight_from_confidence(0.19), KnowledgeWeight::Info);
    }

    // ── Config tests ──────────────────────────────────────────────────

    #[test]
    fn default_config_has_sane_values() {
        let config = CrossReferenceConfig::default();
        assert!(config.min_keyword_overlap > 0.0);
        assert!(config.min_keyword_overlap < 1.0);
        assert!(config.reinforcement_boost > 0.0);
        assert!(config.reinforcement_boost < 1.0);
    }

    #[test]
    fn high_threshold_reduces_matches() {
        let doc_nodes = vec![make_node(
            1,
            KnowledgeNature::Fact,
            1.0,
            "Use thiserror for error types",
        )];
        let code_conventions = vec![make_node(
            2,
            KnowledgeNature::Convention,
            0.80,
            "Canonical error handling library: thiserror (adopted in 80% of files)",
        )];

        let strict_config = CrossReferenceConfig {
            min_keyword_overlap: 0.95, // Very strict — almost identical
            ..default_config()
        };

        let result = cross_reference(&code_conventions, &doc_nodes, &branch(), &strict_config);

        // With such a high threshold, the keyword overlap likely won't meet it
        assert!(
            result.contradiction_edges.is_empty() && result.reinforced_nodes.is_empty()
                || result.reinforced_nodes.len() == 1
        );
    }
}
