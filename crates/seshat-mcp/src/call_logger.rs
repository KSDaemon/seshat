//! Opt-in JSONL call logger for MCP tool calls.
//!
//! Records every MCP tool call with full input parameters, response summary
//! metrics, duration, and status. Entries are written as newline-delimited
//! JSON (JSONL) for easy analysis.

use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use serde::Serialize;

use crate::call_logger_keys;

// ── Call log entry ───────────────────────────────────────────

/// A single MCP tool call log entry.
///
/// Serializes to a flat JSON object suitable for JSONL output.
/// Optional fields (`result`, `error_code`) are omitted when `None`.
#[derive(Debug, Serialize)]
pub struct CallLogEntry {
    /// ISO 8601 UTC timestamp (e.g. `"2026-04-04T15:47:22Z"`).
    pub ts: String,
    /// 8-character lowercase hex session identifier.
    pub session: String,
    /// Monotonically increasing sequence number within the session.
    pub seq: u64,
    /// Tool name (e.g. `"query_convention"`).
    pub tool: String,
    /// Full input parameters as a JSON value.
    pub input: serde_json::Value,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// `"ok"` on success, `"error"` on failure.
    pub status: String,
    /// Tool-specific result summary scalars (present on success).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Error code string representation (present on error).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

// ── Result summary constructors ─────────────────────────────

/// Build a result summary for `query_project_context`.
///
/// Extracts `language_count`, `convention_count`, and `golden_file_count`
/// from the serialized response data.
pub fn project_context_result(response_data: &serde_json::Value) -> serde_json::Value {
    let language_count = response_data
        .get(call_logger_keys::project_context::DATA_LANGUAGES)
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or_else(|| {
            tracing::debug!(
                "project_context_result: key '{}' missing or not an array",
                call_logger_keys::project_context::DATA_LANGUAGES
            );
            0
        });

    let convention_count = response_data
        .get(call_logger_keys::project_context::DATA_CONVENTIONS_COUNT)
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| {
            tracing::debug!(
                "project_context_result: key '{}' missing or not a u64",
                call_logger_keys::project_context::DATA_CONVENTIONS_COUNT
            );
            0
        });

    let golden_file_count = response_data
        .get(call_logger_keys::project_context::DATA_GOLDEN_FILES)
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or_else(|| {
            tracing::debug!(
                "project_context_result: key '{}' missing or not an array",
                call_logger_keys::project_context::DATA_GOLDEN_FILES
            );
            0
        });

    serde_json::json!({
        "language_count": language_count,
        "convention_count": convention_count,
        "golden_file_count": golden_file_count,
    })
}

/// Build a result summary for `query_convention`.
///
/// Extracts `convention_count` and `decision_count` from the serialized
/// response data.
pub fn query_convention_result(response_data: &serde_json::Value) -> serde_json::Value {
    let conventions = response_data
        .get(call_logger_keys::query_convention::DATA_CONVENTIONS)
        .and_then(|v| v.as_array());

    let total = conventions.map(|a| a.len()).unwrap_or_else(|| {
        tracing::debug!(
            "query_convention_result: key '{}' missing or not an array",
            call_logger_keys::query_convention::DATA_CONVENTIONS
        );
        0
    });

    let decision_count = conventions
        .map(|arr| {
            arr.iter()
                .filter(|c| {
                    c.get(call_logger_keys::query_convention::CONVENTION_SOURCE)
                        .and_then(|s| s.as_str())
                        == Some("user")
                })
                .count()
        })
        .unwrap_or(0);

    serde_json::json!({
        "convention_count": total,
        "decision_count": decision_count,
    })
}

