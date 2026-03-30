//! crates.io registry client implementation.
//!
//! Fetches package metadata from <https://crates.io/api/v1/crates/{name}>,
//! extracting `categories[].slug` and `keywords[]`.

use std::time::Duration;

use serde::Deserialize;
use ureq::Agent;

use super::{PackageMetadata, PackageRegistryClient, Registry, RegistryError};

/// Default base URL for the crates.io API.
const DEFAULT_BASE_URL: &str = "https://crates.io/api/v1/crates";

/// User-Agent header value per crates.io API policy.
const USER_AGENT: &str = concat!("seshat/", env!("CARGO_PKG_VERSION"));

/// Request timeout in seconds.
const TIMEOUT_SECS: u64 = 5;

/// Client for fetching package metadata from crates.io.
///
/// Uses the crates.io REST API to retrieve categories and keywords
/// for Rust crates.
pub struct CratesIoClient {
    agent: Agent,
    base_url: String,
}

impl CratesIoClient {
    /// Creates a new client with default configuration.
    #[must_use]
    pub fn new() -> Self {
        let config = Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(TIMEOUT_SECS)))
            .build();
        Self {
            agent: config.into(),
            base_url: DEFAULT_BASE_URL.to_owned(),
        }
    }

    /// Creates a new client with a custom base URL (for testing).
    #[cfg(test)]
    fn with_base_url(base_url: &str) -> Self {
        let config = Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(TIMEOUT_SECS)))
            .build();
        Self {
            agent: config.into(),
            base_url: base_url.to_owned(),
        }
    }
}

impl Default for CratesIoClient {
    fn default() -> Self {
        Self::new()
    }
}

impl PackageRegistryClient for CratesIoClient {
    #[tracing::instrument(skip(self), fields(registry = "crates_io"))]
    fn fetch_metadata(&self, package_name: &str) -> Result<PackageMetadata, RegistryError> {
        let url = format!("{}/{}", self.base_url, package_name);

        let response = self
            .agent
            .get(&url)
            .header("User-Agent", USER_AGENT)
            .call()
            .map_err(|e| map_ureq_error(package_name, e))?;

        let body =
            response
                .into_body()
                .read_to_string()
                .map_err(|e| RegistryError::ParseError {
                    package: package_name.to_owned(),
                    registry: Registry::CratesIo,
                    reason: format!("failed to read response body: {e}"),
                })?;

        parse_crates_io_response(package_name, &body)
    }
}

/// Map a ureq error to our [`RegistryError`].
fn map_ureq_error(package_name: &str, err: ureq::Error) -> RegistryError {
    match err {
        ureq::Error::StatusCode(404) => RegistryError::NotFound {
            package: package_name.to_owned(),
            registry: Registry::CratesIo,
        },
        ureq::Error::StatusCode(code) => RegistryError::StatusError {
            package: package_name.to_owned(),
            registry: Registry::CratesIo,
            status: code,
        },
        other => RegistryError::HttpError {
            package: package_name.to_owned(),
            registry: Registry::CratesIo,
            reason: other.to_string(),
        },
    }
}

// ─── JSON response types for deserialization ───────────────────────────

#[derive(Deserialize)]
struct CratesIoResponse {
    #[serde(rename = "crate")]
    krate: CrateData,
    categories: Vec<CategoryData>,
}

#[derive(Deserialize)]
struct CrateData {
    name: String,
    description: Option<String>,
    keywords: Vec<String>,
}

#[derive(Deserialize)]
struct CategoryData {
    slug: String,
}

