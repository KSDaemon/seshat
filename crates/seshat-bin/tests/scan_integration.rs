//! Integration tests for the `seshat scan` command.
//!
//! Tests the CLI binary end-to-end: argument validation, error output,
//! exit codes, and output patterns on real (fixture) project directories.

use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;

/// Helper: get a `Command` for the seshat binary.
fn seshat() -> Command {
    Command::cargo_bin("seshat").expect("binary exists")
}

/// RAII guard that removes the project database created by `seshat scan <dir>`
/// when it goes out of scope (including on panic / assert failure).
///
/// `seshat scan` stores its DB at `$XDG_DATA_HOME/seshat/repos/{dir_name}.db`.
/// Wrapping cleanup in `Drop` ensures orphan `.tmp*.db` files never accumulate
/// even when a test assertion fails.
struct ProjectDbGuard {
    db_file: Option<std::path::PathBuf>,
}

impl ProjectDbGuard {
    fn new(scanned_path: &Path) -> Self {
        let dir_name = scanned_path
            .file_name()
            .expect("scanned path has a file_name component")
            .to_string_lossy()
            .to_string();

        let db_file = dirs::data_dir().map(|d| {
            d.join("seshat")
                .join("repos")
                .join(format!("{dir_name}.db"))
        });
        Self { db_file }
    }
}

impl Drop for ProjectDbGuard {
    fn drop(&mut self) {
        if let Some(ref db_file) = self.db_file {
            // Remove the main DB file and SQLite WAL/SHM sidecars.
            for ext in ["", "-wal", "-shm"] {
                let path = if ext.is_empty() {
                    db_file.clone()
                } else {
                    let name = format!("{}{ext}", db_file.file_name().unwrap().to_string_lossy());
                    db_file.with_file_name(name)
                };
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

// ── Error cases ──────────────────────────────────────────────────────

#[test]
fn scan_nonexistent_path_exits_with_error() {
    seshat()
        .args(["scan", "/tmp/seshat-test-nonexistent-path-12345"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("error:"))
        .stderr(predicates::str::contains("does not exist"));
}

#[test]
fn scan_file_instead_of_directory_exits_with_error() {
    // Create a temp file (not a directory).
    let tmp = tempfile::NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_str().expect("valid path");

    seshat()
        .args(["scan", path])
        .assert()
        .failure()
        .stderr(predicates::str::contains("error:"))
        .stderr(predicates::str::contains("not a directory"));
}

// ── Success cases ────────────────────────────────────────────────────

#[test]
fn scan_empty_directory_succeeds_with_warning() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let _guard = ProjectDbGuard::new(tmp.path());

    seshat()
        .args(["scan", tmp.path().to_str().expect("valid path")])
        .assert()
        .success()
        .stderr(predicates::str::contains("Scanned 0 files"))
        .stderr(predicates::str::contains("no files discovered"));
}

#[test]
fn scan_fixture_project_succeeds() {
    // Use the Rust fixture project from the test fixtures directory.
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("parent")
        .parent()
        .expect("workspace root")
        .join("tests/fixtures/rust_project");

    if !fixture.exists() {
        // Skip if fixture not available (CI environments).
        return;
    }

    seshat()
        .args(["scan", fixture.to_str().expect("valid path")])
        .assert()
        .success()
        .stderr(predicates::str::contains("Scanned"))
        .stderr(predicates::str::contains("Completed in"));

    // Note: not cleaning up rust_project.db — it's a stable fixture name
    // and re-scanning it is idempotent.
}

#[test]
fn scan_directory_with_no_parseable_files_succeeds() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let _guard = ProjectDbGuard::new(tmp.path());

    // Create files with unrecognized extensions.
    std::fs::write(tmp.path().join("readme.txt"), "hello").expect("write file");
    std::fs::write(tmp.path().join("data.csv"), "a,b,c").expect("write file");

    seshat()
        .args(["scan", tmp.path().to_str().expect("valid path")])
        .assert()
        .success()
        .stderr(predicates::str::contains("Scanned 0 files"));
}

// ── Verbosity ────────────────────────────────────────────────────────

#[test]
fn scan_quiet_mode_minimal_output() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let _guard = ProjectDbGuard::new(tmp.path());

    seshat()
        .args(["scan", tmp.path().to_str().expect("valid path"), "--quiet"])
        .assert()
        .success()
        .stderr(predicates::str::contains("Scanned"))
        .stderr(predicates::str::contains("Completed in"))
        // Quiet mode should NOT show the version header.
        .stderr(predicates::str::contains("seshat v").not());
}

#[test]
fn scan_verbose_mode_shows_timing() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let _guard = ProjectDbGuard::new(tmp.path());

    seshat()
        .args([
            "scan",
            tmp.path().to_str().expect("valid path"),
            "--verbose",
        ])
        .assert()
        .success()
        .stderr(predicates::str::contains("Timing"))
        .stderr(predicates::str::contains("Total:"));
}

// ── Version ──────────────────────────────────────────────────────────

#[test]
fn version_flag_prints_version() {
    seshat()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicates::str::contains("seshat"));
}

// ── No subcommand ────────────────────────────────────────────────────

#[test]
fn no_subcommand_shows_help() {
    seshat().assert().failure();
}

// ── Stubbed commands ─────────────────────────────────────────────────

#[test]
fn serve_starts_and_shows_startup_info() {
    // Without a real MCP client on stdin, the server starts, displays
    // startup info, then exits with a transport error. We verify the
    // startup display is printed correctly.
    seshat()
        .arg("serve")
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "Waiting for MCP client connection",
        ));
}

#[test]
fn status_shows_output() {
    seshat().arg("status").assert().success();
}

#[test]
fn review_not_yet_implemented() {
    seshat()
        .arg("review")
        .assert()
        .failure()
        .stderr(predicates::str::contains("not yet implemented"));
}

#[test]
fn init_not_yet_implemented() {
    seshat()
        .arg("init")
        .assert()
        .failure()
        .stderr(predicates::str::contains("not yet implemented"));
}
