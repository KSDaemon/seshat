//! Dependency usage detector — canonical libraries per domain.
//!
//! Analyzes [`DependencyUsage`] entries from parsed IR to identify which
//! library is canonical (most used) for each functional domain (HTTP,
//! logging, testing, etc.). Conflicting libraries within the same domain
//! are flagged as `Observation` findings. Dead dependencies (declared in
//! manifest but never imported) are also flagged.
//!
//! Domains are classified via a curated mapping of known crate/package names.
//! The detector supports all four languages (Rust, TypeScript, JavaScript,
//! Python).

use std::collections::HashMap;

use seshat_core::{
    CodeEvidence, ConventionFinding, DependencyDomain, DependencyUsage, KnowledgeNature, Language,
    ProjectFile,
};

use crate::trait_def::ConventionDetector;

// ---------------------------------------------------------------------------
// Domain classification
// ---------------------------------------------------------------------------

/// Classify a package name into a domain for the given language.
///
/// Returns `None` if the package does not map to any known domain.
pub fn classify_domain(package: &str, language: Language) -> Option<DependencyDomain> {
    match language {
        Language::Rust => classify_rust(package),
        Language::TypeScript | Language::JavaScript => classify_js_ts(package),
        Language::Python => classify_python(package),
    }
}

fn classify_rust(package: &str) -> Option<DependencyDomain> {
    match package {
        // HTTP clients
        "reqwest" | "hyper" | "ureq" | "curl" | "attohttpc" | "isahc" => {
            Some(DependencyDomain::Http)
        }
        // Web frameworks
        "actix-web" | "axum" | "warp" | "rocket" | "tide" | "poem" => {
            Some(DependencyDomain::WebFramework)
        }
        // Logging
        "tracing" | "log" | "env_logger" | "pretty_env_logger" | "slog" | "flexi_logger"
        | "tracing-subscriber" | "tracing-log" => Some(DependencyDomain::Logging),
        // Testing (beyond built-in #[test])
        "proptest" | "quickcheck" | "rstest" | "criterion" | "test-case" | "mockall"
        | "wiremock" | "assert_cmd" | "assert_fs" | "insta" => Some(DependencyDomain::Testing),
        // Validation
        "validator" | "garde" | "schemars" => Some(DependencyDomain::Validation),
        // Serialization
        "serde" | "serde_json" | "serde_yaml" | "serde_toml" | "bincode" | "ciborium"
        | "postcard" | "rmp-serde" | "toml" => Some(DependencyDomain::Serialization),
        // Database
        "sqlx" | "diesel" | "sea-orm" | "rusqlite" | "tokio-postgres" | "deadpool-postgres"
        | "mongodb" | "redis" | "surrealdb" => Some(DependencyDomain::Database),
        // CLI
        "clap" | "structopt" | "argh" | "pico-args" | "bpaf" => Some(DependencyDomain::Cli),
        // Async runtime
        "tokio" | "async-std" | "smol" => Some(DependencyDomain::AsyncRuntime),
        // Crypto
        "sha2" | "ring" | "rustls" | "openssl" | "aes" | "argon2" | "bcrypt" | "hmac" => {
            Some(DependencyDomain::Crypto)
        }
        _ => None,
    }
}

fn classify_js_ts(package: &str) -> Option<DependencyDomain> {
    match package {
        // HTTP clients
        "axios" | "node-fetch" | "got" | "ky" | "superagent" | "undici" => {
            Some(DependencyDomain::Http)
        }
        // Web frameworks
        "express" | "fastify" | "koa" | "hapi" | "next" | "hono" | "nest" | "nuxt" | "react"
        | "vue" | "angular" | "svelte" => Some(DependencyDomain::WebFramework),
        // Logging
        "winston" | "pino" | "bunyan" | "log4js" | "loglevel" | "signale" | "consola" => {
            Some(DependencyDomain::Logging)
        }
        // Testing
        "jest"
        | "mocha"
        | "vitest"
        | "ava"
        | "jasmine"
        | "chai"
        | "sinon"
        | "cypress"
        | "playwright"
        | "testing-library"
        | "@testing-library/react"
        | "@testing-library/jest-dom"
        | "supertest"
        | "nock"
        | "msw" => Some(DependencyDomain::Testing),
        // Validation
        "zod" | "joi" | "yup" | "ajv" | "class-validator" | "superstruct" | "io-ts" | "valibot" => {
            Some(DependencyDomain::Validation)
        }
        // Serialization (JSON is built-in, but some use explicit libs)
        "protobufjs" | "avro-js" | "msgpack" | "@msgpack/msgpack" | "flatbuffers" => {
            Some(DependencyDomain::Serialization)
        }
        // Database
        "prisma" | "@prisma/client" | "typeorm" | "sequelize" | "knex" | "mongoose"
        | "drizzle-orm" | "pg" | "mysql2" | "better-sqlite3" | "ioredis" | "redis" => {
            Some(DependencyDomain::Database)
        }
        // CLI
        "commander" | "yargs" | "meow" | "cac" | "citty" | "oclif" | "inquirer" => {
            Some(DependencyDomain::Cli)
        }
        _ => None,
    }
}

