//! Integration tests for the JavaScript parser.
//!
//! Parses fixture files in `tests/fixtures/javascript_project/` and verifies
//! the expected IR is produced.

use std::fs;
use std::path::Path;

use seshat_core::{JavaScriptIR, Language, LanguageIR, ModuleSystem, TypeDefKind};
use seshat_scanner::parse_file;

fn fixture_path(relative: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/javascript_project")
        .join(relative)
}

fn parse_fixture(relative: &str) -> seshat_core::ProjectFile {
    let path = fixture_path(relative);
    let source = fs::read_to_string(&path).expect("fixture file should exist");
    parse_file(&path, &source, Language::JavaScript)
}

fn js_ir(pf: &seshat_core::ProjectFile) -> &JavaScriptIR {
    match &pf.language_ir {
        LanguageIR::JavaScript(ir) => ir,
        _ => panic!("expected JavaScriptIR"),
    }
}

// ---------------------------------------------------------------------------
// index.mjs — ESM entry point with re-exports
// ---------------------------------------------------------------------------

#[test]
fn index_mjs_is_esm() {
    let pf = parse_fixture("src/index.mjs");
    let ir = js_ir(&pf);
    assert_eq!(ir.module_system, ModuleSystem::ESM);
}

#[test]
fn index_mjs_imports() {
    let pf = parse_fixture("src/index.mjs");

    let svc_import = pf
        .imports
        .iter()
        .find(|i| i.module == "./services.mjs")
        .expect("should find ./services.mjs import");
    assert!(svc_import.names.contains(&"UserService".to_string()));

    let utils_import = pf
        .imports
        .iter()
        .find(|i| i.module == "./utils.mjs")
        .expect("should find ./utils.mjs import");
    assert!(utils_import.names.contains(&"formatName".to_string()));
    assert!(utils_import.names.contains(&"delay".to_string()));

    let config_import = pf
        .imports
        .iter()
        .find(|i| i.module == "./config.mjs")
        .expect("should find ./config.mjs import");
    assert!(config_import.names[0].contains("* as"));
}

#[test]
fn index_mjs_re_exports() {
    let pf = parse_fixture("src/index.mjs");

    // export { UserService } from './services.mjs'
    assert!(
        pf.exports
            .iter()
            .any(|e| e.name.contains("UserService") && e.name.contains("./services.mjs"))
    );

    // export * from './constants.mjs'
    assert!(
        pf.exports
            .iter()
            .any(|e| e.name.contains("* from") && e.name.contains("./constants.mjs"))
    );
}