/// Build a result summary for `query_code_pattern`.
///
/// Counts patterns and related conventions directly from the response data arrays.
pub fn code_pattern_result(response_data: &serde_json::Value) -> serde_json::Value {
    let pattern_count = response_data
        .get(call_logger_keys::code_pattern::DATA_PATTERNS)
        .and_then(|v| v.as_array())
        .map(|a| a.len() as u64)
        .unwrap_or_else(|| {
            tracing::debug!(
                "code_pattern_result: key '{}' missing or not an array",
                call_logger_keys::code_pattern::DATA_PATTERNS
            );
            0
        });

    let convention_count = response_data
        .get(call_logger_keys::code_pattern::DATA_RELATED_CONVENTIONS)
        .and_then(|v| v.as_array())
        .map(|a| a.len() as u64)
        .unwrap_or_else(|| {
            tracing::debug!(
                "code_pattern_result: key '{}' missing or not an array",
                call_logger_keys::code_pattern::DATA_RELATED_CONVENTIONS
            );
            0
        });

    serde_json::json!({
        "pattern_count": pattern_count,
        "convention_count": convention_count,
    })
}

/// Build a result summary for `query_dependencies`.
///
/// Extracts `dependent_count`, `dependency_count`, `blast_radius`,
/// `transitive_dependent_count`, and `requested_depth` from the
/// serialized response data. `dependent_count` reflects only direct
/// dependents (the length of the `dependents` array, which is also the
/// direct count when `requested_depth == 1`); `transitive_dependent_count`
/// captures the full BFS total surfaced by the graph layer.
pub fn dependencies_result(response_data: &serde_json::Value) -> serde_json::Value {
    let dependent_count = response_data
        .get(call_logger_keys::dependencies::DATA_DEPENDENTS)
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or_else(|| {
            tracing::debug!(
                "dependencies_result: key '{}' missing or not an array",
                call_logger_keys::dependencies::DATA_DEPENDENTS
            );
            0
        });

    let dependency_count = response_data
        .get(call_logger_keys::dependencies::DATA_DEPENDENCIES)
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or_else(|| {
            tracing::debug!(
                "dependencies_result: key '{}' missing or not an array",
                call_logger_keys::dependencies::DATA_DEPENDENCIES
            );
            0
        });

    let blast_radius = response_data
        .get(call_logger_keys::dependencies::DATA_BLAST_RADIUS)
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| {
            tracing::debug!(
                "dependencies_result: key '{}' missing or not a string",
                call_logger_keys::dependencies::DATA_BLAST_RADIUS
            );
            "unknown"
        });

    let transitive_dependent_count = response_data
        .get(call_logger_keys::dependencies::DATA_TRANSITIVE_DEPENDENT_COUNT)
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| {
            tracing::debug!(
                "dependencies_result: key '{}' missing or not a u64",
                call_logger_keys::dependencies::DATA_TRANSITIVE_DEPENDENT_COUNT
            );
            0
        });

    let requested_depth = response_data
        .get(call_logger_keys::dependencies::DATA_REQUESTED_DEPTH)
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| {
            tracing::debug!(
                "dependencies_result: key '{}' missing or not a u64",
                call_logger_keys::dependencies::DATA_REQUESTED_DEPTH
            );
            0
        });

    serde_json::json!({
        "dependent_count": dependent_count,
        "dependency_count": dependency_count,
        "blast_radius": blast_radius,
        "transitive_dependent_count": transitive_dependent_count,
        "requested_depth": requested_depth,
    })
}

