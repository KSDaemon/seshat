//! Performance guard for transitive `query_dependencies` (US-005).
//!
//! Builds a synthetic 3000-file IR shaped as a 3-level fan-out tree rooted at
//! `src/root.ts`, then runs `query_dependencies` at `depth = 3` and asserts
//! the wall-clock stays under 500 ms — a 2× headroom on the 1s MCP P95 target
//! that `query_dependencies` is contracted against in production.
//!
//! Gated with `#[ignore]` so the perf budget is never enforced as part of the
//! default `cargo test` flow. Run explicitly via:
//!
//! ```sh
//! cargo test -p seshat-graph --test transitive_perf -- --ignored --nocapture
//! ```
//!
//! Notes on shape:
//! - The tree fans out as `1 root → 100 L1 → 200 L2 → 2699 L3` for a total of
//!   exactly **3000** files. Branching at L2 and L3 is round-robin so the
//!   work is uniformly distributed across parents.
//! - At `depth = 3` the BFS visits all 100 L1 entries and all 200 L2 entries
//!   (300 < `MAX_DEPENDENTS = 500`) and then fills the remaining 200 slots
//!   from L3 before truncating — so the test exercises the full IR-load +
//!   reverse-adjacency build + truncated BFS path that production callers
//!   take.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rusqlite::{Connection, params};
use seshat_core::{Import, Language, LanguageIR, ProjectFile, RustIR};
use seshat_graph::{QueryDependenciesOptions, query_dependencies};
use seshat_storage::Database;

const TOTAL_FILES: usize = 3000;
const L1_COUNT: usize = 100;
const L2_COUNT: usize = 200;
const L3_COUNT: usize = TOTAL_FILES - 1 - L1_COUNT - L2_COUNT; // 2699
const PERF_BUDGET_MS: u128 = 500;

/// Open an in-memory database with all migrations applied.
fn test_conn() -> Arc<Mutex<Connection>> {
    let db = Database::open(":memory:").expect("in-memory DB");
    db.connection().clone()
}

/// Bulk-insert every IR row in one transaction. Insertion is outside the
/// timed region but a single transaction keeps test setup snappy
/// (≈ 90 ms for 3000 rows on dev hardware).
fn bulk_insert_ir(conn: &Arc<Mutex<Connection>>, branch_id: &str, files: &[ProjectFile]) {
    let mut c = conn.lock().expect("conn lock");
    let tx = c.transaction().expect("begin tx");
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO files_ir (branch_id, file_path, language, content_hash, ir_data)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .expect("prepare");
        for file in files {
            let ir_data = seshat_storage::serialize_ir(file).expect("serialize IR");
            let file_path = file.path.to_string_lossy();
            stmt.execute(params![
                branch_id,
                file_path.as_ref(),
                file.language.as_str(),
                file.content_hash,
                ir_data,
            ])
            .expect("insert IR");
        }
    }
    tx.commit().expect("commit tx");
}

/// Build a minimal `ProjectFile` at `path` that imports a single relative
/// module (or no module if `import_module` is `None`).
fn make_file(path: String, import_module: Option<String>) -> ProjectFile {
    let imports = import_module
        .map(|m| {
            vec![Import {
                module: m,
                names: Vec::new(),
                is_type_only: false,
                line: 1,
            }]
        })
        .unwrap_or_default();

    ProjectFile {
        path: PathBuf::from(path.clone()),
        language: Language::TypeScript,
        content_hash: format!("hash_{path}"),
        imports,
        exports: Vec::new(),
        functions: Vec::new(),
        types: Vec::new(),
        dependencies_used: Vec::new(),
        language_ir: LanguageIR::Rust(RustIR::default()),
        file_doc: None,
    }
}

/// Construct the 3-level fan-out IR.
///
/// Layout:
/// - `src/root.ts` (1 file, no imports)
/// - `src/l1/file_NNNN.ts` (`L1_COUNT` files, each imports `"../root"`)
/// - `src/l2/file_NNNN.ts` (`L2_COUNT` files, each imports a unique L1
///   neighbour via `"../l1/file_PPPP"`, round-robin so the work is
///   uniformly distributed)
/// - `src/l3/file_NNNN.ts` (`L3_COUNT` files, each imports a unique L2
///   neighbour via `"../l2/file_PPPP"`, also round-robin)
fn build_fanout_tree() -> Vec<ProjectFile> {
    let mut files: Vec<ProjectFile> = Vec::with_capacity(TOTAL_FILES);

    // Root.
    files.push(make_file("src/root.ts".to_owned(), None));

    // Level 1: every L1 file imports the root.
    for i in 0..L1_COUNT {
        let path = format!("src/l1/file_{i:04}.ts");
        files.push(make_file(path, Some("../root".to_owned())));
    }

    // Level 2: round-robin onto L1 parents.
    for j in 0..L2_COUNT {
        let path = format!("src/l2/file_{j:04}.ts");
        let parent_idx = j % L1_COUNT;
        let import = format!("../l1/file_{parent_idx:04}");
        files.push(make_file(path, Some(import)));
    }

    // Level 3: round-robin onto L2 parents.
    for k in 0..L3_COUNT {
        let path = format!("src/l3/file_{k:04}.ts");
        let parent_idx = k % L2_COUNT;
        let import = format!("../l2/file_{parent_idx:04}");
        files.push(make_file(path, Some(import)));
    }

    assert_eq!(files.len(), TOTAL_FILES);
    files
}

#[test]
#[ignore = "perf budget — run explicitly with --ignored"]
fn query_dependencies_depth_3_under_500ms_with_3000_files() {
    let conn = test_conn();
    let files = build_fanout_tree();
    bulk_insert_ir(&conn, "main", &files);

    // Warm-up: pay the IR load + adjacency build cost once so the timed call
    // measures a hot run (matches production where the IR cache amortises
    // load cost across requests).
    let _ = query_dependencies(
        &conn,
        "main",
        "src/root.ts",
        QueryDependenciesOptions { depth: 3 },
    )
    .expect("warm-up query_dependencies");

    let start = Instant::now();
    let result = query_dependencies(
        &conn,
        "main",
        "src/root.ts",
        QueryDependenciesOptions { depth: 3 },
    )
    .expect("timed query_dependencies");
    let elapsed = start.elapsed();

    let elapsed_ms = elapsed.as_millis();
    eprintln!(
        "query_dependencies(depth=3) over {TOTAL_FILES} files: {elapsed_ms} ms \
         (dependents={}, requested_depth={}, transitive_count={})",
        result.dependents.len(),
        result.requested_depth,
        result.transitive_dependent_count,
    );

    // Sanity: BFS must have reached all three depths and capped at the
    // internal `MAX_DEPENDENTS = 500` budget. Without these checks the perf
    // assertion below would silently pass even if the work was a no-op
    // (e.g. an unresolved import chain).
    assert_eq!(result.requested_depth, 3);
    assert!(
        result.dependents.iter().any(|d| d.depth == 3),
        "expected at least one depth=3 entry in the BFS result"
    );
    assert_eq!(
        result.dependents.len(),
        500,
        "expected BFS to fill the MAX_DEPENDENTS cap with a 3000-file tree"
    );

    assert!(
        elapsed_ms < PERF_BUDGET_MS,
        "query_dependencies(depth=3) took {elapsed_ms} ms, exceeds {PERF_BUDGET_MS} ms budget"
    );
}
