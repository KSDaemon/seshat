//! # Seshat Graph
//!
//! Knowledge graph intelligence layer. All query logic, duplicate detection,
//! and graduated response generation lives here. The MCP crate calls into
//! this crate — graph is the brain, MCP is the mouth.
//!
//! Responsibilities:
//! - `query_project_context` — project overview with languages, modules,
//!   dependencies
//! - `query_convention` — convention lookup by topic with FTS5
//! - `query_code_pattern` — code pattern search (FTS5 + optional vector)
//! - `validate_approach` — graduated response with verdict, summary,
//!   and categorized findings
//! - `query_dependencies` — dependency analysis with blast radius
//! - Convention aggregate recalculation (warm tier)
//! - Cross-reference code conventions vs documentation
//! - LRU cache for IR and frequent queries

pub mod cross_reference;
pub mod error;
pub mod fts;

pub use cross_reference::{
    CrossReferenceConfig, CrossReferenceResult, ReinforcedNode, cross_reference,
};
pub use error::GraphError;
pub use fts::{delete_fts_entry, insert_fts_entry, rebuild_fts_index, search_conventions};
