//! # Seshat Storage
//!
//! SQLite storage layer for Seshat. Owns ALL database interaction — no other
//! crate touches SQL directly.
//!
//! Responsibilities:
//! - Database lifecycle (`Database::open`, migrations, WAL mode)
//! - Repository traits and SQLite implementations for nodes, edges, files_ir,
//!   and branches
//! - FTS5 full-text search
//! - Automatic database backups
//! - Schema migrations via `refinery` (`embed_migrations!`)
//!
//! Connection management: single writer (`Arc<Mutex<Connection>>`) with
//! pooled read-only connections for concurrent queries (SQLite WAL mode).

pub mod db;
pub mod error;
pub mod ir_serialization;
pub mod repository;

pub use db::Database;
pub use error::StorageError;
pub use ir_serialization::{IR_SCHEMA_VERSION, deserialize_ir, serialize_ir};
pub use repository::{
    BranchRepository, EdgeRepository, FileIRRepository, NodeRepository, SqliteBranchRepository,
    SqliteEdgeRepository, SqliteFileIRRepository, SqliteNodeRepository,
};
