//! Unified dependency domain taxonomy.
//!
//! Provides a single [`DependencyDomain`] enum that classifies dependencies
//! by their functional role. This is the **single source of truth** used by
//! both the scanner (manifest analysis) and the detectors (usage analysis).

use serde::{Deserialize, Serialize};

/// Functional domain a dependency belongs to.
///
/// Covers the union of all categories previously split across
/// `DependencyCategory` (scanner) and the old `DependencyDomain` (detectors).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyDomain {
    /// HTTP clients (reqwest, axios, httpx, etc.).
    Http,
    /// Web frameworks (actix-web, express, flask, django, axum, rocket, etc.).
    WebFramework,
    /// Logging and observability (tracing, winston, loguru, etc.).
    Logging,
    /// Testing frameworks and utilities (jest, pytest, proptest, etc.).
    Testing,
    /// Input validation and schema enforcement (zod, pydantic, validator, etc.).
    Validation,
    /// Serialization and deserialization (serde, protobuf, msgpack, etc.).
    Serialization,
    /// Database clients and ORMs (sqlx, prisma, sqlalchemy, etc.).
    Database,
    /// CLI argument parsing (clap, commander, click, etc.).
    Cli,
    /// Async runtimes and utilities (tokio, asyncio, trio, etc.).
    AsyncRuntime,
    /// Cryptography and security (ring, bcrypt, hashlib, etc.).
    Crypto,
    /// General-purpose utility libraries.
    Utilities,
    /// Could not be classified into any known domain.
    Unknown,
}

impl DependencyDomain {
    /// Human-readable name used in finding descriptions.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "HTTP",
            Self::WebFramework => "web framework",
            Self::Logging => "logging",
            Self::Testing => "testing",
            Self::Validation => "validation",
            Self::Serialization => "serialization",
            Self::Database => "database",
            Self::Cli => "CLI",
            Self::AsyncRuntime => "async runtime",
            Self::Crypto => "crypto",
            Self::Utilities => "utilities",
            Self::Unknown => "unknown",
        }
    }
}
