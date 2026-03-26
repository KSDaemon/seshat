//! # Seshat Scanner
//!
//! Parses source code files into intermediate representation (IR) using
//! Tree-sitter grammars. Produces [`seshat_core::ProjectFile`] structs
//! consumed by convention detectors.
//!
//! Responsibilities:
//! - File discovery with `.gitignore` respect (via `ignore` crate)
//! - Tree-sitter AST parsing for Rust, TypeScript, JavaScript, Python
//! - Dependency manifest analysis (`Cargo.toml`, `package.json`, `pyproject.toml`)
//! - Documentation ingestion (Markdown, JSON schema, OpenAPI)
//! - Content hashing (SHA256) for incremental change detection

pub mod discovery;
pub mod error;
pub mod parser;

pub use discovery::{DiscoveredFile, discover_files};
pub use error::ScanError;
pub use parser::{Parser, content_hash, parse_file};
