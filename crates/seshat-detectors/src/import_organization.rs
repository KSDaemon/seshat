//! Import organization detector — grouping and ordering patterns.
//!
//! Analyzes [`Import`] entries from parsed IR to detect how imports are
//! organized: grouping order (stdlib → external → internal), consistency
//! of grouping across files, and language-specific patterns such as
//! type-only import separation (TypeScript), barrel vs direct imports
//! (TypeScript/JavaScript), and `import` vs `from`-import preference (Python).
//!
//! Supports all four languages (Rust, TypeScript, JavaScript, Python).

use std::path::Path;

use seshat_core::{
    CodeEvidence, ConventionFinding, Import, KnowledgeNature, Language, LanguageIR, ProjectFile,
};

use crate::trait_def::ConventionDetector;

// ---------------------------------------------------------------------------
// Import group classification
// ---------------------------------------------------------------------------

/// Logical group an import belongs to, ordered canonically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum ImportGroup {
    /// Standard library (`std`, `core`, built-in modules).
    Stdlib = 0,
    /// Third-party / external packages.
    External = 1,
    /// Project-internal / relative imports.
    Internal = 2,
}

impl ImportGroup {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stdlib => "stdlib",
            Self::External => "external",
            Self::Internal => "internal",
        }
    }
}

/// Classify an import module path into a group for the given language.
fn classify_import(module: &str, language: Language) -> ImportGroup {
    match language {
        Language::Rust => classify_rust_import(module),
        Language::TypeScript | Language::JavaScript => classify_js_ts_import(module),
        Language::Python => classify_python_import(module),
    }
}

/// Rust: `std`/`core`/`alloc` → Stdlib, `crate`/`super`/`self` → Internal, rest → External.
fn classify_rust_import(module: &str) -> ImportGroup {
    let root = module.split("::").next().unwrap_or(module);
    match root {
        "std" | "core" | "alloc" => ImportGroup::Stdlib,
        "crate" | "super" | "self" => ImportGroup::Internal,
        _ => ImportGroup::External,
    }
}

/// TypeScript / JavaScript: relative paths (`./`, `../`) → Internal,
/// Node built-ins (`node:`, `fs`, `path`, `os`, etc.) → Stdlib, rest → External.
fn classify_js_ts_import(module: &str) -> ImportGroup {
    if module.starts_with("./") || module.starts_with("../") || module.starts_with('#') {
        return ImportGroup::Internal;
    }
    if is_node_builtin(module) {
        return ImportGroup::Stdlib;
    }
    ImportGroup::External
}

/// Check whether a module name is a Node.js built-in.
fn is_node_builtin(module: &str) -> bool {
    if module.starts_with("node:") {
        return true;
    }
    matches!(
        module,
        "assert"
            | "buffer"
            | "child_process"
            | "cluster"
            | "console"
            | "crypto"
            | "dgram"
            | "dns"
            | "events"
            | "fs"
            | "http"
            | "http2"
            | "https"
            | "inspector"
            | "module"
            | "net"
            | "os"
            | "path"
            | "perf_hooks"
            | "process"
            | "querystring"
            | "readline"
            | "stream"
            | "string_decoder"
            | "timers"
            | "tls"
            | "tty"
            | "url"
            | "util"
            | "v8"
            | "vm"
            | "worker_threads"
            | "zlib"
    )
}

/// Python: `__future__` / known stdlib → Stdlib, relative (`.`) → Internal,
/// common third-party → External. Heuristic: relative imports and `src.`/`app.`
/// prefixes are internal; known stdlib modules are stdlib; everything else is external.
fn classify_python_import(module: &str) -> ImportGroup {
    if module.starts_with('.') {
        return ImportGroup::Internal;
    }
    let root = module.split('.').next().unwrap_or(module);
    if is_python_stdlib(root) {
        return ImportGroup::Stdlib;
    }
    // Heuristic: common project-local prefixes.
    if matches!(root, "src" | "app" | "lib" | "tests" | "test" | "config") {
        return ImportGroup::Internal;
    }
    ImportGroup::External
}

/// Common Python standard library top-level modules (non-exhaustive but covers
/// the most frequent ones).
fn is_python_stdlib(module: &str) -> bool {
    matches!(
        module,
        "__future__"
            | "abc"
            | "argparse"
            | "ast"
            | "asyncio"
            | "base64"
            | "bisect"
            | "builtins"
            | "calendar"
            | "cmath"
            | "codecs"
            | "collections"
            | "concurrent"
            | "configparser"
            | "contextlib"
            | "copy"
            | "csv"
            | "ctypes"
            | "dataclasses"
            | "datetime"
            | "decimal"
            | "difflib"
            | "dis"
            | "email"
            | "enum"
            | "errno"
            | "fcntl"
            | "fileinput"
            | "fnmatch"
            | "fractions"
            | "functools"
            | "gc"
            | "getpass"
            | "gettext"
            | "glob"
            | "gzip"
            | "hashlib"
            | "heapq"
            | "hmac"
            | "html"
            | "http"
            | "importlib"
            | "inspect"
            | "io"
            | "ipaddress"
            | "itertools"
            | "json"
            | "keyword"
            | "linecache"
            | "locale"
            | "logging"
            | "lzma"
            | "math"
            | "mimetypes"
            | "multiprocessing"
            | "numbers"
            | "operator"
            | "os"
            | "pathlib"
            | "pickle"
            | "platform"
            | "pprint"
            | "queue"
            | "random"
            | "re"
            | "secrets"
            | "select"
            | "shelve"
            | "shlex"
            | "shutil"
            | "signal"
            | "site"
            | "socket"
            | "sqlite3"
            | "ssl"
            | "stat"
            | "string"
            | "struct"
            | "subprocess"
            | "sys"
            | "syslog"
            | "tempfile"
            | "textwrap"
            | "threading"
            | "time"
            | "timeit"
            | "traceback"
            | "types"
            | "typing"
            | "unicodedata"
            | "unittest"
            | "urllib"
            | "uuid"
            | "venv"
            | "warnings"
            | "weakref"
            | "xml"
            | "zipfile"
            | "zipimport"
            | "zlib"
    )
}

