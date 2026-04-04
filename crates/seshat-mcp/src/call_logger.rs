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
        .get("languages")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let convention_count = response_data
        .get("conventions_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let golden_file_count = response_data
        .get("golden_files")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

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
    let conventions = response_data.get("conventions").and_then(|v| v.as_array());

    let total = conventions.map(|a| a.len()).unwrap_or(0);

    let decision_count = conventions
        .map(|arr| {
            arr.iter()
                .filter(|c| c.get("source").and_then(|s| s.as_str()) == Some("user"))
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
/// Extracts `pattern_count` and `convention_count` from the serialized
/// response data's embedded metadata.
pub fn code_pattern_result(response_data: &serde_json::Value) -> serde_json::Value {
    let pattern_count = response_data
        .get("metadata")
        .and_then(|m| m.get("pattern_count"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let convention_count = response_data
        .get("metadata")
        .and_then(|m| m.get("convention_count"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    serde_json::json!({
        "pattern_count": pattern_count,
        "convention_count": convention_count,
    })
}

/// Build a result summary for `query_dependencies`.
///
/// Extracts `dependent_count`, `dependency_count`, and `blast_radius`
/// from the serialized response data.
pub fn dependencies_result(response_data: &serde_json::Value) -> serde_json::Value {
    let dependent_count = response_data
        .get("dependents")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let dependency_count = response_data
        .get("dependencies")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let blast_radius = response_data
        .get("blast_radius")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    serde_json::json!({
        "dependent_count": dependent_count,
        "dependency_count": dependency_count,
        "blast_radius": blast_radius,
    })
}

/// Build a result summary for `validate_approach`.
///
/// Extracts `verdict`, `rule_count`, `duplicate_count`, `convention_count`,
/// and `ready` from the serialized response data.
pub fn validate_approach_result(response_data: &serde_json::Value) -> serde_json::Value {
    let verdict = response_data
        .get("verdict")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let rule_count = response_data
        .get("rules")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let duplicate_count = response_data
        .get("duplicates")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let convention_count = response_data
        .get("conventions")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let ready = response_data
        .get("ready")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

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
pub fn decision_result(node_id: i64) -> serde_json::Value {
    serde_json::json!({ "node_id": node_id })
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
    fn decision_result_produces_node_id() {
        assert_eq!(decision_result(42)["node_id"], 42);
        assert_eq!(decision_result(99)["node_id"], 99);
        assert_eq!(decision_result(0)["node_id"], 0);
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
