use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VersionCache {
    pub latest_version: String,
    pub checked_at: String,
}

impl VersionCache {
    pub fn cache_dir() -> Option<PathBuf> {
        Some(dirs::data_dir()?.join("seshat"))
    }

    pub fn cache_path() -> Option<PathBuf> {
        Some(Self::cache_dir()?.join("version-check.json"))
    }

    pub fn read_from_path(path: &std::path::Path) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        if content.trim().is_empty() {
            return None;
        }
        serde_json::from_str(&content).ok()
    }

    pub fn read() -> Option<Self> {
        Self::read_from_path(&Self::cache_path()?)
    }

    pub fn write(&self) -> Result<(), std::io::Error> {
        let path = Self::cache_path().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "could not determine cache path",
            )
        })?;
        self.write_to_path(&path)
    }

    pub fn write_to_path(&self, path: &std::path::Path) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json)
    }

    pub fn is_fresh(&self) -> bool {
        chrono::DateTime::parse_from_rfc3339(&self.checked_at)
            .ok()
            .map(|checked_time| {
                let now = chrono::Utc::now();
                let age = now.signed_duration_since(checked_time);
                age.num_hours() < 24
            })
            .unwrap_or(false)
    }

    pub fn new(latest_version: String) -> Self {
        Self {
            latest_version,
            checked_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    pub fn expired_at(version: &str, hours_ago: i64) -> Self {
        Self {
            latest_version: version.to_owned(),
            checked_at: (chrono::Utc::now() - chrono::Duration::hours(hours_ago)).to_rfc3339(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn cache_json(version: &str, rfc3339: &str) -> String {
        format!(
            r#"{{"latest_version":"{}","checked_at":"{}"}}"#,
            version, rfc3339
        )
    }

    #[test]
    fn fresh_cache_is_fresh() {
        let cache = VersionCache::new("1.0.0".to_owned());
        assert!(cache.is_fresh());
    }

    #[test]
    fn old_cache_is_stale() {
        let cache = VersionCache::expired_at("1.0.0", 25);
        assert!(!cache.is_fresh());
    }

    #[test]
    fn exactly_24h_is_stale() {
        let cache = VersionCache::expired_at("1.0.0", 24);
        assert!(!cache.is_fresh());
    }

    #[test]
    fn missing_file_returns_none() {
        let result = VersionCache::read_from_path(std::path::Path::new("/nonexistent/cache.json"));
        assert!(result.is_none());
    }

    #[test]
    fn empty_file_returns_none() {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "").unwrap();
        let result = VersionCache::read_from_path(file.path());
        assert!(result.is_none());
    }

    #[test]
    fn whitespace_only_file_returns_none() {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "   \n  ").unwrap();
        let result = VersionCache::read_from_path(file.path());
        assert!(result.is_none());
    }

    #[test]
    fn corrupted_json_returns_none() {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "not json").unwrap();
        let result = VersionCache::read_from_path(file.path());
        assert!(result.is_none());
    }

    #[test]
    fn missing_required_fields_returns_none() {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, r#"{{"wrong_field":"x"}}"#).unwrap();
        let result = VersionCache::read_from_path(file.path());
        assert!(result.is_none());
    }

    #[test]
    fn valid_cache_reads_correctly() {
        let mut file = NamedTempFile::new().unwrap();
        let now_rfc3339 = chrono::Utc::now().to_rfc3339();
        write!(file, "{}", cache_json("2.0.0", &now_rfc3339)).unwrap();
        let result = VersionCache::read_from_path(file.path());
        assert!(result.is_some());
        let cache = result.unwrap();
        assert_eq!(cache.latest_version, "2.0.0");
        assert_eq!(cache.checked_at, now_rfc3339);
    }

    #[test]
    fn write_creates_directories_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("subdir").join("nested").join("cache.json");
        let cache = VersionCache::new("3.0.0".to_owned());
        cache.write_to_path(&path).unwrap();
        assert!(path.exists());
        let read_back = VersionCache::read_from_path(&path).unwrap();
        assert_eq!(read_back.latest_version, "3.0.0");
    }

    #[test]
    fn write_then_read_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let cache = VersionCache::new("4.0.0".to_owned());
        cache.write_to_path(&path).unwrap();
        let read_back = VersionCache::read_from_path(&path).unwrap();
        assert_eq!(read_back.latest_version, "4.0.0");
        assert_eq!(read_back.checked_at, cache.checked_at);
    }

    #[test]
    fn invalid_timestamp_is_stale() {
        let cache = VersionCache {
            latest_version: "1.0.0".to_owned(),
            checked_at: "not-a-timestamp".to_owned(),
        };
        assert!(!cache.is_fresh());
    }

    #[test]
    fn new_creates_with_current_timestamp() {
        let before = chrono::Utc::now() - chrono::Duration::seconds(1);
        let cache = VersionCache::new("1.0.0".to_owned());
        let after = chrono::Utc::now() + chrono::Duration::seconds(1);

        let parsed = chrono::DateTime::parse_from_rfc3339(&cache.checked_at).unwrap();
        assert!(parsed > before);
        assert!(parsed < after);
    }

    #[test]
    fn cache_path_is_in_data_dir() {
        let path = VersionCache::cache_path();
        assert!(path.is_some());
        let p = path.unwrap();
        assert!(p.ends_with("version-check.json"));
        assert!(p.to_string_lossy().contains("seshat"));
    }
}
