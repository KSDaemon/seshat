//! # Seshat Detectors
//!
//! Convention detection engine that analyzes parsed IR to identify coding
//! patterns and conventions. Each detector implements the
//! [`ConventionDetector`] trait and produces
//! [`seshat_core::ConventionFinding`] results.
//!
//! Detectors:
//! 1. Dependency usage — canonical libraries per domain
//! 2. Import organization — grouping and ordering patterns
//! 3. Error handling — error types, propagation, wrapping
//! 4. Naming conventions — file, function, type, constant naming
//! 5. Export patterns — default vs named, barrel exports
//! 6. Logging & observability — canonical logging library
//! 7. Test patterns — framework, placement, naming
//! 8. File structure — directory organization patterns
//!
//! Files are processed in parallel via `rayon`; detectors run sequentially
//! per file.

pub mod confidence;
pub mod dependency_usage;
pub mod error;
pub mod pipeline;
pub mod trait_def;

pub use confidence::{
    AggregatedConvention, aggregate_findings, compute_confidence, weight_from_confidence,
};
pub use error::DetectorError;
pub use pipeline::{all_detectors, run_all_detectors, run_detectors};
pub use trait_def::ConventionDetector;