/// Build a result summary for `validate_approach`.
///
/// Extracts `verdict`, `rule_count`, `duplicate_count`, `convention_count`,
/// and `ready` from the serialized response data.
pub fn validate_approach_result(response_data: &serde_json::Value) -> serde_json::Value {
    let verdict = response_data
        .get(call_logger_keys::validate_approach::DATA_VERDICT)
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| {
            tracing::debug!(
                "validate_approach_result: key '{}' missing or not a string",
                call_logger_keys::validate_approach::DATA_VERDICT
            );
            "unknown"
        });

    let rule_count = response_data
        .get(call_logger_keys::validate_approach::DATA_RULES)
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or_else(|| {
            tracing::debug!(
                "validate_approach_result: key '{}' missing or not an array",
                call_logger_keys::validate_approach::DATA_RULES
            );
            0
        });

    let duplicate_count = response_data
        .get(call_logger_keys::validate_approach::DATA_DUPLICATES)
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or_else(|| {
            tracing::debug!(
                "validate_approach_result: key '{}' missing or not an array",
                call_logger_keys::validate_approach::DATA_DUPLICATES
            );
            0
        });

    let convention_count = response_data
        .get(call_logger_keys::validate_approach::DATA_CONVENTIONS)
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or_else(|| {
            tracing::debug!(
                "validate_approach_result: key '{}' missing or not an array",
                call_logger_keys::validate_approach::DATA_CONVENTIONS
            );
            0
        });

    let ready = response_data
        .get(call_logger_keys::validate_approach::DATA_READY)
        .and_then(|v| v.as_bool())
        .unwrap_or_else(|| {
            tracing::debug!(
                "validate_approach_result: key '{}' missing or not a bool",
                call_logger_keys::validate_approach::DATA_READY
            );
            false
        });

    serde_json::json!({
        "verdict": verdict,
        "rule_count": rule_count,
        "duplicate_count": duplicate_count,
        "convention_count": convention_count,
        "ready": ready,
    })
}

/// Build a result summary for any decision mutation tool
/// (`record_decision`, `update_decision`, `remove_decision`).
pub fn decision_result(description_hash: &str) -> serde_json::Value {
    serde_json::json!({ "description_hash": description_hash })
}

/// Build a result summary for `map_diff_impact`.
///
/// Extracts `changed_file_count`, `affected_symbol_count`,
/// `convention_risk_count`, `blast_radius`, and `total_hunks` from the
/// serialized response data. `total_hunks` is the count of distinct hunks
/// observed across every analyzable changed file (a binary or oversized
/// blob contributes a single sentinel hunk); it captures the content-level
/// noise of the diff independently of how many symbols were touched.
pub fn diff_impact_result(response_data: &serde_json::Value) -> serde_json::Value {
    let changed_file_count = response_data
        .get(call_logger_keys::diff_impact::DATA_CHANGED_FILES)
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or_else(|| {
            tracing::debug!(
                "diff_impact_result: key '{}' missing or not an array",
                call_logger_keys::diff_impact::DATA_CHANGED_FILES
            );
            0
        });

    let affected_symbol_count = response_data
        .get(call_logger_keys::diff_impact::DATA_AFFECTED_SYMBOLS)
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or_else(|| {
            tracing::debug!(
                "diff_impact_result: key '{}' missing or not an array",
                call_logger_keys::diff_impact::DATA_AFFECTED_SYMBOLS
            );
            0
        });

    let convention_risk_count = response_data
        .get(call_logger_keys::diff_impact::DATA_CONVENTION_RISKS)
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or_else(|| {
            tracing::debug!(
                "diff_impact_result: key '{}' missing or not an array",
                call_logger_keys::diff_impact::DATA_CONVENTION_RISKS
            );
            0
        });

    let blast_radius = response_data
        .get(call_logger_keys::diff_impact::DATA_BLAST_RADIUS_SUMMARY)
        .and_then(|v| v.get(call_logger_keys::diff_impact::BLAST_RADIUS_RISK))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| {
            tracing::debug!(
                "diff_impact_result: key '{}.{}' missing or not a string",
                call_logger_keys::diff_impact::DATA_BLAST_RADIUS_SUMMARY,
                call_logger_keys::diff_impact::BLAST_RADIUS_RISK
            );
            "none"
        });

    let total_hunks = response_data
        .get(call_logger_keys::diff_impact::DATA_TOTAL_HUNKS)
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| {
            tracing::debug!(
                "diff_impact_result: key '{}' missing or not a u64",
                call_logger_keys::diff_impact::DATA_TOTAL_HUNKS
            );
            0
        });

    serde_json::json!({
        "changed_file_count": changed_file_count,
        "affected_symbol_count": affected_symbol_count,
        "convention_risk_count": convention_risk_count,
        "blast_radius": blast_radius,
        "total_hunks": total_hunks,
    })
}

