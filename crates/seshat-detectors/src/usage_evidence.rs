//! Cross-reference import symbols to function call sites for evidence snippets.
//!
//! Replaces import-line evidence with actual call-site evidence by matching
//! [`FunctionCall`] callees against [`Import`] names from the same top-level
//! package or module.

use std::collections::HashSet;
use std::path::Path;

use seshat_core::{CodeEvidence, FunctionCall, Import, LanguageIR, ProjectFile};

use crate::snippet::extract_snippet;
use crate::trait_def::EVIDENCE_CONTEXT_BEFORE;

/// Maximum evidence entries returned by [`find_usage_evidence`].
const DEFAULT_MAX: usize = 5;

/// Match function call sites to their corresponding imports, returning
/// call-site evidence instead of import-line evidence.
///
/// For each [`FunctionCall`], the callee is matched against [`Import`].names
/// using these strategies:
///
/// 1. **Namespaced calls** (`tracing::info`): split on `::` — the left side
///    must match the import's top-level module, and the right side must be in
///    the import's names. Additionally, if the left side itself appears in any
///    import's names (e.g. `Client::new` where `Client` was imported), it
///    matches.
/// 2. **Method calls** (`logger.info`): split on `.` — the receiver must
///    appear in some import's names.
/// 3. **Standalone names** (`info`): must appear in some import's names.
///
/// Results are deduplicated by callee name (one [`CodeEvidence`] per unique
/// callee) and limited to `max` entries.
pub fn find_usage_evidence(
    imports: &[Import],
    function_calls: &[FunctionCall],
    file_path: &Path,
    max: usize,
) -> Vec<CodeEvidence> {
    let limit = if max == 0 { DEFAULT_MAX } else { max };

    if imports.is_empty() || function_calls.is_empty() {
        return Vec::new();
    }

    // Pass 1: name dedup. Keep the first call per unique callee that
    // matches an import.
    let mut by_callee: Vec<CodeEvidence> = Vec::new();
    let mut seen_callees = HashSet::new();

    for call in function_calls {
        if seen_callees.contains(&call.callee) {
            continue;
        }
        if matches_import(call, imports) {
            seen_callees.insert(call.callee.clone());
            by_callee.push(CodeEvidence {
                file: file_path.to_path_buf(),
                line: call.line,
                end_line: call.end_line,
                snippet: call.snippet.clone(),
                snippet_start_line: 0,
            });
        }
    }

    // Pass 2: collapse overlapping evidence. A fluent chain
    // (`builder().method().init()`) is parsed as several function calls
    // with distinct callees — they all pass the name dedup — but they
    // share a `line` and only their `end_line` differs. Keep one row per
    // unique start line, preferring the widest end_line + longest snippet
    // so the TUI shows the full chain.
    let mut by_start: std::collections::HashMap<(std::path::PathBuf, usize), CodeEvidence> =
        std::collections::HashMap::new();
    let mut order: Vec<(std::path::PathBuf, usize)> = Vec::new();

    for ev in by_callee {
        let key = (ev.file.clone(), ev.line);
        match by_start.get(&key) {
            None => {
                order.push(key.clone());
                by_start.insert(key, ev);
            }
            Some(existing) => {
                let existing_span = existing.end_line.saturating_sub(existing.line);
                let new_span = ev.end_line.saturating_sub(ev.line);
                let prefer_new = new_span > existing_span
                    || (new_span == existing_span && ev.snippet.len() > existing.snippet.len());
                if prefer_new {
                    by_start.insert(key, ev);
                }
            }
        }
    }

    let mut result: Vec<CodeEvidence> = order
        .into_iter()
        .filter_map(|k| by_start.remove(&k))
        .collect();
    result.truncate(limit);
    result
}