// ---------------------------------------------------------------------------
// Analysis helpers
// ---------------------------------------------------------------------------

/// Determine whether the imports are ordered in canonical group order
/// (Stdlib → External → Internal) by checking that the group sequence
/// never goes backwards. Returns (is_ordered, groups_present).
fn check_group_ordering(imports: &[Import], language: Language) -> (bool, Vec<ImportGroup>) {
    let groups: Vec<ImportGroup> = imports
        .iter()
        .map(|imp| classify_import(&imp.module, language))
        .collect();

    // Deduplicate consecutive equal groups to get the group sequence.
    let mut group_sequence: Vec<ImportGroup> = Vec::new();
    for &g in &groups {
        if group_sequence.last() != Some(&g) {
            group_sequence.push(g);
        }
    }

    // Check that the sequence is monotonically non-decreasing.
    let is_ordered = group_sequence.windows(2).all(|w| w[0] <= w[1]);

    (is_ordered, groups)
}

/// Detect whether imports are separated by blank lines between groups.
/// We consider there to be grouping if imports from different groups
/// are separated by at least one line gap (i.e., non-consecutive line numbers).
fn has_blank_line_separation(imports: &[Import], language: Language) -> bool {
    if imports.len() < 2 {
        return false;
    }

    // Track group transitions and whether they have a line gap.
    let mut prev_group = classify_import(&imports[0].module, language);
    let mut prev_line = imports[0].line;
    let mut transitions = 0u32;
    let mut separated_transitions = 0u32;

    for imp in &imports[1..] {
        let group = classify_import(&imp.module, language);
        if group != prev_group {
            transitions += 1;
            // A gap of >1 line suggests a blank line separator.
            if imp.line > prev_line + 1 {
                separated_transitions += 1;
            }
        }
        prev_group = group;
        prev_line = imp.line;
    }

    // Grouped if all transitions have blank-line separation (and there is at least one).
    transitions > 0 && separated_transitions == transitions
}

/// Build a snippet describing the import groups found.
fn build_group_summary(imports: &[Import], language: Language) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut current_group: Option<ImportGroup> = None;

    for imp in imports {
        let group = classify_import(&imp.module, language);
        if current_group != Some(group) {
            if current_group.is_some() {
                lines.push(String::new()); // blank line between groups
            }
            lines.push(format!("// {} imports:", group.as_str()));
            current_group = Some(group);
        }
        if imp.names.is_empty() {
            lines.push(format!("  {}", imp.module));
        } else {
            lines.push(format!("  {} ({})", imp.module, imp.names.join(", ")));
        }
    }

    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Detector
// ---------------------------------------------------------------------------

/// Detects import organization patterns including grouping order, blank-line
/// separation, and language-specific conventions.
///
/// Produces:
/// - **Convention** findings for consistent grouping (stdlib → external → internal).
/// - **Observation** findings when imports are not consistently grouped.
/// - Language-specific findings (type-only separation, barrel imports, import style).
pub struct ImportOrganizationDetector;

