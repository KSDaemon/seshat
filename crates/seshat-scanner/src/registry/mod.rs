//! Package registry metadata types, client trait, and implementations.
//!
//! Defines the [`PackageRegistryClient`] trait for fetching metadata from
//! package registries (crates.io, npm, PyPI) and the associated types.
//!
//! Concrete implementations:
//! - [`crates_io::CratesIoClient`] — fetches from crates.io REST API
//! - [`npm::NpmClient`] — fetches from npm registry API
//! - [`pypi::PyPIClient`] — fetches from PyPI JSON API

pub mod crates_io;
pub mod npm;
pub mod pypi;
pub mod registry_mapping;

use std::time::Duration;

use serde::{Deserialize, Serialize};
use ureq::Agent;

/// Cache TTL for package metadata: 30 days in seconds.
///
/// Entries older than this are considered stale and will be re-fetched
/// on the next scan.
pub const CACHE_TTL_SECS: i64 = 30 * 24 * 60 * 60;

/// Which package registry a dependency originates from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Registry {
    /// Rust crates: <https://crates.io>
    CratesIo,
    /// Node.js packages: <https://www.npmjs.com>
    Npm,
    /// Python packages: <https://pypi.org>
    PyPI,
}

impl Registry {
    /// String representation used in the database `registry` column.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CratesIo => "crates_io",
            Self::Npm => "npm",
            Self::PyPI => "pypi",
        }
    }
}

impl std::fmt::Display for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Registry {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "crates_io" => Ok(Self::CratesIo),
            "npm" => Ok(Self::Npm),
            "pypi" => Ok(Self::PyPI),
            other => Err(format!("unknown registry: {other}")),
        }
    }
}

/// Metadata fetched from a package registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageMetadata {
    /// Package name as it appears in the registry.
    pub name: String,
    /// Which registry this metadata was fetched from.
    pub registry: Registry,
    /// Registry-defined categories (e.g., crates.io categories, PyPI classifiers).
    pub categories: Vec<String>,
    /// Author-defined keywords.
    pub keywords: Vec<String>,
    /// Package description, if available.
    pub description: Option<String>,
}

/// Errors that can occur when fetching package metadata from a registry.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// HTTP request failed (timeout, DNS, connection error, etc.).
    #[error("HTTP error fetching {package} from {registry}: {reason}")]
    HttpError {
        package: String,
        registry: Registry,
        reason: String,
    },

    /// The registry returned a non-success status code.
    #[error("{registry} returned status {status} for {package}")]
    StatusError {
        package: String,
        registry: Registry,
        status: u16,
    },

    /// Failed to parse the registry's JSON response.
    #[error("Failed to parse {registry} response for {package}: {reason}")]
    ParseError {
        package: String,
        registry: Registry,
        reason: String,
    },

    /// The requested package was not found (404).
    #[error("Package {package} not found on {registry}")]
    NotFound { package: String, registry: Registry },
}

/// Trait for fetching package metadata from a registry.
///
/// Each registry (crates.io, npm, PyPI) provides a concrete implementation.
/// Implementations must:
/// - Set an appropriate `User-Agent` header per registry API policies
/// - Use a reasonable timeout (≤ 5 seconds)
/// - Return `RegistryError` on failure rather than panicking
pub trait PackageRegistryClient: Send + Sync {
    /// Fetch metadata for the given package from the registry.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError`] if the HTTP request fails, the package is
    /// not found, or the response cannot be parsed.
    fn fetch_metadata(&self, package_name: &str) -> Result<PackageMetadata, RegistryError>;
}

// ---------------------------------------------------------------------------
// Shared HTTP infrastructure for registry clients
// ---------------------------------------------------------------------------

/// User-Agent header value per registry API policies.
const USER_AGENT: &str = concat!("seshat/", env!("CARGO_PKG_VERSION"));

/// Request timeout in seconds.
const TIMEOUT_SECS: u64 = 5;

