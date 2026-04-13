use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::knowledge::KnowledgeNature;

/// Output of a single convention detector for a single file.
///
/// Lives in `seshat-core` because it flows: detectors -> storage -> graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConventionFinding {
    pub file_path: PathBuf,
    pub detector_name: String,
    pub nature: KnowledgeNature,
    pub description: String,
    pub evidence: Vec<CodeEvidence>,
    /// Whether this file follows the detected convention pattern.
    pub follows_convention: bool,
}

/// A snippet of code serving as evidence for a finding.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CodeEvidence {
    /// Path to the source file this evidence comes from.
    pub file: PathBuf,
    pub line: usize,
    pub end_line: usize,
    /// Real source code lines extracted from the file.
    /// Empty string when only IR-based detection was run (unchanged files).
    pub snippet: String,
}

/// Aggregate output of all detectors for a single file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DetectorResults {
    pub file_path: PathBuf,
    pub findings: Vec<ConventionFinding>,
}
