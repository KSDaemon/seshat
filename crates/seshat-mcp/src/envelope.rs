//! Consistent JSON response envelopes for all MCP tools.
//!
//! Every tool response is wrapped in either [`ResponseEnvelope<T>`] (success)
//! or [`ErrorEnvelope`] (failure). This gives AI agents a single schema to
//! parse regardless of which tool they called.

use std::time::Instant;

use serde::{Deserialize, Serialize};

// ── Error codes ──────────────────────────────────────────────

/// Structured error codes for MCP tool failures.
///
/// These are serialized as SCREAMING_SNAKE_CASE strings in JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// No scanned project database found.
    RepoNotScanned,
    /// The `topic` parameter was empty or whitespace-only.
    EmptyTopic,
    /// A required parameter was missing or had an invalid value.
    InvalidInput,
    /// The requested knowledge node does not exist.
    NodeNotFound,
    /// Attempted to modify an auto-detected convention (only user decisions
    /// can be updated/removed).
    NotUserDecision,
    /// An unexpected internal error.
    InternalError,
}

impl ErrorCode {
    /// Canonical string representation (matches serde output).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RepoNotScanned => "REPO_NOT_SCANNED",
            Self::EmptyTopic => "EMPTY_TOPIC",
            Self::InvalidInput => "INVALID_INPUT",
            Self::NodeNotFound => "NODE_NOT_FOUND",
            Self::NotUserDecision => "NOT_USER_DECISION",
            Self::InternalError => "INTERNAL_ERROR",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── Metadata ─────────────────────────────────────────────────

/// Optional metadata attached to every success response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseMetadata {
    /// Context-aware hints suggesting what the agent should do next.
    pub next_steps: Vec<String>,

    /// Arbitrary extra metadata fields (search_type, results_count, etc.).
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl ResponseMetadata {
    /// Create metadata with the given next-step suggestions.
    pub fn new(next_steps: Vec<String>) -> Self {
        Self {
            next_steps,
            extra: serde_json::Map::new(),
        }
    }

    /// Add an arbitrary key-value pair to the metadata.
    pub fn with_extra(
        mut self,
        key: impl Into<String>,
        value: impl Into<serde_json::Value>,
    ) -> Self {
        self.extra.insert(key.into(), value.into());
        self
    }
}

// ── Success envelope ─────────────────────────────────────────

/// Uniform success envelope wrapping tool-specific data.
///
/// ```json
/// {
///   "status": "success",
///   "tool": "query_convention",
///   "repo": "seshat",
///   "branch": "main",
///   "scope": "root",
///   "duration_ms": 12,
///   "data": { ... },
///   "metadata": { "next_steps": ["..."] }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseEnvelope<T: Serialize> {
    /// Always `"success"`.
    pub status: String,
    /// Name of the tool that produced the response.
    pub tool: String,
    /// Repository name.
    pub repo: String,
    /// Active branch.
    pub branch: String,
    /// Always `"root"` (multi-repo scoping deferred).
    pub scope: String,
    /// Wall-clock time in milliseconds.
    pub duration_ms: u64,
    /// Tool-specific payload.
    pub data: T,
    /// Context-aware hints and extra metadata.
    pub metadata: ResponseMetadata,
}

impl<T: Serialize> ResponseEnvelope<T> {
    /// Build a success envelope, computing `duration_ms` from `start`.
    pub fn success(
        tool: impl Into<String>,
        repo: impl Into<String>,
        branch: impl Into<String>,
        data: T,
        metadata: ResponseMetadata,
        start: Instant,
    ) -> Self {
        Self {
            status: "success".to_owned(),
            tool: tool.into(),
            repo: repo.into(),
            branch: branch.into(),
            scope: "root".to_owned(),
            duration_ms: start.elapsed().as_millis() as u64,
            data,
            metadata,
        }
    }
}

// ── Error detail ─────────────────────────────────────────────

/// Structured error detail inside an [`ErrorEnvelope`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    /// Machine-readable error code.
    pub code: ErrorCode,
    /// Human-readable error description.
    pub message: String,
    /// Actionable suggestion for the caller.
    pub suggestion: String,
}

// ── Error envelope ───────────────────────────────────────────

/// Uniform error envelope for any MCP tool failure.
///
/// ```json
/// {
///   "status": "error",
///   "tool": "query_convention",
///   "repo": "seshat",
///   "error": {
///     "code": "EMPTY_TOPIC",
///     "message": "The topic parameter must not be empty",
///     "suggestion": "Provide a topic like 'error handling' or 'logging'"
///   }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    /// Always `"error"`.
    pub status: String,
    /// Name of the tool that produced the error.
    pub tool: String,
    /// Repository name (may be empty if unknown).
    pub repo: String,
    /// Structured error detail.
    pub error: ErrorDetail,
}

