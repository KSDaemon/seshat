//! # Application Configuration
//!
//! Loads and merges configuration from `seshat.toml` with sensible defaults.
//! Seshat works zero-config out of the box — all config sections have defaults.
//!
//! Config file search order:
//! 1. `./seshat.toml` (current working directory)
//! 2. `$XDG_CONFIG_HOME/seshat/seshat.toml` (e.g. `~/.config/seshat/seshat.toml`)
//!
//! Environment variable overrides:
//! - `SESHAT_LOG` overrides `server.log_level`

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use seshat_core::{DetectionConfig, ScanConfig, ServerConfig};

/// Top-level application configuration.
///
/// All sections use `#[serde(default)]` so that partial TOML files are
/// merged cleanly with built-in defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct AppConfig {
    /// Scanning pipeline settings.
    pub scan: ScanConfig,

    /// Convention detection thresholds.
    pub detection: DetectionConfig,

    /// MCP server settings.
    pub server: ServerConfig,

    /// File-watcher settings.
    pub watcher: WatcherConfig,

    /// Backup settings.
    pub backup: BackupConfig,

    /// Cache settings.
    pub cache: CacheConfig,

    /// Optional embedding / vector search settings.
    /// `None` when the section is absent from the config file.
    pub embedding: Option<EmbeddingConfig>,
}

/// Configuration for the file-watcher subsystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct WatcherConfig {
    /// Whether the watcher is enabled.
    pub enabled: bool,
    /// Debounce delay in milliseconds before processing file events.
    pub debounce_ms: u64,
    /// Additional glob patterns to ignore (on top of `scan.exclude_patterns`).
    pub ignore_patterns: Vec<String>,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            debounce_ms: 500,
            ignore_patterns: Vec::new(),
        }
    }
}

/// Configuration for database backups.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct BackupConfig {
    /// Whether automatic backups are enabled.
    pub enabled: bool,
    /// Maximum number of backup copies to retain.
    pub max_backups: usize,
    /// Backup directory path. Defaults to `.seshat/backups` relative to the
    /// project root.
    pub backup_dir: String,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_backups: 5,
            backup_dir: ".seshat/backups".to_owned(),
        }
    }
}

/// Configuration for the IR / query cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct CacheConfig {
    /// Whether caching is enabled.
    pub enabled: bool,
    /// Maximum cache size in megabytes.
    pub max_size_mb: u64,
    /// Time-to-live for cache entries in seconds.
    pub ttl_seconds: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_size_mb: 128,
            ttl_seconds: 3600,
        }
    }
}

/// Configuration for optional embedding / vector search integration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct EmbeddingConfig {
    /// Embedding model name or path.
    pub model: String,
    /// Embedding vector dimension.
    pub dimension: usize,
    /// Batch size for embedding generation.
    pub batch_size: usize,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            model: "all-MiniLM-L6-v2".to_owned(),
            dimension: 384,
            batch_size: 32,
        }
    }
}

/// The config file name Seshat searches for.
const CONFIG_FILENAME: &str = "seshat.toml";

/// Environment variable that overrides `server.log_level`.
const SESHAT_LOG_ENV: &str = "SESHAT_LOG";

impl AppConfig {
    /// Load configuration by searching for `seshat.toml` in standard locations.
    ///
    /// Search order:
    /// 1. Current working directory
    /// 2. `$XDG_CONFIG_HOME/seshat/` (via [`dirs::config_dir`])
    ///
    /// If no config file is found, defaults are returned (zero-config).
    /// Partial config files are merged with defaults — missing keys use
    /// their default values.
    ///
    /// After file loading, the `SESHAT_LOG` environment variable is checked
    /// and, if set, overrides `server.log_level`.
    pub fn load() -> Result<Self, ConfigError> {
        let mut config = if let Some(path) = Self::find_config_file() {
            Self::load_from_file(&path)?
        } else {
            Self::default()
        };

        // Environment variable override
        if let Ok(log_level) = std::env::var(SESHAT_LOG_ENV) {
            config.server.log_level = log_level;
        }

        Ok(config)
    }

