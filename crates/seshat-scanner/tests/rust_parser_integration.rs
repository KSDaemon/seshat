//! Integration tests for the Rust parser.
//!
//! Parses fixture files in `tests/fixtures/rust_project/` and verifies
//! the expected IR is produced.

use std::fs;
use std::path::Path;

use seshat_core::{Language, LanguageIR, TypeDefKind};
use seshat_scanner::parse_file;

fn fixture_path(relative: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rust_project")
        .join(relative)
}

fn parse_fixture(relative: &str) -> seshat_core::ProjectFile {
    let path = fixture_path(relative);
    let source = fs::read_to_string(&path).expect("fixture file should exist");
    parse_file(&path, &source, Language::Rust)
}

// ---------------------------------------------------------------------------
// main.rs
// ---------------------------------------------------------------------------

#[test]
fn main_rs_imports() {
    let pf = parse_fixture("src/main.rs");

    // use std::io::{self, Read, Write}
    let io_import = pf
        .imports
        .iter()
        .find(|i| i.module.contains("std::io"))
        .expect("should find std::io import");
    assert!(io_import.names.contains(&"Read".to_string()));
    assert!(io_import.names.contains(&"Write".to_string()));

    // use serde::{Deserialize, Serialize}
    let serde_import = pf
        .imports
        .iter()
        .find(|i| i.module.contains("serde"))
        .expect("should find serde import");
    assert!(serde_import.names.contains(&"Deserialize".to_string()));
    assert!(serde_import.names.contains(&"Serialize".to_string()));
}

#[test]
fn main_rs_mod_declarations() {
    let pf = parse_fixture("src/main.rs");
    let ir = match &pf.language_ir {
        LanguageIR::Rust(ir) => ir,
        _ => panic!("expected RustIR"),
    };

    assert!(ir.mod_declarations.iter().any(|m| m.name == "config"));
    assert!(ir.mod_declarations.iter().any(|m| m.name == "error"));
    assert!(ir.mod_declarations.iter().any(|m| m.name == "server"));
}

#[test]
fn main_rs_functions() {
    let pf = parse_fixture("src/main.rs");

    let run_fn = pf
        .functions
        .iter()
        .find(|f| f.name == "run")
        .expect("should find 'run' function");
    assert!(run_fn.is_public);
    assert!(run_fn.is_async);

    let main_fn = pf
        .functions
        .iter()
        .find(|f| f.name == "main")
        .expect("should find 'main' function");
    assert!(!main_fn.is_public);
    assert!(!main_fn.is_async);
}

#[test]
fn main_rs_exports() {
    let pf = parse_fixture("src/main.rs");

    // pub async fn run should be exported
    assert!(pf.exports.iter().any(|e| e.name == "run"));
    // fn main should NOT be exported
    assert!(!pf.exports.iter().any(|e| e.name == "main"));
}

// ---------------------------------------------------------------------------
// config.rs
// ---------------------------------------------------------------------------

#[test]
fn config_rs_imports() {
    let pf = parse_fixture("src/config.rs");
    assert!(pf.imports.iter().any(|i| i.module.contains("std::path")));
}

#[test]
fn config_rs_types() {
    let pf = parse_fixture("src/config.rs");

    let config_type = pf
        .types
        .iter()
        .find(|t| t.name == "Config")
        .expect("should find Config struct");
    assert_eq!(config_type.kind, TypeDefKind::Struct);
    assert!(config_type.is_public);

    let error_type = pf
        .types
        .iter()
        .find(|t| t.name == "ConfigError")
        .expect("should find ConfigError struct");
    assert_eq!(error_type.kind, TypeDefKind::Struct);
    assert!(error_type.is_public);

    let alias = pf
        .types
        .iter()
        .find(|t| t.name == "ConfigResult")
        .expect("should find ConfigResult type alias");
    assert_eq!(alias.kind, TypeDefKind::TypeAlias);
}

#[test]
fn config_rs_derive_macros() {
    let pf = parse_fixture("src/config.rs");
    let ir = match &pf.language_ir {
        LanguageIR::Rust(ir) => ir,
        _ => panic!("expected RustIR"),
    };

    let config_derive = ir
        .derive_macros
        .iter()
        .find(|d| d.type_name == "Config")
        .expect("should find derive for Config");
    assert!(config_derive.derives.contains(&"Debug".to_string()));
    assert!(config_derive.derives.contains(&"Clone".to_string()));
    assert!(config_derive.derives.contains(&"Serialize".to_string()));
    assert!(config_derive.derives.contains(&"Deserialize".to_string()));
}

#[test]
fn config_rs_trait_impl() {
    let pf = parse_fixture("src/config.rs");
    let ir = match &pf.language_ir {
        LanguageIR::Rust(ir) => ir,
        _ => panic!("expected RustIR"),
    };

    assert!(
        ir.trait_implementations
            .iter()
            .any(|ti| ti.trait_name == "Default" && ti.type_name == "Config")
    );
}

#[test]
fn config_rs_impl_methods() {
    let pf = parse_fixture("src/config.rs");

    assert!(pf.functions.iter().any(|f| f.name == "new" && f.is_public));
    assert!(pf.functions.iter().any(|f| f.name == "load" && f.is_public));
    assert!(
        pf.functions
            .iter()
            .any(|f| f.name == "validate" && !f.is_public)
    );
}

