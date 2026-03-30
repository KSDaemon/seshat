//! Category/classifier-to-[`DependencyDomain`] mapping rules and three-tier lookup.
//!
//! Provides ~30 mapping rules that translate registry-specific category strings
//! (crates.io slugs, PyPI classifiers, npm keywords) into the unified
//! [`DependencyDomain`] taxonomy. Also implements the three-tier classification
//! pipeline:
//!
//! 1. **Cache hit** — check SQLite `package_metadata` table
//! 2. **Registry fetch** — query the registry API, cache the result, then map
//! 3. **Hardcoded fallback** — use existing `categorize_dependency()` / `classify_domain()`

use seshat_core::DependencyDomain;
use seshat_storage::{PackageMetadataRepository, PackageMetadataRow};

use super::{CACHE_TTL_SECS, PackageMetadata, PackageRegistryClient, Registry};

// ---------------------------------------------------------------------------
// Confidence levels for classification results
// ---------------------------------------------------------------------------

/// How confident we are in a domain classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ClassificationConfidence {
    /// Registry metadata confirmed the classification (tier 2).
    Registry,
    /// Hardcoded fallback list (tier 3) — still good, but less adaptable.
    Fallback,
}

/// A domain classification with its confidence level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassificationResult {
    pub domain: DependencyDomain,
    pub confidence: ClassificationConfidence,
}

// ---------------------------------------------------------------------------
// Category → DependencyDomain mapping rules (~30 rules)
// ---------------------------------------------------------------------------

/// Map a crates.io category slug to a [`DependencyDomain`].
///
/// Category slugs are lowercase, hyphen-separated strings like
/// `"web-programming::http-client"` or `"asynchronous"`.
/// See <https://crates.io/categories> for the full list.
#[tracing::instrument]
pub fn map_crates_io_category(slug: &str) -> Option<DependencyDomain> {
    // Exact matches first, then prefix matches for nested categories.
    match slug {
        // HTTP / networking
        "web-programming::http-client" => Some(DependencyDomain::Http),
        // Web frameworks
        "web-programming::http-server" | "web-programming::websocket" => {
            Some(DependencyDomain::WebFramework)
        }
        // Logging / observability
        "development-tools::debugging" => Some(DependencyDomain::Logging),
        // Testing
        "development-tools::testing" => Some(DependencyDomain::Testing),
        // Serialization / encoding
        "encoding" | "parser-implementations" => Some(DependencyDomain::Serialization),
        // Database
        "database" | "database-implementations" => Some(DependencyDomain::Database),
        // CLI
        "command-line-interface" | "command-line-utilities" => Some(DependencyDomain::Cli),
        // Async
        "asynchronous" | "concurrency" => Some(DependencyDomain::AsyncRuntime),
        // Crypto
        "cryptography" | "authentication" => Some(DependencyDomain::Crypto),
        _ => {
            // Prefix-based fallback for nested categories
            if slug.starts_with("web-programming") {
                Some(DependencyDomain::WebFramework)
            } else if slug.starts_with("database") {
                Some(DependencyDomain::Database)
            } else {
                None
            }
        }
    }
}

/// Map a PyPI classifier string to a [`DependencyDomain`].
///
/// PyPI classifiers use `" :: "` as delimiters, e.g.
/// `"Topic :: Internet :: WWW/HTTP :: Dynamic Content"`.
/// See <https://pypi.org/classifiers/> for the full list.
#[tracing::instrument]
pub fn map_pypi_classifier(classifier: &str) -> Option<DependencyDomain> {
    let lower = classifier.to_lowercase();

    // Framework classifiers are strong signals
    if lower.contains("framework :: flask")
        || lower.contains("framework :: django")
        || lower.contains("framework :: fastapi")
        || lower.contains("framework :: tornado")
        || lower.contains("framework :: pyramid")
        || lower.contains("framework :: bottle")
    {
        return Some(DependencyDomain::WebFramework);
    }

    // Topic-based classifiers
    if lower.contains("topic :: internet :: www/http") {
        // Could be either Http client or WebFramework.
        // If "framework" was already matched above, this won't fire.
        // Default to Http for generic HTTP topic.
        return Some(DependencyDomain::Http);
    }
    if lower.contains("topic :: software development :: testing") {
        return Some(DependencyDomain::Testing);
    }
    if lower.contains("topic :: system :: logging") {
        return Some(DependencyDomain::Logging);
    }
    if lower.contains("topic :: database") {
        return Some(DependencyDomain::Database);
    }
    if lower.contains("topic :: security :: cryptography")
        || lower.contains("topic :: security")
            && !lower.contains("topic :: security :: cryptography")
    {
        return Some(DependencyDomain::Crypto);
    }
    if lower.contains("topic :: terminals") || lower.contains("environment :: console") {
        return Some(DependencyDomain::Cli);
    }

    None
}

