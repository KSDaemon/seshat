//! # Seshat Core
//!
//! Foundational types, traits, and intermediate representation (IR) shared
//! across all Seshat crates. This crate has zero dependencies on other seshat
//! crates and defines the common vocabulary used throughout the pipeline:
//!
//! - **IR types** (`ProjectFile`, `LanguageIR`): normalized, language-agnostic
//!   representation of parsed source code
//! - **Knowledge types** (`KnowledgeNode`, `KnowledgeNature`, `KnowledgeWeight`):
//!   the two-dimensional typing system for the knowledge graph
//! - **Edge types** (`Edge`, `EdgeType`): typed relationships between knowledge nodes
//! - **Type-safe IDs** (`NodeId`, `EdgeId`, `BranchId`): newtype wrappers preventing
//!   accidental misuse
//! - **Detector results** (`ConventionFinding`, `DetectorResults`): output types
//!   flowing from detectors through storage to the graph
//! - **Configuration** (`ScanConfig`, `DetectionConfig`, `ServerConfig`): all
//!   implement `Default` for zero-config operation

pub mod config;
pub mod dependency;
pub mod detector_result;
pub mod edge;
pub mod error;
pub mod ids;
pub mod ir;
pub mod knowledge;
pub mod snippet;

#[cfg(any(test, feature = "test-helpers"))]
pub mod test_helpers;

pub use config::{BackupConfig, DetectionConfig, ScanConfig, ServerConfig};
pub use dependency::{DependencyDomain, classify_domain};
pub use detector_result::{CodeEvidence, ConventionFinding, DetectorResults};
pub use edge::{Edge, EdgeType};
pub use error::{CoreError, ParseEnumError};
pub use ids::{BranchId, EdgeId, NodeId};
pub use ir::{
    DependencyUsage, DeriveUsage, Export, Function, Import, JavaScriptIR, Language, LanguageIR,
    ModuleSystem, ProjectFile, PythonIR, RustIR, TraitImpl, TypeDef, TypeDefKind, TypeScriptIR,
};
pub use knowledge::{KnowledgeNature, KnowledgeNode, KnowledgeWeight, Trend};
pub use snippet::{CodeSnippet, MAX_SNIPPET_LINES, truncate_snippet};
