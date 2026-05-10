//! Performance bench for [`seshat_graph::map_diff_impact`] (US-011).
//!
//! Drives a 50-file synthetic git repo with a mix of single-hunk (25 files)
//! and multi-hunk (25 files) modifications and benchmarks the full
//! `map_diff_impact` pipeline:
//!
//! 1. `enumerate_changes_with_blobs` (gix index walk + blob ID extraction)
//! 2. `read_blob_pair` + `diff_blobs_to_hunks` per modified file
//! 3. `query_dependencies_batch` at the production
//!    [`DEFAULT_TRANSITIVE_DEPTH`](seshat_graph::DEFAULT_TRANSITIVE_DEPTH)
//!    (= 3), which builds the reverse-adjacency map exactly once across the
//!    full IR
//! 4. Hunk × symbol intersection + per-symbol blast radius classification
//! 5. Convention-risk lookup and overall risk roll-up
//!
//! ## Perf budget
//!
//! The MCP `map_diff_impact` tool is contracted against a 1 s P95 wall-clock
//! budget. This bench locks that budget by:
//!
//! - **Asserting** a single timed reference call (post warm-up) stays under
//!   [`PERF_BUDGET_MS`] = 1000 ms — cheap CI gate runnable via
//!   `cargo bench -p seshat-graph --bench diff_impact_bench`.
//! - **Documenting** the same budget via criterion's median statistic, which
//!   shows up in the standard criterion report (regressions surface as a
//!   median above 1000 ms even before the assert fires).
//!
//! The 50-file shape is the representative pre-merge diff size on the
//! seshat repo itself; if the budget needs to grow the constants below
//! should be revisited together (file count, modification mix, depth).
//!
//! ## Running
//!
//! ```sh
//! cargo bench -p seshat-graph --bench diff_impact_bench
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use std::{fs, hint::black_box};

use criterion::{Criterion, criterion_group, criterion_main};
use rusqlite::{Connection, params};
use seshat_core::{Export, Function, Import, Language, LanguageIR, ProjectFile, RustIR};
use seshat_graph::{DiffImpactRequest, map_diff_impact};
use seshat_storage::Database;
use tempfile::TempDir;

/// Total number of files in the synthetic diff (mirrors the AC).
const FILE_COUNT: usize = 50;
/// Files 0..SINGLE_HUNK_COUNT get a single-hunk modification.
/// Files SINGLE_HUNK_COUNT..FILE_COUNT get a multi-hunk modification.
const SINGLE_HUNK_COUNT: usize = FILE_COUNT / 2;
/// 1 s wall-clock budget — see module-level docs for context.
const PERF_BUDGET_MS: u128 = 1000;

/// Single-line `\n` literal used by every fixture so line numbers in the
/// IR exactly match line numbers in the on-disk source.
const NL: &str = "\n";

/// In-memory DB connection with all migrations applied.
fn test_conn() -> Arc<Mutex<Connection>> {
    let db = Database::open(":memory:").expect("in-memory DB");
    db.connection().clone()
}

/// Bulk-insert IR rows in a single transaction (~30× faster than
/// row-by-row inserts on the default test profile — matches the
/// `transitive_perf.rs` pattern).
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

/// Initialise a git repo at `dir` with deterministic identity so commits
/// are reproducible across bench runs.
fn init_git_repo(dir: &Path) {
    Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(dir)
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "bench@example.com"])
        .current_dir(dir)
        .output()
        .expect("git config email");
    Command::new("git")
        .args(["config", "user.name", "Bench"])
        .current_dir(dir)
        .output()
        .expect("git config name");
    Command::new("git")
        .args(["config", "commit.gpgsign", "false"])
        .current_dir(dir)
        .output()
        .expect("git config gpgsign");
}

fn git_commit_all(dir: &Path, msg: &str) {
    Command::new("git")
        .args(["add", "."])
        .current_dir(dir)
        .output()
        .expect("git add");
    Command::new("git")
        .args(["commit", "-m", msg, "--quiet"])
        .current_dir(dir)
        .output()
        .expect("git commit");
}

