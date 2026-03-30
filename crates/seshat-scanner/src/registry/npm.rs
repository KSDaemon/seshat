//! npm registry client implementation.
//!
//! Fetches package metadata from <https://registry.npmjs.org/{name}>,
//! extracting `keywords[]`.

use std::time::Duration;

use serde::Deserialize;
use ureq::Agent;

use super::{PackageMetadata, PackageRegistryClient, Registry, RegistryError};

/// Default base URL for the npm registry API.
const DEFAULT_BASE_URL: &str = "https://registry.npmjs.org";

/// User-Agent header value.
const USER_AGENT: &str = concat!("seshat/", env!("CARGO_PKG_VERSION"));

/// Request timeout in seconds.
const TIMEOUT_SECS: u64 = 5;

/// Client for fetching package metadata from the npm registry.
///
/// Uses the npm registry API to retrieve keywords for Node.js packages.
/// Note: npm does not have a structured categories system like crates.io;
/// only keywords are available.
pub struct NpmClient {
    agent: Agent,
    base_url: String,
}

impl NpmClient {
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

impl Default for NpmClient {
    fn default() -> Self {
        Self::new()
    }
}

impl PackageRegistryClient for NpmClient {
    #[tracing::instrument(skip(self), fields(registry = "npm"))]
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
                    registry: Registry::Npm,
                    reason: format!("failed to read response body: {e}"),
                })?;

        parse_npm_response(package_name, &body)
    }
}

/// Map a ureq error to our [`RegistryError`].
fn map_ureq_error(package_name: &str, err: ureq::Error) -> RegistryError {
    match err {
        ureq::Error::StatusCode(404) => RegistryError::NotFound {
            package: package_name.to_owned(),
            registry: Registry::Npm,
        },
        ureq::Error::StatusCode(code) => RegistryError::StatusError {
            package: package_name.to_owned(),
            registry: Registry::Npm,
            status: code,
        },
        other => RegistryError::HttpError {
            package: package_name.to_owned(),
            registry: Registry::Npm,
            reason: other.to_string(),
        },
    }
}

// ─── JSON response types for deserialization ───────────────────────────

#[derive(Deserialize)]
struct NpmResponse {
    name: String,
    description: Option<String>,
    /// Keywords may be missing entirely from the response.
    #[serde(default)]
    keywords: Option<Vec<String>>,
}

/// Parse an npm registry JSON response into [`PackageMetadata`].
///
/// Extracted as a standalone function for unit testing without HTTP.
fn parse_npm_response(package_name: &str, json: &str) -> Result<PackageMetadata, RegistryError> {
    let resp: NpmResponse = serde_json::from_str(json).map_err(|e| RegistryError::ParseError {
        package: package_name.to_owned(),
        registry: Registry::Npm,
        reason: e.to_string(),
    })?;

    Ok(PackageMetadata {
        name: resp.name,
        registry: Registry::Npm,
        // npm has no structured categories — only keywords
        categories: Vec::new(),
        keywords: resp.keywords.unwrap_or_default(),
        description: resp.description,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Realistic npm response for `express`.
    const EXPRESS_RESPONSE: &str = r#"{
        "name": "express",
        "description": "Fast, unopinionated, minimalist web framework",
        "keywords": ["express", "framework", "sinatra", "web", "http", "rest", "restful", "router", "app", "api"],
        "dist-tags": {"latest": "4.18.2"},
        "versions": {}
    }"#;

    /// Response for a package with no keywords.
    const NO_KEYWORDS_RESPONSE: &str = r#"{
        "name": "tiny-pkg",
        "description": "A tiny package"
    }"#;

    /// Response for a package with null description and empty keywords.
    const NULL_DESC_RESPONSE: &str = r#"{
        "name": "mystery-pkg",
        "description": null,
        "keywords": []
    }"#;

    /// Scoped package response.
    const SCOPED_RESPONSE: &str = r#"{
        "name": "@types/node",
        "description": "TypeScript definitions for Node.js",
        "keywords": ["typescript", "types", "node"]
    }"#;

    #[test]
    fn parse_express_response() {
        let meta = parse_npm_response("express", EXPRESS_RESPONSE).unwrap();
        assert_eq!(meta.name, "express");
        assert_eq!(meta.registry, Registry::Npm);
        // npm has no categories
        assert!(meta.categories.is_empty());
        assert!(meta.keywords.contains(&"express".to_owned()));
        assert!(meta.keywords.contains(&"web".to_owned()));
        assert!(meta.keywords.contains(&"http".to_owned()));
        assert_eq!(
            meta.description.as_deref(),
            Some("Fast, unopinionated, minimalist web framework")
        );
    }

    #[test]
    fn parse_no_keywords() {
        let meta = parse_npm_response("tiny-pkg", NO_KEYWORDS_RESPONSE).unwrap();
        assert_eq!(meta.name, "tiny-pkg");
        assert!(meta.keywords.is_empty());
        assert_eq!(meta.description.as_deref(), Some("A tiny package"));
    }

    #[test]
    fn parse_null_description() {
        let meta = parse_npm_response("mystery-pkg", NULL_DESC_RESPONSE).unwrap();
        assert_eq!(meta.name, "mystery-pkg");
        assert!(meta.keywords.is_empty());
        assert!(meta.description.is_none());
    }

    #[test]
    fn parse_scoped_package() {
        let meta = parse_npm_response("@types/node", SCOPED_RESPONSE).unwrap();
        assert_eq!(meta.name, "@types/node");
        assert_eq!(meta.keywords, vec!["typescript", "types", "node"]);
    }

    #[test]
    fn parse_invalid_json() {
        let result = parse_npm_response("bad", "not json");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RegistryError::ParseError { .. }
        ));
    }

    #[test]
    fn parse_missing_name_field() {
        let json = r#"{"description": "no name"}"#;
        let result = parse_npm_response("x", json);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RegistryError::ParseError { .. }
        ));
    }

    #[test]
    fn client_has_correct_defaults() {
        let client = NpmClient::new();
        assert_eq!(client.base_url, DEFAULT_BASE_URL);
    }

    #[test]
    fn client_with_custom_base_url() {
        let client = NpmClient::with_base_url("http://localhost:9999");
        assert_eq!(client.base_url, "http://localhost:9999");
    }
}
