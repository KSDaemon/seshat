//! Unified dependency domain taxonomy and package classification.
//!
//! Provides a single [`DependencyDomain`] enum that classifies dependencies
//! by their functional role, plus [`classify_domain`] — the **single source of
//! truth** for mapping package names to domains. Both the scanner (manifest
//! analysis) and the detectors (usage analysis) call this function.

use serde::{Deserialize, Serialize};

use crate::ir::Language;

/// Functional domain a dependency belongs to.
///
/// Covers the union of all categories previously split across
/// `DependencyCategory` (scanner) and the old `DependencyDomain` (detectors).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyDomain {
    /// HTTP clients (reqwest, axios, httpx, etc.).
    Http,
    /// Web frameworks (actix-web, express, flask, django, axum, rocket, etc.).
    WebFramework,
    /// Logging and observability (tracing, winston, loguru, etc.).
    Logging,
    /// Testing frameworks and utilities (jest, pytest, proptest, etc.).
    Testing,
    /// Input validation and schema enforcement (zod, pydantic, validator, etc.).
    Validation,
    /// Serialization and deserialization (serde, protobuf, msgpack, etc.).
    Serialization,
    /// Database clients and ORMs (sqlx, prisma, sqlalchemy, etc.).
    Database,
    /// CLI argument parsing (clap, commander, click, etc.).
    Cli,
    /// Async runtimes and utilities (tokio, asyncio, trio, etc.).
    AsyncRuntime,
    /// Cryptography and security (ring, bcrypt, hashlib, etc.).
    Crypto,
    /// General-purpose utility libraries.
    Utilities,
    /// Could not be classified into any known domain.
    Unknown,
}

impl DependencyDomain {
    /// Human-readable name used in finding descriptions.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "HTTP",
            Self::WebFramework => "web framework",
            Self::Logging => "logging",
            Self::Testing => "testing",
            Self::Validation => "validation",
            Self::Serialization => "serialization",
            Self::Database => "database",
            Self::Cli => "CLI",
            Self::AsyncRuntime => "async runtime",
            Self::Crypto => "crypto",
            Self::Utilities => "utilities",
            Self::Unknown => "unknown",
        }
    }
}

// ---------------------------------------------------------------------------
// Module-path helpers (single source of truth)
// ---------------------------------------------------------------------------

/// Extract the top-level package/module name from an import path or
/// callee, regardless of the source language's separator.
///
/// Handles every separator the four supported languages use today:
/// - Rust: `tracing::subscriber` → `tracing`, `crate::foo` → `crate`
/// - Python: `logging.config` → `logging`
/// - npm: `@scope/package` → `@scope`, `lodash/fp` → `lodash`
/// - All: leading whitespace boundary.
///
/// This is the single helper for "what is the top-level package name of
/// this thing?" — replacing several bespoke `split("::").next().unwrap_or(...)`
/// chains spread across the detectors.
///
/// # Examples
///
/// ```
/// use seshat_core::dependency::top_level_module;
///
/// assert_eq!(top_level_module("tracing"), "tracing");
/// assert_eq!(top_level_module("tracing::subscriber"), "tracing");
/// assert_eq!(top_level_module("logging.config"), "logging");
/// assert_eq!(top_level_module("@scope/pkg"), "@scope");
/// assert_eq!(top_level_module("crate::foo::bar"), "crate");
/// ```
pub fn top_level_module(module: &str) -> &str {
    let pos = module
        .chars()
        .position(|c| [' ', ':', '.', '/'].contains(&c));
    match pos {
        Some(p) => &module[..p],
        None => module,
    }
}