/// Build the baseline source for `src/file_NN.ts`.
///
/// Layout (1-indexed lines):
///
/// ```text
///  1  // file NN baseline
///  2
///  3  export function func_NN_0(): number {
///  4    const x = 1;
///  5    const y = 2;
///  6    return x + y;
///  7  }
///  8
///  9  // separator
/// 10
/// 11  export function func_NN_1(): number {
/// 12    const x = 1;
/// 13    const y = 2;
/// 14    return x + y;
/// 15  }
/// 16
/// 17  // separator
/// 18
/// 19  export function func_NN_2(): number {
/// 20    const x = 1;
/// 21    const y = 2;
/// 22    return x + y;
/// 23  }
/// 24
/// ```
///
/// Each function body is 5 lines wide (3..=7, 11..=15, 19..=23). The
/// modifications below target lines inside func_0 and/or func_2 so the
/// hunk-intersection logic flags the matching symbols.
fn baseline_source(idx: usize) -> String {
    let mut s = String::with_capacity(512);
    s.push_str(&format!("// file {idx} baseline"));
    s.push_str(NL);
    s.push_str(NL);
    for which in 0..3 {
        s.push_str(&format!("export function func_{idx}_{which}(): number {{"));
        s.push_str(NL);
        s.push_str("  const x = 1;");
        s.push_str(NL);
        s.push_str("  const y = 2;");
        s.push_str(NL);
        s.push_str("  return x + y;");
        s.push_str(NL);
        s.push('}');
        s.push_str(NL);
        s.push_str(NL);
        if which < 2 {
            s.push_str("// separator");
            s.push_str(NL);
            s.push_str(NL);
        }
    }
    s
}

/// Apply a single-hunk modification: rewrite line 5 (inside func_0 body).
fn apply_single_hunk(src: &str) -> String {
    rewrite_line(src, 5, "  const x = 11;")
}

/// Apply a multi-hunk modification: rewrite line 5 (func_0 body) AND line 21
/// (func_2 body) so the diff produces two separate hunks per file.
fn apply_multi_hunk(src: &str) -> String {
    let s = rewrite_line(src, 5, "  const x = 11;");
    rewrite_line(&s, 21, "  const y = 22;")
}

fn rewrite_line(src: &str, line_1based: usize, replacement: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for (i, line) in src.lines().enumerate() {
        if i + 1 == line_1based {
            out.push_str(replacement);
        } else {
            out.push_str(line);
        }
        out.push_str(NL);
    }
    out
}

/// Build the IR row matching `baseline_source(idx)`.
///
/// Imports: every file (except `idx == 0`) imports `func_(idx-1)_0` from the
/// previous peer. This wires up a 50-deep import chain so `map_diff_impact`
/// exercises depth=3 BFS just like production callers do.
fn make_ir(idx: usize) -> ProjectFile {
    let path = PathBuf::from(format!("src/file_{idx:03}.ts"));

    let imports = if idx == 0 {
        Vec::new()
    } else {
        vec![Import {
            module: format!("./file_{:03}", idx - 1),
            names: vec![format!("func_{}_0", idx - 1)],
            is_type_only: false,
            line: 1,
        }]
    };

    // Function ranges mirror the layout in `baseline_source`.
    let bodies = [(3usize, 7usize), (11, 15), (19, 23)];

    let exports: Vec<Export> = (0..3)
        .map(|which| Export {
            name: format!("func_{idx}_{which}"),
            is_default: false,
            is_type_only: false,
            line: bodies[which].0,
            end_line: bodies[which].1,
        })
        .collect();

    let functions: Vec<Function> = (0..3)
        .map(|which| Function {
            name: format!("func_{idx}_{which}"),
            is_public: true,
            is_async: false,
            line: bodies[which].0,
            end_line: bodies[which].1,
            parameters: Vec::new(),
            doc_comment: None,
        })
        .collect();

    ProjectFile {
        path,
        language: Language::TypeScript,
        content_hash: format!("hash_{idx}"),
        imports,
        exports,
        functions,
        types: Vec::new(),
        dependencies_used: Vec::new(),
        language_ir: LanguageIR::Rust(RustIR::default()),
        file_doc: None,
    }
}

/// Setup state held across criterion iterations — built once, reused for
/// every measured call. `_dir` keeps the tempdir alive for the lifetime
/// of the bench; dropping it would clean up the repo on disk.
struct Setup {
    _dir: TempDir,
    repo: PathBuf,
    conn: Arc<Mutex<Connection>>,
    request: DiffImpactRequest,
}

