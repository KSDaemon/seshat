use serde::{Deserialize, Serialize};

/// Configuration for the scanning pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ScanConfig {
    /// Additional glob patterns to exclude from scanning.
    pub exclude_patterns: Vec<String>,
    /// Maximum file size in kilobytes. Files larger than this are skipped.
    pub max_file_size_kb: u64,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            exclude_patterns: Vec::new(),
            max_file_size_kb: 512,
        }
    }
}

/// Configuration for the convention detection engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DetectionConfig {
    /// Confidence threshold for "Strong" weight.
    pub confidence_strong: f64,
    /// Confidence threshold for "Moderate" weight.
    pub confidence_moderate: f64,
    /// Confidence threshold for "Weak" weight.
    pub confidence_weak: f64,
    /// Maximum number of lines per code snippet.
    pub max_snippet_lines: usize,
}

impl Default for DetectionConfig {
    fn default() -> Self {
        Self {
            confidence_strong: 0.85,
            confidence_moderate: 0.50,
            confidence_weak: 0.20,
            max_snippet_lines: 20,
        }
    }
}

/// Configuration for the MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ServerConfig {
    /// Log level for the server.
    pub log_level: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            log_level: "info".to_owned(),
        }
    }
}
