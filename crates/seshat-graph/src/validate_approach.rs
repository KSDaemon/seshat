//! Graduated approach validation against the knowledge graph.
//!
//! Provides `validate_approach()` which checks a proposed approach against
//! rules, contradictions, duplicates, conventions, decisions, and observations.
//! Returns a graduated response with verdict, evidence gating, and actionable
//! suggestions.
//!
//! Reuses `query_code_pattern` for duplicate detection and optionally
//! `query_dependencies` for enriching `used_by` counts.

use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde::Serialize;
use seshat_core::CodeSnippet;

use crate::code_pattern::query_code_pattern;
use crate::conventions::{ConventionResult, QueryConventionData};
use crate::dependencies::query_dependencies;
use crate::error::GraphError;
use crate::{SQL_NOT_REMOVED, query_convention};

// ── Constants ────────────────────────────────────────────────

/// Minimum score from `query_code_pattern` to consider a pattern a duplicate.
const DUPLICATE_SCORE_THRESHOLD: f64 = 0.6;

/// Confidence threshold (as pct 0–100) below which conventions are considered stale/uncertain.
const LOW_CONFIDENCE_THRESHOLD_PCT: u32 = 50;

/// Maximum rules surfaced in a `validate_approach` response.
const MAX_RULES_RETURNED: usize = 10;

/// Maximum non-rule conventions surfaced.
const MAX_CONVENTIONS_RETURNED: usize = 10;

/// Maximum user-recorded decisions surfaced.
const MAX_DECISIONS_RETURNED: usize = 10;

/// Maximum low-confidence observations surfaced.
const MAX_OBSERVATIONS_RETURNED: usize = 5;

/// Maximum duplicate code patterns surfaced.
const MAX_DUPLICATES_RETURNED: usize = 10;

/// Maximum contradictions surfaced.
const MAX_CONTRADICTIONS_RETURNED: usize = 10;

/// Maximum evidence examples included per convention inside the
/// `validate_approach` response. `validate_approach` is a summary tool —
/// callers should hit `query_convention` for full evidence.
const MAX_EVIDENCE_PER_CONVENTION: usize = 1;

/// Minimum number of distinct significant tokens an approach description must
/// share with a `rule`-weighted convention before that rule is promoted to a
/// blocking `must_fix` violation.
///
/// FTS5 uses OR semantics, so a single incidental token overlap (e.g. an
/// unrelated migration rule matching a `map_diff_impact` task on the shared
/// token "impact") was enough to surface the rule and flip the verdict to
/// `rules_violated` / `ready: false`. A blocking red light must be *earned*:
/// requiring ≥2 shared discriminative tokens kills incidental matches while
/// keeping genuine same-domain rules (which always overlap on several terms).
/// Tuned conservatively — a missed soft rule is cheaper than a false block.
const MIN_RULE_RELEVANCE_TOKENS: usize = 2;

/// Common English stop-words plus high-frequency "code-prose" filler filtered
/// from keyword extraction.
///
/// Two reasons to drop a word here:
///
/// 1. **English connectives / determiners** (`the`, `and`, `of`, ...) — they
///    appear in virtually every sentence and inflate FTS5 OR-matching across
///    unrelated rules.
/// 2. **Code-prose filler** (`description`, `fix`, `function`, `value`, ...) —
///    nouns and verbs that appear in almost every technical description but
///    carry no domain signal. They were the actual root cause of phantom
///    `rules_violated` verdicts: a long bug-fix description sharing
///    `{description, fix, without}` with a completely unrelated breaking-changes
///    rule was enough to flip the verdict, because every overlap token cost the
///    same as a rare domain term.
///
/// The list is `pub(crate)` because the decision-side keyword search
/// (`search_decisions_by_topic`) shares the same filter.
pub(crate) const STOP_WORDS: &[&str] = &[
    // ── English connectives, determiners, pronouns, modals ───────
    "a",
    "an",
    "the",
    "and",
    "or",
    "but",
    "if",
    "of",
    "at",
    "by",
    "for",
    "with",
    "about",
    "against",
    "between",
    "into",
    "through",
    "during",
    "before",
    "after",
    "above",
    "below",
    "to",
    "from",
    "up",
    "down",
    "in",
    "out",
    "on",
    "off",
    "over",
    "under",
    "again",
    "further",
    "then",
    "once",
    "here",
    "there",
    "when",
    "where",
    "why",
    "how",
    "all",
    "both",
    "each",
    "few",
    "more",
    "most",
    "other",
    "some",
    "such",
    "no",
    "nor",
    "not",
    "only",
    "own",
    "same",
    "so",
    "than",
    "too",
    "very",
    "can",
    "will",
    "just",
    "should",
    "now",
    "also",
    "is",
    "are",
    "was",
    "were",
    "be",
    "been",
    "being",
    "have",
    "has",
    "had",
    "do",
    "does",
    "did",
    "would",
    "could",
    "may",
    "might",
    "shall",
    "as",
    "this",
    "that",
    "these",
    "those",
    "it",
    "its",
    "they",
    "them",
    "their",
    "he",
    "she",
    "his",
    "her",
    "we",
    "our",
    "you",
    "your",
    "which",
    "who",
    "whom",
    "whose",
    "else",
    "every",
    // ── Code-prose filler (verbs, nouns, modifiers that describe *anything*
    //    about code without carrying domain signal) ──────────────
    "description",
    "descriptions",
    "fix",
    "fixes",
    "fixed",
    "without",
    "function",
    "functions",
    "method",
    "methods",
    "class",
    "classes",
    "code",
    "name",
    "names",
    "value",
    "values",
    "return",
    "returns",
    "returned",
    "result",
    "results",
    "path",
    "paths",
    "line",
    "lines",
    "file",
    "files",
    "case",
    "cases",
    "kind",
    "kinds",
    "way",
    "ways",
    "thing",
    "things",
    "part",
    "parts",
    "point",
    "points",
    "side",
    "sides",
    "step",
    "steps",
    "item",
    "items",
    "entry",
    "entries",
    "add",
    "adds",
    "added",
    "remove",
    "removes",
    "removed",
    "change",
    "changes",
    "changed",
    "call",
    "calls",
    "called",
    "make",
    "makes",
    "made",
    "making",
    "set",
    "sets",
    "get",
    "gets",
    "use",
    "uses",
    "used",
    "using",
    "ensure",
    "ensures",
    "ensured",
    "handle",
    "handles",
    "handled",
    "emit",
    "emits",
    "emitted",
    "log",
    "logs",
    "logged",
    "logging",
    "raise",
    "raises",
    "raised",
    "throw",
    "throws",
    "thrown",
    "work",
    "works",
    "worked",
    "need",
    "needs",
    "needed",
    "want",
    "wants",
    "wanted",
    "like",
    "likely",
    "instead",
    "yet",
    "still",
    "already",
    "real",
    "actual",
    "actually",
    "new",
    "old",
    "simple",
    "simpler",
    "basic",
    "full",
    "fully",
    "partial",
    "partially",
    "complete",
    "completely",
    "various",
    "multiple",
    "single",
    "several",
    "current",
    "currently",
    "recent",
    "recently",
    "though",
    "although",
    "however",
    "otherwise",
    "hence",
    "thus",
    "therefore",
    "must",
    "etc",
    "per",
    "something",
    "anything",
    "everything",
    "nothing",
];

// ── Input parameters ─────────────────────────────────────────

/// Parameters for the `validate_approach` function.
#[derive(Debug, Clone)]
pub struct ValidateApproachParams {
    /// Description of the proposed approach.
    pub description: String,
    /// Optional file context for enriching results (e.g., used_by counts).
    pub file_context: Option<String>,
    /// Optional approach type for filtering (e.g., "refactor", "new_feature").
    ///
    /// Reserved for future use — currently accepted but not used in validation
    /// logic. Exposed via the MCP handler so callers can start passing it today
    /// without a breaking change when filtering is implemented.
    pub approach_type: Option<String>,
}

// ── Response data types ──────────────────────────────────────