/// Shared HTTP transport for all registry clients.
///
/// Handles [`ureq::Agent`] creation, timeout, `User-Agent` header, error
/// mapping, and response body reading. Each concrete client (`CratesIoClient`,
/// `NpmClient`, `PyPIClient`) wraps this and adds only its JSON parsing logic.
pub(crate) struct RegistryHttpClient {
    agent: Agent,
    base_url: String,
    registry: Registry,
    /// Suffix appended after `/{package_name}` in the URL.
    ///
    /// Most registries use `""` (empty), PyPI uses `"/json"`.
    url_suffix: &'static str,
}

impl RegistryHttpClient {
    /// Create a new HTTP client for the given registry.
    pub(crate) fn new(
        registry: Registry,
        default_base_url: &str,
        url_suffix: &'static str,
    ) -> Self {
        let config = Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(TIMEOUT_SECS)))
            .build();
        Self {
            agent: config.into(),
            base_url: default_base_url.to_owned(),
            registry,
            url_suffix,
        }
    }

    /// Create a new HTTP client with a custom base URL (for testing).
    #[cfg(test)]
    pub(crate) fn with_base_url(
        registry: Registry,
        base_url: &str,
        url_suffix: &'static str,
    ) -> Self {
        let config = Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(TIMEOUT_SECS)))
            .build();
        Self {
            agent: config.into(),
            base_url: base_url.to_owned(),
            registry,
            url_suffix,
        }
    }

    /// The current base URL (useful for assertions in tests).
    #[cfg(test)]
    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Perform a GET request for the given package and return the raw
    /// response body as a string.
    ///
    /// Handles HTTP errors (status codes, timeouts) and body reading,
    /// mapping everything into [`RegistryError`].
    pub(crate) fn fetch_raw(&self, package_name: &str) -> Result<String, RegistryError> {
        let url = format!("{}/{}{}", self.base_url, package_name, self.url_suffix);

        let response = self
            .agent
            .get(&url)
            .header("User-Agent", USER_AGENT)
            .call()
            .map_err(|e| map_ureq_error(package_name, self.registry, e))?;

        response
            .into_body()
            .read_to_string()
            .map_err(|e| RegistryError::ParseError {
                package: package_name.to_owned(),
                registry: self.registry,
                reason: format!("failed to read response body: {e}"),
            })
    }
}

