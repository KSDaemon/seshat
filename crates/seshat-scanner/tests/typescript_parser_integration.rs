//! Integration tests for the TypeScript parser.
//!
//! Parses fixture files in `tests/fixtures/typescript_project/` and verifies
//! the expected IR is produced.

use std::fs;
use std::path::Path;

use seshat_core::{Language, LanguageIR, TypeDefKind, TypeScriptIR};
use seshat_scanner::parse_file;

fn fixture_path(relative: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/typescript_project")
        .join(relative)
}

fn parse_fixture(relative: &str) -> seshat_core::ProjectFile {
    let path = fixture_path(relative);
    let source = fs::read_to_string(&path).expect("fixture file should exist");
    parse_file(&path, &source, Language::TypeScript)
}

fn ts_ir(pf: &seshat_core::ProjectFile) -> &TypeScriptIR {
    match &pf.language_ir {
        LanguageIR::TypeScript(ir) => ir,
        _ => panic!("expected TypeScriptIR"),
    }
}

// ---------------------------------------------------------------------------
// index.ts — barrel exports and re-exports
// ---------------------------------------------------------------------------

#[test]
fn index_ts_imports() {
    let pf = parse_fixture("src/index.ts");

    // import { UserService } from './services'
    let svc_import = pf
        .imports
        .iter()
        .find(|i| i.module == "./services")
        .expect("should find ./services import");
    assert!(svc_import.names.contains(&"UserService".to_string()));

    // import type { User, UserRole } from './types'
    let type_import = pf
        .imports
        .iter()
        .find(|i| i.module == "./types" && i.is_type_only)
        .expect("should find type-only ./types import");
    assert!(type_import.names.contains(&"User".to_string()));
    assert!(type_import.names.contains(&"UserRole".to_string()));
}

#[test]
fn index_ts_type_only_imports_in_ir() {
    let pf = parse_fixture("src/index.ts");
    let ir = ts_ir(&pf);
    assert!(ir.type_only_imports.contains(&"User".to_string()));
    assert!(ir.type_only_imports.contains(&"UserRole".to_string()));
}

#[test]
fn index_ts_barrel_exports() {
    let pf = parse_fixture("src/index.ts");
    let ir = ts_ir(&pf);
    assert!(ir.has_barrel_exports, "index.ts has `export * from ...`");
}

#[test]
fn index_ts_re_exports() {
    let pf = parse_fixture("src/index.ts");

    // export { UserService } from './services'
    assert!(
        pf.exports
            .iter()
            .any(|e| e.name.contains("UserService") && e.name.contains("./services"))
    );

    // export { default as App } from './app'
    assert!(
        pf.exports
            .iter()
            .any(|e| e.name.contains("default") && e.name.contains("./app"))
    );
}

#[test]
fn index_ts_exported_function() {
    let pf = parse_fixture("src/index.ts");

    let main_fn = pf
        .functions
        .iter()
        .find(|f| f.name == "main")
        .expect("should find 'main' function");
    assert!(main_fn.is_async);
    assert!(main_fn.is_public);

    assert!(pf.exports.iter().any(|e| e.name == "main"));
}

#[test]
fn index_ts_exported_const() {
    let pf = parse_fixture("src/index.ts");
    assert!(pf.exports.iter().any(|e| e.name == "VERSION"));
}

// ---------------------------------------------------------------------------
// types.ts — interfaces, type aliases, enums
// ---------------------------------------------------------------------------

#[test]
fn types_ts_interfaces() {
    let pf = parse_fixture("src/types.ts");

    let user = pf
        .types
        .iter()
        .find(|t| t.name == "User")
        .expect("should find User interface");
    assert_eq!(user.kind, TypeDefKind::Interface);
    assert!(user.is_public);

    let create_input = pf
        .types
        .iter()
        .find(|t| t.name == "UserCreateInput")
        .expect("should find UserCreateInput interface");
    assert_eq!(create_input.kind, TypeDefKind::Interface);
    assert!(create_input.is_public);
}

#[test]
fn types_ts_type_aliases() {
    let pf = parse_fixture("src/types.ts");

    let role = pf
        .types
        .iter()
        .find(|t| t.name == "UserRole")
        .expect("should find UserRole type alias");
    assert_eq!(role.kind, TypeDefKind::TypeAlias);
    assert!(role.is_public);

    let id = pf
        .types
        .iter()
        .find(|t| t.name == "ID")
        .expect("should find ID type alias");
    assert_eq!(id.kind, TypeDefKind::TypeAlias);
    assert!(id.is_public);
}

