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
pub mod documentation;
pub mod error;
pub mod git_dates;
pub mod git_utils;
pub mod manifest;
pub mod module_structure;
pub mod orchestrator;
pub mod parser;
pub mod registry;

pub use discovery::{DiscoveredFile, DiscoveryResult, detect_submodule_paths, discover_files};
pub use documentation::{DocType, DocumentationResult, parse_documentation};
pub use error::ScanError;
pub use git_dates::collect_git_file_dates;
pub use git_utils::get_submodule_commit_hash;
pub use manifest::{
    DeclaredDependency, ManifestAnalysis, ManifestType, analyze_manifests, categorize_dependency,
    parse_manifest,
};
pub use module_structure::{ModuleGraph, ModuleInfo, build_module_graph};
pub use orchestrator::{
    IncrementalStats, ScanProgress, ScanResult, scan_project, scan_project_with_progress,
};
pub use parser::{Parser, content_hash, parse_file};
pub use registry::{
    CACHE_TTL_SECS, PackageMetadata, PackageRegistryClient, Registry, RegistryError,
    crates_io::CratesIoClient,
    npm::NpmClient,
    pypi::PyPIClient,
    registry_mapping::{
        ClassificationConfidence, ClassificationResult, classify_with_registry,
        infer_domain_from_metadata, map_crates_io_category, map_keyword, map_pypi_classifier,
    },
};