/// Full response data for the `validate_approach` tool.
#[derive(Debug, Clone, Serialize)]
pub struct ValidateApproachData {
    /// Rules that the approach violates (weight = Rule).
    pub rules: Vec<RuleViolation>,
    /// Contradictions found in the knowledge graph (Contradicts edges).
    pub contradictions: Vec<Contradiction>,
    /// Potential duplicate code patterns (from IR search, score > 0.6).
    pub duplicates: Vec<DuplicatePattern>,
    /// Matching conventions from FTS5 search.
    pub conventions: Vec<ConventionResult>,
    /// User-recorded decisions relevant to the approach.
    pub decisions: Vec<DecisionEntry>,
    /// Low-confidence observations.
    pub observations: Vec<ObservationEntry>,
    /// Overall verdict.
    pub verdict: String,
    /// Whether the approach is ready to proceed.
    pub ready: bool,
    /// Suggestions when not ready.
    pub what_would_help: Vec<String>,
    /// Deterministic summary counting each section.
    pub summary: String,
    /// Whether the response was truncated for size — either because IR
    /// loading hit its limit during duplicate search, or because at least
    /// one of the response sections (rules / conventions / decisions /
    /// observations / duplicates / contradictions / per-convention
    /// evidence) was capped by `MAX_*_RETURNED`. Call `query_convention`
    /// or `query_code_pattern` directly to see the full set when this is
    /// `true`.
    #[serde(default)]
    pub truncated: bool,
}

/// A rule violation (conventions with weight = "rule").
#[derive(Debug, Clone, Serialize)]
pub struct RuleViolation {
    /// Description of the rule.
    pub description: String,
    /// Evidence snippet from the codebase, when the rule has an associated code
    /// example.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<CodeSnippet>,
    /// Severity is always "must_fix" for rules.
    pub severity: String,
}

/// A contradiction found via Contradicts edges in the graph.
#[derive(Debug, Clone, Serialize)]
pub struct Contradiction {
    /// The source node ID.
    pub source_id: i64,
    /// The target node ID.
    pub target_id: i64,
    /// Description of the source node.
    pub source_description: String,
    /// Description of the target node.
    pub target_description: String,
    /// Edge weight.
    pub weight: f64,
}

/// A potential duplicate pattern found via IR search.
#[derive(Debug, Clone, Serialize)]
pub struct DuplicatePattern {
    /// Name of the function, type, or export.
    pub name: String,
    /// File path where the pattern was found.
    pub file_path: String,
    /// Start line number.
    pub line: usize,
    /// Code snippet.
    pub snippet: CodeSnippet,
    /// Number of files that depend on (use) this pattern.
    pub used_by: usize,
}

/// A user-recorded decision relevant to the approach.
#[derive(Debug, Clone, Serialize)]
pub struct DecisionEntry {
    /// Description hash — the canonical identifier for a decision. Pass this to
    /// `update_decision` / `remove_decision` to modify or remove it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description_hash: String,
    /// Description of the decision.
    pub description: String,
    /// Weight of the decision.
    pub weight: String,
    /// Confidence score.
    pub confidence: f64,
    /// Source of the decision (user or auto_detected).
    pub source: String,
    /// Nature of the knowledge (always "decision" here).
    pub nature: String,
    /// Category for grouping (e.g., "naming", "error-handling").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

/// A low-confidence observation.
#[derive(Debug, Clone, Serialize)]
pub struct ObservationEntry {
    /// Node ID in the knowledge graph.
    /// Pass this value to `update_decision` or `remove_decision` to modify
    /// or remove this observation.
    pub id: i64,
    /// Description of the observation.
    pub description: String,
    /// Confidence score.
    pub confidence: f64,
    /// Source of the observation (user or auto_detected).
    pub source: String,
    /// Nature of the knowledge (always "observation" here).
    pub nature: String,
    /// Category for grouping (e.g., "naming", "error-handling").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

// ── Public API ───────────────────────────────────────────────

/// Validate a proposed approach against the knowledge graph.
///
/// Checks rules, contradictions, duplicates, conventions, decisions, and
/// observations. Returns a graduated response with verdict and evidence gating.
///
/// Returns `Err(GraphError::InvalidInput)` for empty descriptions.
pub fn validate_approach(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    params: ValidateApproachParams,
) -> Result<ValidateApproachData, GraphError> {
    let description = params.description.trim();
    if description.is_empty() {
        return Err(GraphError::InvalidInput(
            "description must not be empty".to_owned(),
        ));
    }

    // Track whether any section was capped or any evidence was trimmed.
    // Combined with `ir_truncated` from the duplicate search at the end.
    let mut response_truncated = false;

    // Single FTS5 pass — partition into mutually exclusive buckets so the
    // same node never appears in two sections (prior implementation called
    // `query_convention` 3× and let one node land in conventions + decisions
    // + observations, ballooning the payload).
    //
    // Precedence:
    //   1. weight == "rule"               → rules
    //   2. user_confirmed                 → decisions  (user knowledge wins)
    //   3. nature == "observation"        → observations
    //   4. otherwise                      → conventions
    //
    // Note: a user-confirmed `nature="observation"` row lands in `decisions`,
    // not `observations`, because rule (2) intentionally outranks rule (3) —
    // once the user has confirmed a row, it's settled project knowledge
    // regardless of its original nature.
    let all_conventions = query_convention(conn, branch_id, description).unwrap_or_else(|e| {
        tracing::warn!("Convention search failed in validate_approach: {e}");
        QueryConventionData {
            conventions: Vec::new(),
        }
    });

    // Relevance gate for `rule`-weighted conventions (see
    // `MIN_RULE_RELEVANCE_TOKENS` for the rationale): a rule blocks only when it
    // shares enough discriminative tokens with the approach description. Rules
    // that fail the gate are demoted into `other_convs` so the agent still sees
    // them as (non-blocking) conventions instead of being silently dropped.
    let description_tokens = significant_tokens(description);

    let mut rule_convs: Vec<ConventionResult> = Vec::new();
    let mut decision_convs: Vec<ConventionResult> = Vec::new();
    let mut observation_convs: Vec<ConventionResult> = Vec::new();
    let mut other_convs: Vec<ConventionResult> = Vec::new();
    for c in all_conventions.conventions {
        if c.weight == "rule" {
            if rule_is_relevant(&description_tokens, &c.description) {
                rule_convs.push(c);
            } else {
                other_convs.push(c);
            }
        } else if c.user_confirmed {
            decision_convs.push(c);
        } else if c.nature == "observation" {
            observation_convs.push(c);
        } else {
            other_convs.push(c);
        }
    }

    sort_by_confidence_desc(&mut rule_convs);
    sort_by_confidence_desc(&mut decision_convs);
    sort_by_confidence_desc(&mut observation_convs);
    sort_by_confidence_desc(&mut other_convs);

    // Capture the stale-evidence signal BEFORE capping. Otherwise the top-N
    // cap (sorted desc by confidence) can drop every stale row and flip
    // `has_stale_conventions` to false, which would silently flip `ready`
    // from `false` to `true` for a project with plenty of stale evidence.
    let has_stale_conventions = other_convs
        .iter()
        .any(|c| c.confidence_pct <= LOW_CONFIDENCE_THRESHOLD_PCT);

    response_truncated |= cap_to(&mut rule_convs, MAX_RULES_RETURNED);
    response_truncated |= cap_to(&mut decision_convs, MAX_DECISIONS_RETURNED);
    response_truncated |= cap_to(&mut observation_convs, MAX_OBSERVATIONS_RETURNED);
    response_truncated |= cap_to(&mut other_convs, MAX_CONVENTIONS_RETURNED);

    // Trim per-convention evidence — `validate_approach` is a summary tool.
    // `rule_convs` is intentionally excluded: `rules_from_conventions` only
    // surfaces the first example anyway, so trimming it here would flip
    // `response_truncated = true` for evidence that is never serialized.
    response_truncated |= trim_examples_per_convention(&mut decision_convs);
    response_truncated |= trim_examples_per_convention(&mut observation_convs);
    response_truncated |= trim_examples_per_convention(&mut other_convs);

    // Build typed sections from the partitioned buckets.
    let rules = rules_from_conventions(rule_convs);
    let conventions = other_convs;
    let decisions: Vec<DecisionEntry> = decision_convs
        .into_iter()
        .map(convention_to_decision_entry)
        .collect();
    let observations: Vec<ObservationEntry> = observation_convs
        .into_iter()
        .map(convention_to_observation_entry)
        .collect();

    // Contradictions: edges with type = "contradicts". Fail-soft (warn +
    // empty) — symmetric with the FTS path above. A transient SQLite error
    // here should not throw away successfully-fetched rules/conventions.
    let mut contradictions = match find_contradictions(conn, branch_id, description) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Contradiction search failed in validate_approach: {e}");
            response_truncated = true;
            Vec::new()
        }
    };
    response_truncated |= cap_to(&mut contradictions, MAX_CONTRADICTIONS_RETURNED);