/// Map a [`ureq::Error`] to our [`RegistryError`].
fn map_ureq_error(package_name: &str, registry: Registry, err: ureq::Error) -> RegistryError {
    match err {
        ureq::Error::StatusCode(404) => RegistryError::NotFound {
            package: package_name.to_owned(),
            registry,
        },
        ureq::Error::StatusCode(code) => RegistryError::StatusError {
            package: package_name.to_owned(),
            registry,
            status: code,
        },
        other => RegistryError::HttpError {
            package: package_name.to_owned(),
            registry,
            reason: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_as_str() {
        assert_eq!(Registry::CratesIo.as_str(), "crates_io");
        assert_eq!(Registry::Npm.as_str(), "npm");
        assert_eq!(Registry::PyPI.as_str(), "pypi");
    }

    #[test]
    fn registry_display() {
        assert_eq!(format!("{}", Registry::CratesIo), "crates_io");
        assert_eq!(format!("{}", Registry::Npm), "npm");
        assert_eq!(format!("{}", Registry::PyPI), "pypi");
    }

    #[test]
    fn registry_from_str_valid() {
        assert_eq!("crates_io".parse(), Ok(Registry::CratesIo));
        assert_eq!("npm".parse(), Ok(Registry::Npm));
        assert_eq!("pypi".parse(), Ok(Registry::PyPI));
    }

    #[test]
    fn registry_from_str_invalid() {
        let result = "maven".parse::<Registry>();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown registry"));
    }

    #[test]
    fn registry_serde_roundtrip() {
        let json = serde_json::to_string(&Registry::CratesIo).unwrap();
        assert_eq!(json, "\"crates_io\"");
        let parsed: Registry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, Registry::CratesIo);
    }

    #[test]
    fn registry_serde_all_variants() {
        for r in [Registry::CratesIo, Registry::Npm, Registry::PyPI] {
            let json = serde_json::to_string(&r).unwrap();
            let back: Registry = serde_json::from_str(&json).unwrap();
            assert_eq!(back, r);
        }
    }

    #[test]
    fn package_metadata_construction() {
        let meta = PackageMetadata {
            name: "serde".to_owned(),
            registry: Registry::CratesIo,
            categories: vec!["parsing".to_owned()],
            keywords: vec!["serialization".to_owned()],
            description: Some("A serialization framework".to_owned()),
        };
        assert_eq!(meta.name, "serde");
        assert_eq!(meta.registry, Registry::CratesIo);
        assert!(!meta.categories.is_empty());
        assert!(!meta.keywords.is_empty());
        assert_eq!(
            meta.description,
            Some("A serialization framework".to_owned())
        );
    }

    #[test]
    fn package_metadata_no_description() {
        let meta = PackageMetadata {
            name: "foo".to_owned(),
            registry: Registry::Npm,
            categories: vec![],
            keywords: vec![],
            description: None,
        };
        assert_eq!(meta.description, None);
    }

    #[test]
    fn registry_error_display_not_found() {
        let err = RegistryError::NotFound {
            package: "foobar123".to_owned(),
            registry: Registry::CratesIo,
        };
        let s = err.to_string();
        assert!(s.contains("foobar123"));
        assert!(s.contains("crates_io"));
        assert!(s.contains("not found"));
    }

    #[test]
    fn registry_error_display_http_error() {
        let err = RegistryError::HttpError {
            package: "baz".to_owned(),
            registry: Registry::Npm,
            reason: "connection timeout".to_owned(),
        };
        let s = err.to_string();
        assert!(s.contains("baz"));
        assert!(s.contains("timeout"));
        assert!(s.contains("HTTP error"));
    }

    #[test]
    fn registry_error_display_status_error() {
        let err = RegistryError::StatusError {
            package: "pkg".to_owned(),
            registry: Registry::PyPI,
            status: 500,
        };
        let s = err.to_string();
        assert!(s.contains("500"));
        assert!(s.contains("pypi"));
        assert!(s.contains("pkg"));
    }

    #[test]
    fn registry_error_display_parse_error() {
        let err = RegistryError::ParseError {
            package: "pkg".to_owned(),
            registry: Registry::Npm,
            reason: "invalid JSON".to_owned(),
        };
        let s = err.to_string();
        assert!(s.contains("invalid JSON"));
    }

    #[test]
    fn cache_ttl_is_30_days() {
        assert_eq!(CACHE_TTL_SECS, 30 * 24 * 60 * 60);
    }

    #[test]
    fn registry_http_client_new() {
        let client = RegistryHttpClient::new(Registry::CratesIo, "https://crates.io/api/v1", "");
        assert_eq!(client.base_url(), "https://crates.io/api/v1");
    }

    #[test]
    fn registry_http_client_with_base_url() {
        let client =
            RegistryHttpClient::with_base_url(Registry::Npm, "https://registry.npmjs.org", "");
        assert_eq!(client.base_url(), "https://registry.npmjs.org");
    }

    #[test]
    fn map_ureq_error_transport_error() {
        // Trigger a connection error by connecting to a non-listening port.
        let result = ureq::get("http://127.0.0.1:1/nonexistent").call();
        assert!(result.is_err());
        let cli_err = map_ureq_error("testpkg", Registry::CratesIo, result.unwrap_err());
        assert!(matches!(cli_err, RegistryError::HttpError { .. }));
        assert!(cli_err.to_string().contains("testpkg"));
    }

    #[test]
    fn fetch_raw_connection_error_returns_http_error() {
        let client = RegistryHttpClient::with_base_url(Registry::Npm, "http://127.0.0.1:1", "");
        let result = client.fetch_raw("some-package");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RegistryError::HttpError { .. }
        ));
    }
}