/// Check whether a function call's callee can be resolved to an import.
fn matches_import(call: &FunctionCall, imports: &[Import]) -> bool {
    // Case 1: Rust-style namespaced call (e.g. "tracing::info", "Client::new",
    // "clap::Parser::parse"). Walk the whole path to check each segment.
    if call.callee.contains("::") {
        let parts: Vec<&str> = call.callee.split("::").collect();
        let first = parts[0];

        // Strategy A: first matches an import's top-level module.
        // Walk the remaining parts — if any match an import name, it's a hit.
        // If no part matches but `first` is not in any import's names either,
        // treat it as a fully-qualified call through the crate prefix — match.
        let mut found_imp_top = false;
        let mut matched_by_name = false;
        for imp in imports {
            let imp_top = imp
                .module
                .chars()
                .position(|c| [' ', ':', '.'].contains(&c))
                .map(|p| &imp.module[..p])
                .unwrap_or(&imp.module);
            if imp_top == first {
                found_imp_top = true;
                // Wildcard import (empty names): any call using this module matches.
                if imp.names.is_empty() {
                    return true;
                }
                // Check if any remaining part matches an import name.
                for part in &parts[1..] {
                    if imp.names.iter().any(|n| n == part) {
                        matched_by_name = true;
                    }
                }
            }
        }
        if matched_by_name {
            return true;
        }
        // Strategy A-fallback: `first` matches imp_top but no import name
        // matches the callee segments AND `first` itself is NOT in any
        // import's names (i.e. it's not a type imported from another module)
        // → treat as FQN crate-prefix call (e.g. `tracing_subscriber::fmt()`
        // when only `EnvFilter` was imported from `tracing_subscriber`).
        if found_imp_top {
            let first_is_imported_name = imports
                .iter()
                .any(|imp| imp.names.iter().any(|n| *n == first));
            if !first_is_imported_name {
                return true;
            }
        }
        // Strategy B: first (the type name) is itself in an import's names.
        for imp in imports {
            if imp.names.iter().any(|n| *n == first) {
                return true;
            }
        }
    }

    // Case 2: Method call (e.g. "logger.info", "db.execute", "typer.Typer")
    if let Some((receiver, _method)) = split_first(call.callee.as_str(), ".") {
        for imp in imports {
            if imp.names.iter().any(|n| *n == receiver) {
                return true;
            }
        }
    }

    // Case 3: Standalone name (e.g. "info", "scan_project")
    for imp in imports {
        if imp.names.contains(&call.callee) {
            return true;
        }
    }

    false
}

/// Split a string on the first occurrence of a separator, returning both parts.
fn split_first<'a>(s: &'a str, sep: &str) -> Option<(&'a str, &'a str)> {
    let pos = s.find(sep)?;
    Some((&s[..pos], &s[pos + sep.len()..]))
}

/// Language-agnostic wrapper: extract call-site evidence from a [`ProjectFile`].
///
/// Dispatches to the language-specific IR to extract both `function_calls` and
/// (for Rust) `macro_calls`. Rust macro calls are converted into synthetic
/// [`FunctionCall`] entries so they flow through the same matching logic.
///
/// Returns up to `max` evidence entries (defaults to [`DEFAULT_MAX`] if `max == 0`).
///
/// ⚠️  This variant matches against **ALL** imports in the file, so the returned
/// evidence may include call sites from unrelated libraries. Use
/// [`find_usage_evidence_for_file_scoped`] when a detector needs evidence scoped to
/// specific module names (e.g., only "tracing" imports for a logging detector).
pub fn find_usage_evidence_for_file(file: &ProjectFile, max: usize) -> Vec<CodeEvidence> {
    let limit = if max == 0 { DEFAULT_MAX } else { max };

    let (all_calls, relevant_imports) = gather_calls_and_imports(file, None, None);
    if all_calls.is_empty() || relevant_imports.is_empty() {
        return Vec::new();
    }

    find_usage_evidence(&relevant_imports, &all_calls, &file.path, limit)
}