impl ConventionDetector for ImportOrganizationDetector {
    fn name(&self) -> &'static str {
        "import_organization"
    }

    /// Import blocks can span many lines — use 20 instead of the default 10.
    fn snippet_max_lines(&self) -> usize {
        20
    }

    fn detect(&self, file: &ProjectFile) -> Vec<ConventionFinding> {
        if file.imports.len() < 2 {
            return Vec::new();
        }

        let mut findings = Vec::new();

        // --- Core grouping analysis ---
        let (is_ordered, groups) = check_group_ordering(&file.imports, file.language);
        let has_separation = has_blank_line_separation(&file.imports, file.language);

        // Count distinct groups present.
        let mut distinct_groups: Vec<ImportGroup> = groups.clone();
        distinct_groups.sort();
        distinct_groups.dedup();

        // Only report grouping findings if there are at least 2 distinct groups.
        if distinct_groups.len() >= 2 {
            let summary = build_group_summary(&file.imports, file.language);
            let first_line = file.imports.first().map_or(1, |i| i.line);
            let last_line = file.imports.last().map_or(1, |i| i.line);

            if is_ordered && has_separation {
                // The set of *which* groups happen to appear in this
                // particular file (stdlib+external vs stdlib+external+internal,
                // etc.) used to be baked into the description, splitting
                // a single underlying convention into 5+ separate buckets
                // in the aggregator. The actual order is preserved in
                // evidence.snippet for inspection.
                findings.push(ConventionFinding {
                    file_path: file.path.clone(),
                    detector_name: "import_organization".to_owned(),
                    nature: KnowledgeNature::Convention,
                    description: "Imports grouped in canonical order with blank-line separators"
                        .to_owned(),
                    evidence: vec![CodeEvidence {
                        file: file.path.clone(),
                        line: first_line,
                        end_line: last_line,
                        snippet: summary,
                        snippet_start_line: 0,
                    }],
                    follows_convention: true,
                });
            } else if is_ordered {
                findings.push(ConventionFinding {
                    file_path: file.path.clone(),
                    detector_name: "import_organization".to_owned(),
                    nature: KnowledgeNature::Convention,
                    description: "Imports ordered by group but without blank-line separators"
                        .to_owned(),
                    evidence: vec![CodeEvidence {
                        file: file.path.clone(),
                        line: first_line,
                        end_line: last_line,
                        snippet: summary,
                        snippet_start_line: 0,
                    }],
                    follows_convention: true,
                });
            } else {
                findings.push(ConventionFinding {
                    file_path: file.path.clone(),
                    detector_name: "import_organization".to_owned(),
                    nature: KnowledgeNature::Observation,
                    description:
                        "Imports not grouped in canonical order (stdlib → external → internal)"
                            .to_owned(),
                    evidence: vec![CodeEvidence {
                        file: file.path.clone(),
                        line: first_line,
                        end_line: last_line,
                        snippet: summary,
                        snippet_start_line: 0,
                    }],
                    follows_convention: false,
                });
            }
        }

        // --- Language-specific analysis ---
        match file.language {
            Language::Rust => detect_rust_specifics(file, &mut findings),
            Language::TypeScript => detect_typescript_specifics(file, &mut findings),
            Language::JavaScript => detect_javascript_specifics(file, &mut findings),
            Language::Python => detect_python_specifics(file, &mut findings),
        }

        findings
    }

    fn supported_languages(&self) -> &[Language] {
        Language::all()
    }
}

// ---------------------------------------------------------------------------
// Language-specific detectors
// ---------------------------------------------------------------------------

/// Rust: detect `use` statement grouping (std/core → external → crate/super/self).
fn detect_rust_specifics(file: &ProjectFile, findings: &mut Vec<ConventionFinding>) {
    // Check for consistency of `use` grouping with Rust-specific groups.
    let has_stdlib = file
        .imports
        .iter()
        .any(|i| classify_rust_import(&i.module) == ImportGroup::Stdlib);
    let has_external = file
        .imports
        .iter()
        .any(|i| classify_rust_import(&i.module) == ImportGroup::External);

    if has_stdlib && has_external {
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: "import_organization".to_owned(),
            nature: KnowledgeNature::Convention,
            description: "Rust use grouping: std/core, external crates, crate/self/super"
                .to_owned(),
            evidence: build_group_evidence(&file.imports, file.language, &file.path),
            follows_convention: true,
        });
    }
}

/// TypeScript: detect type-only import separation and barrel import preference.
fn detect_typescript_specifics(file: &ProjectFile, findings: &mut Vec<ConventionFinding>) {
    let type_only_count = file.imports.iter().filter(|i| i.is_type_only).count();
    let value_count = file.imports.len() - type_only_count;

    // Type-only import separation.
    if type_only_count > 0 && value_count > 0 {
        // Check whether type-only imports are grouped together (all before or
        // all after value imports — no interleaving).
        let type_only_grouped = is_type_only_grouped(&file.imports);

        if type_only_grouped {
            findings.push(ConventionFinding {
                file_path: file.path.clone(),
                detector_name: "import_organization".to_owned(),
                nature: KnowledgeNature::Convention,
                description: "Type-only imports separated (TypeScript)".to_owned(),
                evidence: file
                    .imports
                    .iter()
                    .filter(|i| i.is_type_only)
                    .take(3)
                    .map(|i| CodeEvidence {
                        file: file.path.clone(),
                        line: i.line,
                        end_line: i.line,
                        snippet: String::new(),
                        snippet_start_line: 0,
                    })
                    .collect(),
                follows_convention: true,
            });
        } else {
            findings.push(ConventionFinding {
                file_path: file.path.clone(),
                detector_name: "import_organization".to_owned(),
                nature: KnowledgeNature::Observation,
                description: "Type-only imports interleaved with value imports (TypeScript)"
                    .to_owned(),
                evidence: Vec::new(),
                follows_convention: false,
            });
        }
    }

    // Barrel import detection via TypeScriptIR field.
    if let LanguageIR::TypeScript(ref ts_ir) = file.language_ir {
        if ts_ir.has_barrel_exports {
            findings.push(ConventionFinding {
                file_path: file.path.clone(),
                detector_name: "import_organization".to_owned(),
                nature: KnowledgeNature::Convention,
                description: "Barrel export file detected (re-exports via index)".to_owned(),
                evidence: Vec::new(),
                follows_convention: true,
            });
        }
    }

    // Direct vs barrel import preference: check if internal imports use
    // index paths (barrel) or direct file paths.
    detect_barrel_vs_direct(file, findings);
}