    // Duplicates: reuse query_code_pattern for IR search, filter by score
    // threshold. Fail-soft as above.
    let (mut duplicates, ir_truncated) =
        match find_duplicates(conn, branch_id, description, params.file_context.as_deref()) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("Duplicate search failed in validate_approach: {e}");
                response_truncated = true;
                (Vec::new(), false)
            }
        };
    response_truncated |= cap_to(&mut duplicates, MAX_DUPLICATES_RETURNED);

    // Verdict logic
    let verdict = compute_verdict(&rules, &contradictions, &conventions);

    // Evidence gating (`has_stale_conventions` was captured pre-cap above).
    let ready = verdict != "rules_violated" && !has_stale_conventions;

    // what_would_help
    let what_would_help = build_what_would_help(
        &verdict,
        &rules,
        &contradictions,
        &conventions,
        has_stale_conventions,
    );

    // Summary
    let summary = build_summary(
        rules.len(),
        contradictions.len(),
        duplicates.len(),
        conventions.len(),
        decisions.len(),
        observations.len(),
        &verdict,
    );

    Ok(ValidateApproachData {
        rules,
        contradictions,
        duplicates,
        conventions,
        decisions,
        observations,
        verdict,
        ready,
        what_would_help,
        summary,
        truncated: response_truncated || ir_truncated,
    })
}

/// Cap a vector in place, returning `true` if any items were dropped.
fn cap_to<T>(items: &mut Vec<T>, max: usize) -> bool {
    if items.len() > max {
        items.truncate(max);
        true
    } else {
        false
    }
}

/// Sort conventions by descending confidence so the most authoritative
/// rows survive the per-section cap. Tied confidences are broken
/// deterministically by `description_hash` then `id`, so the cap drops
/// the same rows on every run instead of relying on FTS5 row order.
fn sort_by_confidence_desc(items: &mut [ConventionResult]) {
    items.sort_by(|a, b| {
        b.confidence_pct
            .cmp(&a.confidence_pct)
            .then_with(|| a.description_hash.cmp(&b.description_hash))
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// Trim per-convention evidence to `MAX_EVIDENCE_PER_CONVENTION`. Returns
/// `true` if any convention had evidence dropped.
fn trim_examples_per_convention(items: &mut [ConventionResult]) -> bool {
    let mut trimmed = false;
    for c in items.iter_mut() {
        if c.examples.len() > MAX_EVIDENCE_PER_CONVENTION {
            c.examples.truncate(MAX_EVIDENCE_PER_CONVENTION);
            trimmed = true;
        }
    }
    trimmed
}

fn convention_to_decision_entry(c: ConventionResult) -> DecisionEntry {
    DecisionEntry {
        description_hash: c.description_hash,
        description: c.description,
        weight: c.weight,
        confidence: c.confidence_pct as f64 / 100.0,
        source: c.source,
        nature: c.nature,
        category: c.category,
    }
}

fn convention_to_observation_entry(c: ConventionResult) -> ObservationEntry {
    ObservationEntry {
        id: c.id,
        description: c.description,
        confidence: c.confidence_pct as f64 / 100.0,
        source: c.source,
        nature: c.nature,
        category: c.category,
    }
}

// ── Internal helpers ─────────────────────────────────────────

/// Convert pre-filtered rule conventions into `RuleViolation` structs.
fn rules_from_conventions(rule_convs: Vec<ConventionResult>) -> Vec<RuleViolation> {
    rule_convs
        .into_iter()
        .map(|c| {
            // Only attach evidence when there is a non-empty snippet. Rules
            // without a code example omit the field rather than serializing an
            // empty `{content:"", truncated:false}` placeholder.
            let evidence = c
                .examples
                .first()
                .map(|ex| CodeSnippet {
                    content: ex.snippet.content.clone(),
                    truncated: ex.snippet.truncated,
                })
                .filter(|snippet| !snippet.content.is_empty());

            RuleViolation {
                description: c.description,
                evidence,
                severity: "must_fix".to_owned(),
            }
        })
        .collect()
}

/// Find contradictions from the edges table.
///
/// Batches all matching node IDs into a single SQL query (avoids N+1) and
/// normalises the dedup key so `(A,B)` and `(B,A)` are treated as the same edge.
fn find_contradictions(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    description: &str,
) -> Result<Vec<Contradiction>, GraphError> {
    let conn_guard = crate::lock_conn(conn)?;

    // Find nodes that match the description terms, then check for Contradicts edges.
    let node_ids = find_matching_node_ids(&conn_guard, branch_id, description)?;

    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Build a single batched query: WHERE … AND (source_id IN (?,?,..) OR target_id IN (?,?,..))
    let placeholders: Vec<String> = (0..node_ids.len()).map(|i| format!("?{}", i + 2)).collect();
    let in_list = placeholders.join(", ");
    let sql = format!(
        "SELECT e.source_id, e.target_id, e.weight,
                s.description, t.description
         FROM edges e
         JOIN nodes s ON s.id = e.source_id
         JOIN nodes t ON t.id = e.target_id
         WHERE e.edge_type = 'contradicts'
           AND e.branch_id = ?1
           AND (e.source_id IN ({in_list}) OR e.target_id IN ({in_list}))"
    );

    let mut stmt = conn_guard
        .prepare(&sql)
        .map_err(|e| GraphError::query(format!("Failed to prepare contradiction query: {e}")))?;

    // Bind: [branch_id, id1, id2, …]
    let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> =
        vec![Box::new(branch_id.to_owned())];
    for id in &node_ids {
        bind_values.push(Box::new(*id));
    }
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        bind_values.iter().map(|b| b.as_ref()).collect();

    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(Contradiction {
                source_id: row.get(0)?,
                target_id: row.get(1)?,
                weight: row.get(2)?,
                source_description: row.get(3)?,
                target_description: row.get(4)?,
            })
        })
        .map_err(|e| GraphError::query(format!("Failed to query contradictions: {e}")))?;

    let mut contradictions = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for row in rows {
        match row {
            Ok(contradiction) => {
                // Normalise the pair so (A,B) and (B,A) map to the same key.
                let lo = contradiction.source_id.min(contradiction.target_id);
                let hi = contradiction.source_id.max(contradiction.target_id);
                if seen.insert((lo, hi)) {
                    contradictions.push(contradiction);
                }
            }
            Err(e) => {
                tracing::warn!("Skipping contradiction row: {e}");
            }
        }
    }

    Ok(contradictions)
}

/// Extract significant keywords (len > 1, lowercased, non-stop-word) from a description.
///
/// Common English stop-words are filtered to prevent overly broad LIKE/FTS5
/// matches. Threshold is 2+ chars so short identifiers like "io", "fs", "db",
/// "id" are retained while single-char noise ("a", "I") is still excluded.
fn extract_keywords(description: &str) -> Vec<String> {
    description
        .split_whitespace()
        .filter(|w| w.len() > 1)
        .map(|w| w.to_lowercase())
        .filter(|w| !STOP_WORDS.contains(&w.as_str()))
        .collect()
}

/// Tokenize `text` into a set of distinct significant tokens for relevance
/// scoring. Unlike [`extract_keywords`], this splits on any non-alphanumeric
/// character so compound identifiers decompose the same way the FTS5 tokenizer
/// sees them (`map_diff_impact` → `map`, `diff`, `impact`; `blast_radius` →
/// `blast`, `radius`). Stop-words and single-char noise are dropped.
fn significant_tokens(text: &str) -> std::collections::HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1)
        // Require at least one letter so pure-numeric tokens ("123", "2024")
        // don't count as shared discriminative tokens for rule relevance.
        .filter(|w| w.chars().any(|c| c.is_alphabetic()))
        .map(|w| w.to_lowercase())
        .filter(|w| !STOP_WORDS.contains(&w.as_str()))
        .collect()
}

