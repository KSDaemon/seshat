//! Package registry metadata types and client trait.
//!
//! Defines the [`PackageRegistryClient`] trait for fetching metadata from
//! package registries (crates.io, npm, PyPI) and the associated types.
//! Concrete implementations live in separate modules; this module provides
//! only the trait, data types, and error type.

use serde::{Deserialize, Serialize};

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