#[test]
fn types_ts_private_type() {
    let pf = parse_fixture("src/types.ts");

    let internal = pf
        .types
        .iter()
        .find(|t| t.name == "InternalConfig")
        .expect("should find InternalConfig");
    assert!(!internal.is_public);
}

#[test]
fn types_ts_enum() {
    let pf = parse_fixture("src/types.ts");

    let status = pf
        .types
        .iter()
        .find(|t| t.name == "Status")
        .expect("should find Status enum");
    assert_eq!(status.kind, TypeDefKind::Enum);
    assert!(status.is_public);
}

#[test]
fn types_ts_exports() {
    let pf = parse_fixture("src/types.ts");

    // Exported types should appear as exports with is_type_only
    assert!(
        pf.exports
            .iter()
            .any(|e| e.name == "User" && e.is_type_only)
    );
    assert!(
        pf.exports
            .iter()
            .any(|e| e.name == "UserRole" && e.is_type_only)
    );
    assert!(pf.exports.iter().any(|e| e.name == "ID" && e.is_type_only));

    // Enum export should NOT be type_only
    assert!(
        pf.exports
            .iter()
            .any(|e| e.name == "Status" && !e.is_type_only)
    );
}

/// Schema v8: every TypeDef and Export in TypeScript carries an `end_line`
/// covering the full source range of the declaration node.
#[test]
fn types_ts_typedef_and_export_end_lines() {
    let pf = parse_fixture("src/types.ts");

    // export interface User { ... } — multi-line interface body
    let user = pf
        .types
        .iter()
        .find(|t| t.name == "User")
        .expect("should find User interface");
    assert!(
        user.end_line > user.line,
        "multi-line interface should have end_line > line, got line={} end_line={}",
        user.line,
        user.end_line
    );

    // export type UserRole = '…' | '…' | '…'; — single-line type alias
    let role = pf
        .types
        .iter()
        .find(|t| t.name == "UserRole")
        .expect("should find UserRole type alias");
    assert_eq!(
        role.end_line, role.line,
        "single-line type alias should have end_line == line, got line={} end_line={}",
        role.line, role.end_line
    );

    // export enum Status { ... } — multi-line enum body
    let status_export = pf
        .exports
        .iter()
        .find(|e| e.name == "Status")
        .expect("should find Status export");
    assert!(
        status_export.end_line > status_export.line,
        "multi-line enum export should have end_line > line, got line={} end_line={}",
        status_export.line,
        status_export.end_line
    );
}

// ---------------------------------------------------------------------------
// services.ts — classes, decorators, default export
// ---------------------------------------------------------------------------

#[test]
fn services_ts_imports() {
    let pf = parse_fixture("src/services.ts");

    let types_import = pf
        .imports
        .iter()
        .find(|i| i.module == "./types")
        .expect("should find ./types import");
    assert!(types_import.is_type_only);
    assert!(types_import.names.contains(&"User".to_string()));
    assert!(types_import.names.contains(&"UserCreateInput".to_string()));
}

#[test]
fn services_ts_interface() {
    let pf = parse_fixture("src/services.ts");

    let repo = pf
        .types
        .iter()
        .find(|t| t.name == "Repository")
        .expect("should find Repository interface");
    assert_eq!(repo.kind, TypeDefKind::Interface);
    assert!(repo.is_public);
}

#[test]
fn services_ts_class() {
    let pf = parse_fixture("src/services.ts");

    let svc = pf
        .types
        .iter()
        .find(|t| t.name == "UserService")
        .expect("should find UserService class");
    assert_eq!(svc.kind, TypeDefKind::Class);
    assert!(svc.is_public);
}

#[test]
fn services_ts_decorators() {
    let pf = parse_fixture("src/services.ts");
    let ir = ts_ir(&pf);
    assert!(ir.decorators.contains(&"Injectable".to_string()));
    assert!(ir.decorators.contains(&"Singleton".to_string()));
}

#[test]
fn services_ts_default_export() {
    let pf = parse_fixture("src/services.ts");
    let ir = ts_ir(&pf);
    assert!(ir.default_export);

    assert!(pf.exports.iter().any(|e| e.is_default));
}

#[test]
fn services_ts_private_function() {
    let pf = parse_fixture("src/services.ts");

    let validate = pf
        .functions
        .iter()
        .find(|f| f.name == "validateEmail")
        .expect("should find validateEmail function");
    assert!(!validate.is_public);
}