impl ErrorEnvelope {
    /// Build an error envelope.
    pub fn new(
        tool: impl Into<String>,
        repo: impl Into<String>,
        code: ErrorCode,
        message: impl Into<String>,
        suggestion: impl Into<String>,
    ) -> Self {
        Self {
            status: "error".to_owned(),
            tool: tool.into(),
            repo: repo.into(),
            error: ErrorDetail {
                code,
                message: message.into(),
                suggestion: suggestion.into(),
            },
        }
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_envelope_serializes_correctly() {
        let start = Instant::now();
        let data = serde_json::json!({"languages": ["rust", "python"]});
        let meta =
            ResponseMetadata::new(vec!["Run query_convention to explore conventions".into()]);

        let envelope =
            ResponseEnvelope::success("query_project_context", "seshat", "main", data, meta, start);

        let json = serde_json::to_value(&envelope).unwrap();

        assert_eq!(json["status"], "success");
        assert_eq!(json["tool"], "query_project_context");
        assert_eq!(json["repo"], "seshat");
        assert_eq!(json["branch"], "main");
        assert_eq!(json["scope"], "root");
        assert!(json["duration_ms"].is_u64());
        assert_eq!(json["data"]["languages"][0], "rust");
        assert_eq!(
            json["metadata"]["next_steps"][0],
            "Run query_convention to explore conventions"
        );
    }

    #[test]
    fn success_envelope_deserializes_roundtrip() {
        let start = Instant::now();
        let data = serde_json::json!({"count": 42});
        let meta = ResponseMetadata::new(vec![]);

        let envelope = ResponseEnvelope::success("test_tool", "repo", "branch", data, meta, start);

        let json_str = serde_json::to_string(&envelope).unwrap();
        let parsed: ResponseEnvelope<serde_json::Value> = serde_json::from_str(&json_str).unwrap();

        assert_eq!(parsed.status, "success");
        assert_eq!(parsed.tool, "test_tool");
        assert_eq!(parsed.data["count"], 42);
    }

    #[test]
    fn error_envelope_serializes_correctly() {
        let envelope = ErrorEnvelope::new(
            "query_convention",
            "seshat",
            ErrorCode::EmptyTopic,
            "The topic parameter must not be empty",
            "Provide a topic like 'error handling' or 'logging'",
        );

        let json = serde_json::to_value(&envelope).unwrap();

        assert_eq!(json["status"], "error");
        assert_eq!(json["tool"], "query_convention");
        assert_eq!(json["repo"], "seshat");
        assert_eq!(json["error"]["code"], "EMPTY_TOPIC");
        assert_eq!(
            json["error"]["message"],
            "The topic parameter must not be empty"
        );
        assert_eq!(
            json["error"]["suggestion"],
            "Provide a topic like 'error handling' or 'logging'"
        );
    }

    #[test]
    fn error_envelope_deserializes_roundtrip() {
        let envelope = ErrorEnvelope::new(
            "record_decision",
            "seshat",
            ErrorCode::InvalidInput,
            "description is required",
            "Provide a non-empty description string",
        );

        let json_str = serde_json::to_string(&envelope).unwrap();
        let parsed: ErrorEnvelope = serde_json::from_str(&json_str).unwrap();

        assert_eq!(parsed.status, "error");
        assert_eq!(parsed.error.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn all_error_codes_serialize_screaming_snake() {
        let codes = [
            (ErrorCode::RepoNotScanned, "REPO_NOT_SCANNED"),
            (ErrorCode::EmptyTopic, "EMPTY_TOPIC"),
            (ErrorCode::InvalidInput, "INVALID_INPUT"),
            (ErrorCode::NodeNotFound, "NODE_NOT_FOUND"),
            (ErrorCode::NotUserDecision, "NOT_USER_DECISION"),
            (ErrorCode::InternalError, "INTERNAL_ERROR"),
        ];

        for (code, expected) in &codes {
            let json = serde_json::to_value(code).unwrap();
            assert_eq!(json.as_str().unwrap(), *expected, "code: {:?}", code);
        }
    }

    #[test]
    fn error_code_as_str_matches_serde() {
        let codes = [
            ErrorCode::RepoNotScanned,
            ErrorCode::EmptyTopic,
            ErrorCode::InvalidInput,
            ErrorCode::NodeNotFound,
            ErrorCode::NotUserDecision,
            ErrorCode::InternalError,
        ];

        for code in &codes {
            let serde_str = serde_json::to_value(code)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned();
            assert_eq!(code.as_str(), serde_str);
        }
    }

    #[test]
    fn error_code_display() {
        assert_eq!(ErrorCode::EmptyTopic.to_string(), "EMPTY_TOPIC");
        assert_eq!(ErrorCode::InternalError.to_string(), "INTERNAL_ERROR");
    }

    #[test]
    fn metadata_with_extra_fields() {
        let meta = ResponseMetadata::new(vec!["next".into()])
            .with_extra("search_type", "fts5")
            .with_extra("results_count", 7);

        let json = serde_json::to_value(&meta).unwrap();

        assert_eq!(json["next_steps"][0], "next");
        assert_eq!(json["search_type"], "fts5");
        assert_eq!(json["results_count"], 7);
    }

    #[test]
    fn scope_always_root() {
        let start = Instant::now();
        let envelope = ResponseEnvelope::success(
            "any_tool",
            "any_repo",
            "any_branch",
            serde_json::json!({}),
            ResponseMetadata::new(vec![]),
            start,
        );
        assert_eq!(envelope.scope, "root");
    }
}