/// Parse a crates.io JSON response into [`PackageMetadata`].
///
/// Extracted as a standalone function for unit testing without HTTP.
fn parse_crates_io_response(
    package_name: &str,
    json: &str,
) -> Result<PackageMetadata, RegistryError> {
    let resp: CratesIoResponse =
        serde_json::from_str(json).map_err(|e| RegistryError::ParseError {
            package: package_name.to_owned(),
            registry: Registry::CratesIo,
            reason: e.to_string(),
        })?;

    Ok(PackageMetadata {
        name: resp.krate.name,
        registry: Registry::CratesIo,
        categories: resp.categories.into_iter().map(|c| c.slug).collect(),
        keywords: resp.krate.keywords,
        description: resp.krate.description,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Realistic crates.io response for `serde`.
    const SERDE_RESPONSE: &str = r#"{
        "crate": {
            "name": "serde",
            "description": "A generic serialization/deserialization framework",
            "keywords": ["serde", "serialization", "no_std"],
            "id": "serde",
            "max_version": "1.0.200"
        },
        "categories": [
            {"slug": "encoding", "id": "encoding", "category": "Encoding"},
            {"slug": "no-std", "id": "no-std", "category": "No standard library"}
        ]
    }"#;

    /// Response for a crate with no categories or keywords.
    const EMPTY_METADATA_RESPONSE: &str = r#"{
        "crate": {
            "name": "tiny-crate",
            "description": null,
            "keywords": [],
            "id": "tiny-crate",
            "max_version": "0.1.0"
        },
        "categories": []
    }"#;

    /// Response with multiple categories and keywords.
    const RICH_RESPONSE: &str = r#"{
        "crate": {
            "name": "tokio",
            "description": "An event-driven, non-blocking I/O platform",
            "keywords": ["io", "async", "non-blocking", "futures"],
            "id": "tokio",
            "max_version": "1.40.0"
        },
        "categories": [
            {"slug": "asynchronous", "id": "asynchronous", "category": "Asynchronous"},
            {"slug": "network-programming", "id": "network-programming", "category": "Network programming"}
        ]
    }"#;

    #[test]
    fn parse_serde_response() {
        let meta = parse_crates_io_response("serde", SERDE_RESPONSE).unwrap();
        assert_eq!(meta.name, "serde");
        assert_eq!(meta.registry, Registry::CratesIo);
        assert_eq!(meta.categories, vec!["encoding", "no-std"]);
        assert_eq!(meta.keywords, vec!["serde", "serialization", "no_std"]);
        assert_eq!(
            meta.description.as_deref(),
            Some("A generic serialization/deserialization framework")
        );
    }

    #[test]
    fn parse_empty_metadata() {
        let meta = parse_crates_io_response("tiny-crate", EMPTY_METADATA_RESPONSE).unwrap();
        assert_eq!(meta.name, "tiny-crate");
        assert_eq!(meta.registry, Registry::CratesIo);
        assert!(meta.categories.is_empty());
        assert!(meta.keywords.is_empty());
        assert!(meta.description.is_none());
    }

    #[test]
    fn parse_rich_response() {
        let meta = parse_crates_io_response("tokio", RICH_RESPONSE).unwrap();
        assert_eq!(meta.name, "tokio");
        assert_eq!(meta.categories, vec!["asynchronous", "network-programming"]);
        assert_eq!(
            meta.keywords,
            vec!["io", "async", "non-blocking", "futures"]
        );
        assert_eq!(
            meta.description.as_deref(),
            Some("An event-driven, non-blocking I/O platform")
        );
    }

    #[test]
    fn parse_invalid_json() {
        let result = parse_crates_io_response("bad", "not json");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, RegistryError::ParseError { .. }));
    }

    #[test]
    fn parse_missing_fields() {
        // Missing required `keywords` field in crate data
        let json = r#"{"crate": {"name": "x"}, "categories": []}"#;
        let result = parse_crates_io_response("x", json);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RegistryError::ParseError { .. }
        ));
    }

    #[test]
    fn client_has_correct_defaults() {
        let client = CratesIoClient::new();
        assert_eq!(client.base_url, DEFAULT_BASE_URL);
    }

    #[test]
    fn client_with_custom_base_url() {
        let client = CratesIoClient::with_base_url("http://localhost:9999");
        assert_eq!(client.base_url, "http://localhost:9999");
    }
}