#[test]
fn services_ts_arrow_function() {
    let pf = parse_fixture("src/services.ts");

    let formatter = pf
        .functions
        .iter()
        .find(|f| f.name == "formatName")
        .expect("should find formatName arrow function");
    assert!(!formatter.is_public);
}

// ---------------------------------------------------------------------------
// app.tsx — TSX file
// ---------------------------------------------------------------------------

#[test]
fn app_tsx_parses_without_errors() {
    let pf = parse_fixture("src/app.tsx");
    assert_eq!(pf.language, Language::TypeScript);
}

#[test]
fn app_tsx_imports() {
    let pf = parse_fixture("src/app.tsx");

    // import React, { useState, useEffect } from 'react'
    // This is a combined default + named import — tree-sitter may parse as one or two
    assert!(pf.imports.iter().any(|i| i.module == "react"));

    // import type { User } from './types'
    assert!(
        pf.imports
            .iter()
            .any(|i| i.module == "./types" && i.is_type_only)
    );

    // import { UserService } from './services'
    assert!(pf.imports.iter().any(|i| i.module == "./services"));
}

#[test]
fn app_tsx_interface() {
    let pf = parse_fixture("src/app.tsx");

    let props = pf
        .types
        .iter()
        .find(|t| t.name == "AppProps")
        .expect("should find AppProps interface");
    assert_eq!(props.kind, TypeDefKind::Interface);
}

#[test]
fn app_tsx_exported_const_component() {
    let pf = parse_fixture("src/app.tsx");

    // export const App = ... is exported
    assert!(pf.exports.iter().any(|e| e.name == "App"));
}

#[test]
fn app_tsx_default_export() {
    let pf = parse_fixture("src/app.tsx");
    let ir = ts_ir(&pf);
    assert!(ir.default_export);
}

// ---------------------------------------------------------------------------
// utils.ts — namespace imports, type re-exports
// ---------------------------------------------------------------------------

#[test]
fn utils_ts_namespace_import() {
    let pf = parse_fixture("src/utils.ts");

    let crypto_import = pf
        .imports
        .iter()
        .find(|i| i.module == "crypto")
        .expect("should find crypto import");
    assert!(crypto_import.names[0].contains("* as"));
}

#[test]
fn utils_ts_exported_functions() {
    let pf = parse_fixture("src/utils.ts");

    let gen_id = pf
        .functions
        .iter()
        .find(|f| f.name == "generateId")
        .expect("should find generateId");
    assert!(gen_id.is_public);
    assert!(!gen_id.is_async);

    let delay_fn = pf
        .functions
        .iter()
        .find(|f| f.name == "delay")
        .expect("should find delay");
    assert!(delay_fn.is_public);
    assert!(delay_fn.is_async);
}

#[test]
fn utils_ts_private_functions() {
    let pf = parse_fixture("src/utils.ts");

    let helper = pf
        .functions
        .iter()
        .find(|f| f.name == "internalHelper")
        .expect("should find internalHelper");
    assert!(!helper.is_public);

    let formatter = pf
        .functions
        .iter()
        .find(|f| f.name == "privateFormatter")
        .expect("should find privateFormatter");
    assert!(!formatter.is_public);
}

#[test]
fn utils_ts_type_re_export() {
    let pf = parse_fixture("src/utils.ts");

    // export type { User } from './types'
    assert!(pf.exports.iter().any(|e| e.is_type_only));
}

// ---------------------------------------------------------------------------
// Cross-cutting concerns
// ---------------------------------------------------------------------------

#[test]
fn all_fixtures_have_content_hash() {
    for rel in &[
        "src/index.ts",
        "src/types.ts",
        "src/services.ts",
        "src/app.tsx",
        "src/utils.ts",
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
fn all_fixtures_are_typescript_language() {
    for rel in &[
        "src/index.ts",
        "src/types.ts",
        "src/services.ts",
        "src/app.tsx",
        "src/utils.ts",
    ] {
        let pf = parse_fixture(rel);
        assert_eq!(
            pf.language,
            Language::TypeScript,
            "{rel} should be TypeScript"
        );
        assert!(
            matches!(pf.language_ir, LanguageIR::TypeScript(_)),
            "{rel} should have TypeScriptIR"
        );
    }
}

#[test]
fn parsing_errors_gracefully_degraded() {
    // Malformed TypeScript should not panic — tree-sitter is error-tolerant
    let source = "export function {{{{ invalid }}}";
    let path = Path::new("broken.ts");
    let pf = parse_file(path, source, Language::TypeScript);
    assert_eq!(pf.language, Language::TypeScript);
    assert!(!pf.content_hash.is_empty());
}
