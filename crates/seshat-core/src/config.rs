use serde::{Deserialize, Serialize};

/// Configuration for the scanning pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct ScanConfig {
    /// Glob patterns (relative to project root) to exclude from **all**
    /// discovery flows — source files, documentation ingestion, and any
    /// future filesystem walks.
    ///
    /// Examples:
    /// ```toml
    /// [scan]
    /// exclude_paths = [".opencode/**", "_bmad/**", "logs/**", "*.log"]
    /// ```
    ///
    /// The old name `exclude_patterns` is accepted as a TOML alias for
    /// backwards compatibility.
    #[serde(alias = "exclude_patterns")]
    pub exclude_paths: Vec<String>,
    /// Maximum file size in kilobytes. Files larger than this are skipped.
    pub max_file_size_kb: u64,
    /// Whether to exclude separate submodule scans.
    /// Defaults to `false` — submodules are scanned into their own DBs by default.
    /// Root discovery always excludes submodule dirs (they get their own DBs);
    /// this flag controls whether separate submodule scans happen at all.
    #[serde(default)]
    pub exclude_submodules: bool,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            exclude_paths: Vec::new(),
            max_file_size_kb: 512,
            exclude_submodules: false,
        }
    }
}

/// Configuration for the convention detection engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct DetectionConfig {
    /// Confidence threshold for "Strong" weight.
    pub confidence_strong: f64,
    /// Confidence threshold for "Moderate" weight.
    pub confidence_moderate: f64,
    /// Confidence threshold for "Weak" weight.
    pub confidence_weak: f64,
    /// Maximum number of lines per code snippet.
    pub max_snippet_lines: usize,
    /// Age threshold (in days) below which a convention is considered Rising.
    /// If the P90 commit date is fewer than this many days ago, trend = Rising.
    pub trend_rising_days: u32,
    /// Age threshold (in days) below which a convention is considered Stable.
    /// If the P90 commit date is fewer than this many days ago but at least
    /// `trend_rising_days`, trend = Stable. Beyond this threshold, trend = Declining.
    pub trend_stable_days: u32,
}

impl Default for DetectionConfig {
    fn default() -> Self {
        Self {
            confidence_strong: 0.85,
            confidence_moderate: 0.50,
            confidence_weak: 0.20,
            max_snippet_lines: 20,
            trend_rising_days: 90,
            trend_stable_days: 365,
        }
    }
}

/// Configuration for automatic database backups.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct BackupConfig {
    /// Whether automatic backups are enabled.
    pub enabled: bool,
    /// Maximum number of backup files to retain. Older backups beyond this
    /// count are deleted.
    pub retention_count: usize,
    /// Minimum interval between backups, in hours.
    pub interval_hours: u64,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            retention_count: 3,
            interval_hours: 24,
        }
    }
}

/// Configuration for the MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct ServerConfig {
    /// Log level for the server.
    pub log_level: String,
    /// Host to bind the HTTP/SSE transport to.
    pub host: String,
    /// Port for the HTTP/SSE transport.
    pub port: u16,
    /// Enabled transports. Possible values: `"stdio"`, `"sse"`, `"http"`.
    pub transports: Vec<String>,
    /// Path to JSONL file for MCP tool call logging. `None` means disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_log: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            log_level: "info".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 6174,
            transports: vec!["stdio".to_owned(), "sse".to_owned(), "http".to_owned()],
            call_log: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_config_defaults() {
        let cfg = ScanConfig::default();
        assert!(cfg.exclude_paths.is_empty());
        assert_eq!(cfg.max_file_size_kb, 512);
    }

    #[test]
    fn detection_config_defaults() {
        let cfg = DetectionConfig::default();
        assert!((cfg.confidence_strong - 0.85).abs() < f64::EPSILON);
        assert!((cfg.confidence_moderate - 0.50).abs() < f64::EPSILON);
        assert!((cfg.confidence_weak - 0.20).abs() < f64::EPSILON);
        assert_eq!(cfg.max_snippet_lines, 20);
        assert_eq!(cfg.trend_rising_days, 90);
        assert_eq!(cfg.trend_stable_days, 365);
    }

    #[test]
    fn server_config_defaults() {
        let cfg = ServerConfig::default();
        assert_eq!(cfg.log_level, "info");
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 6174);
        assert_eq!(cfg.transports, vec!["stdio", "sse", "http"]);
        assert_eq!(cfg.call_log, None);
    }

    #[test]
    fn backup_config_defaults() {
        let cfg = BackupConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.retention_count, 3);
        assert_eq!(cfg.interval_hours, 24);
    }

    #[test]
    fn config_serialization_roundtrip() {
        let cfg = DetectionConfig::default();
        let json = serde_json::to_string(&cfg).expect("serialize");
        let deserialized: DetectionConfig = serde_json::from_str(&json).expect("deserialize");
        assert!((deserialized.confidence_strong - cfg.confidence_strong).abs() < f64::EPSILON);
    }
}