#[test]
fn index_mjs_exported_function() {
    let pf = parse_fixture("src/index.mjs");

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
fn index_mjs_exported_const() {
    let pf = parse_fixture("src/index.mjs");
    assert!(pf.exports.iter().any(|e| e.name == "VERSION"));
}

#[test]
fn index_mjs_default_export() {
    let pf = parse_fixture("src/index.mjs");
    assert!(
        pf.exports
            .iter()
            .any(|e| e.name == "bootstrap" && e.is_default)
    );
}

// ---------------------------------------------------------------------------
// services.mjs — ESM classes and functions
// ---------------------------------------------------------------------------

#[test]
fn services_mjs_imports() {
    let pf = parse_fixture("src/services.mjs");

    let utils_import = pf
        .imports
        .iter()
        .find(|i| i.module == "./utils.mjs")
        .expect("should find ./utils.mjs import");
    assert!(utils_import.names.contains(&"validateEmail".to_string()));
}

#[test]
fn services_mjs_classes() {
    let pf = parse_fixture("src/services.mjs");

    let user_svc = pf
        .types
        .iter()
        .find(|t| t.name == "UserService")
        .expect("should find UserService class");
    assert_eq!(user_svc.kind, TypeDefKind::Class);
    assert!(user_svc.is_public);

    let admin_svc = pf
        .types
        .iter()
        .find(|t| t.name == "AdminService")
        .expect("should find AdminService class");
    assert_eq!(admin_svc.kind, TypeDefKind::Class);
    assert!(admin_svc.is_public);
}

#[test]
fn services_mjs_functions() {
    let pf = parse_fixture("src/services.mjs");

    let create_fn = pf
        .functions
        .iter()
        .find(|f| f.name == "createService")
        .expect("should find createService function");
    assert!(create_fn.is_public);

    // internalHelper is a const arrow function, not exported
    let helper = pf
        .functions
        .iter()
        .find(|f| f.name == "internalHelper")
        .expect("should find internalHelper function");
    assert!(!helper.is_public);
}

#[test]
fn services_mjs_default_export() {
    let pf = parse_fixture("src/services.mjs");
    assert!(pf.exports.iter().any(|e| e.is_default));
}

// ---------------------------------------------------------------------------
// utils.mjs — ESM utility functions
// ---------------------------------------------------------------------------

#[test]
fn utils_mjs_namespace_import() {
    let pf = parse_fixture("src/utils.mjs");

    let crypto_import = pf
        .imports
        .iter()
        .find(|i| i.module == "crypto")
        .expect("should find crypto import");
    assert!(crypto_import.names[0].contains("* as"));
}

#[test]
fn utils_mjs_exported_functions() {
    let pf = parse_fixture("src/utils.mjs");

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

    let validate = pf
        .functions
        .iter()
        .find(|f| f.name == "validateEmail")
        .expect("should find validateEmail");
    assert!(validate.is_public);
}

#[test]
fn utils_mjs_private_functions() {
    let pf = parse_fixture("src/utils.mjs");

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
fn utils_mjs_exported_const() {
    let pf = parse_fixture("src/utils.mjs");
    assert!(pf.exports.iter().any(|e| e.name == "MAX_RETRIES"));
}

#[test]
fn utils_mjs_is_esm() {
    let pf = parse_fixture("src/utils.mjs");
    let ir = js_ir(&pf);
    assert_eq!(ir.module_system, ModuleSystem::ESM);
}

// ---------------------------------------------------------------------------
// config.cjs — CommonJS with require/module.exports
// ---------------------------------------------------------------------------

#[test]
fn config_cjs_is_commonjs() {
    let pf = parse_fixture("src/config.cjs");
    let ir = js_ir(&pf);
    assert_eq!(ir.module_system, ModuleSystem::CommonJS);
}

#[test]
fn config_cjs_require_calls() {
    let pf = parse_fixture("src/config.cjs");
    let ir = js_ir(&pf);

    assert!(ir.require_calls.contains(&"path".to_string()));
    assert!(ir.require_calls.contains(&"fs".to_string()));
}

#[test]
fn config_cjs_imports() {
    let pf = parse_fixture("src/config.cjs");

    // const path = require('path')
    let path_import = pf
        .imports
        .iter()
        .find(|i| i.module == "path")
        .expect("should find path import");
    assert_eq!(path_import.names, vec!["path"]);

    // const { readFileSync } = require('fs')
    let fs_import = pf
        .imports
        .iter()
        .find(|i| i.module == "fs")
        .expect("should find fs import");
    assert!(fs_import.names.contains(&"readFileSync".to_string()));
}

#[test]
fn config_cjs_module_exports() {
    let pf = parse_fixture("src/config.cjs");
    let ir = js_ir(&pf);
    assert!(ir.has_module_exports);

    // module.exports = { loadConfig, getDefaultConfig, ConfigManager }
    assert!(pf.exports.iter().any(|e| e.name == "loadConfig"));
    assert!(pf.exports.iter().any(|e| e.name == "getDefaultConfig"));
    assert!(pf.exports.iter().any(|e| e.name == "ConfigManager"));
}

#[test]
fn config_cjs_functions() {
    let pf = parse_fixture("src/config.cjs");

    let load = pf
        .functions
        .iter()
        .find(|f| f.name == "loadConfig")
        .expect("should find loadConfig");
    assert!(!load.is_public); // CJS functions aren't marked public
    assert!(!load.is_async);

    let get_default = pf
        .functions
        .iter()
        .find(|f| f.name == "getDefaultConfig")
        .expect("should find getDefaultConfig");
    assert!(!get_default.is_public);
}

#[test]
fn config_cjs_class() {
    let pf = parse_fixture("src/config.cjs");

    let mgr = pf
        .types
        .iter()
        .find(|t| t.name == "ConfigManager")
        .expect("should find ConfigManager class");
    assert_eq!(mgr.kind, TypeDefKind::Class);
}

// ---------------------------------------------------------------------------
// middleware.js — CommonJS with exports.foo pattern
// ---------------------------------------------------------------------------

#[test]
fn middleware_js_is_commonjs() {
    let pf = parse_fixture("src/middleware.js");
    let ir = js_ir(&pf);
    assert_eq!(ir.module_system, ModuleSystem::CommonJS);
}

#[test]
fn middleware_js_require_calls() {
    let pf = parse_fixture("src/middleware.js");
    let ir = js_ir(&pf);

    assert!(ir.require_calls.contains(&"./utils".to_string()));
    assert!(ir.require_calls.contains(&"./logger".to_string()));
}

#[test]
fn middleware_js_imports() {
    let pf = parse_fixture("src/middleware.js");

    // const { validateEmail } = require('./utils')
    let utils_import = pf
        .imports
        .iter()
        .find(|i| i.module == "./utils")
        .expect("should find ./utils import");
    assert!(utils_import.names.contains(&"validateEmail".to_string()));

    // const logger = require('./logger')
    let logger_import = pf
        .imports
        .iter()
        .find(|i| i.module == "./logger")
        .expect("should find ./logger import");
    assert_eq!(logger_import.names, vec!["logger"]);
}

#[test]
fn middleware_js_exports() {
    let pf = parse_fixture("src/middleware.js");
    let ir = js_ir(&pf);
    assert!(ir.has_module_exports);

    // exports.authMiddleware = ...
    assert!(pf.exports.iter().any(|e| e.name == "authMiddleware"));
    // exports.rateLimiter = ...
    assert!(pf.exports.iter().any(|e| e.name == "rateLimiter"));
    // module.exports.errorHandler = ...
    assert!(pf.exports.iter().any(|e| e.name == "errorHandler"));
}

#[test]
fn middleware_js_functions() {
    let pf = parse_fixture("src/middleware.js");

    let auth = pf
        .functions
        .iter()
        .find(|f| f.name == "authMiddleware")
        .expect("should find authMiddleware");
    assert!(!auth.is_async);

    let rate = pf
        .functions
        .iter()
        .find(|f| f.name == "rateLimiter")
        .expect("should find rateLimiter");
    assert!(rate.is_async);

    let err_handler = pf
        .functions
        .iter()
        .find(|f| f.name == "errorHandler")
        .expect("should find errorHandler");
    assert!(!err_handler.is_public);
}

// ---------------------------------------------------------------------------
// app.jsx — JSX component with ESM
// ---------------------------------------------------------------------------

#[test]
fn app_jsx_parses_without_errors() {
    let pf = parse_fixture("src/app.jsx");
    assert_eq!(pf.language, Language::JavaScript);
}

#[test]
fn app_jsx_is_esm() {
    let pf = parse_fixture("src/app.jsx");
    let ir = js_ir(&pf);
    assert_eq!(ir.module_system, ModuleSystem::ESM);
}

#[test]
fn app_jsx_imports() {
    let pf = parse_fixture("src/app.jsx");

    // import React, { useState, useEffect } from 'react'
    assert!(pf.imports.iter().any(|i| i.module == "react"));

    // import { UserService } from './services.mjs'
    assert!(pf.imports.iter().any(|i| i.module == "./services.mjs"));
}

#[test]
fn app_jsx_functions() {
    let pf = parse_fixture("src/app.jsx");

    // UserList is a private function
    let user_list = pf
        .functions
        .iter()
        .find(|f| f.name == "UserList")
        .expect("should find UserList function");
    assert!(!user_list.is_public);

    // App is an exported function
    let app = pf
        .functions
        .iter()
        .find(|f| f.name == "App")
        .expect("should find App function");
    assert!(app.is_public);
}

#[test]
fn app_jsx_exports() {
    let pf = parse_fixture("src/app.jsx");

    assert!(pf.exports.iter().any(|e| e.name == "App"));
    assert!(pf.exports.iter().any(|e| e.name == "AppTitle"));
    assert!(pf.exports.iter().any(|e| e.name == "App" && e.is_default));
}

// ---------------------------------------------------------------------------
// Cross-cutting concerns
// ---------------------------------------------------------------------------

#[test]
fn all_fixtures_have_content_hash() {
    for rel in &[
        "src/index.mjs",
        "src/services.mjs",
        "src/utils.mjs",
        "src/config.cjs",
        "src/middleware.js",
        "src/app.jsx",
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
fn all_fixtures_are_javascript_language() {
    for rel in &[
        "src/index.mjs",
        "src/services.mjs",
        "src/utils.mjs",
        "src/config.cjs",
        "src/middleware.js",
        "src/app.jsx",
    ] {
        let pf = parse_fixture(rel);
        assert_eq!(
            pf.language,
            Language::JavaScript,
            "{rel} should be JavaScript"
        );
        assert!(
            matches!(pf.language_ir, LanguageIR::JavaScript(_)),
            "{rel} should have JavaScriptIR"
        );
    }
}

#[test]
fn parsing_errors_gracefully_degraded() {
    // Malformed JavaScript should not panic — tree-sitter is error-tolerant
    let source = "export function {{{{ invalid }}}";
    let path = Path::new("broken.js");
    let pf = parse_file(path, source, Language::JavaScript);
    assert_eq!(pf.language, Language::JavaScript);
    assert!(!pf.content_hash.is_empty());
}