#[test]
fn config_rs_error_types() {
    let pf = parse_fixture("src/config.rs");
    let ir = match &pf.language_ir {
        LanguageIR::Rust(ir) => ir,
        _ => panic!("expected RustIR"),
    };

    assert!(ir.error_types.contains(&"ConfigError".to_string()));
}

// ---------------------------------------------------------------------------
// error.rs
// ---------------------------------------------------------------------------

#[test]
fn error_rs_error_types() {
    let pf = parse_fixture("src/error.rs");
    let ir = match &pf.language_ir {
        LanguageIR::Rust(ir) => ir,
        _ => panic!("expected RustIR"),
    };

    assert!(ir.error_types.contains(&"AppError".to_string()));
}

#[test]
fn error_rs_types() {
    let pf = parse_fixture("src/error.rs");

    let app_error = pf
        .types
        .iter()
        .find(|t| t.name == "AppError")
        .expect("should find AppError enum");
    assert_eq!(app_error.kind, TypeDefKind::Enum);
    assert!(app_error.is_public);

    // type alias: pub type Result<T> = ...
    let result_alias = pf
        .types
        .iter()
        .find(|t| t.name == "Result")
        .expect("should find Result type alias");
    assert_eq!(result_alias.kind, TypeDefKind::TypeAlias);
}

#[test]
fn error_rs_trait_impls() {
    let pf = parse_fixture("src/error.rs");
    let ir = match &pf.language_ir {
        LanguageIR::Rust(ir) => ir,
        _ => panic!("expected RustIR"),
    };

    // impl Display for AppError
    assert!(
        ir.trait_implementations
            .iter()
            .any(|ti| ti.trait_name.contains("Display") && ti.type_name == "AppError")
    );

    // impl From<io::Error> for AppError
    assert!(
        ir.trait_implementations
            .iter()
            .any(|ti| ti.trait_name.contains("From") && ti.type_name == "AppError")
    );
}

// ---------------------------------------------------------------------------
// server.rs
// ---------------------------------------------------------------------------

#[test]
fn server_rs_trait() {
    let pf = parse_fixture("src/server.rs");

    let handler = pf
        .types
        .iter()
        .find(|t| t.name == "Handler")
        .expect("should find Handler trait");
    assert_eq!(handler.kind, TypeDefKind::Trait);
    assert!(handler.is_public);
}

#[test]
fn server_rs_struct() {
    let pf = parse_fixture("src/server.rs");

    let echo = pf
        .types
        .iter()
        .find(|t| t.name == "EchoServer")
        .expect("should find EchoServer struct");
    assert_eq!(echo.kind, TypeDefKind::Struct);
    assert!(echo.is_public);
}

#[test]
fn server_rs_trait_impl() {
    let pf = parse_fixture("src/server.rs");
    let ir = match &pf.language_ir {
        LanguageIR::Rust(ir) => ir,
        _ => panic!("expected RustIR"),
    };

    assert!(
        ir.trait_implementations
            .iter()
            .any(|ti| ti.trait_name == "Handler" && ti.type_name == "EchoServer")
    );
}

#[test]
fn server_rs_async_method() {
    let pf = parse_fixture("src/server.rs");

    let start_fn = pf
        .functions
        .iter()
        .find(|f| f.name == "start")
        .expect("should find 'start' method");
    assert!(start_fn.is_async);
    assert!(start_fn.is_public);
}

#[test]
fn server_rs_private_method() {
    let pf = parse_fixture("src/server.rs");

    let log_fn = pf
        .functions
        .iter()
        .find(|f| f.name == "log")
        .expect("should find 'log' method");
    assert!(!log_fn.is_public);
}

#[test]
fn server_rs_wildcard_import() {
    let pf = parse_fixture("src/server.rs");

    let io_import = pf
        .imports
        .iter()
        .find(|i| i.module.contains("std::io"))
        .expect("should find std::io wildcard import");
    assert!(io_import.names.contains(&"*".to_string()));
}

// ---------------------------------------------------------------------------
// Cross-cutting concerns
// ---------------------------------------------------------------------------

#[test]
fn all_fixtures_have_content_hash() {
    for rel in &[
        "src/main.rs",
        "src/config.rs",
        "src/error.rs",
        "src/server.rs",
    ] {
        let pf = parse_fixture(rel);
        assert!(
            !pf.content_hash.is_empty(),
            "{rel} should have a content hash"
        );
        assert_eq!(
            pf.content_hash.len(),
            64,
            "{rel} hash should be 64 hex chars"
        );
    }
}

#[test]
fn all_fixtures_are_rust_language() {
    for rel in &[
        "src/main.rs",
        "src/config.rs",
        "src/error.rs",
        "src/server.rs",
    ] {
        let pf = parse_fixture(rel);
        assert_eq!(pf.language, Language::Rust, "{rel} should be Rust");
        assert!(
            matches!(pf.language_ir, LanguageIR::Rust(_)),
            "{rel} should have RustIR"
        );
    }
}

#[test]
fn parsing_errors_gracefully_degraded() {
    // Malformed Rust should not panic — it should still produce a ProjectFile
    let source = "fn invalid( { struct }}}";
    let path = Path::new("broken.rs");
    let pf = parse_file(path, source, Language::Rust);
    assert_eq!(pf.language, Language::Rust);
    assert!(!pf.content_hash.is_empty());
    // The file should still parse (tree-sitter is error-tolerant), though IR may be partial
}