/// Check whether `module` is a Python standard-library top-level package.
///
/// Splits on `.` to get the root segment, then matches a curated list
/// of stdlib modules. Used by heuristic detectors to skip stdlib
/// imports — e.g. `traceback`, `unittest.mock`, `logging.config` should
/// not surface as "Possible logging library (name heuristic)" or
/// "Testing-related import (heuristic)" since they're language built-ins,
/// not project-internal nor third-party.
///
/// # Examples
///
/// ```
/// use seshat_core::dependency::is_python_stdlib_module;
///
/// assert!(is_python_stdlib_module("logging"));
/// assert!(is_python_stdlib_module("logging.config"));
/// assert!(is_python_stdlib_module("traceback"));
/// assert!(is_python_stdlib_module("unittest.mock"));
/// assert!(!is_python_stdlib_module("loguru"));
/// assert!(!is_python_stdlib_module("waltchat"));
/// ```
pub fn is_python_stdlib_module(module: &str) -> bool {
    let root = module.split('.').next().unwrap_or(module);
    matches!(
        root,
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
// Word-boundary keyword matching (shared by heuristic classifiers)
// ---------------------------------------------------------------------------

/// True when any of `keywords` appears in `name` at a word boundary.
///
/// Word boundaries: start-of-string, the byte after `_` / `-`, or a
/// camelCase transition (lowercase byte → uppercase byte). ASCII-only
/// — non-ASCII bytes degrade gracefully (their boundary checks return
/// false, so we never panic on UTF-8 byte-index drift).
///
/// This is the **single source of truth** for the heuristic boundary
/// rules. Used by [`crate`]'s consumers in two parallel classifiers:
/// `dependency_usage::classify_heuristic_domain` (which scans multiple
/// keyword groups, one per domain) and `test_patterns::is_heuristic_test_dep`.
/// Keeping the rule in one place prevents the two from drifting.
///
/// Empty keywords are skipped to avoid an infinite loop on `find("")`.
///
/// # Examples
///
/// ```
/// use seshat_core::dependency::matches_keyword_at_boundary;
///
/// // start-of-string boundary
/// assert!(matches_keyword_at_boundary("ormlib", &["orm"]));
/// // `_` separator boundary
/// assert!(matches_keyword_at_boundary("my_orm_lib", &["orm"]));
/// // `-` separator boundary
/// assert!(matches_keyword_at_boundary("my-orm-lib", &["orm"]));
/// // camelCase transition boundary
/// assert!(matches_keyword_at_boundary("myOrmLib", &["orm"]));
/// // substring inside another word — NOT a boundary match
/// assert!(!matches_keyword_at_boundary("format", &["orm"]));
/// // empty keyword: never matches (and never loops)
/// assert!(!matches_keyword_at_boundary("anything", &[""]));
/// ```
pub fn matches_keyword_at_boundary(name: &str, keywords: &[&str]) -> bool {
    let lower = name.to_ascii_lowercase();
    let bytes = name.as_bytes();
    for kw in keywords {
        if kw.is_empty() {
            continue;
        }
        let mut search_start = 0usize;
        while let Some(pos) = lower[search_start..].find(kw) {
            let abs_pos = search_start + pos;
            let prev = abs_pos.checked_sub(1).and_then(|i| bytes.get(i)).copied();
            let curr = bytes.get(abs_pos).copied();
            let is_boundary = abs_pos == 0
                || prev.is_some_and(|b| b == b'_' || b == b'-')
                || (prev.is_some_and(|b| b.is_ascii_lowercase())
                    && curr.is_some_and(|b| b.is_ascii_uppercase()));
            if is_boundary {
                return true;
            }
            search_start = abs_pos + 1;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Package → Domain classification (single source of truth)
// ---------------------------------------------------------------------------

/// Classify a package name into its functional domain for the given language.
///
/// The name is **normalised** internally — lowercased and hyphens replaced with
/// underscores — so both manifest names (`"serde-json"`) and import-path names
/// (`"serde_json"`) resolve correctly.
///
/// Returns `None` when the package does not appear in any known list.
///
/// # Examples
///
/// ```
/// use seshat_core::ir::Language;
/// use seshat_core::dependency::{DependencyDomain, classify_domain};
///
/// assert_eq!(classify_domain("reqwest", Language::Rust), Some(DependencyDomain::Http));
/// assert_eq!(classify_domain("serde-json", Language::Rust), Some(DependencyDomain::Serialization));
/// assert_eq!(classify_domain("my-custom-lib", Language::Rust), None);
/// ```
pub fn classify_domain(package: &str, language: Language) -> Option<DependencyDomain> {
    let normalised = package.to_lowercase().replace('-', "_");
    match language {
        Language::Rust => classify_rust(&normalised),
        Language::TypeScript | Language::JavaScript => classify_js_ts(&normalised),
        Language::Python => classify_python(&normalised),
    }
}

fn classify_rust(name: &str) -> Option<DependencyDomain> {
    match name {
        // HTTP clients
        "reqwest" | "hyper" | "ureq" | "curl" | "attohttpc" | "isahc" | "tonic" | "prost"
        | "tower" | "tower_http" => Some(DependencyDomain::Http),
        // Web frameworks
        "actix_web" | "axum" | "warp" | "rocket" | "tide" | "poem" | "salvo" | "ntex" => {
            Some(DependencyDomain::WebFramework)
        }
        // Logging
        "tracing" | "tracing_subscriber" | "tracing_log" | "log" | "env_logger"
        | "pretty_env_logger" | "slog" | "flexi_logger" => Some(DependencyDomain::Logging),
        // Testing
        "proptest" | "quickcheck" | "rstest" | "criterion" | "test_case" | "mockall"
        | "wiremock" | "assert_cmd" | "assert_fs" | "assert_matches" | "pretty_assertions"
        | "insta" | "tempfile" => Some(DependencyDomain::Testing),
        // Validation
        "validator" | "garde" | "schemars" => Some(DependencyDomain::Validation),
        // Serialization
        "serde" | "serde_json" | "serde_yaml" | "serde_toml" | "toml" | "bincode" | "ciborium"
        | "postcard" | "rmp_serde" | "ron" | "csv" => Some(DependencyDomain::Serialization),
        // Database
        "sqlx" | "diesel" | "sea_orm" | "rusqlite" | "tokio_postgres" | "deadpool_postgres"
        | "mongodb" | "redis" | "surrealdb" => Some(DependencyDomain::Database),
        // CLI
        "clap" | "structopt" | "argh" | "pico_args" | "bpaf" => Some(DependencyDomain::Cli),
        // Async runtime
        "tokio" | "async_std" | "smol" | "futures" | "async_trait" | "rayon" | "crossbeam"
        | "crossbeam_channel" => Some(DependencyDomain::AsyncRuntime),
        // Crypto
        "sha2" | "ring" | "rustls" | "openssl" | "aes" | "argon2" | "bcrypt" | "hmac" => {
            Some(DependencyDomain::Crypto)
        }
        // Utilities
        "uuid" | "chrono" | "time" | "url" | "bytes" | "indexmap" | "dashmap" | "parking_lot"
        | "once_cell" | "lazy_static" | "anyhow" | "thiserror" | "eyre" | "itertools" | "regex"
        | "rand" => Some(DependencyDomain::Utilities),
        _ => None,
    }
}

fn classify_js_ts(name: &str) -> Option<DependencyDomain> {
    match name {
        // HTTP clients
        "axios"
        | "node_fetch"
        | "got"
        | "ky"
        | "superagent"
        | "undici"
        | "socket_io"
        | "socket_io_client"
        | "socket.io"
        | "socket.io_client"
        | "graphql"
        | "@apollo/client"
        | "urql"
        | "@tanstack/react_query"
        | "react_query"
        | "@tanstack/query_core"
        | "swr" => Some(DependencyDomain::Http),
        // Web frameworks
        "express" | "fastify" | "koa" | "hapi" | "next" | "hono" | "nest" | "nuxt" | "react"
        | "vue" | "angular" | "svelte" | "remix" | "astro" => Some(DependencyDomain::WebFramework),
        // Logging
        "winston" | "pino" | "bunyan" | "morgan" | "log4js" | "loglevel" | "debug" | "signale"
        | "consola" => Some(DependencyDomain::Logging),
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
        | "testing_library"
        | "@testing_library/react"
        | "@testing_library/jest_dom"
        | "supertest"
        | "nock"
        | "msw" => Some(DependencyDomain::Testing),
        // Validation
        "zod" | "joi" | "yup" | "ajv" | "class_validator" | "superstruct" | "io_ts" | "valibot" => {
            Some(DependencyDomain::Validation)
        }
        // Serialization
        "protobufjs" | "avro_js" | "msgpack" | "@msgpack/msgpack" | "flatbuffers" => {
            Some(DependencyDomain::Serialization)
        }
        // Database
        "prisma" | "@prisma/client" | "typeorm" | "sequelize" | "knex" | "mongoose"
        | "drizzle_orm" | "pg" | "mysql2" | "better_sqlite3" | "ioredis" | "redis" => {
            Some(DependencyDomain::Database)
        }
        // CLI
        "commander" | "yargs" | "meow" | "cac" | "citty" | "oclif" | "inquirer" => {
            Some(DependencyDomain::Cli)
        }
        // Utilities
        "zustand"
        | "redux"
        | "@reduxjs/toolkit"
        | "recoil"
        | "jotai"
        | "mobx"
        | "xstate"
        | "react_router"
        | "react_router_dom"
        | "@tanstack/react_router"
        | "lodash"
        | "ramda"
        | "underscore"
        | "immer"
        | "date_fns"
        | "dayjs"
        | "moment"
        | "luxon"
        | "dotenv"
        | "cross_env"
        | "@sentry/react"
        | "@sentry/nextjs"
        | "@sentry/node" => Some(DependencyDomain::Utilities),
        _ => None,
    }
}

fn classify_python(name: &str) -> Option<DependencyDomain> {
    // Note: several names here (`logging`, `asyncio`, `hashlib`, `sqlite3`, `json`,
    // `argparse`, `unittest`, `pickle`) are Python stdlib modules and will be filtered
    // out by `is_python_stdlib_or_relative` in the parser before they ever reach
    // this function. They are kept in the match for completeness and for any
    // caller that bypasses the parser filter (e.g. tests or future heuristics).
    match name {
        // HTTP clients
        "requests" | "httpx" | "aiohttp" | "urllib3" | "httplib2" | "websockets"
        | "websocket_client" | "python_socketio" | "grpcio" | "grpcio_tools" => {
            Some(DependencyDomain::Http)
        }
        // Web frameworks
        "flask" | "django" | "fastapi" | "starlette" | "tornado" | "sanic" | "pyramid"
        | "bottle" | "litestar" | "blacksheep" => Some(DependencyDomain::WebFramework),
        // Logging (loguru / structlog are third-party; `logging` is stdlib but kept for completeness)
        "logging" | "loguru" | "structlog" => Some(DependencyDomain::Logging),
        // Testing (unittest is stdlib but kept for completeness)
        "pytest" | "unittest" | "nose" | "hypothesis" | "mock" | "unittest_mock" | "faker"
        | "factory_boy" | "responses" | "pytest_mock" | "pytest_asyncio" | "tox" | "coverage"
        | "pytest_cov" => Some(DependencyDomain::Testing),
        // Validation
        "pydantic" | "marshmallow" | "cerberus" | "attrs" | "voluptuous" | "cattrs" => {
            Some(DependencyDomain::Validation)
        }
        // Serialization (json/pickle are stdlib but kept for completeness)
        "json" | "msgpack" | "protobuf" | "avro" | "pickle" | "pyyaml" | "toml" | "orjson"
        | "ujson" => Some(DependencyDomain::Serialization),
        // Database (sqlite3 is stdlib but kept for completeness)
        "sqlalchemy" | "psycopg2" | "asyncpg" | "pymongo" | "redis" | "peewee" | "tortoise"
        | "tortoise_orm" | "databases" | "sqlite3" | "alembic" | "aioredis" | "motor" | "neo4j"
        | "py2neo" | "pinecone" | "qdrant_client" | "chromadb" | "weaviate_client" | "pymilvus"
        | "elasticsearch" | "opensearch_py" => Some(DependencyDomain::Database),
        // CLI (argparse is stdlib but kept for completeness)
        "click" | "argparse" | "typer" | "fire" | "docopt" | "rich" => Some(DependencyDomain::Cli),
        // Async runtime (asyncio is stdlib but kept for completeness)
        "asyncio" | "trio" | "anyio" | "uvloop" | "twisted" | "celery" | "dramatiq" | "uvicorn"
        | "gunicorn" | "hypercorn" | "daphne" => Some(DependencyDomain::AsyncRuntime),
        // Crypto (hashlib is stdlib but kept for completeness)
        "cryptography" | "pycryptodome" | "hashlib" | "passlib" | "bcrypt" | "itsdangerous"
        | "jwt" | "python_jose" | "authlib" => Some(DependencyDomain::Crypto),
        // Utilities — AI/ML, data science, cloud, misc popular libs
        "openai"
        | "anthropic"
        | "cohere"
        | "google_generativeai"
        | "google_genai"
        | "langchain"
        | "langchain_core"
        | "langchain_openai"
        | "langchain_anthropic"
        | "langfuse"
        | "litellm"
        | "transformers"
        | "sentence_transformers"
        | "pandas"
        | "numpy"
        | "scipy"
        | "polars"
        | "pyarrow"
        | "boto3"
        | "botocore"
        | "aiobotocore"
        | "google_cloud_storage"
        | "azure_storage_blob"
        | "jinja2"
        | "mako"
        | "tenacity"
        | "backoff"
        | "retry"
        | "paramiko"
        | "fabric"
        | "pillow"
        | "pil"
        | "cv2"
        | "opencv_python"
        | "stripe"
        | "sendgrid"
        | "sqlglot"
        | "alembic_utils" => Some(DependencyDomain::Utilities),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Rust --

    #[test]
    fn rust_http_clients() {
        assert_eq!(
            classify_domain("reqwest", Language::Rust),
            Some(DependencyDomain::Http)
        );
        assert_eq!(
            classify_domain("hyper", Language::Rust),
            Some(DependencyDomain::Http)
        );
    }

    #[test]
    fn rust_web_frameworks() {
        assert_eq!(
            classify_domain("axum", Language::Rust),
            Some(DependencyDomain::WebFramework)
        );
        // Hyphenated name normalises to underscore.
        assert_eq!(
            classify_domain("actix-web", Language::Rust),
            Some(DependencyDomain::WebFramework)
        );
    }

    #[test]
    fn rust_logging() {
        assert_eq!(
            classify_domain("tracing", Language::Rust),
            Some(DependencyDomain::Logging)
        );
        assert_eq!(
            classify_domain("log", Language::Rust),
            Some(DependencyDomain::Logging)
        );
        assert_eq!(
            classify_domain("tracing-subscriber", Language::Rust),
            Some(DependencyDomain::Logging)
        );
    }

    #[test]
    fn rust_testing() {
        assert_eq!(
            classify_domain("proptest", Language::Rust),
            Some(DependencyDomain::Testing)
        );
        assert_eq!(
            classify_domain("pretty_assertions", Language::Rust),
            Some(DependencyDomain::Testing)
        );
        assert_eq!(
            classify_domain("tempfile", Language::Rust),
            Some(DependencyDomain::Testing)
        );
    }

    #[test]
    fn rust_serialization() {
        assert_eq!(
            classify_domain("serde", Language::Rust),
            Some(DependencyDomain::Serialization)
        );
        assert_eq!(
            classify_domain("serde-json", Language::Rust),
            Some(DependencyDomain::Serialization)
        );
        assert_eq!(
            classify_domain("serde_json", Language::Rust),
            Some(DependencyDomain::Serialization)
        );
    }

    #[test]
    fn rust_database() {
        assert_eq!(
            classify_domain("sqlx", Language::Rust),
            Some(DependencyDomain::Database)
        );
        assert_eq!(
            classify_domain("sea-orm", Language::Rust),
            Some(DependencyDomain::Database)
        );
        assert_eq!(
            classify_domain("rusqlite", Language::Rust),
            Some(DependencyDomain::Database)
        );
    }

    #[test]
    fn rust_async_runtime() {
        assert_eq!(
            classify_domain("tokio", Language::Rust),
            Some(DependencyDomain::AsyncRuntime)
        );
        assert_eq!(
            classify_domain("async-std", Language::Rust),
            Some(DependencyDomain::AsyncRuntime)
        );
    }

    #[test]
    fn rust_crypto() {
        assert_eq!(
            classify_domain("ring", Language::Rust),
            Some(DependencyDomain::Crypto)
        );
    }

    // -- JS/TS --

    #[test]
    fn js_ts_http_clients() {
        assert_eq!(
            classify_domain("axios", Language::TypeScript),
            Some(DependencyDomain::Http)
        );
        assert_eq!(
            classify_domain("node-fetch", Language::JavaScript),
            Some(DependencyDomain::Http)
        );
    }

    #[test]
    fn js_ts_web_frameworks() {
        assert_eq!(
            classify_domain("express", Language::JavaScript),
            Some(DependencyDomain::WebFramework)
        );
        assert_eq!(
            classify_domain("react", Language::TypeScript),
            Some(DependencyDomain::WebFramework)
        );
        assert_eq!(
            classify_domain("hono", Language::TypeScript),
            Some(DependencyDomain::WebFramework)
        );
    }

    #[test]
    fn js_ts_testing() {
        assert_eq!(
            classify_domain("jest", Language::TypeScript),
            Some(DependencyDomain::Testing)
        );
        assert_eq!(
            classify_domain("vitest", Language::TypeScript),
            Some(DependencyDomain::Testing)
        );
        assert_eq!(
            classify_domain("cypress", Language::JavaScript),
            Some(DependencyDomain::Testing)
        );
    }

    #[test]
    fn js_ts_database() {
        assert_eq!(
            classify_domain("prisma", Language::TypeScript),
            Some(DependencyDomain::Database)
        );
        assert_eq!(
            classify_domain("drizzle-orm", Language::TypeScript),
            Some(DependencyDomain::Database)
        );
    }

    // -- Python --

    #[test]
    fn python_http_clients() {
        assert_eq!(
            classify_domain("requests", Language::Python),
            Some(DependencyDomain::Http)
        );
        assert_eq!(
            classify_domain("httpx", Language::Python),
            Some(DependencyDomain::Http)
        );
    }

    #[test]
    fn python_web_frameworks() {
        assert_eq!(
            classify_domain("django", Language::Python),
            Some(DependencyDomain::WebFramework)
        );
        assert_eq!(
            classify_domain("fastapi", Language::Python),
            Some(DependencyDomain::WebFramework)
        );
    }

    #[test]
    fn python_testing() {
        assert_eq!(
            classify_domain("pytest", Language::Python),
            Some(DependencyDomain::Testing)
        );
        assert_eq!(
            classify_domain("hypothesis", Language::Python),
            Some(DependencyDomain::Testing)
        );
    }

    #[test]
    fn python_database() {
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
    fn python_async_runtime() {
        assert_eq!(
            classify_domain("asyncio", Language::Python),
            Some(DependencyDomain::AsyncRuntime)
        );
        assert_eq!(
            classify_domain("trio", Language::Python),
            Some(DependencyDomain::AsyncRuntime)
        );
    }

    #[test]
    fn python_crypto() {
        assert_eq!(
            classify_domain("cryptography", Language::Python),
            Some(DependencyDomain::Crypto)
        );
    }

    #[test]
    fn python_utilities_ai_ml() {
        assert_eq!(
            classify_domain("openai", Language::Python),
            Some(DependencyDomain::Utilities)
        );
        assert_eq!(
            classify_domain("anthropic", Language::Python),
            Some(DependencyDomain::Utilities)
        );
        assert_eq!(
            classify_domain("langchain", Language::Python),
            Some(DependencyDomain::Utilities)
        );
        assert_eq!(
            classify_domain("pandas", Language::Python),
            Some(DependencyDomain::Utilities)
        );
        assert_eq!(
            classify_domain("numpy", Language::Python),
            Some(DependencyDomain::Utilities)
        );
        assert_eq!(
            classify_domain("boto3", Language::Python),
            Some(DependencyDomain::Utilities)
        );
    }

    #[test]
    fn python_async_runtime_extended() {
        assert_eq!(
            classify_domain("celery", Language::Python),
            Some(DependencyDomain::AsyncRuntime)
        );
        assert_eq!(
            classify_domain("uvicorn", Language::Python),
            Some(DependencyDomain::AsyncRuntime)
        );
    }

    #[test]
    fn python_database_extended() {
        assert_eq!(
            classify_domain("aioredis", Language::Python),
            Some(DependencyDomain::Database)
        );
        assert_eq!(
            classify_domain("neo4j", Language::Python),
            Some(DependencyDomain::Database)
        );
        assert_eq!(
            classify_domain("qdrant-client", Language::Python),
            Some(DependencyDomain::Database)
        );
    }

    #[test]
    fn python_http_extended() {
        assert_eq!(
            classify_domain("websockets", Language::Python),
            Some(DependencyDomain::Http)
        );
        assert_eq!(
            classify_domain("grpcio", Language::Python),
            Some(DependencyDomain::Http)
        );
    }

    #[test]
    fn js_ts_utilities() {
        assert_eq!(
            classify_domain("zustand", Language::TypeScript),
            Some(DependencyDomain::Utilities)
        );
        assert_eq!(
            classify_domain("redux", Language::TypeScript),
            Some(DependencyDomain::Utilities)
        );
        assert_eq!(
            classify_domain("lodash", Language::JavaScript),
            Some(DependencyDomain::Utilities)
        );
        assert_eq!(
            classify_domain("date-fns", Language::TypeScript),
            Some(DependencyDomain::Utilities)
        );
        assert_eq!(
            classify_domain("dayjs", Language::TypeScript),
            Some(DependencyDomain::Utilities)
        );
    }

    #[test]
    fn js_ts_http_extended() {
        assert_eq!(
            classify_domain("socket.io-client", Language::TypeScript),
            Some(DependencyDomain::Http)
        );
        assert_eq!(
            classify_domain("swr", Language::TypeScript),
            Some(DependencyDomain::Http)
        );
    }

    #[test]
    fn rust_utilities() {
        assert_eq!(
            classify_domain("uuid", Language::Rust),
            Some(DependencyDomain::Utilities)
        );
        assert_eq!(
            classify_domain("chrono", Language::Rust),
            Some(DependencyDomain::Utilities)
        );
        assert_eq!(
            classify_domain("anyhow", Language::Rust),
            Some(DependencyDomain::Utilities)
        );
        assert_eq!(
            classify_domain("thiserror", Language::Rust),
            Some(DependencyDomain::Utilities)
        );
    }

    #[test]
    fn rust_http_extended() {
        assert_eq!(
            classify_domain("tonic", Language::Rust),
            Some(DependencyDomain::Http)
        );
        assert_eq!(
            classify_domain("tower", Language::Rust),
            Some(DependencyDomain::Http)
        );
    }

    // -- Cross-cutting --

    #[test]
    fn unknown_returns_none() {
        assert_eq!(classify_domain("my-custom-lib", Language::Rust), None);
        assert_eq!(
            classify_domain("internal-utils", Language::TypeScript),
            None
        );
        assert_eq!(classify_domain("my_app", Language::Python), None);
    }

    #[test]
    fn hyphen_underscore_normalization() {
        // Both forms resolve to the same domain.
        assert_eq!(
            classify_domain("serde-json", Language::Rust),
            classify_domain("serde_json", Language::Rust)
        );
        assert_eq!(
            classify_domain("actix-web", Language::Rust),
            classify_domain("actix_web", Language::Rust)
        );
        assert_eq!(
            classify_domain("node-fetch", Language::JavaScript),
            classify_domain("node_fetch", Language::JavaScript)
        );
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(
            classify_domain("Reqwest", Language::Rust),
            Some(DependencyDomain::Http)
        );
        assert_eq!(
            classify_domain("AXIOS", Language::TypeScript),
            Some(DependencyDomain::Http)
        );
    }

    // ---- matches_keyword_at_boundary ----

    #[test]
    fn keyword_boundary_start_of_string() {
        assert!(matches_keyword_at_boundary("ormlib", &["orm"]));
        assert!(matches_keyword_at_boundary("test_helper", &["test"]));
    }

    #[test]
    fn keyword_boundary_after_separator() {
        assert!(matches_keyword_at_boundary("my_orm_lib", &["orm"]));
        assert!(matches_keyword_at_boundary("my-orm-lib", &["orm"]));
        assert!(matches_keyword_at_boundary("a_test_b", &["test"]));
    }

    #[test]
    fn keyword_boundary_camel_case() {
        assert!(matches_keyword_at_boundary("myOrmLib", &["orm"]));
        assert!(matches_keyword_at_boundary("notTestLib", &["test"]));
    }

    #[test]
    fn keyword_substring_inside_word_does_not_match() {
        // The whole point of the boundary check: substrings inside
        // longer words must NOT trigger.
        assert!(!matches_keyword_at_boundary("format", &["orm"]));
        assert!(!matches_keyword_at_boundary("request_id", &["test"]));
        assert!(!matches_keyword_at_boundary("timestamp", &["test"]));
        assert!(!matches_keyword_at_boundary("inspect", &["spec"]));
    }

    #[test]
    fn keyword_empty_keyword_does_not_loop_or_match() {
        // Defensive: `find("")` returns Some(0) and would loop forever.
        // The helper must skip empty keywords without scanning.
        assert!(!matches_keyword_at_boundary("anything", &[""]));
        // Mixed list: empty entries are silently skipped, real ones still hit.
        assert!(matches_keyword_at_boundary("orm_lib", &["", "orm", ""]));
    }

    #[test]
    fn keyword_empty_keyword_list_returns_false() {
        assert!(!matches_keyword_at_boundary("orm_lib", &[]));
    }

    #[test]
    fn keyword_non_ascii_input_degrades_gracefully() {
        // Cyrillic / mixed UTF-8: must not panic, must not match.
        assert!(!matches_keyword_at_boundary("ормлиб", &["orm"]));
        // ASCII keyword inside non-ASCII surroundings — boundary checks
        // operate on raw bytes; we only require no panic.
        let _ = matches_keyword_at_boundary("İorm_lib", &["orm"]);
    }

    #[test]
    fn keyword_multiple_keywords_first_match_wins() {
        // The function returns on the first hit; order in the slice
        // doesn't change correctness, just early-exit timing.
        assert!(matches_keyword_at_boundary("my_log_pkg", &["http", "log"]));
        assert!(matches_keyword_at_boundary("my_log_pkg", &["log", "http"]));
    }
}
