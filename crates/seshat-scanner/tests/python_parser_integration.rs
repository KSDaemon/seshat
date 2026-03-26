//! Integration tests for the Python parser.
//!
//! Parses fixture files in `tests/fixtures/python_project/` and verifies
//! the expected IR is produced.

use std::fs;
use std::path::Path;

use seshat_core::{Language, LanguageIR, PythonIR, TypeDefKind};
use seshat_scanner::parse_file;

fn fixture_path(relative: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/python_project")
        .join(relative)
}

fn parse_fixture(relative: &str) -> seshat_core::ProjectFile {
    let path = fixture_path(relative);
    let source = fs::read_to_string(&path).expect("fixture file should exist");
    parse_file(&path, &source, Language::Python)
}

fn py_ir(pf: &seshat_core::ProjectFile) -> &PythonIR {
    match &pf.language_ir {
        LanguageIR::Python(ir) => ir,
        _ => panic!("expected PythonIR"),
    }
}

// ---------------------------------------------------------------------------
// __init__.py — Package init file with re-exports
// ---------------------------------------------------------------------------

#[test]
fn init_is_python() {
    let pf = parse_fixture("mypackage/__init__.py");
    assert_eq!(pf.language, Language::Python);
}

#[test]
fn init_detects_init_file() {
    let pf = parse_fixture("mypackage/__init__.py");
    let ir = py_ir(&pf);
    assert!(ir.is_init_file);
}

#[test]
fn init_has_all_export() {
    let pf = parse_fixture("mypackage/__init__.py");
    let ir = py_ir(&pf);
    assert!(ir.has_all_export);
}

#[test]
fn init_exports_contain_expected_names() {
    let pf = parse_fixture("mypackage/__init__.py");
    let export_names: Vec<&str> = pf.exports.iter().map(|e| e.name.as_str()).collect();
    assert!(export_names.contains(&"User"));
    assert!(export_names.contains(&"Config"));
    assert!(export_names.contains(&"UserService"));
    assert!(export_names.contains(&"format_name"));
}

#[test]
fn init_has_relative_imports() {
    let pf = parse_fixture("mypackage/__init__.py");
    assert!(
        pf.imports
            .iter()
            .any(|i| i.module == ".models" && i.names.contains(&"User".to_string()))
    );
    assert!(
        pf.imports
            .iter()
            .any(|i| i.module == ".services" && i.names.contains(&"UserService".to_string()))
    );
    assert!(
        pf.imports
            .iter()
            .any(|i| i.module == ".utils" && i.names.contains(&"format_name".to_string()))
    );
}

#[test]
fn init_has_content_hash() {
    let pf = parse_fixture("mypackage/__init__.py");
    assert!(!pf.content_hash.is_empty());
    assert_eq!(pf.content_hash.len(), 64);
}

// ---------------------------------------------------------------------------
// models.py — Data models with dataclass and enum
// ---------------------------------------------------------------------------

#[test]
fn models_imports() {
    let pf = parse_fixture("mypackage/models.py");
    assert!(pf.imports.iter().any(|i| i.module == "dataclasses"
        && i.names.contains(&"dataclass".to_string())
        && i.names.contains(&"field".to_string())));
    assert!(pf.imports.iter().any(|i| i.module == "typing"));
    assert!(pf.imports.iter().any(|i| i.module == "enum"));
}

#[test]
fn models_has_classes() {
    let pf = parse_fixture("mypackage/models.py");
    let class_names: Vec<&str> = pf.types.iter().map(|t| t.name.as_str()).collect();
    assert!(class_names.contains(&"Role"));
    assert!(class_names.contains(&"User"));
    assert!(class_names.contains(&"Config"));
}

#[test]
fn models_all_classes() {
    let pf = parse_fixture("mypackage/models.py");
    assert!(pf.types.iter().all(|t| t.kind == TypeDefKind::Class));
}

#[test]
fn models_has_decorators() {
    let pf = parse_fixture("mypackage/models.py");
    let ir = py_ir(&pf);
    assert!(ir.decorators.contains(&"dataclass".to_string()));
}

#[test]
fn models_type_hints_used() {
    let pf = parse_fixture("mypackage/models.py");
    let ir = py_ir(&pf);
    assert!(ir.type_hints_used);
}

#[test]
fn models_not_init_file() {
    let pf = parse_fixture("mypackage/models.py");
    let ir = py_ir(&pf);
    assert!(!ir.is_init_file);
}

// ---------------------------------------------------------------------------
// services.py — Service layer with classes and inheritance
// ---------------------------------------------------------------------------

#[test]
fn services_imports() {
    let pf = parse_fixture("mypackage/services.py");
    assert!(pf.imports.iter().any(|i| i.module == "logging"));
    assert!(pf.imports.iter().any(|i| i.module == "typing"));
    assert!(
        pf.imports
            .iter()
            .any(|i| i.module == ".models" && i.names.contains(&"User".to_string()))
    );
}

#[test]
fn services_has_classes() {
    let pf = parse_fixture("mypackage/services.py");
    assert!(pf.types.iter().any(|t| t.name == "UserService"));
    assert!(pf.types.iter().any(|t| t.name == "AdminService"));
}

#[test]
fn services_has_function() {
    let pf = parse_fixture("mypackage/services.py");
    assert!(
        pf.functions
            .iter()
            .any(|f| f.name == "create_default_service")
    );
}

