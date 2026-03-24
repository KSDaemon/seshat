//! # Seshat Watcher
//!
//! File watching and incremental update pipeline. Orchestrates the hot tier
//! (immediate file change -> re-parse -> update IR) and warm tier (periodic
//! convention recalculation).
//!
//! Architecture (ADR-12): two independent tokio tasks:
//! - **Hot tier task**: `notify` events -> re-parse file -> update IR in DB
//!   -> update edges. Target: <1s latency.
//! - **Warm tier task**: timer (30s) -> check `has_pending_changes` ->
//!   recalculate convention aggregates.
//!
//! Also handles branch switch detection via `.git/HEAD` watch (ADR-14).

pub mod error;

pub use error::WatcherError;