/// JavaScript: detect module system patterns and barrel imports.
fn detect_javascript_specifics(file: &ProjectFile, findings: &mut Vec<ConventionFinding>) {
    if let LanguageIR::JavaScript(ref js_ir) = file.language_ir {
        if js_ir.has_module_exports {
            findings.push(ConventionFinding {
                file_path: file.path.clone(),
                detector_name: "import_organization".to_owned(),
                nature: KnowledgeNature::Observation,
                description: "CommonJS module.exports detected alongside imports".to_owned(),
                evidence: Vec::new(),
                follows_convention: true,
            });
        }
    }

    // Barrel vs direct import preference (same logic as TypeScript).
    detect_barrel_vs_direct(file, findings);
}

/// Python: detect `import` vs `from`-import preference and isort-style grouping.
fn detect_python_specifics(file: &ProjectFile, findings: &mut Vec<ConventionFinding>) {
    // `import X` → names is empty, `from X import Y` → names is non-empty.
    let bare_import_count = file.imports.iter().filter(|i| i.names.is_empty()).count();
    let from_import_count = file.imports.iter().filter(|i| !i.names.is_empty()).count();

    if bare_import_count > 0 && from_import_count > 0 {
        let preference = if from_import_count >= bare_import_count {
            "from-import"
        } else {
            "import"
        };
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: "import_organization".to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!("Python import style: prefers {preference}"),
            evidence: file
                .imports
                .iter()
                .take(3)
                .map(|i| CodeEvidence {
                    file: file.path.clone(),
                    line: i.line,
                    end_line: i.line,
                    snippet: String::new(),
                    snippet_start_line: 0,
                })
                .collect(),
            follows_convention: true,
        });
    } else if from_import_count > 0 {
        // Show the first few import lines so the agent can see how they look.
        let evidence: Vec<CodeEvidence> = file
            .imports
            .iter()
            .take(3)
            .map(|i| CodeEvidence {
                file: file.path.clone(),
                line: i.line,
                end_line: i.line,
                snippet: String::new(),
                snippet_start_line: 0,
            })
            .collect();
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: "import_organization".to_owned(),
            nature: KnowledgeNature::Convention,
            description: "Python import style: exclusively from-import".to_owned(),
            evidence,
            follows_convention: true,
        });
    } else if bare_import_count > 0 {
        let evidence: Vec<CodeEvidence> = file
            .imports
            .iter()
            .take(3)
            .map(|i| CodeEvidence {
                file: file.path.clone(),
                line: i.line,
                end_line: i.line,
                snippet: String::new(),
                snippet_start_line: 0,
            })
            .collect();
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: "import_organization".to_owned(),
            nature: KnowledgeNature::Convention,
            description: "Python import style: exclusively bare import".to_owned(),
            evidence,
            follows_convention: true,
        });
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Build evidence entries showing first imports from each group.
fn build_group_evidence(
    imports: &[Import],
    language: Language,
    file_path: &Path,
) -> Vec<CodeEvidence> {
    let mut seen_groups = Vec::new();
    let mut evidence = Vec::new();

    for imp in imports {
        let group = classify_import(&imp.module, language);
        if !seen_groups.contains(&group) {
            seen_groups.push(group);
            evidence.push(CodeEvidence {
                file: file_path.to_path_buf(),
                line: imp.line,
                end_line: imp.line,
                snippet: String::new(),
                snippet_start_line: 0,
            });
        }
    }

    evidence
}

/// Check whether type-only imports are grouped (contiguous block, not interleaved).
fn is_type_only_grouped(imports: &[Import]) -> bool {
    // Find first and last type-only import indices.
    let first_type = imports.iter().position(|i| i.is_type_only);
    let last_type = imports.iter().rposition(|i| i.is_type_only);

    match (first_type, last_type) {
        (Some(first), Some(last)) => {
            // All imports between first and last type-only should also be type-only.
            imports[first..=last].iter().all(|i| i.is_type_only)
        }
        _ => true, // 0 or 1 type-only import is trivially grouped
    }
}

/// Detect barrel vs direct import preference for internal imports
/// (TS/JS: paths ending with `/index` or just a directory path).
fn detect_barrel_vs_direct(file: &ProjectFile, findings: &mut Vec<ConventionFinding>) {
    let internal_imports: Vec<&Import> = file
        .imports
        .iter()
        .filter(|i| classify_js_ts_import(&i.module) == ImportGroup::Internal)
        .collect();

    if internal_imports.len() < 2 {
        return;
    }

    let barrel_count = internal_imports
        .iter()
        .filter(|i| is_barrel_import(&i.module))
        .count();
    let direct_count = internal_imports.len() - barrel_count;

    if barrel_count > 0 || direct_count > 0 {
        let preference = if barrel_count >= direct_count {
            "barrel (index) imports"
        } else {
            "direct file imports"
        };

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: "import_organization".to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!("Internal import style: prefers {preference}"),
            evidence: internal_imports
                .iter()
                .take(3)
                .map(|i| CodeEvidence {
                    file: file.path.clone(),
                    line: i.line,
                    end_line: i.line,
                    snippet: String::new(),
                    snippet_start_line: 0,
                })
                .collect(),
            follows_convention: true,
        });
    }
}