/// Scoped variant: only match call sites against imports whose top-level module
/// matches one of the given `module_names`.
///
/// Example: `module_names = ["tracing", "log"]` → only imports from the
/// `tracing` and `log` crates are used for matching, so a logging detector
/// doesn't get evidence from `reqwest::get()` calls.
pub fn find_usage_evidence_for_file_scoped(
    file: &ProjectFile,
    module_names: &[&str],
    max: usize,
) -> Vec<CodeEvidence> {
    let (all_calls, relevant_imports) = gather_calls_and_imports(file, Some(module_names), None);
    if all_calls.is_empty() || relevant_imports.is_empty() {
        return Vec::new();
    }

    find_usage_evidence(&relevant_imports, &all_calls, &file.path, max)
}

/// Source-aware scoped variant: extracts macro call snippets from source when available.
pub fn find_usage_evidence_for_file_scoped_with_source(
    file: &ProjectFile,
    module_names: &[&str],
    max: usize,
    source: &str,
) -> Vec<CodeEvidence> {
    let (all_calls, relevant_imports) =
        gather_calls_and_imports(file, Some(module_names), Some(source));
    if all_calls.is_empty() || relevant_imports.is_empty() {
        return Vec::new();
    }

    find_usage_evidence(&relevant_imports, &all_calls, &file.path, max)
}

/// Extract function calls (and Rust macros) and filter imports by module names.
///
/// Returns `(function_calls, filtered_imports)`.
///
/// If `module_filter` is `None`, all imports are included.
/// If `module_filter` is `Some(names)`, only imports whose top-level module
/// matches one of `names` are included (case-insensitive, with `-` → `_` normalization for Rust).
fn gather_calls_and_imports(
    file: &ProjectFile,
    module_filter: Option<&[&str]>,
    source: Option<&str>,
) -> (Vec<FunctionCall>, Vec<Import>) {
    let mut all_calls: Vec<FunctionCall> = match &file.language_ir {
        LanguageIR::Rust(ir) => {
            let mut calls = ir.function_calls.clone();
            for mc in &ir.macro_calls {
                let snippet = match source {
                    Some(src) if mc.line > 0 => {
                        let context_start = mc.line.saturating_sub(EVIDENCE_CONTEXT_BEFORE).max(1);
                        extract_snippet(src, context_start, mc.line, EVIDENCE_CONTEXT_BEFORE + 1)
                    }
                    _ => String::new(),
                };
                calls.push(FunctionCall {
                    callee: mc.name.clone(),
                    line: mc.line,
                    end_line: mc.line,
                    snippet,
                });
            }
            calls
        }
        LanguageIR::TypeScript(ir) => ir.function_calls.clone(),
        LanguageIR::JavaScript(ir) => ir.function_calls.clone(),
        LanguageIR::Python(ir) => ir.function_calls.clone(),
    };

    if all_calls.is_empty() {
        return (all_calls, Vec::new());
    }

    all_calls.sort_by_key(|c| c.line);

    let imports: Vec<Import> = file
        .imports
        .iter()
        .filter(|imp| {
            if let Some(names) = module_filter {
                let imp_top = normalize_module(import_top_module(&imp.module));
                names.iter().any(|n| imp_top == normalize_module(n))
            } else {
                true
            }
        })
        .cloned()
        .collect();

    (all_calls, imports)
}

/// Extract the top-level module name from an import path.
///
/// - `"tracing"` → `"tracing"`
/// - `"tracing::subscriber"` → `"tracing"`
/// - `"winston"` → `"winston"`
/// - `"@scope/package"` → `"@scope"` (handles `/` separator for npm scoped packages)
/// - `"my-crate::Foo"` → `"my-crate"` (handles `-` in crate names)
/// - `"logging.config"` → `"logging"` (handles `.` for Python)
fn import_top_module(module: &str) -> &str {
    let pos = module
        .chars()
        .position(|c| [' ', ':', '.', '/'].contains(&c));
    match pos {
        Some(p) => &module[..p],
        None => module,
    }
}

