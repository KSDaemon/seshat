// Fixture: Rust config module with derives, structs, impls

use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub name: String,
    pub port: u16,
    pub data_dir: PathBuf,
}

impl Config {
    pub fn new(name: String) -> Self {
        Self {
            name,
            port: 8080,
            data_dir: PathBuf::from("./data"),
        }
    }

    pub fn load() -> Result<Self, ConfigError> {
        Ok(Self::new("default".to_string()))
    }

    fn validate(&self) -> bool {
        !self.name.is_empty() && self.port > 0
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new("default".to_string())
    }
}

pub type ConfigResult<T> = Result<T, ConfigError>;

#[derive(Debug)]
pub struct ConfigError {
    pub message: String,
}