/// Decide whether a `rule`-weighted convention is relevant enough to the
/// proposed approach to be surfaced as a blocking `must_fix` violation.
///
/// Relevance = number of distinct significant tokens shared between the
/// approach description and the rule description. A rule blocks only when the
/// overlap reaches [`MIN_RULE_RELEVANCE_TOKENS`] (see that constant for the
/// rationale).
fn rule_is_relevant(
    description_tokens: &std::collections::HashSet<String>,
    rule_description: &str,
) -> bool {
    let rule_tokens = significant_tokens(rule_description);
    let shared = description_tokens.intersection(&rule_tokens).count();
    shared >= MIN_RULE_RELEVANCE_TOKENS
}

/// Decide whether a whitespace token looks like a code identifier
/// (snake_case, kebab-case, or camelCase/PascalCase) rather than a plain prose
/// word. Used to restrict duplicate detection to the concrete symbol names an
/// agent actually mentions.
fn is_identifier_like(token: &str) -> bool {
    if token.len() < 2 || !token.chars().any(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    let has_separator = token.contains('_') || token.contains('-');
    // camelCase / PascalCase: an uppercase letter after position 0 *and* at
    // least one lowercase letter. Requiring a lowercase letter excludes plain
    // prose acronyms like "HTTP" / "URL" (all-caps, no separator) while keeping
    // "handleRequest" / "BlastRadius".
    let is_camel_case = token.chars().skip(1).any(|c| c.is_ascii_uppercase())
        && token.chars().any(|c| c.is_ascii_lowercase());
    has_separator || is_camel_case
}

/// Extract identifier-like candidates from a free-text description for
/// duplicate detection. Surrounding punctuation is trimmed so `high,` or
/// `(map_diff_impact)` normalise to bare identifiers.
///
/// Why restrict: duplicate detection used to feed the *entire* description into
/// the symbol-name search, so every existing symbol that shared a common prose
/// word ("files", "check", "summary", "high") surfaced as a bogus duplicate. A
/// real duplicate signal is "you are about to create `X` but `X` already
/// exists" — which only makes sense for the concrete identifiers the agent
/// names, not for prose.
fn extract_identifier_candidates(description: &str) -> Vec<String> {
    // Case-insensitive dedup — `Foo` and `foo` would otherwise both survive and
    // produce two redundant searches that the symbol-name match normalises to
    // the same hit anyway.
    let mut seen = std::collections::HashSet::new();
    description
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-'))
        .filter(|w| is_identifier_like(w))
        .filter(|w| seen.insert(w.to_lowercase()))
        .map(str::to_owned)
        .collect()
}

/// Max number of LIKE keywords to use — capped to 5 longest (most discriminative).
const MAX_LIKE_KEYWORDS: usize = 5;

/// Build parameterized LIKE clauses and corresponding bind values using AND logic.
///
/// Keywords are capped at [`MAX_LIKE_KEYWORDS`] (5 longest) for tighter results.
/// Returns `(where_fragment, params)` where `where_fragment` is e.g.
/// `(LOWER(description) LIKE ?2 AND LOWER(description) LIKE ?3)` and `params`
/// are the `%keyword%` patterns. `param_offset` is the first `?N` index to use
/// (e.g. 2 when `?1` is already taken by `branch_id`).
fn build_keyword_like(keywords: &[String], param_offset: usize) -> (String, Vec<String>) {
    let mut sorted: Vec<&String> = keywords.iter().collect();
    sorted.sort_by_key(|k| std::cmp::Reverse(k.len()));
    sorted.truncate(MAX_LIKE_KEYWORDS);

    let clauses: Vec<String> = sorted
        .iter()
        .enumerate()
        .map(|(i, _)| format!("LOWER(description) LIKE ?{}", param_offset + i))
        .collect();
    let params: Vec<String> = sorted.iter().map(|k| format!("%{k}%")).collect();
    (clauses.join(" AND "), params)
}

/// Execute a keyword-based LIKE search on the `nodes` table with AND logic.
///
/// `columns` — the SELECT columns (e.g. `"id"` or `"id, description, weight, confidence"`).
/// `extra_where` — additional AND clause (e.g. `"AND nature = 'decision'"`) or empty string.
///
/// Keywords are capped at [`MAX_LIKE_KEYWORDS`] (5 longest) and results are
/// limited to 50 rows for performance. Uses parameterized queries for safety.
fn keyword_search_nodes<T, F>(
    conn_guard: &rusqlite::Connection,
    branch_id: &str,
    description: &str,
    columns: &str,
    extra_where: &str,
    context: &str,
    row_mapper: F,
) -> Result<Vec<T>, GraphError>
where
    F: Fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
{
    let keywords = extract_keywords(description);
    if keywords.is_empty() {
        return Ok(Vec::new());
    }

    let (like_where, like_params) = build_keyword_like(&keywords, 2);

    let sql = format!(
        "SELECT {columns} FROM nodes WHERE branch_id = ?1 AND ({like_where}) {extra_where} AND {SQL_NOT_REMOVED} LIMIT 50"
    );

    let mut stmt = conn_guard
        .prepare(&sql)
        .map_err(|e| GraphError::query(format!("Failed to prepare {context} query: {e}")))?;

    // Build dynamic params: [branch_id, "%kw1%", "%kw2%", ...]
    let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> =
        vec![Box::new(branch_id.to_owned())];
    for p in &like_params {
        bind_values.push(Box::new(p.clone()));
    }
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        bind_values.iter().map(|b| b.as_ref()).collect();

    let rows = stmt
        .query_map(param_refs.as_slice(), &row_mapper)
        .map_err(|e| GraphError::query(format!("Failed to query {context}: {e}")))?;

    let mut results = Vec::new();
    for row in rows {
        match row {
            Ok(item) => results.push(item),
            Err(e) => tracing::warn!("Skipping {context} row: {e}"),
        }
    }

    Ok(results)
}

/// Find matching node IDs by checking if description keywords appear in node descriptions.
fn find_matching_node_ids(
    conn_guard: &rusqlite::Connection,
    branch_id: &str,
    description: &str,
) -> Result<Vec<i64>, GraphError> {
    keyword_search_nodes(
        conn_guard,
        branch_id,
        description,
        "id",
        "",
        "matching nodes",
        |row| row.get::<_, i64>(0),
    )
}

/// Find potential duplicates using `query_code_pattern`.
fn find_duplicates(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    description: &str,
    file_context: Option<&str>,
) -> Result<(Vec<DuplicatePattern>, bool), GraphError> {
    // Duplicate detection only makes sense for the concrete identifiers the
    // agent names (e.g. `map_diff_impact`, `BlastRadius`), not for the prose
    // words of the description. Feeding the whole sentence made every symbol
    // sharing a common word surface as a bogus duplicate. If the description
    // contains no identifier-like tokens, there is nothing to dedup against.
    let candidates = extract_identifier_candidates(description);
    if candidates.is_empty() {
        return Ok((Vec::new(), false));
    }
    let query = candidates.join(" ");

    // No kind filter — duplicate detection wants matches across function /
    // type / export alike.
    let pattern_data = match query_code_pattern(conn, branch_id, &query, None) {
        Ok(data) => data,
        Err(e) => {
            tracing::warn!("Code pattern search failed in validate_approach: {e}");
            return Ok((Vec::new(), false));
        }
    };

    let truncated = pattern_data.truncated;

    // Filter by score threshold and convert to DuplicatePattern.
    let mut duplicates: Vec<DuplicatePattern> = pattern_data
        .patterns
        .into_iter()
        .filter(|p| p.score >= DUPLICATE_SCORE_THRESHOLD)
        .map(|p| DuplicatePattern {
            name: p.name.clone(),
            file_path: p.file_path.clone(),
            line: p.line,
            snippet: p.snippet,
            used_by: 0,
        })
        .collect();

    // Enrich used_by counts only when caller provides file_context.
    //
    // Why conditional: each duplicate requires a full `query_dependencies` call
    // which loads ALL IR for the branch (O(files) per duplicate). For D duplicates
    // this is O(D × files) — prohibitively expensive without explicit opt-in.
    // When file_context is absent, used_by stays at 0.
    if file_context.is_some() {
        enrich_used_by(conn, branch_id, &mut duplicates);
    }

    Ok((duplicates, truncated))
}

/// Enrich `used_by` counts for duplicate patterns by querying dependencies.
fn enrich_used_by(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    duplicates: &mut [DuplicatePattern],
) {
    for dup in duplicates.iter_mut() {
        match query_dependencies(
            conn,
            branch_id,
            &dup.file_path,
            crate::dependencies::QueryDependenciesOptions::default(),
        ) {
            Ok(dep_data) => {
                dup.used_by = dep_data.dependents.len();
            }
            Err(e) => {
                tracing::debug!("Could not get dependency info for {}: {e}", dup.file_path);
            }
        }
    }
}

/// Compute the verdict based on findings.
///
/// - `rules_violated`: any rules found
/// - `warnings_found`: contradictions or high-weight (strong) conventions
/// - `info_only`: some findings but nothing critical
/// - `approved`: nothing matches
fn compute_verdict(
    rules: &[RuleViolation],
    contradictions: &[Contradiction],
    conventions: &[ConventionResult],
) -> String {
    if !rules.is_empty() {
        return "rules_violated".to_owned();
    }

    let has_strong_conventions = conventions.iter().any(|c| c.weight == "strong");
    if !contradictions.is_empty() || has_strong_conventions {
        return "warnings_found".to_owned();
    }

    if !conventions.is_empty() {
        return "info_only".to_owned();
    }

    "approved".to_owned()
}

/// Build actionable suggestions when the approach is not ready.
fn build_what_would_help(
    verdict: &str,
    rules: &[RuleViolation],
    contradictions: &[Contradiction],
    conventions: &[ConventionResult],
    has_stale_conventions: bool,
) -> Vec<String> {
    let mut suggestions = Vec::new();

    if verdict == "rules_violated" {
        // Rule descriptions are intentionally NOT echoed here — they are
        // already returned verbatim in `rules[].description`.
        suggestions.push(format!(
            "Fix {} rule violation(s) before proceeding — see `rules[]`",
            rules.len()
        ));
    }

    if !contradictions.is_empty() {
        suggestions.push(format!(
            "Resolve {} contradiction(s) in the knowledge graph",
            contradictions.len()
        ));
    }

    if has_stale_conventions {
        let stale_count = conventions
            .iter()
            .filter(|c| c.confidence_pct <= LOW_CONFIDENCE_THRESHOLD_PCT)
            .count();
        suggestions.push(format!(
            "Review {} convention(s) with low confidence (<{}%) — they may be outdated",
            stale_count, LOW_CONFIDENCE_THRESHOLD_PCT
        ));
    }

    suggestions
}

/// Build a deterministic summary counting each section.
fn build_summary(
    rules: usize,
    contradictions: usize,
    duplicates: usize,
    conventions: usize,
    decisions: usize,
    observations: usize,
    verdict: &str,
) -> String {
    format!(
        "Verdict: {verdict}. Found {rules} rule(s), {contradictions} contradiction(s), \
         {duplicates} duplicate(s), {conventions} convention(s), {decisions} decision(s), \
         {observations} observation(s)."
    )
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashSet;
    use std::path::PathBuf;

    use rusqlite::params;
    use seshat_core::{
        Export, Function, Language, LanguageIR, ProjectFile, RustIR, TypeDef, TypeDefKind,
    };

    use crate::test_helpers::{insert_convention_node, insert_ir, test_conn};

    /// Helper: create a sample ProjectFile.
    fn sample_project_file(path: &str) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::Rust,
            content_hash: "abc123".to_owned(),
            imports: Vec::new(),
            exports: vec![Export {
                name: "handle_error".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
                end_line: 1,
            }],
            functions: vec![Function {
                name: "handle_error".to_owned(),
                is_public: true,
                is_async: false,
                line: 10,
                end_line: 50,
                parameters: vec!["err".to_owned()],
                doc_comment: None,
            }],
            types: vec![TypeDef {
                name: "ErrorHandler".to_owned(),
                kind: TypeDefKind::Struct,
                is_public: true,
                line: 5,
                end_line: 5,
                doc_comment: None,
            }],
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
            file_doc: None,
        }
    }

    /// Alias for convenience — delegates to shared test_helpers.
    fn insert_convention(
        conn: &Arc<Mutex<Connection>>,
        branch_id: &str,
        description: &str,
        weight: &str,
        confidence: f64,
        nature: &str,
    ) -> i64 {
        insert_convention_node(conn, branch_id, description, weight, confidence, nature)
    }

    /// Helper: insert a contradicts edge between two nodes.
    fn insert_contradiction_edge(
        conn: &Arc<Mutex<Connection>>,
        branch_id: &str,
        source_id: i64,
        target_id: i64,
    ) {
        let c = conn.lock().unwrap();
        c.execute(
            "INSERT INTO edges (source_id, target_id, edge_type, branch_id, weight)
             VALUES (?1, ?2, 'contradicts', ?3, 1.0)",
            params![source_id, target_id, branch_id],
        )
        .unwrap();
    }

    #[test]
    fn approach_matching_rule_returns_rules_violated() {
        let conn = test_conn();

        // Insert a rule-weight convention. Use terms that will match the query via FTS5.
        insert_convention(
            &conn,
            "main",
            "Always use thiserror for error types",
            "rule",
            1.0,
            "convention",
        );
        crate::fts::rebuild_fts_index(&conn).unwrap();

        // Insert IR so code pattern search works.
        let file = sample_project_file("src/errors.rs");
        insert_ir(&conn, "main", &file);

        // Use terms that overlap with the rule description so FTS5 can find it.
        // FTS5 uses AND semantics — all tokens must be present.
        let params = ValidateApproachParams {
            description: "thiserror error types".to_owned(),
            file_context: None,
            approach_type: None,
        };

        let result = validate_approach(&conn, "main", params).unwrap();

        assert_eq!(result.verdict, "rules_violated");
        assert!(!result.ready);
        assert!(!result.rules.is_empty());
        assert_eq!(result.rules[0].severity, "must_fix");
        assert!(!result.what_would_help.is_empty());
    }

    #[test]
    fn significant_tokens_splits_compound_identifiers() {
        let toks = significant_tokens("map_diff_impact blast_radius");
        assert!(toks.contains("map"));
        assert!(toks.contains("diff"));
        assert!(toks.contains("impact"));
        assert!(toks.contains("blast"));
        assert!(toks.contains("radius"));
        // Stop-words, single-char noise, and pure-numeric tokens are excluded.
        let toks = significant_tokens("verify a check to the diff in 2024 v2");
        assert!(!toks.contains("a"));
        assert!(!toks.contains("to"));
        assert!(!toks.contains("the"));
        assert!(!toks.contains("2024")); // pure numeric dropped
        assert!(toks.contains("verify"));
        assert!(toks.contains("check"));
        assert!(toks.contains("diff"));
        assert!(toks.contains("v2")); // alphanumeric kept
    }

    #[test]
    fn rule_is_relevant_requires_multiple_shared_tokens() {
        // Same-domain approach shares several discriminative tokens -> relevant.
        let desc = significant_tokens("validate input parameters before persisting");
        assert!(rule_is_relevant(
            &desc,
            "Always validate input parameters strictly"
        ));

        // Unrelated rule shares only ONE incidental token ("breaking") -> not
        // relevant, must not block.
        let desc = significant_tokens("add a breaking change to the diff renderer");
        assert!(!rule_is_relevant(
            &desc,
            "Database schema migrations must be marked as breaking changes"
        ));

        // Sharing only stop-words -> zero significant overlap -> not relevant.
        let desc = significant_tokens("render the diff before the report");
        assert!(!rule_is_relevant(
            &desc,
            "Migrations must run before the deploy completes"
        ));
    }

    #[test]
    fn code_prose_stop_words_are_filtered() {
        // The code-prose section of STOP_WORDS catches generic verbs and
        // nouns that show up in every technical description. None of these
        // should survive tokenization.
        let toks = significant_tokens(
            "Fix the function so its return value handles the case without panicking",
        );
        for w in [
            "fix", "function", "return", "value", "case", "without", "handles",
        ] {
            assert!(
                !toks.contains(w),
                "{w:?} must be filtered as code-prose stop-word; got {toks:?}"
            );
        }
        // Domain-specific tokens still survive.
        assert!(toks.contains("panicking"));
    }

    #[test]
    fn long_prose_does_not_phantom_match_unrelated_rule() {
        // Production reproducer (2026-05-27): a 600-char bug-fix description
        // about `find_duplicates` accidentally triggered an unrelated
        // commit-message rule via the shared `{description, fix, without}`
        // overlap. With the code-prose stop-words extension, none of those
        // tokens survive tokenization, so the gate stays closed.
        let prose = significant_tokens(
            "In crates/seshat-graph/src/validate_approach.rs, the find_duplicates \
             function passes the full description string to query_code_pattern \
             without significant token filtering. This silently triggers FTS5 \
             OR-expansion across all stop-words present in the prose, returning \
             irrelevant matches when descriptions contain phrases like the \
             catch-all, above, below, this, that etc. Fix: route the description \
             through extract_identifier_candidates first and only query symbols \
             whose names match. The duplicate-detection branch above already \
             handles the explicit name case; this prose branch is the one \
             polluting the results.",
        );
        let rule = "DB schema migrations and dropped read sites MUST be marked \
                    breaking in the commit message itself, not just in \
                    CHANGELOG.md. Use either `feat!:` / `fix!:` in the subject \
                    (exclamation before colon) OR a `BREAKING CHANGE: <description>` \
                    footer in the body. release-plz / git-cliff only inspect commit \
                    messages — text inside CHANGELOG.md is invisible to the bump \
                    algorithm.";
        assert!(
            !rule_is_relevant(&prose, rule),
            "long code-prose description must not phantom-match an unrelated rule"
        );
    }

    #[test]
    fn genuine_domain_overlap_still_fires() {
        // Inverse of the reproducer: an approach that genuinely shares
        // domain-specific tokens with a rule should still surface as
        // relevant. Tokens like `rusqlite`, `connection`, `prepared`,
        // `idempotent` are NOT in any stop-list and accumulate honest
        // overlap.
        let prose = significant_tokens(
            "Use rusqlite Connection with prepared statements for idempotent \
             inserts in the storage layer",
        );
        let rule = "Database access goes through rusqlite Connection — prefer \
                    prepared statements; idempotent writes use ON CONFLICT DO \
                    NOTHING";
        assert!(
            rule_is_relevant(&prose, rule),
            "genuine same-domain overlap must still fire the rule gate"
        );
    }

    #[test]
    fn incidental_rule_overlap_does_not_block_verdict() {
        let conn = test_conn();

        // A user-recorded RULE about code documentation comments. The OR-based
        // decision search surfaces it because it shares the single significant
        // token "documentation" with the approach, but the relevance gate must
        // demote it (needs >=2 shared tokens) so the verdict is NOT
        // rules_violated.
        crate::decisions::record_decision(
            &conn,
            "main",
            crate::decisions::RecordDecisionParams {
                description: "All public functions must have documentation comments".to_owned(),
                nature: "convention".to_owned(),
                weight: "rule".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();
        crate::fts::rebuild_fts_index(&conn).unwrap();

        let file = sample_project_file("src/web.rs");
        insert_ir(&conn, "main", &file);

        // "add a documentation page to the website" shares only "documentation".
        let params = ValidateApproachParams {
            description: "add a documentation page to the website".to_owned(),
            file_context: None,
            approach_type: None,
        };

        let result = validate_approach(&conn, "main", params).unwrap();

        assert_ne!(
            result.verdict, "rules_violated",
            "incidental overlap must not flip the verdict to rules_violated"
        );
        assert!(
            result.rules.is_empty(),
            "irrelevant rule must not be surfaced as a must_fix violation"
        );
        assert!(
            result.ready,
            "approach should be ready despite the unrelated rule"
        );
        // The demoted rule is still visible to the agent as a (non-blocking) convention.
        assert!(
            result.conventions.iter().any(|c| c.weight == "rule"),
            "demoted rule should appear in conventions, not be dropped"
        );
    }

    #[test]
    fn approach_with_duplicates_populates_duplicates() {
        let conn = test_conn();

        // Insert an IR file with a function named "handle_error".
        let file = sample_project_file("src/errors.rs");
        insert_ir(&conn, "main", &file);

        let params = ValidateApproachParams {
            description: "handle_error".to_owned(),
            file_context: Some("src/errors.rs".to_owned()),
            approach_type: None,
        };

        let result = validate_approach(&conn, "main", params).unwrap();

        // Should find "handle_error" as a duplicate (exact match score = 1.0 > 0.6).
        assert!(!result.duplicates.is_empty());
        assert!(result.duplicates.iter().any(|d| d.name == "handle_error"));
    }

    #[test]
    fn is_identifier_like_distinguishes_code_from_prose() {
        assert!(is_identifier_like("map_diff_impact"));
        assert!(is_identifier_like("blast_radius"));
        assert!(is_identifier_like("BlastRadius"));
        assert!(is_identifier_like("handleRequest"));
        assert!(is_identifier_like("convention-risk"));
        // SCREAMING_SNAKE constants carry a separator -> still identifiers.
        assert!(is_identifier_like("MAX_SIZE"));
        // Plain prose words (incl. sentence-case) are NOT identifiers.
        assert!(!is_identifier_like("add"));
        assert!(!is_identifier_like("Add"));
        assert!(!is_identifier_like("convention"));
        assert!(!is_identifier_like("files"));
        assert!(!is_identifier_like("a"));
        // All-caps prose acronyms are NOT identifiers (no separator, no lowercase).
        assert!(!is_identifier_like("HTTP"));
        assert!(!is_identifier_like("URL"));
        // Degenerate separator-only / non-alphanumeric tokens are excluded.
        assert!(!is_identifier_like("__"));
    }

    #[test]
    fn extract_identifier_candidates_trims_punctuation() {
        let c = extract_identifier_candidates(
            "flags whose blast_radius is high, see (map_diff_impact) summary",
        );
        assert!(c.contains(&"blast_radius".to_owned()));
        assert!(c.contains(&"map_diff_impact".to_owned()));
        // Prose words excluded.
        assert!(
            !c.iter()
                .any(|w| w == "whose" || w == "summary" || w == "high")
        );

        // Case-insensitive de-dup: `FooBar` and `fooBar` collapse to one entry.
        let c = extract_identifier_candidates("call FooBar then fooBar again");
        let hits = c
            .iter()
            .filter(|w| w.eq_ignore_ascii_case("FooBar"))
            .count();
        assert_eq!(hits, 1, "case-variant duplicates must collapse: {c:?}");
    }

    #[test]
    fn duplicate_detection_ignores_prose_only_descriptions() {
        let conn = test_conn();
        // Fixture defines handle_error / ErrorHandler.
        let file = sample_project_file("src/errors.rs");
        insert_ir(&conn, "main", &file);

        // Prose-only description: words like "errors"/"handle" appear, but none
        // are identifier-like, so the old code would substring-match symbols
        // while the new code surfaces no bogus duplicates.
        let params = ValidateApproachParams {
            description: "improve the way we report errors back to the user".to_owned(),
            file_context: None,
            approach_type: None,
        };

        let result = validate_approach(&conn, "main", params).unwrap();
        assert!(
            result.duplicates.is_empty(),
            "prose words must not surface bogus duplicates, got: {:?}",
            result
                .duplicates
                .iter()
                .map(|d| &d.name)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn clean_approach_returns_approved_and_ready() {
        let conn = test_conn();

        // Insert IR so queries don't fail.
        let file = sample_project_file("src/utils.rs");
        insert_ir(&conn, "main", &file);

        let params = ValidateApproachParams {
            description: "add new widget component zzz_unique".to_owned(),
            file_context: None,
            approach_type: None,
        };

        let result = validate_approach(&conn, "main", params).unwrap();

        assert_eq!(result.verdict, "approved");
        assert!(result.ready);
        assert!(result.rules.is_empty());
        assert!(result.contradictions.is_empty());
        assert!(result.what_would_help.is_empty());
    }

    #[test]
    fn evidence_gating_with_stale_conventions() {
        let conn = test_conn();

        // Insert a convention with low confidence. Use distinctive terms.
        insert_convention(
            &conn,
            "main",
            "camelCase variable naming",
            "moderate",
            0.3, // Below LOW_CONFIDENCE_THRESHOLD (0.5)
            "convention",
        );
        crate::fts::rebuild_fts_index(&conn).unwrap();

        // Insert IR.
        let file = sample_project_file("src/naming.rs");
        insert_ir(&conn, "main", &file);

        // FTS5 AND semantics: all tokens must match.
        let params = ValidateApproachParams {
            description: "camelCase variable naming".to_owned(),
            file_context: None,
            approach_type: None,
        };

        let result = validate_approach(&conn, "main", params).unwrap();

        // Should not be ready because of low-confidence convention.
        assert!(!result.ready);
        assert!(
            result
                .what_would_help
                .iter()
                .any(|s| s.contains("low confidence"))
        );
    }

    #[test]
    fn what_would_help_populated_when_not_ready() {
        let conn = test_conn();

        insert_convention(
            &conn,
            "main",
            "validate input parameters",
            "rule",
            1.0,
            "convention",
        );
        crate::fts::rebuild_fts_index(&conn).unwrap();

        let file = sample_project_file("src/validation.rs");
        insert_ir(&conn, "main", &file);

        // Use terms matching the rule description for FTS5 to find it.
        let params = ValidateApproachParams {
            description: "validate input parameters".to_owned(),
            file_context: None,
            approach_type: None,
        };

        let result = validate_approach(&conn, "main", params).unwrap();

        assert_eq!(result.verdict, "rules_violated");
        assert!(!result.ready);
        assert!(!result.what_would_help.is_empty());
        assert!(
            result
                .what_would_help
                .iter()
                .any(|s| s.contains("rule violation"))
        );
    }

    #[test]
    fn empty_description_returns_error() {
        let conn = test_conn();

        let params = ValidateApproachParams {
            description: "".to_owned(),
            file_context: None,
            approach_type: None,
        };

        let result = validate_approach(&conn, "main", params);
        assert!(result.is_err());
        match result {
            Err(GraphError::InvalidInput(msg)) => {
                assert!(msg.contains("empty"));
            }
            other => panic!("Expected InvalidInput, got: {other:?}"),
        }
    }

    #[test]
    fn contradictions_detected_from_edges() {
        let conn = test_conn();

        // Insert two nodes that contradict each other.
        let node_a = insert_convention(
            &conn,
            "main",
            "Use REST for API design patterns",
            "strong",
            0.9,
            "convention",
        );
        let node_b = insert_convention(
            &conn,
            "main",
            "Use GraphQL for API design patterns",
            "strong",
            0.8,
            "convention",
        );
        insert_contradiction_edge(&conn, "main", node_a, node_b);
        crate::fts::rebuild_fts_index(&conn).unwrap();

        let file = sample_project_file("src/api.rs");
        insert_ir(&conn, "main", &file);

        let params = ValidateApproachParams {
            description: "API design patterns".to_owned(),
            file_context: None,
            approach_type: None,
        };

        let result = validate_approach(&conn, "main", params).unwrap();

        assert!(!result.contradictions.is_empty());
        assert_eq!(result.verdict, "warnings_found");
    }

    #[test]
    fn decisions_found_when_matching() {
        let conn = test_conn();

        insert_convention(
            &conn,
            "main",
            "Use SQLite for storage backend",
            "strong",
            1.0,
            "decision",
        );
        crate::fts::rebuild_fts_index(&conn).unwrap();

        let file = sample_project_file("src/storage.rs");
        insert_ir(&conn, "main", &file);

        let params = ValidateApproachParams {
            description: "SQLite storage backend".to_owned(),
            file_context: None,
            approach_type: None,
        };

        let result = validate_approach(&conn, "main", params).unwrap();

        assert!(!result.decisions.is_empty());
        assert!(
            result
                .decisions
                .iter()
                .any(|d| d.description.contains("SQLite"))
        );
    }

    #[test]
    fn observations_found_when_matching() {
        let conn = test_conn();

        insert_convention(
            &conn,
            "main",
            "Some files use logging pattern with tracing crate",
            "weak",
            0.3,
            "observation",
        );
        crate::fts::rebuild_fts_index(&conn).unwrap();

        let file = sample_project_file("src/logging.rs");
        insert_ir(&conn, "main", &file);

        let params = ValidateApproachParams {
            description: "logging tracing".to_owned(),
            file_context: None,
            approach_type: None,
        };

        let result = validate_approach(&conn, "main", params).unwrap();

        assert!(!result.observations.is_empty());
        assert!(
            result
                .observations
                .iter()
                .any(|o| o.description.contains("tracing"))
        );
    }

    #[test]
    fn summary_counts_all_sections() {
        let summary = build_summary(2, 1, 3, 4, 1, 2, "rules_violated");
        assert!(summary.contains("2 rule(s)"));
        assert!(summary.contains("1 contradiction(s)"));
        assert!(summary.contains("3 duplicate(s)"));
        assert!(summary.contains("4 convention(s)"));
        assert!(summary.contains("1 decision(s)"));
        assert!(summary.contains("2 observation(s)"));
        assert!(summary.contains("rules_violated"));
    }

    #[test]
    fn verdict_logic_approved_when_empty() {
        let verdict = compute_verdict(&[], &[], &[]);
        assert_eq!(verdict, "approved");
    }

    #[test]
    fn stale_threshold_boundary_at_0_495_is_stale() {
        // confidence=0.495 → rounds to 50 → 50 <= 50 → stale.
        // This documents the intentional <= semantics: when rounding pushes
        // a value exactly to the threshold it is considered stale, preserving
        // the spirit of the original f64 check (0.495 < 0.5 → stale).
        let conn = test_conn();

        insert_convention_node(
            &conn,
            "main",
            "Low confidence convention",
            "strong",
            0.495,
            "convention",
        );
        crate::fts::rebuild_fts_index(&conn).unwrap();

        let result = validate_approach(
            &conn,
            "main",
            ValidateApproachParams {
                description: "low confidence convention".to_owned(),
                file_context: None,
                approach_type: None,
            },
        )
        .unwrap();
        // Convention with confidence_pct=50 (rounded from 0.495) should be stale → not ready.
        assert!(
            !result.ready,
            "confidence_pct=50 should be stale (<=50 threshold)"
        );
    }

    #[test]
    fn stale_threshold_boundary_at_0_51_is_not_stale() {
        // confidence=0.51 → rounds to 51 → 51 <= 50 is false → not stale.
        let conn = test_conn();

        insert_convention_node(
            &conn,
            "main",
            "Slightly above threshold convention",
            "strong",
            0.51,
            "convention",
        );
        crate::fts::rebuild_fts_index(&conn).unwrap();

        let result = validate_approach(
            &conn,
            "main",
            ValidateApproachParams {
                description: "slightly above threshold convention".to_owned(),
                file_context: None,
                approach_type: None,
            },
        )
        .unwrap();
        assert!(
            result.ready,
            "confidence_pct=51 should not be stale (>50 threshold)"
        );
    }

    #[test]
    fn response_capped_when_many_matching_conventions() {
        // Insert 2 × MAX_CONVENTIONS_RETURNED matching conventions so the
        // unbounded FTS5 result must be capped on the way out.
        let conn = test_conn();
        let total = MAX_CONVENTIONS_RETURNED * 2;
        for i in 0..total {
            insert_convention_node(
                &conn,
                "main",
                &format!("retry backoff policy #{i}"),
                "moderate",
                0.8,
                "convention",
            );
        }
        crate::fts::rebuild_fts_index(&conn).unwrap();

        let result = validate_approach(
            &conn,
            "main",
            ValidateApproachParams {
                description: "retry backoff policy".to_owned(),
                file_context: None,
                approach_type: None,
            },
        )
        .unwrap();

        assert!(
            result.conventions.len() <= MAX_CONVENTIONS_RETURNED,
            "conventions section must respect MAX_CONVENTIONS_RETURNED cap"
        );
        assert!(
            result.truncated,
            "truncated must be true when the cap is hit"
        );
    }

    #[test]
    fn convention_evidence_trimmed_to_one_example() {
        // A single auto-detected convention with many evidence rows in
        // ext_data — `validate_approach` must surface at most one example.
        let conn = test_conn();
        let many_evidence: Vec<serde_json::Value> = (0..5)
            .map(|i| {
                serde_json::json!({
                    "file": format!("src/file_{i}.rs"),
                    "line": 10,
                    "end_line": 12,
                    "snippet": format!("example {i}")
                })
            })
            .collect();
        let ext = serde_json::json!({
            "source": "auto_detected",
            "detector_name": "test",
            "trend": "stable",
            "evidence": many_evidence,
        });
        {
            let c = conn.lock().unwrap();
            c.execute(
                "INSERT INTO nodes (branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data)
                 VALUES (?1, 'convention', 'moderate', 0.9, 9, 10, ?2, ?3)",
                params!["main", "evidence trim probe unique zzz", ext.to_string()],
            )
            .unwrap();
        }
        crate::fts::rebuild_fts_index(&conn).unwrap();

        let result = validate_approach(
            &conn,
            "main",
            ValidateApproachParams {
                description: "evidence trim probe unique zzz".to_owned(),
                file_context: None,
                approach_type: None,
            },
        )
        .unwrap();

        let conv = result
            .conventions
            .iter()
            .find(|c| c.description.contains("evidence trim probe"))
            .expect("expected the probe convention to be returned");
        assert!(
            conv.examples.len() <= MAX_EVIDENCE_PER_CONVENTION,
            "expected ≤ {MAX_EVIDENCE_PER_CONVENTION} example(s), got {}",
            conv.examples.len()
        );
        assert!(
            result.truncated,
            "truncated must be true when evidence is trimmed"
        );
    }

    #[test]
    fn convention_appears_in_only_one_section() {
        // A user-confirmed `nature="decision"` row used to appear in BOTH
        // `decisions` and `conventions`. With strict precedence it must
        // appear in `decisions` only.
        let conn = test_conn();
        insert_convention_node(
            &conn,
            "main",
            "always use serde_json for json parsing",
            "strong",
            0.95,
            "decision",
        );
        crate::fts::rebuild_fts_index(&conn).unwrap();

        let result = validate_approach(
            &conn,
            "main",
            ValidateApproachParams {
                description: "serde_json json parsing".to_owned(),
                file_context: None,
                approach_type: None,
            },
        )
        .unwrap();

        let matches_decision = result
            .decisions
            .iter()
            .any(|d| d.description.contains("serde_json"));
        let matches_convention = result
            .conventions
            .iter()
            .any(|c| c.description.contains("serde_json"));
        assert!(
            matches_decision,
            "user-confirmed row must land in decisions"
        );
        assert!(
            !matches_convention,
            "user-confirmed row must NOT also appear in conventions (no overlap)"
        );
    }

    #[test]
    fn sections_remain_disjoint_with_many_mixed_candidates() {
        // Insert several rows of each partition class matching the same FTS
        // term. Each row must land in exactly one section.
        let conn = test_conn();
        let term = "partition_probe_xyz";
        // 3 rules
        for i in 0..3 {
            insert_convention_node(
                &conn,
                "main",
                &format!("{term} rule #{i}"),
                "rule",
                0.9,
                "convention",
            );
        }
        // 3 user-confirmed (decisions)
        for i in 0..3 {
            insert_convention_node(
                &conn,
                "main",
                &format!("{term} decision #{i}"),
                "strong",
                0.9,
                "decision",
            );
        }
        // 3 low-confidence observations
        for i in 0..3 {
            insert_convention_node(
                &conn,
                "main",
                &format!("{term} observation #{i}"),
                "weak",
                0.3,
                "observation",
            );
        }
        // 3 plain conventions
        for i in 0..3 {
            insert_convention_node(
                &conn,
                "main",
                &format!("{term} convention #{i}"),
                "moderate",
                0.8,
                "convention",
            );
        }
        crate::fts::rebuild_fts_index(&conn).unwrap();

        let result = validate_approach(
            &conn,
            "main",
            ValidateApproachParams {
                description: term.to_owned(),
                file_context: None,
                approach_type: None,
            },
        )
        .unwrap();

        // Collect descriptions per section.
        let in_rules: HashSet<&str> = result
            .rules
            .iter()
            .map(|r| r.description.as_str())
            .collect();
        let in_decisions: HashSet<&str> = result
            .decisions
            .iter()
            .map(|d| d.description.as_str())
            .collect();
        let in_observations: HashSet<&str> = result
            .observations
            .iter()
            .map(|o| o.description.as_str())
            .collect();
        let in_conventions: HashSet<&str> = result
            .conventions
            .iter()
            .map(|c| c.description.as_str())
            .collect();

        // Pairwise disjoint.
        for (a_name, a) in [
            ("rules", &in_rules),
            ("decisions", &in_decisions),
            ("observations", &in_observations),
            ("conventions", &in_conventions),
        ] {
            for (b_name, b) in [
                ("rules", &in_rules),
                ("decisions", &in_decisions),
                ("observations", &in_observations),
                ("conventions", &in_conventions),
            ] {
                if a_name == b_name {
                    continue;
                }
                let overlap: Vec<&&str> = a.intersection(b).collect();
                assert!(
                    overlap.is_empty(),
                    "{a_name} and {b_name} must be disjoint, overlap: {overlap:?}",
                );
            }
        }
    }

    #[test]
    fn stale_conventions_dropped_by_cap_still_flip_ready_to_false() {
        // Regression: `has_stale_conventions` used to be computed on the
        // POST-cap slice, so a project full of stale low-confidence rows
        // would silently flip `ready=true` once the cap dropped them all.
        // The fix captures the stale signal BEFORE capping.
        let conn = test_conn();
        let term = "stale_pre_cap_probe";

        // MAX_CONVENTIONS_RETURNED high-confidence rows that will fill the cap.
        for i in 0..MAX_CONVENTIONS_RETURNED {
            insert_convention_node(
                &conn,
                "main",
                &format!("{term} high #{i}"),
                "moderate",
                0.9,
                "convention",
            );
        }
        // One additional stale row that will get dropped by the cap.
        insert_convention_node(
            &conn,
            "main",
            &format!("{term} stale"),
            "moderate",
            0.3,
            "convention",
        );
        crate::fts::rebuild_fts_index(&conn).unwrap();

        let result = validate_approach(
            &conn,
            "main",
            ValidateApproachParams {
                description: term.to_owned(),
                file_context: None,
                approach_type: None,
            },
        )
        .unwrap();

        // Cap is enforced — the stale row was dropped from the returned slice.
        assert_eq!(result.conventions.len(), MAX_CONVENTIONS_RETURNED);
        assert!(
            result
                .conventions
                .iter()
                .all(|c| c.confidence_pct > LOW_CONFIDENCE_THRESHOLD_PCT),
            "no stale rows should survive the confidence-desc cap"
        );
        // Despite the stale row being capped away, the gating decision was
        // taken on the PRE-cap partition — so `ready` must still be false.
        assert!(
            !result.ready,
            "ready must be false when stale conventions exist (even if cap dropped them)"
        );
    }

    #[test]
    fn verdict_logic_info_only_with_moderate_conventions() {
        // A convention with weight "moderate" should give info_only.
        let conv = ConventionResult {
            id: 42,
            description_hash: String::new(),
            nature: "convention".to_owned(),
            weight: "moderate".to_owned(),
            confidence_pct: 70,
            adoption: crate::conventions::AdoptionInfo {
                count: 7,
                total: 10,
                rate_pct: 70,
            },
            trend: "stable".to_owned(),
            description: "Test convention".to_owned(),
            source: "auto_detected".to_owned(),
            user_confirmed: false,
            category: None,
            state: None,
            reason: None,
            examples: vec![],
        };

        let verdict = compute_verdict(&[], &[], &[conv]);
        assert_eq!(verdict, "info_only");
    }
}