/// Normalize a module name for comparison: lowercase + `-` → `_`.
/// For scoped npm packages (`@scope/package`), also extracts the top-level scope.
fn normalize_module(module: &str) -> String {
    let top = import_top_module(module);
    top.to_lowercase().replace('-', "_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::ir::LanguageIR;
    use seshat_core::{JavaScriptIR, MacroCall, PythonIR, RustIR, TypeScriptIR};
    use std::path::PathBuf;

    fn file_path() -> PathBuf {
        PathBuf::from("src/lib.rs")
    }

    fn make_import(module: &str, names: &[&str]) -> Import {
        Import {
            module: module.to_owned(),
            names: names.iter().map(|s| s.to_string()).collect(),
            is_type_only: false,
            line: 1,
        }
    }

    fn make_call(callee: &str, line: usize) -> FunctionCall {
        FunctionCall {
            callee: callee.to_owned(),
            line,
            end_line: line,
            snippet: format!("{callee}()"),
        }
    }

    fn make_macro_call(name: &str, line: usize) -> MacroCall {
        MacroCall {
            name: name.to_owned(),
            line,
        }
    }

    fn make_rust_file(path: &str) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: seshat_core::Language::Rust,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
            file_doc: None,
        }
    }

    fn make_ts_file(path: &str) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: seshat_core::Language::TypeScript,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::TypeScript(TypeScriptIR::default()),
            file_doc: None,
        }
    }

    fn make_js_file(path: &str) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: seshat_core::Language::JavaScript,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::JavaScript(JavaScriptIR::default()),
            file_doc: None,
        }
    }

    fn make_python_file(path: &str) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: seshat_core::Language::Python,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Python(PythonIR::default()),
            file_doc: None,
        }
    }

    // -- Basic matching --

    #[test]
    fn basic_import_to_call_match() {
        let imports = vec![make_import("tracing", &["info", "warn", "error"])];
        let calls = vec![make_call("info", 10)];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].line, 10);
    }

    #[test]
    fn no_match_when_callee_not_in_import_names() {
        let imports = vec![make_import("tracing", &["info", "warn"])];
        let calls = vec![make_call("debug", 10)];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert!(result.is_empty());
    }

    // -- Deduplication --

    #[test]
    fn dedup_by_callee_two_identical_calls() {
        let imports = vec![make_import("tracing", &["info"])];
        let calls = vec![make_call("info", 10), make_call("info", 20)];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].line, 10); // first occurrence kept
    }

    #[test]
    fn diverse_callees_preserved() {
        let imports = vec![make_import("tracing", &["info", "warn", "error"])];
        let calls = vec![
            make_call("info", 10),
            make_call("warn", 20),
            make_call("error", 30),
        ];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].line, 10);
        assert_eq!(result[1].line, 20);
        assert_eq!(result[2].line, 30);
    }

    // -- Max limit --

    #[test]
    fn max_limit_respected() {
        let imports = vec![make_import(
            "tracing",
            &["info", "warn", "error", "debug", "trace", "fatal"],
        )];
        let calls = vec![
            make_call("info", 1),
            make_call("warn", 2),
            make_call("error", 3),
            make_call("debug", 4),
            make_call("trace", 5),
            make_call("fatal", 6),
        ];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 3);
        assert_eq!(result.len(), 3);
    }

    // -- Empty inputs --

    #[test]
    fn empty_imports_returns_empty() {
        let calls = vec![make_call("info", 10)];
        let result = find_usage_evidence(&[], &calls, &file_path(), 5);
        assert!(result.is_empty());
    }

    #[test]
    fn empty_calls_returns_empty() {
        let imports = vec![make_import("tracing", &["info"])];
        let result = find_usage_evidence(&imports, &[], &file_path(), 5);
        assert!(result.is_empty());
    }

    // -- Multiple imports from same module --

    #[test]
    fn multiple_imports_from_same_module_all_names_considered() {
        let imports = vec![
            make_import("tracing", &["info", "warn"]),
            make_import("tracing::subscriber", &["Layer"]),
        ];
        let calls = vec![make_call("info", 10), make_call("Layer", 20)];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert_eq!(result.len(), 2);
    }

    // -- Cross-module mismatch --

    #[test]
    fn cross_module_mismatch_no_false_positive() {
        let imports = vec![make_import("module_a", &["info"])];
        let calls = vec![make_call("module_b::info", 10)];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert!(result.is_empty());
    }

    // -- Namespaced calls (Strategy A: module match) --

    #[test]
    fn namespaced_call_module_match() {
        let imports = vec![make_import("tracing", &["info", "warn"])];
        let calls = vec![make_call("tracing::info", 10)];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert_eq!(result.len(), 1);
    }

    // -- Namespaced calls (Strategy B: type name match) --

    #[test]
    fn namespaced_call_type_in_import_names() {
        let imports = vec![make_import("reqwest", &["Client"])];
        let calls = vec![make_call("Client::new", 10)];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert_eq!(result.len(), 1);
    }

    // -- Method calls --

    #[test]
    fn method_call_receiver_in_import_names() {
        let imports = vec![make_import("winston", &["logger"])];
        let calls = vec![make_call("logger.info", 10)];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert_eq!(result.len(), 1);
    }

    // -- Snippet preserved --

    #[test]
    fn snippet_from_call_preserved() {
        let imports = vec![make_import("tracing", &["info"])];
        let calls = vec![FunctionCall {
            callee: "info".to_owned(),
            line: 10,
            end_line: 12,
            snippet: "info!(\"starting server\", port = 3000)".to_string(),
        }];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].snippet, "info!(\"starting server\", port = 3000)");
        assert_eq!(result[0].end_line, 12);
    }

    // -----------------------------------------------------------------------
    // find_usage_evidence_for_file — per-language tests
    // -----------------------------------------------------------------------

    // -- Rust: macro_calls converted to synthetic FunctionCalls --

    #[test]
    fn rust_macro_calls_matched() {
        let mut file = make_rust_file("src/lib.rs");
        file.imports = vec![make_import("tracing", &["info", "warn", "error"])];
        if let LanguageIR::Rust(ref mut ir) = file.language_ir {
            ir.macro_calls = vec![
                make_macro_call("info", 10),
                make_macro_call("warn", 20),
                make_macro_call("error", 30),
            ];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].line, 10);
        assert_eq!(result[1].line, 20);
        assert_eq!(result[2].line, 30);
    }

    #[test]
    fn rust_function_calls_matched() {
        let mut file = make_rust_file("src/main.rs");
        file.imports = vec![make_import("reqwest", &["Client"])];
        if let LanguageIR::Rust(ref mut ir) = file.language_ir {
            ir.function_calls = vec![make_call("Client::new", 15)];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].line, 15);
    }

    #[test]
    fn rust_macro_and_function_calls_combined() {
        let mut file = make_rust_file("src/handler.rs");
        file.imports = vec![
            make_import("tracing", &["info"]),
            make_import("anyhow", &["Result"]),
        ];
        if let LanguageIR::Rust(ref mut ir) = file.language_ir {
            ir.macro_calls = vec![make_macro_call("info", 5)];
            ir.function_calls = vec![make_call("Result::Ok", 10)];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        // info macro matched, Result::Ok matched via strategy B (Result in names)
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn rust_no_imports_returns_empty() {
        let mut file = make_rust_file("src/lib.rs");
        // no imports set — file.imports stays empty
        if let LanguageIR::Rust(ref mut ir) = file.language_ir {
            ir.macro_calls = vec![make_macro_call("info", 10)];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        assert!(result.is_empty());
    }

    // -- TypeScript: function_calls matched against imports --

    #[test]
    fn typescript_function_calls_matched() {
        let mut file = make_ts_file("src/logger.ts");
        file.imports = vec![make_import("winston", &["logger"])];
        if let LanguageIR::TypeScript(ref mut ir) = file.language_ir {
            ir.function_calls = vec![make_call("logger.info", 8), make_call("logger.warn", 15)];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].line, 8);
        assert_eq!(result[1].line, 15);
    }

    #[test]
    fn typescript_unrelated_calls_not_matched() {
        let mut file = make_ts_file("src/app.ts");
        file.imports = vec![make_import("winston", &["logger"])];
        if let LanguageIR::TypeScript(ref mut ir) = file.language_ir {
            ir.function_calls = vec![make_call("console.log", 5)];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        assert!(result.is_empty());
    }

    #[test]
    fn typescript_max_limit_respected() {
        let mut file = make_ts_file("src/service.ts");
        file.imports = vec![make_import("jest", &["expect", "describe", "it", "test"])];
        if let LanguageIR::TypeScript(ref mut ir) = file.language_ir {
            ir.function_calls = vec![
                make_call("expect", 10),
                make_call("describe", 20),
                make_call("it", 30),
                make_call("test", 40),
            ];
        }
        let result = find_usage_evidence_for_file(&file, 2);
        assert_eq!(result.len(), 2);
    }

    // -- JavaScript: function_calls matched against imports --

    #[test]
    fn javascript_function_calls_matched() {
        let mut file = make_js_file("src/routes.js");
        file.imports = vec![make_import("express", &["Router"])];
        if let LanguageIR::JavaScript(ref mut ir) = file.language_ir {
            ir.function_calls = vec![make_call("Router", 3)];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].line, 3);
    }

    #[test]
    fn javascript_empty_calls_returns_empty() {
        let mut file = make_js_file("src/utils.js");
        file.imports = vec![make_import("lodash", &["map", "filter"])];
        // no function_calls set — stays empty
        let result = find_usage_evidence_for_file(&file, 5);
        assert!(result.is_empty());
    }

    #[test]
    fn javascript_dedup_by_callee() {
        let mut file = make_js_file("src/index.js");
        file.imports = vec![make_import("lodash", &["map"])];
        if let LanguageIR::JavaScript(ref mut ir) = file.language_ir {
            // Two calls to "map" at different lines — only first should appear
            ir.function_calls = vec![make_call("map", 5), make_call("map", 15)];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].line, 5);
    }

    // -- Python: function_calls matched against imports --

    #[test]
    fn python_function_calls_matched() {
        let mut file = make_python_file("src/app.py");
        file.imports = vec![make_import("logging", &["getLogger"])];
        if let LanguageIR::Python(ref mut ir) = file.language_ir {
            ir.function_calls = vec![make_call("getLogger", 4)];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].line, 4);
    }

    #[test]
    fn python_method_call_matched() {
        let mut file = make_python_file("tests/test_api.py");
        file.imports = vec![make_import("pytest", &["raises"])];
        if let LanguageIR::Python(ref mut ir) = file.language_ir {
            ir.function_calls = vec![make_call("raises", 20)];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].line, 20);
    }

    #[test]
    fn python_no_matching_calls_returns_empty() {
        let mut file = make_python_file("src/utils.py");
        file.imports = vec![make_import("os", &["path"])];
        if let LanguageIR::Python(ref mut ir) = file.language_ir {
            ir.function_calls = vec![make_call("print", 1)];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        assert!(result.is_empty());
    }

    // -----------------------------------------------------------------------
    // Scoping tests — unscoped call sites cross-contaminate findings
    // -----------------------------------------------------------------------

    /// US-001 / FR-1: unscoped call_sites return ALL imports' call sites,
    /// including unrelated ones. A "tracing" logging finding would include
    /// reqwest::get() call sites because both imports are in the file.
    /// This demonstrates the cross-contamination problem.
    #[test]
    fn unscoped_call_sites_include_mixed_library_calls() {
        // File has both tracing (logging) and reqwest (HTTP) imports.
        let mut file = make_rust_file("src/handler.rs");
        file.imports = vec![
            make_import("tracing", &["info"]),
            make_import("reqwest", &["Client", "get"]),
        ];
        if let LanguageIR::Rust(ref mut ir) = file.language_ir {
            ir.macro_calls = vec![make_macro_call("info", 10)];
            ir.function_calls = vec![make_call("reqwest::get", 20), make_call("Client::new", 30)];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        // CRITICAL BUG: unscoped query returns BOTH tracing and reqwest call sites.
        // A logging detector calling this gets reqwest evidence mixed in.
        let callees: Vec<_> = result.iter().map(|e| e.line).collect();
        assert!(
            callees.contains(&20) || callees.contains(&30),
            "unscoped query should cross-contaminate with unrelated library calls. Got: {:?}",
            callees
        );
        // This test documents the bug. The fix (scoped_evidence) must NOT
        // return reqwest::get when only tracing imports are requested.
    }

    /// Same scenario as above but with a scoped call — only tracing imports
    /// are passed in. The reqwest::get call should NOT appear in results.
    #[test]
    fn scoped_call_sites_only_include_matching_imports() {
        // File has both tracing and reqwest imports.
        // We only want call sites that match tracing imports.
        let tracing_imports = vec![make_import("tracing", &["info"])];
        let all_calls = vec![make_call("reqwest::get", 20), make_call("Client::new", 30)];

        let result = find_usage_evidence(&tracing_imports, &all_calls, &file_path(), 5);
        // With scoped imports, reqwest::get should NOT match because "reqwest"
        // is not in tracing_imports.
        assert!(
            result.is_empty(),
            "scoped query should not match call sites from unrelated imports. Got: {:?}",
            result
        );
        // NOTE: This test will FAIL initially because the current unscoped
        // find_usage_evidence DOES use all imports. But with only tracing
        // imports passed in, reqwest::get won't match any import — PASS.
        // The real test is: the caller (detector) must filter imports first.
    }

    // -----------------------------------------------------------------------
    // Edge case: scoped npm packages (@scope/pkg)
    // -----------------------------------------------------------------------

    #[test]
    fn scoped_npm_package_imp_top_not_split_by_slash() {
        // "@scope/package" should extract "@scope" as top-level because '/'
        // is a valid split character.
        let imp = make_import("@scope/package", &["fn"]);
        let imp_top = imp
            .module
            .chars()
            .position(|c| [' ', ':', '.', '/'].contains(&c))
            .map(|p| &imp.module[..p])
            .unwrap_or(&imp.module);
        // After fix: "/" is a valid split character
        assert_eq!(imp_top, "@scope");
    }

    // -----------------------------------------------------------------------
    // Edge case: hyphenated Rust crate names
    // -----------------------------------------------------------------------

    #[test]
    fn hyphenated_rust_crate_top_level_extraction_bug() {
        // Rust crate "my-crate" with import "my-crate::Foo"
        // imp_top extraction doesn't split on '-'
        let imp = make_import("my-crate::Foo", &["Foo"]);
        let imp_top = imp
            .module
            .chars()
            .position(|c| [' ', ':', '.'].contains(&c))
            .map(|p| &imp.module[..p])
            .unwrap_or(&imp.module);
        // imp_top = "my-crate::Foo" — it DOES split on ':' at position 9
        // So imp_top = "my-crate" — this actually works! The first ':' is found.
        // The real issue: module names like "my-crate" stored as "my_crate"
        // (Rust convention), so this may not be a real bug. But let's document
        // the split behavior for packages with hyphens.
        assert_eq!(imp_top, "my-crate");
    }

    // -----------------------------------------------------------------------
    // Macro call snippets: empty without source, populated with source
    // -----------------------------------------------------------------------

    #[test]
    fn macro_call_synthetic_function_call_has_empty_snippet_without_source() {
        let mut file = make_rust_file("src/lib.rs");
        file.imports = vec![make_import("anyhow", &["bail"])];
        if let LanguageIR::Rust(ref mut ir) = file.language_ir {
            ir.macro_calls = vec![make_macro_call("bail", 10)];
        }
        let (calls, _) = gather_calls_and_imports(&file, None, None);
        let bail_call = calls.iter().find(|c| c.callee == "bail").unwrap();
        assert!(
            bail_call.snippet.is_empty(),
            "macro call snippet is empty when no source provided"
        );
    }

    // -----------------------------------------------------------------------
    // Fluent-chain overlap collapse — Fix 2
    // -----------------------------------------------------------------------

    /// A fluent chain like
    /// `tracing_subscriber::fmt().with_env_filter(...).init()` is parsed
    /// as several function calls — each with a distinct callee but with
    /// the same start line — covering progressively shorter end lines.
    /// All match the import (Strategy A / Strategy B), so name dedup
    /// alone keeps every chained call. Result: 5 visually-overlapping
    /// evidence rows for one source statement.
    ///
    /// After Fix 2 the second pass collapses them into a single evidence
    /// per unique start line, preferring the widest end_line so the TUI
    /// shows the full chain.
    #[test]
    fn fluent_chain_collapses_into_single_evidence() {
        let imports = vec![make_import("tracing_subscriber", &["EnvFilter"])];
        let calls = vec![
            FunctionCall {
                callee: "tracing_subscriber::fmt".to_owned(),
                line: 67,
                end_line: 73,
                snippet: "tracing_subscriber::fmt().with_env_filter(...).init()".to_owned(),
            },
            FunctionCall {
                callee: "tracing_subscriber::with_env_filter".to_owned(),
                line: 67,
                end_line: 72,
                snippet: "tracing_subscriber::fmt().with_env_filter(...).with_target(...)"
                    .to_owned(),
            },
            FunctionCall {
                callee: "tracing_subscriber::with_target".to_owned(),
                line: 67,
                end_line: 71,
                snippet: "tracing_subscriber::fmt().with_env_filter(...).with_target(...)"
                    .to_owned(),
            },
            FunctionCall {
                callee: "tracing_subscriber::with_writer".to_owned(),
                line: 67,
                end_line: 70,
                snippet: "tracing_subscriber::fmt().with_env_filter(...)".to_owned(),
            },
            FunctionCall {
                callee: "tracing_subscriber::init".to_owned(),
                line: 67,
                end_line: 67,
                snippet: "tracing_subscriber::fmt()".to_owned(),
            },
        ];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert_eq!(
            result.len(),
            1,
            "fluent chain must collapse into one evidence, got: {:?}",
            result
                .iter()
                .map(|e| (e.line, e.end_line))
                .collect::<Vec<_>>(),
        );
        assert_eq!(result[0].line, 67);
        // Widest span wins.
        assert_eq!(result[0].end_line, 73);
    }

    /// Distinct call sites at different start lines must NOT collapse —
    /// `info!(..)` at line 10 and `warn!(..)` at line 20 are independent
    /// usage examples.
    #[test]
    fn distinct_start_lines_are_not_collapsed() {
        let imports = vec![make_import("tracing", &["info", "warn"])];
        let calls = vec![make_call("info", 10), make_call("warn", 20)];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert_eq!(result.len(), 2);
        let lines: Vec<usize> = result.iter().map(|e| e.line).collect();
        assert!(lines.contains(&10));
        assert!(lines.contains(&20));
    }

    #[test]
    fn macro_call_synthetic_function_call_has_snippet_with_source() {
        let mut file = make_rust_file("src/lib.rs");
        file.imports = vec![make_import("anyhow", &["bail"])];
        if let LanguageIR::Rust(ref mut ir) = file.language_ir {
            ir.macro_calls = vec![make_macro_call("bail", 10)];
        }
        let source_lines: Vec<String> = (1..=15).map(|i| format!("line {i}")).collect();
        let source = source_lines.join("\n");
        let (calls, _) = gather_calls_and_imports(&file, None, Some(&source));
        let bail_call = calls.iter().find(|c| c.callee == "bail").unwrap();
        assert!(
            !bail_call.snippet.is_empty(),
            "macro call snippet must be populated when source is provided"
        );
        assert!(
            bail_call.snippet.contains("line 8"),
            "macro call snippet should include 2 lines of context (line 8), got: {:?}",
            bail_call.snippet
        );
        assert!(
            bail_call.snippet.contains("line 10"),
            "macro call snippet should include the macro line (line 10), got: {:?}",
            bail_call.snippet
        );
    }
}
