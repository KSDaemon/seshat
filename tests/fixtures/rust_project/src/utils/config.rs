// Demonstrates:
// - Derive macros (Debug, Clone, Deserialize)
// - Default trait implementation
// - serde(default) for optional fields
// - Constants

use serde::Deserialize;

/// Default server port.
const DEFAULT_PORT: u16 = 3000;

/// Maximum connections allowed.
const MAX_CONNECTIONS: usize = 100;

/// Application configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub host: String,
    pub port: u16,
    pub max_connections: usize,
    pub database: DatabaseConfig,
    pub logging: LoggingConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: DEFAULT_PORT,
            max_connections: MAX_CONNECTIONS,
            database: DatabaseConfig::default(),
            logging: LoggingConfig::default(),
        }
    }
}

/// Database configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DatabaseConfig {
    pub url: String,
    pub pool_size: u32,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: "sqlite://data.db".into(),
            pool_size: 5,
        }
    }
}

/// Logging configuration.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    pub level: String,
    pub format: LogFormat,
}

/// Log output format.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    #[default]
    Json,
    Pretty,
    Compact,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = AppConfig::default();
        assert_eq!(config.port, 3000);
        assert_eq!(config.max_connections, 100);
    }

    #[test]
    fn test_default_database_config() {
        let config = DatabaseConfig::default();
        assert_eq!(config.pool_size, 5);
    }
}
