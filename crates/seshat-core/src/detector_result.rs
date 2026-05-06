use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::knowledge::KnowledgeNature;

/// Structural classification of a [`ConventionFinding`].
///
/// Replaces ad-hoc string matching on `description.contains("(heuristic)")`
/// scattered across the pipeline. Each emit site sets the kind explicitly
/// at construction time; downstream consumers (filters, aggregators, the
/// review TUI) match on this enum instead of parsing free-form text.
///
/// `Other` is the [`Default`] fallback for legacy data deserialised from
/// older DBs that predate this field. New code MUST set the kind
/// explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    /// Canonical library for a domain — emitted by `dependency_usage`.
    /// Description shape: `"Canonical {domain} library: {pkg}"`.
    Canonical,
    /// Heuristic name-based observation. Description shape:
    /// `"Likely {domain} library (heuristic): {pkg}"` /
    /// `"Possible logging library (name heuristic): {module}"`.
    Heuristic,
    /// Logging style observation: `"Logging style: {structured|unstructured} logging"`.
    Style,
    /// Multiple competing libraries in the same file:
    /// `"Conflicting {domain} libraries in same file: A, B"`.
    Conflict,
    /// Naming-convention findings — function / parameter / type / file naming.
    Naming,
    /// File-level structural conventions: by-feature dirs, src-layout, etc.
    FileStructure,
    /// Import organization: ordering, grouping, blank-line separation.
    ImportOrganization,
    /// Test-related conventions: framework, placement, fixture style.
    Testing,
    /// Error handling conventions: Result types, custom enums, etc.
    ErrorHandling,
    /// Export / re-export conventions.
    Export,
    /// Cross-file wrapper / facade detection in `dependency_usage`.
    DependencyWrapper,
    /// Backward-compat fallback for findings deserialised from older DBs
    /// or from external callers that have not been migrated yet.
    #[default]
    Other,
}

/// How a single [`CodeEvidence`] row is anchored in source.
///
/// Each anchor kind has a different downstream policy:
/// - `CallSite` / `Declaration` get source-extracted snippets via
///   `detect_with_source`.
/// - `ImportLine` is the dependency_usage import-line fallback —
///   evidence at the line that brings the lib into scope when no
///   real call sites exist.
/// - `FileLevel` are synthetic file-level signals (line == 0) with a
///   pre-populated descriptive snippet that must NOT be overwritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AnchorKind {
    /// Real call site — function/method call, macro invocation, or
    /// derive macro. Extracted snippet shows the actual usage.
    #[default]
    CallSite,
    /// Declaration site — `fn foo()`, `struct Bar`, parameter line, etc.
    Declaration,
    /// `use foo::*` import line, used as fallback when no call sites
    /// exist for a canonical lib (rayon prelude, transitive deps).
    ImportLine,
    /// Synthetic file-level signal: line == 0, snippet is a
    /// human-readable description set by the detector.
    FileLevel,
}

/// Output of a single convention detector for a single file.
///
/// Lives in `seshat-core` because it flows: detectors -> storage -> graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConventionFinding {
    pub file_path: PathBuf,
    pub detector_name: String,
    pub nature: KnowledgeNature,
    /// Structural classification — see [`FindingKind`]. Defaults to
    /// [`FindingKind::Other`] for backward-compat deserialisation of
    /// older DB rows.
    #[serde(default)]
    pub kind: FindingKind,
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
    /// Line number where the snippet text starts.
    /// May be less than `line` when leading context lines are included.
    /// Defaults to 0 (meaning: use `line` as the start).
    #[serde(default)]
    pub snippet_start_line: usize,
    /// How this row is anchored — see [`AnchorKind`]. Defaults to
    /// [`AnchorKind::CallSite`] for backward-compat deserialisation.
    #[serde(default)]
    pub anchor: AnchorKind,
}

/// Aggregate output of all detectors for a single file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DetectorResults {
    pub file_path: PathBuf,
    pub findings: Vec<ConventionFinding>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_start_line_backward_compat_deserialization() {
        let json = r#"{
            "file": "src/main.rs",
            "line": 10,
            "end_line": 12,
            "snippet": "fn main() {}"
        }"#;
        let evidence: CodeEvidence = serde_json::from_str(json).unwrap();
        assert_eq!(evidence.snippet_start_line, 0);
        assert_eq!(evidence.line, 10);
    }
}