/// Map a keyword (from any registry) to a [`DependencyDomain`].
///
/// Keywords are author-defined free-text labels. We match on common patterns
/// that strongly correlate with a domain. This is less precise than categories
/// so it is only used when category mapping yields no result.
#[tracing::instrument]
pub fn map_keyword(keyword: &str) -> Option<DependencyDomain> {
    let lower = keyword.to_lowercase();
    match lower.as_str() {
        // HTTP
        "http-client" | "http" | "rest-client" | "rest" | "fetch" => Some(DependencyDomain::Http),
        // Web frameworks
        "web-framework" | "framework" | "web-server" | "webapp" | "web-app" | "websocket"
        | "wasm" => Some(DependencyDomain::WebFramework),
        // Logging
        "logging" | "logger" | "tracing" | "observability" | "telemetry" | "log" => {
            Some(DependencyDomain::Logging)
        }
        // Testing
        "testing" | "test" | "mock" | "mocking" | "assertion" | "test-framework" | "tdd"
        | "bdd" => Some(DependencyDomain::Testing),
        // Serialization
        "serialization" | "deserialization" | "serde" | "json" | "yaml" | "toml" | "protobuf"
        | "msgpack" | "encoding" | "codec" | "csv" => Some(DependencyDomain::Serialization),
        // Database
        "database" | "sql" | "orm" | "nosql" | "mongodb" | "postgres" | "mysql" | "sqlite"
        | "redis" => Some(DependencyDomain::Database),
        // CLI
        "cli" | "command-line" | "terminal" | "argument-parser" | "args" => {
            Some(DependencyDomain::Cli)
        }
        // Async
        "async" | "async-runtime" | "futures" | "non-blocking" | "concurrency" | "tokio"
        | "asyncio" => Some(DependencyDomain::AsyncRuntime),
        // Crypto
        "crypto" | "cryptography" | "encryption" | "hashing" | "tls" | "ssl" | "security" => {
            Some(DependencyDomain::Crypto)
        }
        // Validation
        "validation" | "validator" | "schema" | "schema-validation" => {
            Some(DependencyDomain::Validation)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Aggregate mapping: categories + keywords → DependencyDomain
// ---------------------------------------------------------------------------

/// Attempt to infer a [`DependencyDomain`] from package registry metadata.
///
/// Checks categories/classifiers first (stronger signal), then falls back
/// to keywords. Returns `None` if no mapping matched.
#[tracing::instrument(skip(metadata))]
pub fn infer_domain_from_metadata(metadata: &PackageMetadata) -> Option<DependencyDomain> {
    // Try categories first (structured, higher confidence).
    for cat in &metadata.categories {
        let result = match metadata.registry {
            Registry::CratesIo => map_crates_io_category(cat),
            Registry::PyPI => map_pypi_classifier(cat),
            Registry::Npm => None, // npm has no structured categories
        };
        if result.is_some() {
            return result;
        }
    }

    // Fall back to keywords (unstructured, lower confidence).
    for kw in &metadata.keywords {
        let result = map_keyword(kw);
        if result.is_some() {
            return result;
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Three-tier lookup
// ---------------------------------------------------------------------------

/// Classify a dependency using the three-tier lookup pipeline:
///
/// 1. **SQLite cache** — check `package_metadata` table for a cached entry
///    that is not stale (< 30 days old). If found, map its metadata.
/// 2. **Registry API** — fetch metadata from the appropriate registry,
///    cache it, then map.
/// 3. **Hardcoded fallback** — use `hardcoded_domain` (the result of existing
///    `categorize_dependency()` / `classify_domain()`).
///
/// Returns a [`ClassificationResult`] with the domain and confidence level.
/// Known-library matches from the hardcoded list keep their high confidence.
/// Registry-inferred matches have `Registry` confidence.
/// If all tiers fail, returns `DependencyDomain::Unknown` with `Fallback` confidence.
#[tracing::instrument(skip(repo, client))]
pub fn classify_with_registry(
    package_name: &str,
    registry: Registry,
    hardcoded_domain: DependencyDomain,
    repo: &dyn PackageMetadataRepository,
    client: &dyn PackageRegistryClient,
    now_unix: i64,
) -> ClassificationResult {
    // If the hardcoded list already knows this package → return immediately.
    // Known-library matches are high confidence and don't need registry validation.
    if hardcoded_domain != DependencyDomain::Unknown {
        return ClassificationResult {
            domain: hardcoded_domain,
            confidence: ClassificationConfidence::Fallback,
        };
    }

    // Tier 1: SQLite cache
    if let Some(domain) = try_cache_lookup(package_name, registry, repo, now_unix) {
        return ClassificationResult {
            domain,
            confidence: ClassificationConfidence::Registry,
        };
    }

    // Tier 2: Registry API fetch + cache
    if let Some(domain) = try_registry_fetch(package_name, registry, repo, client, now_unix) {
        return ClassificationResult {
            domain,
            confidence: ClassificationConfidence::Registry,
        };
    }

    // Tier 3: Hardcoded fallback (already Unknown at this point)
    ClassificationResult {
        domain: DependencyDomain::Unknown,
        confidence: ClassificationConfidence::Fallback,
    }
}

/// Tier 1: Check the SQLite cache for a non-stale entry and try to map it.
fn try_cache_lookup(
    package_name: &str,
    registry: Registry,
    repo: &dyn PackageMetadataRepository,
    now_unix: i64,
) -> Option<DependencyDomain> {
    let row = repo.get(package_name, registry.as_str()).ok()??;

    // Check staleness
    if now_unix - row.fetched_at > CACHE_TTL_SECS {
        return None;
    }

    let metadata = row_to_metadata(row, registry);
    infer_domain_from_metadata(&metadata)
}

/// Tier 2: Fetch from registry, cache, and try to map.
fn try_registry_fetch(
    package_name: &str,
    registry: Registry,
    repo: &dyn PackageMetadataRepository,
    client: &dyn PackageRegistryClient,
    now_unix: i64,
) -> Option<DependencyDomain> {
    let metadata = match client.fetch_metadata(package_name) {
        Ok(m) => m,
        Err(_) => return None, // Network error → graceful fallback
    };

    // Infer domain *before* moving fields into the cache row to avoid clones.
    let domain = infer_domain_from_metadata(&metadata);

    // Cache the result (move fields instead of cloning).
    let row = PackageMetadataRow {
        name: package_name.to_owned(),
        registry: registry.as_str().to_owned(),
        categories: metadata.categories,
        keywords: metadata.keywords,
        description: metadata.description,
        fetched_at: now_unix,
    };
    // Best-effort cache; don't fail the classification if caching fails.
    let _ = repo.upsert(&row);

    domain
}

/// Convert a [`PackageMetadataRow`] to a [`PackageMetadata`] for mapping.
fn row_to_metadata(row: PackageMetadataRow, registry: Registry) -> PackageMetadata {
    PackageMetadata {
        name: row.name,
        registry,
        categories: row.categories,
        keywords: row.keywords,
        description: row.description,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::RegistryError;

    // -----------------------------------------------------------------------
    // Category mapping tests
    // -----------------------------------------------------------------------

    #[test]
    fn crates_io_http_client_category() {
        assert_eq!(
            map_crates_io_category("web-programming::http-client"),
            Some(DependencyDomain::Http)
        );
    }

    #[test]
    fn crates_io_http_server_category() {
        assert_eq!(
            map_crates_io_category("web-programming::http-server"),
            Some(DependencyDomain::WebFramework)
        );
    }

    #[test]
    fn crates_io_encoding_category() {
        assert_eq!(
            map_crates_io_category("encoding"),
            Some(DependencyDomain::Serialization)
        );
    }

    #[test]
    fn crates_io_database_category() {
        assert_eq!(
            map_crates_io_category("database"),
            Some(DependencyDomain::Database)
        );
    }

    #[test]
    fn crates_io_cli_category() {
        assert_eq!(
            map_crates_io_category("command-line-interface"),
            Some(DependencyDomain::Cli)
        );
    }

    #[test]
    fn crates_io_async_category() {
        assert_eq!(
            map_crates_io_category("asynchronous"),
            Some(DependencyDomain::AsyncRuntime)
        );
    }

    #[test]
    fn crates_io_crypto_category() {
        assert_eq!(
            map_crates_io_category("cryptography"),
            Some(DependencyDomain::Crypto)
        );
    }

    #[test]
    fn crates_io_testing_category() {
        assert_eq!(
            map_crates_io_category("development-tools::testing"),
            Some(DependencyDomain::Testing)
        );
    }

    #[test]
    fn crates_io_unknown_category() {
        assert_eq!(map_crates_io_category("no-std"), None);
    }

    #[test]
    fn crates_io_web_programming_prefix_fallback() {
        // Unknown web-programming sub-category → WebFramework
        assert_eq!(
            map_crates_io_category("web-programming::something-new"),
            Some(DependencyDomain::WebFramework)
        );
    }

    #[test]
    fn crates_io_database_prefix_fallback() {
        assert_eq!(
            map_crates_io_category("database-interfaces"),
            Some(DependencyDomain::Database)
        );
    }

    // -----------------------------------------------------------------------
    // PyPI classifier mapping tests
    // -----------------------------------------------------------------------

    #[test]
    fn pypi_flask_framework() {
        assert_eq!(
            map_pypi_classifier("Framework :: Flask"),
            Some(DependencyDomain::WebFramework)
        );
    }

    #[test]
    fn pypi_django_framework() {
        assert_eq!(
            map_pypi_classifier("Framework :: Django"),
            Some(DependencyDomain::WebFramework)
        );
    }

    #[test]
    fn pypi_www_http_topic() {
        assert_eq!(
            map_pypi_classifier("Topic :: Internet :: WWW/HTTP :: Dynamic Content"),
            Some(DependencyDomain::Http)
        );
    }

    #[test]
    fn pypi_testing_topic() {
        assert_eq!(
            map_pypi_classifier("Topic :: Software Development :: Testing"),
            Some(DependencyDomain::Testing)
        );
    }

    #[test]
    fn pypi_database_topic() {
        assert_eq!(
            map_pypi_classifier("Topic :: Database"),
            Some(DependencyDomain::Database)
        );
    }

    #[test]
    fn pypi_logging_topic() {
        assert_eq!(
            map_pypi_classifier("Topic :: System :: Logging"),
            Some(DependencyDomain::Logging)
        );
    }

    #[test]
    fn pypi_unrelated_classifier() {
        assert_eq!(
            map_pypi_classifier("Programming Language :: Python :: 3"),
            None
        );
    }

    // -----------------------------------------------------------------------
    // Keyword mapping tests
    // -----------------------------------------------------------------------

    #[test]
    fn keyword_http_client() {
        assert_eq!(map_keyword("http-client"), Some(DependencyDomain::Http));
    }

    #[test]
    fn keyword_web_framework() {
        assert_eq!(
            map_keyword("web-framework"),
            Some(DependencyDomain::WebFramework)
        );
    }

    #[test]
    fn keyword_testing() {
        assert_eq!(map_keyword("testing"), Some(DependencyDomain::Testing));
    }

    #[test]
    fn keyword_database() {
        assert_eq!(map_keyword("database"), Some(DependencyDomain::Database));
    }

    #[test]
    fn keyword_async() {
        assert_eq!(map_keyword("async"), Some(DependencyDomain::AsyncRuntime));
    }

    #[test]
    fn keyword_serialization() {
        assert_eq!(
            map_keyword("serialization"),
            Some(DependencyDomain::Serialization)
        );
    }

    #[test]
    fn keyword_cli() {
        assert_eq!(map_keyword("cli"), Some(DependencyDomain::Cli));
    }

    #[test]
    fn keyword_crypto() {
        assert_eq!(map_keyword("crypto"), Some(DependencyDomain::Crypto));
    }

    #[test]
    fn keyword_validation() {
        assert_eq!(
            map_keyword("validation"),
            Some(DependencyDomain::Validation)
        );
    }

    #[test]
    fn keyword_case_insensitive() {
        assert_eq!(map_keyword("HTTP"), Some(DependencyDomain::Http));
        assert_eq!(map_keyword("Testing"), Some(DependencyDomain::Testing));
    }

    #[test]
    fn keyword_unknown() {
        assert_eq!(map_keyword("foobar"), None);
    }

    // -----------------------------------------------------------------------
    // infer_domain_from_metadata tests
    // -----------------------------------------------------------------------

    #[test]
    fn infer_from_crates_io_categories() {
        let meta = PackageMetadata {
            name: "reqwest".to_owned(),
            registry: Registry::CratesIo,
            categories: vec!["web-programming::http-client".to_owned(), "wasm".to_owned()],
            keywords: vec!["http".to_owned()],
            description: Some("HTTP client".to_owned()),
        };
        assert_eq!(
            infer_domain_from_metadata(&meta),
            Some(DependencyDomain::Http)
        );
    }

    #[test]
    fn infer_from_pypi_classifiers() {
        let meta = PackageMetadata {
            name: "flask".to_owned(),
            registry: Registry::PyPI,
            categories: vec![
                "Framework :: Flask".to_owned(),
                "Topic :: Internet :: WWW/HTTP".to_owned(),
            ],
            keywords: vec!["web".to_owned()],
            description: None,
        };
        assert_eq!(
            infer_domain_from_metadata(&meta),
            Some(DependencyDomain::WebFramework)
        );
    }

    #[test]
    fn infer_falls_back_to_keywords() {
        let meta = PackageMetadata {
            name: "some-lib".to_owned(),
            registry: Registry::Npm,
            categories: vec![], // npm has no categories
            keywords: vec!["database".to_owned(), "orm".to_owned()],
            description: None,
        };
        assert_eq!(
            infer_domain_from_metadata(&meta),
            Some(DependencyDomain::Database)
        );
    }

    #[test]
    fn infer_no_match() {
        let meta = PackageMetadata {
            name: "mystery".to_owned(),
            registry: Registry::CratesIo,
            categories: vec!["no-std".to_owned()],
            keywords: vec!["utility".to_owned()],
            description: None,
        };
        assert_eq!(infer_domain_from_metadata(&meta), None);
    }

    // -----------------------------------------------------------------------
    // Three-tier lookup tests
    // -----------------------------------------------------------------------

    /// A mock registry client for testing.
    struct MockRegistryClient {
        response: Result<PackageMetadata, RegistryError>,
    }

    impl PackageRegistryClient for MockRegistryClient {
        fn fetch_metadata(&self, _package_name: &str) -> Result<PackageMetadata, RegistryError> {
            match &self.response {
                Ok(m) => Ok(m.clone()),
                Err(_) => Err(RegistryError::HttpError {
                    package: "mock".to_owned(),
                    registry: Registry::CratesIo,
                    reason: "mock network error".to_owned(),
                }),
            }
        }
    }

    /// A mock repository that stores metadata in a Vec.
    struct MockMetadataRepo {
        rows: std::cell::RefCell<Vec<PackageMetadataRow>>,
    }

    impl MockMetadataRepo {
        fn new() -> Self {
            Self {
                rows: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn with_row(row: PackageMetadataRow) -> Self {
            Self {
                rows: std::cell::RefCell::new(vec![row]),
            }
        }
    }

    impl PackageMetadataRepository for MockMetadataRepo {
        fn upsert(&self, row: &PackageMetadataRow) -> Result<(), seshat_storage::StorageError> {
            self.rows.borrow_mut().push(row.clone());
            Ok(())
        }

        fn get(
            &self,
            name: &str,
            registry: &str,
        ) -> Result<Option<PackageMetadataRow>, seshat_storage::StorageError> {
            Ok(self
                .rows
                .borrow()
                .iter()
                .find(|r| r.name == name && r.registry == registry)
                .cloned())
        }

        fn get_by_registry(
            &self,
            registry: &str,
        ) -> Result<Vec<PackageMetadataRow>, seshat_storage::StorageError> {
            Ok(self
                .rows
                .borrow()
                .iter()
                .filter(|r| r.registry == registry)
                .cloned()
                .collect())
        }

        fn delete_stale(
            &self,
            _before_timestamp: i64,
        ) -> Result<usize, seshat_storage::StorageError> {
            Ok(0)
        }
    }

    const NOW: i64 = 1_700_100_000;

    #[test]
    fn tier1_cache_hit() {
        // Cached metadata with "encoding" category → Serialization
        let repo = MockMetadataRepo::with_row(PackageMetadataRow {
            name: "bincode".to_owned(),
            registry: "crates_io".to_owned(),
            categories: vec!["encoding".to_owned()],
            keywords: vec![],
            description: None,
            fetched_at: NOW - 100, // recent
        });

        let client = MockRegistryClient {
            response: Err(RegistryError::HttpError {
                package: "should-not-be-called".to_owned(),
                registry: Registry::CratesIo,
                reason: "should not be called".to_owned(),
            }),
        };

        let result = classify_with_registry(
            "bincode",
            Registry::CratesIo,
            DependencyDomain::Unknown, // not in hardcoded list
            &repo,
            &client,
            NOW,
        );

        assert_eq!(result.domain, DependencyDomain::Serialization);
        assert_eq!(result.confidence, ClassificationConfidence::Registry);
    }

    #[test]
    fn tier1_cache_stale_falls_through_to_tier2() {
        // Stale cache entry → should fetch from registry
        let repo = MockMetadataRepo::with_row(PackageMetadataRow {
            name: "old-pkg".to_owned(),
            registry: "crates_io".to_owned(),
            categories: vec!["encoding".to_owned()],
            keywords: vec![],
            description: None,
            fetched_at: NOW - CACHE_TTL_SECS - 1, // stale
        });

        let client = MockRegistryClient {
            response: Ok(PackageMetadata {
                name: "old-pkg".to_owned(),
                registry: Registry::CratesIo,
                categories: vec!["database".to_owned()],
                keywords: vec![],
                description: None,
            }),
        };

        let result = classify_with_registry(
            "old-pkg",
            Registry::CratesIo,
            DependencyDomain::Unknown,
            &repo,
            &client,
            NOW,
        );

        assert_eq!(result.domain, DependencyDomain::Database);
        assert_eq!(result.confidence, ClassificationConfidence::Registry);
    }

    #[test]
    fn tier2_cache_miss_then_fetch() {
        // No cached entry → fetch from registry
        let repo = MockMetadataRepo::new();

        let client = MockRegistryClient {
            response: Ok(PackageMetadata {
                name: "new-http-lib".to_owned(),
                registry: Registry::CratesIo,
                categories: vec!["web-programming::http-client".to_owned()],
                keywords: vec!["http".to_owned()],
                description: Some("An HTTP library".to_owned()),
            }),
        };

        let result = classify_with_registry(
            "new-http-lib",
            Registry::CratesIo,
            DependencyDomain::Unknown,
            &repo,
            &client,
            NOW,
        );

        assert_eq!(result.domain, DependencyDomain::Http);
        assert_eq!(result.confidence, ClassificationConfidence::Registry);

        // Verify caching happened
        let cached = repo.get("new-http-lib", "crates_io").unwrap();
        assert!(cached.is_some());
        let cached = cached.unwrap();
        assert_eq!(cached.categories, vec!["web-programming::http-client"]);
        assert_eq!(cached.fetched_at, NOW);
    }

    #[test]
    fn tier3_network_failure_falls_back() {
        // No cache, network fails → fallback to hardcoded Unknown
        let repo = MockMetadataRepo::new();
        let client = MockRegistryClient {
            response: Err(RegistryError::HttpError {
                package: "failing".to_owned(),
                registry: Registry::CratesIo,
                reason: "timeout".to_owned(),
            }),
        };

        let result = classify_with_registry(
            "failing-pkg",
            Registry::CratesIo,
            DependencyDomain::Unknown,
            &repo,
            &client,
            NOW,
        );

        assert_eq!(result.domain, DependencyDomain::Unknown);
        assert_eq!(result.confidence, ClassificationConfidence::Fallback);
    }

    #[test]
    fn hardcoded_known_library_skips_registry() {
        // Known library → return immediately without touching cache or network
        let repo = MockMetadataRepo::new();
        let client = MockRegistryClient {
            response: Err(RegistryError::HttpError {
                package: "should-not-be-called".to_owned(),
                registry: Registry::CratesIo,
                reason: "should not be called".to_owned(),
            }),
        };

        let result = classify_with_registry(
            "serde",
            Registry::CratesIo,
            DependencyDomain::Serialization, // hardcoded known
            &repo,
            &client,
            NOW,
        );

        assert_eq!(result.domain, DependencyDomain::Serialization);
        assert_eq!(result.confidence, ClassificationConfidence::Fallback);
    }

    #[test]
    fn tier2_fetch_no_useful_metadata_falls_to_tier3() {
        // Registry returns metadata with no useful categories/keywords
        let repo = MockMetadataRepo::new();
        let client = MockRegistryClient {
            response: Ok(PackageMetadata {
                name: "obscure-lib".to_owned(),
                registry: Registry::CratesIo,
                categories: vec!["no-std".to_owned()], // no mapping for this
                keywords: vec!["utility".to_owned()],  // no mapping for this
                description: None,
            }),
        };

        let result = classify_with_registry(
            "obscure-lib",
            Registry::CratesIo,
            DependencyDomain::Unknown,
            &repo,
            &client,
            NOW,
        );

        assert_eq!(result.domain, DependencyDomain::Unknown);
        assert_eq!(result.confidence, ClassificationConfidence::Fallback);
    }

    #[test]
    fn tier2_npm_keyword_based_classification() {
        // npm has no categories, but keywords can still classify
        let repo = MockMetadataRepo::new();
        let client = MockRegistryClient {
            response: Ok(PackageMetadata {
                name: "my-orm".to_owned(),
                registry: Registry::Npm,
                categories: vec![], // npm never has categories
                keywords: vec!["database".to_owned(), "orm".to_owned()],
                description: Some("An ORM library".to_owned()),
            }),
        };

        let result = classify_with_registry(
            "my-orm",
            Registry::Npm,
            DependencyDomain::Unknown,
            &repo,
            &client,
            NOW,
        );

        assert_eq!(result.domain, DependencyDomain::Database);
        assert_eq!(result.confidence, ClassificationConfidence::Registry);
    }

    #[test]
    fn tier2_pypi_classifier_classification() {
        let repo = MockMetadataRepo::new();
        let client = MockRegistryClient {
            response: Ok(PackageMetadata {
                name: "custom-framework".to_owned(),
                registry: Registry::PyPI,
                categories: vec![
                    "Framework :: Django".to_owned(),
                    "Programming Language :: Python :: 3".to_owned(),
                ],
                keywords: vec!["web".to_owned()],
                description: None,
            }),
        };

        let result = classify_with_registry(
            "custom-framework",
            Registry::PyPI,
            DependencyDomain::Unknown,
            &repo,
            &client,
            NOW,
        );

        assert_eq!(result.domain, DependencyDomain::WebFramework);
        assert_eq!(result.confidence, ClassificationConfidence::Registry);
    }
}