/// Build the synthetic 50-file repo + IR + working-tree modifications.
fn build_setup() -> Setup {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).expect("create repo dir");
    fs::create_dir_all(repo.join("src")).expect("create src dir");

    // Write every baseline file BEFORE `git init` so the initial commit is
    // a single batched add — significantly faster than one-add-per-file.
    for idx in 0..FILE_COUNT {
        let path = repo.join(format!("src/file_{idx:03}.ts"));
        fs::write(&path, baseline_source(idx)).expect("write baseline");
    }

    init_git_repo(&repo);
    git_commit_all(&repo, "baseline");

    // Insert IR for every file once.
    let conn = test_conn();
    let irs: Vec<ProjectFile> = (0..FILE_COUNT).map(make_ir).collect();
    bulk_insert_ir(&conn, "main", &irs);

    // Apply working-tree modifications (mix of single-hunk + multi-hunk).
    for idx in 0..FILE_COUNT {
        let path = repo.join(format!("src/file_{idx:03}.ts"));
        let baseline = baseline_source(idx);
        let modified = if idx < SINGLE_HUNK_COUNT {
            apply_single_hunk(&baseline)
        } else {
            apply_multi_hunk(&baseline)
        };
        fs::write(&path, modified).expect("write modified");
    }

    let request = DiffImpactRequest {
        staged_only: false,
        base: None,
        repo_path: repo.to_string_lossy().into_owned(),
    };

    Setup {
        _dir: dir,
        repo,
        conn,
        request,
    }
}

/// Sanity-check the synthetic state matches the AC shape so a regression
/// in the fixture itself does not silently mask a real perf regression.
fn assert_setup_shape(setup: &Setup) {
    let result =
        map_diff_impact(&setup.conn, "main", &setup.repo, &setup.request).expect("map_diff_impact");
    assert_eq!(
        result.changed_files.len(),
        FILE_COUNT,
        "expected exactly {FILE_COUNT} changed files in the synthetic diff"
    );
    // Single-hunk files contribute 1 hunk; multi-hunk files contribute 2.
    let expected_hunks = SINGLE_HUNK_COUNT + 2 * (FILE_COUNT - SINGLE_HUNK_COUNT);
    assert_eq!(
        result.total_hunks, expected_hunks,
        "expected {expected_hunks} total hunks (got {})",
        result.total_hunks
    );
    assert!(
        !result.affected_symbols.is_empty(),
        "expected at least one affected symbol from the 50-file diff"
    );
}

fn bench_diff_impact(c: &mut Criterion) {
    let setup = build_setup();
    assert_setup_shape(&setup);

    // Warm-up — pay the IR cache + gix repo discovery cost once so the
    // budget assertion below measures a hot run (matches production where
    // the same MCP server handles repeated calls).
    let _ = map_diff_impact(&setup.conn, "main", &setup.repo, &setup.request)
        .expect("warm-up map_diff_impact");

    // Single-shot CI gate: assert the budget on one timed reference call.
    // Criterion's own median statistic also documents the budget in the
    // standard report, but the explicit assert here makes regressions
    // fail loudly even when the human reading the bench output misses
    // the median climbing.
    let start = Instant::now();
    let result = map_diff_impact(&setup.conn, "main", &setup.repo, &setup.request)
        .expect("timed map_diff_impact");
    let elapsed_ms = start.elapsed().as_millis();
    eprintln!(
        "map_diff_impact reference run over {FILE_COUNT} files: {elapsed_ms} ms \
         (changed_files={}, affected_symbols={}, total_hunks={})",
        result.changed_files.len(),
        result.affected_symbols.len(),
        result.total_hunks,
    );
    assert!(
        elapsed_ms < PERF_BUDGET_MS,
        "map_diff_impact reference run took {elapsed_ms} ms, exceeds {PERF_BUDGET_MS} ms budget"
    );

    c.bench_function("map_diff_impact_50_files_mixed_hunks", |b| {
        b.iter(|| {
            let result = map_diff_impact(
                black_box(&setup.conn),
                black_box("main"),
                black_box(&setup.repo),
                black_box(&setup.request),
            )
            .expect("map_diff_impact");
            black_box(result);
        });
    });
}

criterion_group!(benches, bench_diff_impact);
criterion_main!(benches);
