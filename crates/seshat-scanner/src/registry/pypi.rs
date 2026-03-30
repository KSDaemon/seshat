//! PyPI registry client implementation.
//!
//! Fetches package metadata from <https://pypi.org/pypi/{name}/json>,
//! extracting `classifiers[]` and `keywords`.

use serde::Deserialize;

use super::{PackageMetadata, PackageRegistryClient, Registry, RegistryError, RegistryHttpClient};

/// Default base URL for the PyPI JSON API.
const DEFAULT_BASE_URL: &str = "https://pypi.org/pypi";

/// Client for fetching package metadata from PyPI.
///
/// Uses the PyPI JSON API to retrieve classifiers and keywords
/// for Python packages.
pub struct PyPIClient {
    inner: RegistryHttpClient,
}

impl PyPIClient {
    /// Creates a new client with default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RegistryHttpClient::new(Registry::PyPI, DEFAULT_BASE_URL, "/json"),
        }
    }

    /// Creates a new client with a custom base URL (for testing).
    #[cfg(test)]
    fn with_base_url(base_url: &str) -> Self {
        Self {
            inner: RegistryHttpClient::with_base_url(Registry::PyPI, base_url, "/json"),
        }
    }
}

impl Default for PyPIClient {
    fn default() -> Self {
        Self::new()
    }
}

impl PackageRegistryClient for PyPIClient {
    #[tracing::instrument(skip(self), fields(registry = "pypi"))]
    fn fetch_metadata(&self, package_name: &str) -> Result<PackageMetadata, RegistryError> {
        let body = self.inner.fetch_raw(package_name)?;
        parse_pypi_response(package_name, &body)
    }
}

// ─── JSON response types for deserialization ───────────────────────────

#[derive(Deserialize)]
struct PyPIResponse {
    info: PyPIInfo,
}

#[derive(Deserialize)]
struct PyPIInfo {
    name: String,
    summary: Option<String>,
    /// PyPI classifiers (e.g., "Topic :: Software Development :: Libraries").
    #[serde(default)]
    classifiers: Vec<String>,
    /// Comma-separated or already split keywords. PyPI returns this as a
    /// single string (comma-separated) or null.
    keywords: Option<String>,
}

/// Parse a PyPI JSON API response into [`PackageMetadata`].
///
/// Extracted as a standalone function for unit testing without HTTP.
///
/// PyPI classifiers are stored in `categories` (they serve a similar
/// organizational purpose to crates.io categories). PyPI keywords are
/// stored as a comma-separated string and split into individual entries.
fn parse_pypi_response(package_name: &str, json: &str) -> Result<PackageMetadata, RegistryError> {
    let resp: PyPIResponse = serde_json::from_str(json).map_err(|e| RegistryError::ParseError {
        package: package_name.to_owned(),
        registry: Registry::PyPI,
        reason: e.to_string(),
    })?;

    // PyPI keywords come as a single comma-separated string.
    // Split and trim, filtering out empty segments.
    let keywords = resp
        .info
        .keywords
        .map(|kw| {
            kw.split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(PackageMetadata {
        name: resp.info.name,
        registry: Registry::PyPI,
        categories: resp.info.classifiers,
        keywords,
        description: resp.info.summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Realistic PyPI response for `flask`.
    const FLASK_RESPONSE: &str = r#"{
        "info": {
            "name": "Flask",
            "summary": "A simple framework for building complex web applications.",
            "classifiers": [
                "Development Status :: 5 - Production/Stable",
                "Environment :: Web Environment",
                "Framework :: Flask",
                "Intended Audience :: Developers",
                "License :: OSI Approved :: BSD License",
                "Operating System :: OS Independent",
                "Programming Language :: Python",
                "Topic :: Internet :: WWW/HTTP :: Dynamic Content",
                "Topic :: Internet :: WWW/HTTP :: WSGI :: Application",
                "Topic :: Software Development :: Libraries :: Application Frameworks"
            ],
            "keywords": "flask,web,framework,wsgi"
        },
        "releases": {},
        "urls": []
    }"#;

    /// Response for a package with no keywords and minimal classifiers.
    const MINIMAL_RESPONSE: &str = r#"{
        "info": {
            "name": "tiny-lib",
            "summary": null,
            "classifiers": [],
            "keywords": null
        },
        "releases": {}
    }"#;

    /// Response with keywords that have extra whitespace.
    const WHITESPACE_KEYWORDS_RESPONSE: &str = r#"{
        "info": {
            "name": "messy-pkg",
            "summary": "A messy package",
            "classifiers": ["Programming Language :: Python :: 3"],
            "keywords": " async , http ,  web , ,  "
        }
    }"#;

    /// Response for a package with an empty keywords string.
    const EMPTY_KEYWORDS_RESPONSE: &str = r#"{
        "info": {
            "name": "empty-kw",
            "summary": "Empty keywords",
            "classifiers": [],
            "keywords": ""
        }
    }"#;

    #[test]
    fn parse_flask_response() {
        let meta = parse_pypi_response("flask", FLASK_RESPONSE).unwrap();
        assert_eq!(meta.name, "Flask");
        assert_eq!(meta.registry, Registry::PyPI);
        assert_eq!(meta.categories.len(), 10);
        assert!(meta.categories.contains(&"Framework :: Flask".to_owned()));
        assert!(
            meta.categories
                .contains(&"Environment :: Web Environment".to_owned())
        );
        assert_eq!(meta.keywords, vec!["flask", "web", "framework", "wsgi"]);
        assert_eq!(
            meta.description.as_deref(),
            Some("A simple framework for building complex web applications.")
        );
    }

    #[test]
    fn parse_minimal_response() {
        let meta = parse_pypi_response("tiny-lib", MINIMAL_RESPONSE).unwrap();
        assert_eq!(meta.name, "tiny-lib");
        assert!(meta.categories.is_empty());
        assert!(meta.keywords.is_empty());
        assert!(meta.description.is_none());
    }

    #[test]
    fn parse_whitespace_keywords() {
        let meta = parse_pypi_response("messy-pkg", WHITESPACE_KEYWORDS_RESPONSE).unwrap();
        assert_eq!(meta.keywords, vec!["async", "http", "web"]);
        assert_eq!(meta.categories, vec!["Programming Language :: Python :: 3"]);
    }

    #[test]
    fn parse_empty_keywords_string() {
        let meta = parse_pypi_response("empty-kw", EMPTY_KEYWORDS_RESPONSE).unwrap();
        assert!(meta.keywords.is_empty());
    }

    #[test]
    fn parse_invalid_json() {
        let result = parse_pypi_response("bad", "not json");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RegistryError::ParseError { .. }
        ));
    }

    #[test]
    fn parse_missing_info_field() {
        let json = r#"{"releases": {}}"#;
        let result = parse_pypi_response("x", json);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RegistryError::ParseError { .. }
        ));
    }

    #[test]
    fn client_has_correct_defaults() {
        let client = PyPIClient::new();
        assert_eq!(client.inner.base_url(), DEFAULT_BASE_URL);
    }

    #[test]
    fn client_with_custom_base_url() {
        let client = PyPIClient::with_base_url("http://localhost:9999");
        assert_eq!(client.inner.base_url(), "http://localhost:9999");
    }
}