fn classify_python(package: &str) -> Option<DependencyDomain> {
    match package {
        // HTTP clients
        "requests" | "httpx" | "aiohttp" | "urllib3" => Some(DependencyDomain::Http),
        // Web frameworks
        "flask" | "django" | "fastapi" | "starlette" | "tornado" | "sanic" | "pyramid"
        | "bottle" => Some(DependencyDomain::WebFramework),
        // Logging
        "logging" | "loguru" | "structlog" => Some(DependencyDomain::Logging),
        // Testing
        "pytest" | "unittest" | "nose" | "hypothesis" | "mock" | "unittest.mock" | "faker"
        | "factory_boy" | "responses" | "pytest-mock" | "pytest-asyncio" => {
            Some(DependencyDomain::Testing)
        }
        // Validation
        "pydantic" | "marshmallow" | "cerberus" | "attrs" | "voluptuous" | "cattrs" => {
            Some(DependencyDomain::Validation)
        }
        // Serialization
        "json" | "msgpack" | "protobuf" | "avro" | "pickle" | "pyyaml" | "toml" | "orjson"
        | "ujson" => Some(DependencyDomain::Serialization),
        // Database
        "sqlalchemy" | "psycopg2" | "asyncpg" | "pymongo" | "redis" | "peewee" | "tortoise"
        | "databases" | "sqlite3" => Some(DependencyDomain::Database),
        // CLI
        "click" | "argparse" | "typer" | "fire" | "docopt" => Some(DependencyDomain::Cli),
        // Async runtime
        "asyncio" | "trio" | "anyio" | "uvloop" => Some(DependencyDomain::AsyncRuntime),
        // Crypto
        "cryptography" | "pycryptodome" | "hashlib" | "passlib" | "bcrypt" => {
            Some(DependencyDomain::Crypto)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Detector
// ---------------------------------------------------------------------------

/// Detects canonical library usage per functional domain.
///
/// Produces:
/// - **Convention** findings for the canonical (most-used) library per domain.
/// - **Observation** findings when multiple libraries serve the same domain.
/// - **Observation** findings for dead dependencies (declared but unused).
pub struct DependencyUsageDetector;

impl ConventionDetector for DependencyUsageDetector {
    fn name(&self) -> &'static str {
        "dependency_usage"
    }

    fn detect(&self, file: &ProjectFile) -> Vec<ConventionFinding> {
        if file.dependencies_used.is_empty() {
            return Vec::new();
        }

        let mut findings = Vec::new();

        // Group dependencies by domain.
        let mut domain_packages: HashMap<DependencyDomain, HashMap<&str, Vec<&DependencyUsage>>> =
            HashMap::new();

        for dep in &file.dependencies_used {
            if let Some(domain) = classify_domain(&dep.package, file.language) {
                domain_packages
                    .entry(domain)
                    .or_default()
                    .entry(&dep.package)
                    .or_default()
                    .push(dep);
            }
        }

        // For each domain, identify the canonical library and flag conflicts.
        for (domain, packages) in &domain_packages {
            let domain_name = domain.as_str();

            // Find the most-used package in this domain (by import count).
            let Some((canonical_pkg, canonical_usages)) =
                packages.iter().max_by_key(|(_, usages)| usages.len())
            else {
                continue; // skip empty domain groups (should not happen)
            };

            // Build evidence for the canonical library.
            let evidence: Vec<CodeEvidence> = canonical_usages
                .iter()
                .take(5)
                .map(|dep| CodeEvidence {
                    line: dep.line,
                    end_line: dep.line,
                    snippet: dep.import_path.clone(),
                })
                .collect();

            // Convention: canonical library for this domain.
            findings.push(ConventionFinding {
                file_path: file.path.clone(),
                detector_name: "dependency_usage".to_owned(),
                nature: KnowledgeNature::Convention,
                description: format!("Canonical {domain_name} library: {canonical_pkg}",),
                evidence,
                follows_convention: true,
            });

            // If multiple packages serve the same domain, flag a conflict.
            if packages.len() > 1 {
                let all_pkgs: Vec<&str> = packages.keys().copied().collect();
                let conflict_evidence: Vec<CodeEvidence> = packages
                    .iter()
                    .flat_map(|(_, usages)| usages.iter().take(2))
                    .map(|dep| CodeEvidence {
                        line: dep.line,
                        end_line: dep.line,
                        snippet: format!("{}: {}", dep.package, dep.import_path),
                    })
                    .collect();

                findings.push(ConventionFinding {
                    file_path: file.path.clone(),
                    detector_name: "dependency_usage".to_owned(),
                    nature: KnowledgeNature::Observation,
                    description: format!(
                        "Conflicting {domain_name} libraries: {}",
                        all_pkgs.join(", "),
                    ),
                    evidence: conflict_evidence,
                    follows_convention: false,
                });
            }
        }

        // Flag dead dependencies — packages in dependencies_used that don't
        // appear in any import. We check if the package name appears in
        // any import module path. A dependency is "dead" at the file level
        // if it appears in `dependencies_used` but has no matching import.
        let mut seen_packages: HashMap<&str, Vec<&DependencyUsage>> = HashMap::new();
        for dep in &file.dependencies_used {
            seen_packages.entry(&dep.package).or_default().push(dep);
        }

        for (package, usages) in &seen_packages {
            let has_import = file.imports.iter().any(|imp| {
                imp.module == *package
                    || imp.module.starts_with(&format!("{package}/"))
                    || imp.module.starts_with(&format!("{package}::"))
            });

            if !has_import {
                let evidence: Vec<CodeEvidence> = usages
                    .iter()
                    .take(3)
                    .map(|dep| CodeEvidence {
                        line: dep.line,
                        end_line: dep.line,
                        snippet: dep.import_path.clone(),
                    })
                    .collect();

                findings.push(ConventionFinding {
                    file_path: file.path.clone(),
                    detector_name: "dependency_usage".to_owned(),
                    nature: KnowledgeNature::Observation,
                    description: format!(
                        "Potentially dead dependency: {package} (used but not imported)",
                    ),
                    evidence,
                    follows_convention: false,
                });
            }
        }

        findings
    }

    fn supported_languages(&self) -> &[Language] {
        Language::all()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::ir::{Import, LanguageIR};
    use seshat_core::{Language, RustIR, TypeScriptIR};
    use std::path::PathBuf;

    fn make_rust_file_with_deps(deps: Vec<DependencyUsage>, imports: Vec<Import>) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from("src/lib.rs"),
            language: Language::Rust,
            content_hash: String::new(),
            imports,
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: deps,
            language_ir: LanguageIR::Rust(RustIR::default()),
        }
    }

    fn make_ts_file_with_deps(deps: Vec<DependencyUsage>, imports: Vec<Import>) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from("src/index.ts"),
            language: Language::TypeScript,
            content_hash: String::new(),
            imports,
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: deps,
            language_ir: LanguageIR::TypeScript(TypeScriptIR::default()),
        }
    }

    fn dep(package: &str, import_path: &str, line: usize) -> DependencyUsage {
        DependencyUsage {
            package: package.to_owned(),
            import_path: import_path.to_owned(),
            line,
        }
    }

    fn import(module: &str, names: &[&str]) -> Import {
        Import {
            module: module.to_owned(),
            names: names.iter().map(|s| (*s).to_owned()).collect(),
            is_type_only: false,
            line: 1,
        }
    }

    #[test]
    fn detector_name() {
        let detector = DependencyUsageDetector;
        assert_eq!(detector.name(), "dependency_usage");
    }

    #[test]
    fn supports_all_languages() {
        let detector = DependencyUsageDetector;
        assert_eq!(detector.supported_languages(), Language::all());
    }

    #[test]
    fn empty_dependencies_produces_no_findings() {
        let detector = DependencyUsageDetector;
        let file = make_rust_file_with_deps(Vec::new(), Vec::new());
        let findings = detector.detect(&file);
        assert!(findings.is_empty());
    }

    #[test]
    fn single_rust_http_library_is_canonical() {
        let detector = DependencyUsageDetector;
        let file = make_rust_file_with_deps(
            vec![
                dep("reqwest", "reqwest::Client", 5),
                dep("reqwest", "reqwest::get", 10),
            ],
            vec![import("reqwest", &["Client", "get"])],
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention)
            .expect("should have a convention finding");
        assert!(convention.description.contains("reqwest"));
        assert!(convention.description.contains("HTTP"));
        assert!(convention.follows_convention);
    }

    #[test]
    fn conflicting_rust_http_libraries_flagged() {
        let detector = DependencyUsageDetector;
        let file = make_rust_file_with_deps(
            vec![
                dep("reqwest", "reqwest::Client", 5),
                dep("hyper", "hyper::Server", 10),
            ],
            vec![import("reqwest", &["Client"]), import("hyper", &["Server"])],
        );
        let findings = detector.detect(&file);

        let observation = findings
            .iter()
            .find(|f| {
                f.nature == KnowledgeNature::Observation && f.description.contains("Conflicting")
            })
            .expect("should have a conflict observation");
        assert!(observation.description.contains("HTTP"));
        assert!(!observation.follows_convention);
    }

    #[test]
    fn canonical_is_most_used_in_domain() {
        let detector = DependencyUsageDetector;
        let file = make_rust_file_with_deps(
            vec![
                dep("reqwest", "reqwest::Client", 5),
                dep("reqwest", "reqwest::get", 10),
                dep("reqwest", "reqwest::Url", 15),
                dep("hyper", "hyper::Server", 20),
            ],
            vec![
                import("reqwest", &["Client", "get", "Url"]),
                import("hyper", &["Server"]),
            ],
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention && f.description.contains("HTTP"))
            .expect("should have HTTP convention");
        assert!(
            convention.description.contains("reqwest"),
            "reqwest should be canonical (3 usages vs 1)"
        );
    }

    #[test]
    fn typescript_testing_library_detected() {
        let detector = DependencyUsageDetector;
        let file = make_ts_file_with_deps(
            vec![dep("jest", "jest", 1), dep("jest", "jest", 5)],
            vec![import("jest", &["describe", "it"])],
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention && f.description.contains("testing"))
            .expect("should detect jest as testing library");
        assert!(convention.description.contains("jest"));
    }

    #[test]
    fn dead_dependency_flagged() {
        let detector = DependencyUsageDetector;
        // Dependency is listed but no matching import exists.
        let file = make_rust_file_with_deps(
            vec![dep("serde", "serde::Serialize", 1)],
            Vec::new(), // No imports at all
        );
        let findings = detector.detect(&file);

        let dead = findings
            .iter()
            .find(|f| f.description.contains("dead dependency"))
            .expect("should flag dead dependency");
        assert!(dead.description.contains("serde"));
        assert!(!dead.follows_convention);
    }

    #[test]
    fn dependency_with_matching_import_not_flagged_dead() {
        let detector = DependencyUsageDetector;
        let file = make_rust_file_with_deps(
            vec![dep("serde", "serde::Serialize", 1)],
            vec![import("serde", &["Serialize"])],
        );
        let findings = detector.detect(&file);

        let dead = findings
            .iter()
            .find(|f| f.description.contains("dead dependency"));
        assert!(dead.is_none(), "should not flag serde as dead");
    }

    #[test]
    fn multiple_domains_detected_independently() {
        let detector = DependencyUsageDetector;
        let file = make_rust_file_with_deps(
            vec![
                dep("reqwest", "reqwest::Client", 5),
                dep("tracing", "tracing::info", 10),
                dep("clap", "clap::Parser", 15),
            ],
            vec![
                import("reqwest", &["Client"]),
                import("tracing", &["info"]),
                import("clap", &["Parser"]),
            ],
        );
        let findings = detector.detect(&file);

        let conventions: Vec<&ConventionFinding> = findings
            .iter()
            .filter(|f| f.nature == KnowledgeNature::Convention)
            .collect();

        assert_eq!(conventions.len(), 3, "HTTP, logging, CLI");
        assert!(conventions.iter().any(|f| f.description.contains("HTTP")));
        assert!(
            conventions
                .iter()
                .any(|f| f.description.contains("logging"))
        );
        assert!(conventions.iter().any(|f| f.description.contains("CLI")));
    }

    #[test]
    fn unknown_package_produces_no_domain_finding() {
        let detector = DependencyUsageDetector;
        let file = make_rust_file_with_deps(
            vec![dep("my-internal-crate", "my_internal_crate::Foo", 1)],
            vec![import("my-internal-crate", &["Foo"])],
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention);
        assert!(
            convention.is_none(),
            "unknown packages should not produce domain findings"
        );
    }

    #[test]
    fn python_domain_classification() {
        let detector = DependencyUsageDetector;
        let file = ProjectFile {
            path: PathBuf::from("app.py"),
            language: Language::Python,
            content_hash: String::new(),
            imports: vec![import("fastapi", &["FastAPI"])],
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: vec![dep("fastapi", "fastapi.FastAPI", 1)],
            language_ir: LanguageIR::Python(seshat_core::PythonIR::default()),
        };
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention)
            .expect("should detect fastapi as web framework");
        assert!(convention.description.contains("fastapi"));
        assert!(convention.description.contains("web framework"));
    }

    #[test]
    fn javascript_domain_classification() {
        let detector = DependencyUsageDetector;
        let file = ProjectFile {
            path: PathBuf::from("server.js"),
            language: Language::JavaScript,
            content_hash: String::new(),
            imports: vec![import("express", &["express"])],
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: vec![dep("express", "express", 1)],
            language_ir: LanguageIR::JavaScript(seshat_core::JavaScriptIR::default()),
        };
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention)
            .expect("should detect express as web framework");
        assert!(convention.description.contains("express"));
        assert!(convention.description.contains("web framework"));
    }

    #[test]
    fn evidence_includes_import_paths() {
        let detector = DependencyUsageDetector;
        let file = make_rust_file_with_deps(
            vec![
                dep("tracing", "tracing::info", 5),
                dep("tracing", "tracing::warn", 10),
            ],
            vec![import("tracing", &["info", "warn"])],
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention)
            .expect("should have logging convention");
        assert!(!convention.evidence.is_empty());
        assert!(
            convention
                .evidence
                .iter()
                .any(|e| e.snippet.contains("tracing::info"))
        );
    }

    // --- Domain classification unit tests ---

    #[test]
    fn classify_domain_rust_http() {
        assert_eq!(
            classify_domain("reqwest", Language::Rust),
            Some(DependencyDomain::Http)
        );
        assert_eq!(
            classify_domain("axum", Language::Rust),
            Some(DependencyDomain::WebFramework)
        );
    }

    #[test]
    fn classify_domain_rust_logging() {
        assert_eq!(
            classify_domain("tracing", Language::Rust),
            Some(DependencyDomain::Logging)
        );
        assert_eq!(
            classify_domain("log", Language::Rust),
            Some(DependencyDomain::Logging)
        );
    }

    #[test]
    fn classify_domain_ts_testing() {
        assert_eq!(
            classify_domain("jest", Language::TypeScript),
            Some(DependencyDomain::Testing)
        );
        assert_eq!(
            classify_domain("vitest", Language::TypeScript),
            Some(DependencyDomain::Testing)
        );
    }

    #[test]
    fn classify_domain_python_database() {
        assert_eq!(
            classify_domain("sqlalchemy", Language::Python),
            Some(DependencyDomain::Database)
        );
        assert_eq!(
            classify_domain("asyncpg", Language::Python),
            Some(DependencyDomain::Database)
        );
    }

    #[test]
    fn classify_domain_unknown_returns_none() {
        assert_eq!(classify_domain("my-custom-lib", Language::Rust), None);
        assert_eq!(
            classify_domain("internal-utils", Language::TypeScript),
            None
        );
        assert_eq!(classify_domain("my_app", Language::Python), None);
    }
}