#[test]
fn services_type_hints() {
    let pf = parse_fixture("mypackage/services.py");
    let ir = py_ir(&pf);
    assert!(ir.type_hints_used);
}

// ---------------------------------------------------------------------------
// utils.py — Utility functions, some async
// ---------------------------------------------------------------------------

#[test]
fn utils_imports() {
    let pf = parse_fixture("mypackage/utils.py");
    assert!(pf.imports.iter().any(|i| i.module == "os"));
    assert!(pf.imports.iter().any(|i| i.module == "re"));
    assert!(pf.imports.iter().any(|i| i.module == "pathlib"));
    assert!(pf.imports.iter().any(|i| i.module == "collections.abc"));
}

#[test]
fn utils_has_functions() {
    let pf = parse_fixture("mypackage/utils.py");
    assert!(pf.functions.iter().any(|f| f.name == "format_name"));
    assert!(pf.functions.iter().any(|f| f.name == "load_env"));
    assert!(pf.functions.iter().any(|f| f.name == "_private_helper"));
}

#[test]
fn utils_has_async_function() {
    let pf = parse_fixture("mypackage/utils.py");
    let fetch = pf
        .functions
        .iter()
        .find(|f| f.name == "fetch_remote_config")
        .expect("should find fetch_remote_config");
    assert!(fetch.is_async);
}

#[test]
fn utils_type_hints() {
    let pf = parse_fixture("mypackage/utils.py");
    let ir = py_ir(&pf);
    assert!(ir.type_hints_used);
}

#[test]
fn utils_no_all_export() {
    let pf = parse_fixture("mypackage/utils.py");
    let ir = py_ir(&pf);
    assert!(!ir.has_all_export);
    assert!(pf.exports.is_empty());
}

// ---------------------------------------------------------------------------
// main.py — Application entry point
// ---------------------------------------------------------------------------

#[test]
fn main_imports() {
    let pf = parse_fixture("main.py");
    assert!(pf.imports.iter().any(|i| i.module == "sys"));
    assert!(pf.imports.iter().any(|i| i.module == "pathlib"));
    assert!(
        pf.imports
            .iter()
            .any(|i| i.module == "mypackage" && i.names.contains(&"User".to_string()))
    );
    assert!(
        pf.imports
            .iter()
            .any(|i| i.module == "mypackage.utils" && i.names.contains(&"format_name".to_string()))
    );
}

#[test]
fn main_has_function() {
    let pf = parse_fixture("main.py");
    assert!(pf.functions.iter().any(|f| f.name == "main"));
}

#[test]
fn main_type_hints() {
    let pf = parse_fixture("main.py");
    let ir = py_ir(&pf);
    assert!(ir.type_hints_used);
}

#[test]
fn main_not_init_file() {
    let pf = parse_fixture("main.py");
    let ir = py_ir(&pf);
    assert!(!ir.is_init_file);
}

// ---------------------------------------------------------------------------
// decorators.py — Decorator patterns
// ---------------------------------------------------------------------------

#[test]
fn decorators_has_functions() {
    let pf = parse_fixture("mypackage/decorators.py");
    assert!(pf.functions.iter().any(|f| f.name == "retry"));
    assert!(pf.functions.iter().any(|f| f.name == "deprecated"));
    assert!(pf.functions.iter().any(|f| f.name == "old_handler"));
    assert!(pf.functions.iter().any(|f| f.name == "flaky_operation"));
}

#[test]
fn decorators_async_function() {
    let pf = parse_fixture("mypackage/decorators.py");
    let flaky = pf
        .functions
        .iter()
        .find(|f| f.name == "flaky_operation")
        .expect("should find flaky_operation");
    assert!(flaky.is_async);
}

#[test]
fn decorators_extracted() {
    let pf = parse_fixture("mypackage/decorators.py");
    let ir = py_ir(&pf);
    assert!(ir.decorators.contains(&"deprecated".to_string()));
    assert!(ir.decorators.contains(&"retry".to_string()));
}

#[test]
fn decorators_imports() {
    let pf = parse_fixture("mypackage/decorators.py");
    assert!(pf.imports.iter().any(|i| i.module == "functools"));
    assert!(pf.imports.iter().any(|i| i.module == "typing"));
}

// ---------------------------------------------------------------------------
// Cross-file checks
// ---------------------------------------------------------------------------

#[test]
fn all_fixtures_parse_without_panic() {
    let fixtures = [
        "mypackage/__init__.py",
        "mypackage/models.py",
        "mypackage/services.py",
        "mypackage/utils.py",
        "mypackage/decorators.py",
        "main.py",
    ];
    for fixture in fixtures {
        let pf = parse_fixture(fixture);
        assert_eq!(pf.language, Language::Python);
        assert!(!pf.content_hash.is_empty());
        assert!(matches!(pf.language_ir, LanguageIR::Python(_)));
    }
}

#[test]
fn content_hashes_differ_across_files() {
    let hashes: Vec<String> = [
        "mypackage/__init__.py",
        "mypackage/models.py",
        "mypackage/services.py",
        "mypackage/utils.py",
        "main.py",
    ]
    .iter()
    .map(|f| parse_fixture(f).content_hash)
    .collect();

    // All hashes should be unique
    let unique: std::collections::HashSet<&str> = hashes.iter().map(|h| h.as_str()).collect();
    assert_eq!(unique.len(), hashes.len());
}