// ── Call logger ──────────────────────────────────────────────

/// Append-only JSONL file writer with session identification and sequence
/// numbering.
///
/// Each `CallLogger` instance represents a single session. Log entries are
/// written as newline-delimited JSON, one object per line.
pub struct CallLogger {
    writer: Mutex<BufWriter<File>>,
    session_id: String,
    seq: AtomicU64,
}

impl CallLogger {
    /// Create a new `CallLogger` that appends to the file at `path`.
    ///
    /// Creates parent directories if they do not exist. The file is opened
    /// in append mode so existing content is preserved across restarts.
    pub fn new(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new().create(true).append(true).open(path)?;

        let session_id = generate_session_id();

        Ok(Self {
            writer: Mutex::new(BufWriter::new(file)),
            session_id,
            seq: AtomicU64::new(0),
        })
    }

    /// Return the session identifier for this logger instance.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Return the next sequence number, starting at 0.
    pub fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Serialize `entry` as JSON and write it as a single line to the log
    /// file, followed by a newline. The buffer is flushed immediately.
    pub fn log_call(&self, entry: &CallLogEntry) -> io::Result<()> {
        let line = serde_json::to_string(entry)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        let mut writer = self.writer.lock().expect("call-log mutex poisoned");
        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()
    }
}

impl std::fmt::Debug for CallLogger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallLogger")
            .field("session_id", &self.session_id)
            .field("seq", &self.seq.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

/// Generate an 8-character lowercase hex session identifier derived from the
/// current system time.
///
/// Uses `DefaultHasher` on the system-time duration so we avoid pulling in
/// an external randomness crate. The output is the first 8 characters of
/// the hash formatted as zero-padded lowercase hex.
fn generate_session_id() -> String {
    use std::collections::hash_map::DefaultHasher;

    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();

    let mut hasher = DefaultHasher::new();
    now.as_nanos().hash(&mut hasher);
    let hash = hasher.finish();

    format!("{hash:016x}")[..8].to_owned()
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_success_entry() -> CallLogEntry {
        CallLogEntry {
            ts: "2026-04-04T15:47:22Z".to_owned(),
            session: "a1b2c3d4".to_owned(),
            seq: 0,
            tool: "query_convention".to_owned(),
            input: serde_json::json!({"topic": "error handling"}),
            duration_ms: 12,
            status: "ok".to_owned(),
            result: Some(serde_json::json!({"convention_count": 3, "decision_count": 1})),
            error_code: None,
        }
    }

    fn make_error_entry() -> CallLogEntry {
        CallLogEntry {
            ts: "2026-04-04T15:47:23Z".to_owned(),
            session: "a1b2c3d4".to_owned(),
            seq: 1,
            tool: "query_convention".to_owned(),
            input: serde_json::json!({"topic": ""}),
            duration_ms: 1,
            status: "error".to_owned(),
            result: None,
            error_code: Some("EMPTY_TOPIC".to_owned()),
        }
    }

    #[test]
    fn success_entry_serializes_to_expected_schema() {
        let entry = make_success_entry();
        let json = serde_json::to_value(&entry).unwrap();

        assert_eq!(json["ts"], "2026-04-04T15:47:22Z");
        assert_eq!(json["session"], "a1b2c3d4");
        assert_eq!(json["seq"], 0);
        assert_eq!(json["tool"], "query_convention");
        assert_eq!(json["input"]["topic"], "error handling");
        assert_eq!(json["duration_ms"], 12);
        assert_eq!(json["status"], "ok");
        assert_eq!(json["result"]["convention_count"], 3);
        assert_eq!(json["result"]["decision_count"], 1);
        // error_code should be absent
        assert!(json.get("error_code").is_none());
    }

    #[test]
    fn error_entry_serializes_to_expected_schema() {
        let entry = make_error_entry();
        let json = serde_json::to_value(&entry).unwrap();

        assert_eq!(json["ts"], "2026-04-04T15:47:23Z");
        assert_eq!(json["session"], "a1b2c3d4");
        assert_eq!(json["seq"], 1);
        assert_eq!(json["tool"], "query_convention");
        assert_eq!(json["input"]["topic"], "");
        assert_eq!(json["duration_ms"], 1);
        assert_eq!(json["status"], "error");
        assert_eq!(json["error_code"], "EMPTY_TOPIC");
        // result should be absent
        assert!(json.get("result").is_none());
    }

    #[test]
    fn optional_fields_omitted_when_none() {
        let entry = CallLogEntry {
            ts: "2026-04-04T15:47:22Z".to_owned(),
            session: "x1y2z3w4".to_owned(),
            seq: 0,
            tool: "record_decision".to_owned(),
            input: serde_json::json!({"description": "test"}),
            duration_ms: 5,
            status: "ok".to_owned(),
            result: None,
            error_code: None,
        };

        let json_str = serde_json::to_string(&entry).unwrap();

        // Neither "result" nor "error_code" should appear in the output.
        assert!(
            !json_str.contains("\"result\""),
            "result should be omitted when None"
        );
        assert!(
            !json_str.contains("\"error_code\""),
            "error_code should be omitted when None"
        );

        // Verify it parses back fine and those keys are truly absent.
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert!(parsed.get("result").is_none());
        assert!(parsed.get("error_code").is_none());
    }

    #[test]
    fn project_context_result_extracts_counts() {
        let data = serde_json::json!({
            "languages": [{"language": "rust", "file_count": 10}],
            "conventions_count": 5,
            "golden_files": [{"file": "a.rs"}, {"file": "b.rs"}],
        });

        let result = project_context_result(&data);
        assert_eq!(result["language_count"], 1);
        assert_eq!(result["convention_count"], 5);
        assert_eq!(result["golden_file_count"], 2);
    }

    #[test]
    fn query_convention_result_extracts_counts() {
        let data = serde_json::json!({
            "conventions": [
                {"id": 1, "source": "auto_detected"},
                {"id": 2, "source": "user"},
                {"id": 3, "source": "user"},
            ]
        });

        let result = query_convention_result(&data);
        assert_eq!(result["convention_count"], 3);
        assert_eq!(result["decision_count"], 2);
    }

    #[test]
    fn decision_result_produces_description_hash() {
        // P36: the empty-string assertion that used to live here was a
        // codification of buggy behaviour — the MCP boundary now rejects
        // an empty description_hash with INVALID_INPUT, so the call
        // logger never sees one in normal operation. Removed to avoid
        // pinning an invariant ("decision_result accepts empty input")
        // that the rest of the system actively prevents.
        assert_eq!(decision_result("abc12345")["description_hash"], "abc12345");
        assert_eq!(
            decision_result("deadbeefcafebabe")["description_hash"],
            "deadbeefcafebabe"
        );
    }

    #[test]
    fn project_context_result_missing_keys_returns_zeros() {
        let data = serde_json::json!({});
        let result = project_context_result(&data);
        assert_eq!(result["language_count"], 0);
        assert_eq!(result["convention_count"], 0);
        assert_eq!(result["golden_file_count"], 0);
    }

    #[test]
    fn project_context_result_wrong_types_returns_zeros() {
        let data = serde_json::json!({
            "languages": "not-an-array",
            "conventions_count": "not-a-number",
            "golden_files": 42,
        });
        let result = project_context_result(&data);
        assert_eq!(result["language_count"], 0);
        assert_eq!(result["convention_count"], 0);
        assert_eq!(result["golden_file_count"], 0);
    }

    #[test]
    fn query_convention_result_missing_conventions_returns_zeros() {
        let data = serde_json::json!({});
        let result = query_convention_result(&data);
        assert_eq!(result["convention_count"], 0);
        assert_eq!(result["decision_count"], 0);
    }

    #[test]
    fn query_convention_result_empty_conventions() {
        let data = serde_json::json!({ "conventions": [] });
        let result = query_convention_result(&data);
        assert_eq!(result["convention_count"], 0);
        assert_eq!(result["decision_count"], 0);
    }

    #[test]
    fn query_convention_result_no_user_decisions() {
        let data = serde_json::json!({
            "conventions": [
                {"id": 1, "source": "auto_detected"},
                {"id": 2, "source": "auto_detected"},
            ]
        });
        let result = query_convention_result(&data);
        assert_eq!(result["convention_count"], 2);
        assert_eq!(result["decision_count"], 0);
    }

    #[test]
    fn code_pattern_result_extracts_counts() {
        let data = serde_json::json!({
            "patterns": [{"name": "a"}, {"name": "b"}],
            "related_conventions": [{"id": 1}],
        });
        let result = code_pattern_result(&data);
        assert_eq!(result["pattern_count"], 2);
        assert_eq!(result["convention_count"], 1);
    }

    #[test]
    fn code_pattern_result_missing_keys_returns_zeros() {
        let data = serde_json::json!({});
        let result = code_pattern_result(&data);
        assert_eq!(result["pattern_count"], 0);
        assert_eq!(result["convention_count"], 0);
    }

    #[test]
    fn code_pattern_result_empty_arrays() {
        let data = serde_json::json!({
            "patterns": [],
            "related_conventions": [],
        });
        let result = code_pattern_result(&data);
        assert_eq!(result["pattern_count"], 0);
        assert_eq!(result["convention_count"], 0);
    }

    #[test]
    fn dependencies_result_extracts_all_fields() {
        let data = serde_json::json!({
            "dependents": [{"file": "a.rs"}, {"file": "b.rs"}, {"file": "c.rs"}],
            "dependencies": [{"file": "d.rs"}],
            "blast_radius": "high",
            "transitive_dependent_count": 7,
            "requested_depth": 3,
        });
        let result = dependencies_result(&data);
        assert_eq!(result["dependent_count"], 3);
        assert_eq!(result["dependency_count"], 1);
        assert_eq!(result["blast_radius"], "high");
        assert_eq!(result["transitive_dependent_count"], 7);
        assert_eq!(result["requested_depth"], 3);
    }

    #[test]
    fn dependencies_result_missing_keys_returns_unknown_blast_radius() {
        let data = serde_json::json!({});
        let result = dependencies_result(&data);
        assert_eq!(result["dependent_count"], 0);
        assert_eq!(result["dependency_count"], 0);
        assert_eq!(result["blast_radius"], "unknown");
        assert_eq!(result["transitive_dependent_count"], 0);
        assert_eq!(result["requested_depth"], 0);
    }

    #[test]
    fn dependencies_result_partial_data() {
        let data = serde_json::json!({
            "dependents": [{"x": 1}],
            "blast_radius": "low",
            "requested_depth": 1,
        });
        let result = dependencies_result(&data);
        assert_eq!(result["dependent_count"], 1);
        assert_eq!(result["dependency_count"], 0);
        assert_eq!(result["blast_radius"], "low");
        assert_eq!(result["transitive_dependent_count"], 0);
        assert_eq!(result["requested_depth"], 1);
    }

    #[test]
    fn validate_approach_result_extracts_all_fields() {
        let data = serde_json::json!({
            "verdict": "approved",
            "rules": [{"id": 1}, {"id": 2}],
            "duplicates": [{"name": "x"}],
            "conventions": [{"id": 10}, {"id": 11}, {"id": 12}],
            "ready": true,
        });
        let result = validate_approach_result(&data);
        assert_eq!(result["verdict"], "approved");
        assert_eq!(result["rule_count"], 2);
        assert_eq!(result["duplicate_count"], 1);
        assert_eq!(result["convention_count"], 3);
        assert_eq!(result["ready"], true);
    }

    #[test]
    fn validate_approach_result_missing_keys_returns_safe_defaults() {
        let data = serde_json::json!({});
        let result = validate_approach_result(&data);
        assert_eq!(result["verdict"], "unknown");
        assert_eq!(result["rule_count"], 0);
        assert_eq!(result["duplicate_count"], 0);
        assert_eq!(result["convention_count"], 0);
        assert_eq!(result["ready"], false);
    }

    #[test]
    fn validate_approach_result_rules_violated_not_ready() {
        let data = serde_json::json!({
            "verdict": "rules_violated",
            "rules": [{"id": 1}],
            "ready": false,
        });
        let result = validate_approach_result(&data);
        assert_eq!(result["verdict"], "rules_violated");
        assert_eq!(result["rule_count"], 1);
        assert_eq!(result["ready"], false);
    }

    #[test]
    fn diff_impact_result_extracts_all_fields() {
        let data = serde_json::json!({
            "changed_files": [{"file": "a.rs"}, {"file": "b.rs"}],
            "affected_symbols": [{"name": "x"}],
            "convention_risks": [{"topic": "logging"}, {"topic": "naming"}, {"topic": "errors"}],
            "blast_radius_summary": {"risk": "medium"},
            "total_hunks": 7,
        });
        let result = diff_impact_result(&data);
        assert_eq!(result["changed_file_count"], 2);
        assert_eq!(result["affected_symbol_count"], 1);
        assert_eq!(result["convention_risk_count"], 3);
        assert_eq!(result["blast_radius"], "medium");
        assert_eq!(result["total_hunks"], 7);
    }

    #[test]
    fn diff_impact_result_missing_keys_returns_safe_defaults() {
        let data = serde_json::json!({});
        let result = diff_impact_result(&data);
        assert_eq!(result["changed_file_count"], 0);
        assert_eq!(result["affected_symbol_count"], 0);
        assert_eq!(result["convention_risk_count"], 0);
        assert_eq!(result["blast_radius"], "none");
        assert_eq!(result["total_hunks"], 0);
    }

    #[test]
    fn diff_impact_result_blast_radius_summary_without_risk() {
        let data = serde_json::json!({
            "blast_radius_summary": {"some_other_field": 1},
        });
        let result = diff_impact_result(&data);
        assert_eq!(result["blast_radius"], "none");
        assert_eq!(result["total_hunks"], 0);
    }

    #[test]
    fn diff_impact_result_partial_data_extracts_total_hunks() {
        // Locks the standalone extraction path: even with no changed_files /
        // affected_symbols / convention_risks present, `total_hunks` flows
        // through unmodified from the response envelope.
        let data = serde_json::json!({
            "total_hunks": 3,
        });
        let result = diff_impact_result(&data);
        assert_eq!(result["total_hunks"], 3);
        assert_eq!(result["changed_file_count"], 0);
        assert_eq!(result["affected_symbol_count"], 0);
        assert_eq!(result["convention_risk_count"], 0);
        assert_eq!(result["blast_radius"], "none");
    }

    #[test]
    fn log_call_two_entries_appended_in_order() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("calls.jsonl");

        let logger = CallLogger::new(&path).unwrap();
        for i in 0..3 {
            let entry = CallLogEntry {
                ts: format!("2026-04-04T15:47:2{i}Z"),
                session: logger.session_id().to_owned(),
                seq: logger.next_seq(),
                tool: "query_convention".to_owned(),
                input: serde_json::json!({"i": i}),
                duration_ms: 1,
                status: "ok".to_owned(),
                result: None,
                error_code: None,
            };
            logger.log_call(&entry).unwrap();
        }

        let mut contents = String::new();
        File::open(&path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 3);
        for (i, line) in lines.iter().enumerate() {
            let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(parsed["seq"], i);
            assert_eq!(parsed["input"]["i"], i);
        }
    }

    #[test]
    fn logger_debug_impl_redacts_writer() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("calls.jsonl");
        let logger = CallLogger::new(&path).unwrap();
        let _ = logger.next_seq();
        let s = format!("{logger:?}");
        assert!(s.contains("CallLogger"));
        assert!(s.contains("session_id"));
        assert!(s.contains("seq"));
    }

    // ── CallLogger tests ────────────────────────────────────

    use std::io::Read;
    use tempfile::TempDir;

    #[test]
    fn logger_new_creates_file_at_path() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("calls.jsonl");

        let _logger = CallLogger::new(&path).unwrap();

        assert!(path.exists(), "log file should be created");
    }

    #[test]
    fn logger_new_creates_parent_directories() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a").join("b").join("c").join("calls.jsonl");

        let _logger = CallLogger::new(&path).unwrap();

        assert!(path.exists(), "log file should be created in nested dir");
    }

    #[test]
    fn log_call_writes_valid_jsonl() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("calls.jsonl");

        let logger = CallLogger::new(&path).unwrap();
        let entry = make_success_entry();
        logger.log_call(&entry).unwrap();

        let mut contents = String::new();
        File::open(&path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();

        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1, "should have exactly one line");

        // Verify it parses as valid JSON.
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["tool"], "query_convention");
        assert_eq!(parsed["status"], "ok");
    }

    #[test]
    fn next_seq_returns_monotonically_increasing_values() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("calls.jsonl");

        let logger = CallLogger::new(&path).unwrap();

        assert_eq!(logger.next_seq(), 0);
        assert_eq!(logger.next_seq(), 1);
        assert_eq!(logger.next_seq(), 2);
        assert_eq!(logger.next_seq(), 3);
    }

    #[test]
    fn session_id_is_8_hex_characters() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("calls.jsonl");

        let logger = CallLogger::new(&path).unwrap();
        let sid = logger.session_id();

        assert_eq!(sid.len(), 8, "session ID should be 8 characters");
        assert!(
            sid.chars().all(|c| c.is_ascii_hexdigit()),
            "session ID should be lowercase hex, got: {sid}"
        );
    }

    #[test]
    fn append_behavior_two_loggers_on_same_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("calls.jsonl");

        // First logger writes one entry.
        let logger1 = CallLogger::new(&path).unwrap();
        let entry1 = CallLogEntry {
            ts: "2026-04-04T15:47:22Z".to_owned(),
            session: logger1.session_id().to_owned(),
            seq: logger1.next_seq(),
            tool: "query_convention".to_owned(),
            input: serde_json::json!({"topic": "a"}),
            duration_ms: 5,
            status: "ok".to_owned(),
            result: None,
            error_code: None,
        };
        logger1.log_call(&entry1).unwrap();
        let session1 = logger1.session_id().to_owned();
        drop(logger1);

        // Second logger appends another entry.
        let logger2 = CallLogger::new(&path).unwrap();
        let entry2 = CallLogEntry {
            ts: "2026-04-04T15:48:00Z".to_owned(),
            session: logger2.session_id().to_owned(),
            seq: logger2.next_seq(),
            tool: "record_decision".to_owned(),
            input: serde_json::json!({"description": "b"}),
            duration_ms: 3,
            status: "ok".to_owned(),
            result: None,
            error_code: None,
        };
        logger2.log_call(&entry2).unwrap();
        let session2 = logger2.session_id().to_owned();

        // Read back and verify both lines are present.
        let mut contents = String::new();
        File::open(&path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();

        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2, "should have entries from both sessions");

        let line1: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let line2: serde_json::Value = serde_json::from_str(lines[1]).unwrap();

        assert_eq!(line1["session"], session1);
        assert_eq!(line2["session"], session2);
        assert_eq!(line1["tool"], "query_convention");
        assert_eq!(line2["tool"], "record_decision");
    }
}