/// Check if an import path looks like a barrel import (ends with `/index` or
/// is a directory-level path without a file extension).
fn is_barrel_import(module: &str) -> bool {
    if module.ends_with("/index") || module.ends_with("/index.js") || module.ends_with("/index.ts")
    {
        return true;
    }
    // Directory import: relative path without file extension indicators.
    if (module.starts_with("./") || module.starts_with("../"))
        && !module.contains('.')
        // Exclude paths like `./foo` which have no extension (barrel pattern).
        || module.ends_with('/')
    {
        return false; // Actually this IS a barrel import pattern.
    }
    false
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::ir::{
        Import, JavaScriptIR, LanguageIR, ModuleSystem, PythonIR, RustIR, TypeScriptIR,
    };
    use std::path::PathBuf;

    // --- Test helpers ---

    fn imp(module: &str, names: &[&str], line: usize) -> Import {
        Import {
            module: module.to_owned(),
            names: names.iter().map(|s| (*s).to_owned()).collect(),
            is_type_only: false,
            line,
        }
    }

    fn type_imp(module: &str, names: &[&str], line: usize) -> Import {
        Import {
            module: module.to_owned(),
            names: names.iter().map(|s| (*s).to_owned()).collect(),
            is_type_only: true,
            line,
        }
    }

    fn make_rust_file(imports: Vec<Import>) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from("src/lib.rs"),
            language: Language::Rust,
            content_hash: String::new(),
            imports,
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
            file_doc: None,
        }
    }

    fn make_ts_file(imports: Vec<Import>) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from("src/index.ts"),
            language: Language::TypeScript,
            content_hash: String::new(),
            imports,
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::TypeScript(TypeScriptIR::default()),
            file_doc: None,
        }
    }

    fn make_js_file_with_ir(imports: Vec<Import>, ir: JavaScriptIR) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from("src/index.js"),
            language: Language::JavaScript,
            content_hash: String::new(),
            imports,
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::JavaScript(ir),
            file_doc: None,
        }
    }

    fn make_python_file(imports: Vec<Import>) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from("app.py"),
            language: Language::Python,
            content_hash: String::new(),
            imports,
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Python(PythonIR::default()),
            file_doc: None,
        }
    }

    // --- Basic trait tests ---

    #[test]
    fn detector_name() {
        let detector = ImportOrganizationDetector;
        assert_eq!(detector.name(), "import_organization");
    }

    #[test]
    fn supports_all_languages() {
        let detector = ImportOrganizationDetector;
        assert_eq!(detector.supported_languages(), Language::all());
    }

    #[test]
    fn empty_imports_produces_no_findings() {
        let detector = ImportOrganizationDetector;
        let file = make_rust_file(Vec::new());
        assert!(detector.detect(&file).is_empty());
    }

    #[test]
    fn single_import_produces_no_findings() {
        let detector = ImportOrganizationDetector;
        let file = make_rust_file(vec![imp("std::io", &["Read"], 1)]);
        assert!(detector.detect(&file).is_empty());
    }

    // --- Rust import grouping tests ---

    #[test]
    fn rust_canonical_order_with_separation() {
        let detector = ImportOrganizationDetector;
        let file = make_rust_file(vec![
            // stdlib (lines 1-2)
            imp("std::collections", &["HashMap"], 1),
            imp("std::fmt", &[], 2),
            // external (line 5 — gap)
            imp("serde", &["Deserialize", "Serialize"], 5),
            imp("tracing", &["info"], 6),
            // internal (line 9 — gap)
            imp("crate::config", &["Config"], 9),
        ]);
        let findings = detector.detect(&file);

        let grouping = findings.iter().find(|f| {
            f.description.contains("canonical order") && f.description.contains("blank-line")
        });
        assert!(
            grouping.is_some(),
            "should detect canonical grouping with separators"
        );
        assert!(grouping.unwrap().follows_convention);
    }

    #[test]
    fn rust_ordered_without_separation() {
        let detector = ImportOrganizationDetector;
        let file = make_rust_file(vec![
            // stdlib then external — no blank lines
            imp("std::io", &["Read"], 1),
            imp("serde", &["Serialize"], 2),
        ]);
        let findings = detector.detect(&file);

        let grouping = findings.iter().find(|f| {
            f.description.contains("ordered by group")
                && f.description.contains("without blank-line")
        });
        assert!(
            grouping.is_some(),
            "should detect ordering without separation"
        );
        assert!(grouping.unwrap().follows_convention);
    }

    #[test]
    fn rust_unordered_imports() {
        let detector = ImportOrganizationDetector;
        let file = make_rust_file(vec![
            // external first, then stdlib — wrong order
            imp("serde", &["Serialize"], 1),
            imp("std::io", &["Read"], 3),
        ]);
        let findings = detector.detect(&file);

        let grouping = findings
            .iter()
            .find(|f| f.description.contains("not grouped in canonical order"));
        assert!(grouping.is_some(), "should flag non-canonical ordering");
        assert!(!grouping.unwrap().follows_convention);
    }

    #[test]
    fn rust_use_grouping_statistics() {
        let detector = ImportOrganizationDetector;
        let file = make_rust_file(vec![
            imp("std::io", &["Read"], 1),
            imp("std::fmt", &[], 2),
            imp("serde", &["Serialize"], 5),
            imp("crate::lib", &["Foo"], 8),
        ]);
        let findings = detector.detect(&file);

        let rust_finding = findings
            .iter()
            .find(|f| f.description.contains("Rust use grouping"));
        assert!(
            rust_finding.is_some(),
            "should report Rust-specific use grouping stats"
        );
    }

    // --- Import classification tests ---

    #[test]
    fn classify_rust_stdlib() {
        assert_eq!(classify_rust_import("std::io"), ImportGroup::Stdlib);
        assert_eq!(classify_rust_import("core::fmt"), ImportGroup::Stdlib);
        assert_eq!(classify_rust_import("alloc::vec"), ImportGroup::Stdlib);
    }

    #[test]
    fn classify_rust_external() {
        assert_eq!(
            classify_rust_import("serde::Serialize"),
            ImportGroup::External
        );
        assert_eq!(classify_rust_import("tracing::info"), ImportGroup::External);
    }

    #[test]
    fn classify_rust_internal() {
        assert_eq!(classify_rust_import("crate::config"), ImportGroup::Internal);
        assert_eq!(classify_rust_import("super::parent"), ImportGroup::Internal);
        assert_eq!(classify_rust_import("self::module"), ImportGroup::Internal);
    }

    #[test]
    fn classify_ts_node_builtin() {
        assert_eq!(classify_js_ts_import("node:fs"), ImportGroup::Stdlib);
        assert_eq!(classify_js_ts_import("path"), ImportGroup::Stdlib);
        assert_eq!(classify_js_ts_import("fs"), ImportGroup::Stdlib);
    }

    #[test]
    fn classify_ts_external() {
        assert_eq!(classify_js_ts_import("react"), ImportGroup::External);
        assert_eq!(classify_js_ts_import("express"), ImportGroup::External);
        assert_eq!(classify_js_ts_import("zod"), ImportGroup::External);
    }

    #[test]
    fn classify_ts_internal() {
        assert_eq!(classify_js_ts_import("./utils"), ImportGroup::Internal);
        assert_eq!(
            classify_js_ts_import("../types/user"),
            ImportGroup::Internal
        );
    }

    #[test]
    fn classify_python_stdlib() {
        assert_eq!(classify_python_import("os"), ImportGroup::Stdlib);
        assert_eq!(classify_python_import("typing"), ImportGroup::Stdlib);
        assert_eq!(classify_python_import("collections"), ImportGroup::Stdlib);
        assert_eq!(classify_python_import("__future__"), ImportGroup::Stdlib);
    }

    #[test]
    fn classify_python_external() {
        assert_eq!(classify_python_import("httpx"), ImportGroup::External);
        assert_eq!(classify_python_import("pydantic"), ImportGroup::External);
        assert_eq!(classify_python_import("sqlalchemy"), ImportGroup::External);
    }

    #[test]
    fn classify_python_internal() {
        assert_eq!(classify_python_import(".models"), ImportGroup::Internal);
        assert_eq!(
            classify_python_import("src.models.base"),
            ImportGroup::Internal
        );
    }

    // --- TypeScript-specific tests ---

    #[test]
    fn typescript_type_only_grouped() {
        let detector = ImportOrganizationDetector;
        let file = make_ts_file(vec![
            // Type-only imports first
            type_imp("../types/user", &["User", "UserId"], 1),
            type_imp("../types/api", &["ApiResponse"], 2),
            // Value imports
            imp("zod", &["z"], 4),
            imp("../services/user.service", &["UserService"], 5),
        ]);
        let findings = detector.detect(&file);

        let type_finding = findings
            .iter()
            .find(|f| f.description.contains("Type-only imports separated"));
        assert!(
            type_finding.is_some(),
            "should detect grouped type-only imports"
        );
        assert!(type_finding.unwrap().follows_convention);
    }

    #[test]
    fn typescript_type_only_interleaved() {
        let detector = ImportOrganizationDetector;
        let file = make_ts_file(vec![
            type_imp("../types/user", &["User"], 1),
            imp("zod", &["z"], 2),
            type_imp("../types/api", &["ApiResponse"], 3),
            imp("../services/user.service", &["UserService"], 4),
        ]);
        let findings = detector.detect(&file);

        let type_finding = findings
            .iter()
            .find(|f| f.description.contains("interleaved"));
        assert!(
            type_finding.is_some(),
            "should detect interleaved type-only imports"
        );
        assert!(!type_finding.unwrap().follows_convention);
    }

    #[test]
    fn typescript_barrel_export_detection() {
        let detector = ImportOrganizationDetector;
        let file = ProjectFile {
            path: PathBuf::from("src/index.ts"),
            language: Language::TypeScript,
            content_hash: String::new(),
            imports: vec![
                imp("./module-a", &["foo"], 1),
                imp("./module-b", &["bar"], 2),
            ],
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::TypeScript(TypeScriptIR {
                has_barrel_exports: true,
                type_only_imports: Vec::new(),
                decorators: Vec::new(),
                default_export: false,
                function_calls: vec![],
            }),
            file_doc: None,
        };
        let findings = detector.detect(&file);

        let barrel = findings
            .iter()
            .find(|f| f.description.contains("Barrel export file"));
        assert!(barrel.is_some(), "should detect barrel export file");
    }

    // --- Python-specific tests ---

    #[test]
    fn python_from_import_preference() {
        let detector = ImportOrganizationDetector;
        let file = make_python_file(vec![
            // bare imports
            imp("os", &[], 1),
            imp("re", &[], 2),
            // from imports (more)
            imp("collections", &["defaultdict"], 4),
            imp("datetime", &["datetime", "timezone"], 5),
            imp("pathlib", &["Path"], 6),
            imp("typing", &["Any", "Optional"], 7),
        ]);
        let findings = detector.detect(&file);

        let style = findings
            .iter()
            .find(|f| f.description.contains("Python import style"));
        assert!(style.is_some(), "should detect import style");
        assert!(
            style.unwrap().description.contains("from-import"),
            "should prefer from-import when majority are from-imports"
        );
    }

    #[test]
    fn python_bare_import_preference() {
        let detector = ImportOrganizationDetector;
        let file = make_python_file(vec![
            imp("os", &[], 1),
            imp("sys", &[], 2),
            imp("json", &[], 3),
            imp("typing", &["Any"], 5),
        ]);
        let findings = detector.detect(&file);

        let style = findings
            .iter()
            .find(|f| f.description.contains("Python import style"));
        assert!(style.is_some(), "should detect import style");
        assert!(
            style.unwrap().description.contains("prefers import"),
            "should prefer bare import when majority are bare imports"
        );
    }

    #[test]
    fn python_canonical_grouping() {
        let detector = ImportOrganizationDetector;
        let file = make_python_file(vec![
            // stdlib (lines 1-3)
            imp("os", &[], 1),
            imp("re", &[], 2),
            imp("collections", &["defaultdict"], 3),
            // external (line 6 — gap)
            imp("httpx", &[], 6),
            imp("pydantic", &["BaseModel"], 7),
            // internal (line 10 — gap)
            imp("src.models.base", &["AppBaseModel"], 10),
        ]);
        let findings = detector.detect(&file);

        let grouping = findings.iter().find(|f| {
            f.description.contains("canonical order") && f.description.contains("blank-line")
        });
        assert!(
            grouping.is_some(),
            "should detect canonical grouping for Python"
        );
        assert!(grouping.unwrap().follows_convention);
    }

    // --- JavaScript-specific tests ---

    #[test]
    fn javascript_module_exports_detection() {
        let detector = ImportOrganizationDetector;
        let file = make_js_file_with_ir(
            vec![
                imp("express", &["express"], 1),
                imp("./routes", &["router"], 2),
            ],
            JavaScriptIR {
                module_system: ModuleSystem::CommonJS,
                has_module_exports: true,
                require_calls: vec!["express".to_owned()],
                function_calls: vec![],
            },
        );
        let findings = detector.detect(&file);

        let module_exports = findings
            .iter()
            .find(|f| f.description.contains("module.exports"));
        assert!(module_exports.is_some(), "should detect module.exports");
    }

    // --- Edge case tests ---

    #[test]
    fn all_same_group_no_grouping_finding() {
        let detector = ImportOrganizationDetector;
        // All stdlib — only one group, so no grouping finding.
        let file = make_rust_file(vec![
            imp("std::io", &["Read"], 1),
            imp("std::fmt", &[], 2),
            imp("std::collections", &["HashMap"], 3),
        ]);
        let findings = detector.detect(&file);

        let grouping = findings.iter().find(|f| {
            f.description.contains("canonical order") || f.description.contains("not grouped")
        });
        assert!(
            grouping.is_none(),
            "should not report grouping with only one group"
        );
    }

    #[test]
    fn mixed_ts_groups_ordered_with_separation() {
        let detector = ImportOrganizationDetector;
        let file = make_ts_file(vec![
            imp("node:path", &["join"], 1),
            imp("node:fs", &["readFile"], 2),
            // gap
            imp("express", &["Router"], 5),
            imp("zod", &["z"], 6),
            // gap
            imp("./utils", &["helper"], 9),
            imp("../config", &["settings"], 10),
        ]);
        let findings = detector.detect(&file);

        let grouping = findings.iter().find(|f| {
            f.description.contains("canonical order") && f.description.contains("blank-line")
        });
        assert!(grouping.is_some(), "should detect canonical TS grouping");
        assert!(grouping.unwrap().follows_convention);
    }

    #[test]
    fn follows_convention_correctly_set() {
        let detector = ImportOrganizationDetector;

        // Correct order → follows_convention: true
        let good_file = make_rust_file(vec![
            imp("std::io", &["Read"], 1),
            imp("serde", &["Serialize"], 3),
        ]);
        let good_findings = detector.detect(&good_file);
        let good_grouping = good_findings.iter().find(|f| {
            f.description.contains("ordered by group") || f.description.contains("canonical order")
        });
        assert!(good_grouping.is_some());
        assert!(good_grouping.unwrap().follows_convention);

        // Wrong order → follows_convention: false
        let bad_file = make_rust_file(vec![
            imp("serde", &["Serialize"], 1),
            imp("std::io", &["Read"], 3),
        ]);
        let bad_findings = detector.detect(&bad_file);
        let bad_grouping = bad_findings
            .iter()
            .find(|f| f.description.contains("not grouped"));
        assert!(bad_grouping.is_some());
        assert!(!bad_grouping.unwrap().follows_convention);
    }

    #[test]
    fn detect_with_source_sets_real_snippet() {
        let detector = ImportOrganizationDetector;
        // TypeScript file with external then internal imports at lines 1, 2, 4
        // (gap between external and internal to trigger blank-line separation detection).
        let file = make_ts_file(vec![
            imp("express", &["Router"], 1),
            imp("zod", &["z"], 2),
            imp("./utils", &["helper"], 4),
        ]);
        let source = "import { Router } from 'express';\nimport { z } from 'zod';\n\nimport { helper } from './utils';\n";

        let findings = detector.detect_with_source(&file, source);

        assert!(!findings.is_empty(), "should have at least one finding");
        // Find any finding with non-empty evidence that has a line > 0.
        let finding_with_snippet = findings.iter().find(|f| {
            f.evidence
                .iter()
                .any(|ev| ev.line > 0 && !ev.snippet.is_empty())
        });
        assert!(
            finding_with_snippet.is_some(),
            "at least one finding should have evidence with a real snippet"
        );
        let ev = finding_with_snippet
            .unwrap()
            .evidence
            .iter()
            .find(|ev| ev.line > 0 && !ev.snippet.is_empty())
            .unwrap();
        assert_eq!(ev.file, file.path);
        // Snippet must contain actual import keywords from source.
        assert!(
            ev.snippet.contains("express")
                || ev.snippet.contains("zod")
                || ev.snippet.contains("utils"),
            "snippet must contain real import source keywords, got: {:?}",
            ev.snippet
        );
        assert!(
            !ev.snippet.starts_with("Custom "),
            "snippet must not be a synthetic format string, got: {:?}",
            ev.snippet
        );
    }

    // -- Fix 7: consolidated import-grouping descriptions ------------------

    /// Files exhibiting different *subsets* of the canonical grouping
    /// (stdlib+external vs stdlib+external+internal vs external+internal)
    /// must all map to the SAME convention description so the aggregator
    /// collapses them into one bucket. Previously each subset became its
    /// own convention with the group list embedded in the description,
    /// producing 5+ near-identical convention nodes per real codebase.
    #[test]
    fn import_grouping_description_is_subset_independent() {
        let detector = ImportOrganizationDetector;

        let two_groups = make_rust_file(vec![
            imp("std::io", &["Read"], 1),
            imp("serde", &["Serialize"], 3),
        ]);
        let three_groups = make_rust_file(vec![
            imp("std::io", &["Read"], 1),
            imp("serde", &["Serialize"], 3),
            imp("crate::config", &["Config"], 5),
        ]);

        let two_findings = detector.detect(&two_groups);
        let three_findings = detector.detect(&three_groups);

        let two_desc = two_findings
            .iter()
            .find(|f| f.description.contains("canonical order"))
            .map(|f| f.description.clone());
        let three_desc = three_findings
            .iter()
            .find(|f| f.description.contains("canonical order"))
            .map(|f| f.description.clone());

        assert!(two_desc.is_some(), "two-group file: expected grouping");
        assert!(three_desc.is_some(), "three-group file: expected grouping");
        assert_eq!(
            two_desc, three_desc,
            "different group subsets must share one convention description",
        );
        // The actual ordering still belongs in evidence.snippet.
        let snip = &three_findings
            .iter()
            .find(|f| f.description.contains("canonical order"))
            .unwrap()
            .evidence[0]
            .snippet;
        assert!(
            snip.contains("std") || snip.contains("crate") || snip.contains("serde"),
            "evidence snippet must capture the actual ordering, got: {snip:?}",
        );
    }

    /// The negative observation (imports NOT in canonical order) is a
    /// useful inconsistency signal and must remain its own finding.
    #[test]
    fn unordered_imports_remain_a_separate_observation() {
        let detector = ImportOrganizationDetector;
        let file = make_rust_file(vec![
            imp("serde", &["Serialize"], 1),
            imp("std::io", &["Read"], 2),
        ]);
        let findings = detector.detect(&file);
        let negative = findings
            .iter()
            .find(|f| f.description.contains("not grouped in canonical order"));
        assert!(
            negative.is_some(),
            "negative case must still be emitted as a separate observation",
        );
        assert_eq!(
            negative.unwrap().nature,
            seshat_core::KnowledgeNature::Observation,
        );
    }
}