    /// Load and parse a specific config file, merging with defaults.
    pub fn load_from_file(path: &Path) -> Result<Self, ConfigError> {
        let contents = std::fs::read_to_string(path).map_err(|e| ConfigError::ReadFile {
            path: path.to_path_buf(),
            source: e,
        })?;
        Self::from_toml_str(&contents)
    }

    /// Parse a TOML string into [`AppConfig`], merging with defaults.
    pub fn from_toml_str(s: &str) -> Result<Self, ConfigError> {
        toml::from_str(s).map_err(|e| ConfigError::Parse {
            details: e.to_string(),
        })
    }

    /// Search for `seshat.toml` in the standard locations.
    ///
    /// Returns the path to the first config file found, or `None`.
    fn find_config_file() -> Option<PathBuf> {
        // 1. Current directory
        let cwd_path = PathBuf::from(CONFIG_FILENAME);
        if cwd_path.is_file() {
            return Some(cwd_path);
        }

        // 2. XDG config directory
        if let Some(config_dir) = dirs::config_dir() {
            let xdg_path = config_dir.join("seshat").join(CONFIG_FILENAME);
            if xdg_path.is_file() {
                return Some(xdg_path);
            }
        }

        None
    }
}

/// Errors that can occur when loading configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Failed to read the config file from disk.
    #[error("failed to read config file '{path}': {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Failed to parse the TOML content.
    #[error("failed to parse config: {details}")]
    Parse { details: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let cfg = AppConfig::default();
        assert!(cfg.scan.exclude_patterns.is_empty());
        assert_eq!(cfg.scan.max_file_size_kb, 512);
        assert!((cfg.detection.confidence_strong - 0.85).abs() < f64::EPSILON);
        assert_eq!(cfg.server.log_level, "info");
        assert!(cfg.watcher.enabled);
        assert_eq!(cfg.watcher.debounce_ms, 500);
        assert!(cfg.backup.enabled);
        assert_eq!(cfg.backup.max_backups, 5);
        assert!(cfg.cache.enabled);
        assert_eq!(cfg.cache.max_size_mb, 128);
        assert!(cfg.embedding.is_none());
    }

    #[test]
    fn from_toml_full_config() {
        let toml_str = r#"
[scan]
exclude_patterns = ["*.log", "target/"]
max_file_size_kb = 1024

[detection]
confidence_strong = 0.90
confidence_moderate = 0.60
confidence_weak = 0.30
max_snippet_lines = 30

[server]
log_level = "debug"

[watcher]
enabled = false
debounce_ms = 1000
ignore_patterns = ["*.tmp"]

[backup]
enabled = false
max_backups = 10
backup_dir = "/tmp/seshat-backups"

[cache]
enabled = false
max_size_mb = 256
ttl_seconds = 7200

[embedding]
model = "text-embedding-3-small"
dimension = 1536
batch_size = 64
"#;
        let cfg = AppConfig::from_toml_str(toml_str).expect("valid TOML");
        assert_eq!(cfg.scan.exclude_patterns, vec!["*.log", "target/"]);
        assert_eq!(cfg.scan.max_file_size_kb, 1024);
        assert!((cfg.detection.confidence_strong - 0.90).abs() < f64::EPSILON);
        assert!((cfg.detection.confidence_moderate - 0.60).abs() < f64::EPSILON);
        assert_eq!(cfg.detection.max_snippet_lines, 30);
        assert_eq!(cfg.server.log_level, "debug");
        assert!(!cfg.watcher.enabled);
        assert_eq!(cfg.watcher.debounce_ms, 1000);
        assert_eq!(cfg.watcher.ignore_patterns, vec!["*.tmp"]);
        assert!(!cfg.backup.enabled);
        assert_eq!(cfg.backup.max_backups, 10);
        assert_eq!(cfg.backup.backup_dir, "/tmp/seshat-backups");
        assert!(!cfg.cache.enabled);
        assert_eq!(cfg.cache.max_size_mb, 256);
        assert_eq!(cfg.cache.ttl_seconds, 7200);
        let emb = cfg.embedding.expect("embedding section present");
        assert_eq!(emb.model, "text-embedding-3-small");
        assert_eq!(emb.dimension, 1536);
        assert_eq!(emb.batch_size, 64);
    }

    #[test]
    fn from_toml_partial_config_merges_defaults() {
        let toml_str = r#"
[scan]
max_file_size_kb = 2048

[server]
log_level = "warn"
"#;
        let cfg = AppConfig::from_toml_str(toml_str).expect("valid TOML");
        // Overridden values
        assert_eq!(cfg.scan.max_file_size_kb, 2048);
        assert_eq!(cfg.server.log_level, "warn");
        // Defaults for everything else
        assert!(cfg.scan.exclude_patterns.is_empty());
        assert!((cfg.detection.confidence_strong - 0.85).abs() < f64::EPSILON);
        assert!(cfg.watcher.enabled);
        assert_eq!(cfg.watcher.debounce_ms, 500);
        assert!(cfg.backup.enabled);
        assert_eq!(cfg.backup.max_backups, 5);
        assert!(cfg.cache.enabled);
        assert!(cfg.embedding.is_none());
    }

    #[test]
    fn from_toml_empty_string_gives_defaults() {
        let cfg = AppConfig::from_toml_str("").expect("empty is valid");
        assert_eq!(cfg.scan.max_file_size_kb, 512);
        assert_eq!(cfg.server.log_level, "info");
        assert!(cfg.watcher.enabled);
        assert!(cfg.embedding.is_none());
    }

    #[test]
    fn env_var_overrides_log_level() {
        // Set env var, load (no file → defaults), check override
        let original = std::env::var(SESHAT_LOG_ENV).ok();
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var(SESHAT_LOG_ENV, "trace") };

        let cfg = AppConfig::load().expect("load succeeds");
        assert_eq!(cfg.server.log_level, "trace");

        // Restore
        match original {
            // TODO: Audit that the environment access only happens in single-threaded code.
            Some(val) => unsafe { std::env::set_var(SESHAT_LOG_ENV, val) },
            // TODO: Audit that the environment access only happens in single-threaded code.
            None => unsafe { std::env::remove_var(SESHAT_LOG_ENV) },
        }
    }

    #[test]
    fn invalid_toml_returns_parse_error() {
        let result = AppConfig::from_toml_str("not valid {{{{ toml");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn config_serialization_roundtrip() {
        let cfg = AppConfig::default();
        let toml_str = toml::to_string_pretty(&cfg).expect("serialize");
        let roundtripped = AppConfig::from_toml_str(&toml_str).expect("deserialize");
        assert_eq!(
            roundtripped.scan.max_file_size_kb,
            cfg.scan.max_file_size_kb
        );
        assert_eq!(roundtripped.server.log_level, cfg.server.log_level);
        assert_eq!(roundtripped.watcher.debounce_ms, cfg.watcher.debounce_ms);
        assert_eq!(roundtripped.backup.max_backups, cfg.backup.max_backups);
        assert_eq!(roundtripped.cache.max_size_mb, cfg.cache.max_size_mb);
    }

    #[test]
    fn load_from_nonexistent_file_returns_error() {
        let result = AppConfig::load_from_file(Path::new("/nonexistent/seshat.toml"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ConfigError::ReadFile { .. }));
    }

    #[test]
    fn load_from_file_works() {
        // Create a temp file with partial config
        let dir = std::env::temp_dir().join("seshat-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("seshat.toml");
        std::fs::write(
            &file_path,
            r#"
[server]
log_level = "error"

[watcher]
debounce_ms = 2000
"#,
        )
        .unwrap();

        let cfg = AppConfig::load_from_file(&file_path).expect("load from file");
        assert_eq!(cfg.server.log_level, "error");
        assert_eq!(cfg.watcher.debounce_ms, 2000);
        // Defaults for unspecified
        assert!(cfg.watcher.enabled);
        assert_eq!(cfg.scan.max_file_size_kb, 512);

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }
}
