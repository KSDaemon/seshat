//! Integration tests for the `seshat scan` command.
//!
//! Tests the CLI binary end-to-end: argument validation, error output,
//! exit codes, and output patterns on real (fixture) project directories.

use std::path::Path;

use assert_cmd::Command;
use chrono::Utc;
use predicates::prelude::*;

/// Helper: get a `Command` for the seshat binary, with HOME isolated to a
/// freshly-leaked tempdir so the test cannot read or write the user's real
/// `~/Library/Application Support/seshat/` (or `~/.local/share/seshat/`).
///
/// `dirs::data_dir()` is built from `$HOME` on every supported platform, so
/// overriding HOME redirects every DB / cache / version-check write produced
/// by the binary into the isolated tempdir. We also clear `XDG_DATA_HOME`
/// because Linux `dirs::data_dir()` honors it ahead of HOME.
///
/// The tempdir intentionally outlives the test (`into_path` leaks it) — the
/// OS cleans `/tmp` later, and this avoids tying the lifetime of HOME to a
/// guard that callers would need to thread through every test.
fn seshat() -> Command {
    let home = tempfile::tempdir().expect("create isolated HOME tempdir");
    // `keep` consumes the TempDir but disables auto-cleanup; the path
    // outlives the test process, so HOME stays valid for the whole binary run.
    let home_path = home.keep();
    let mut cmd = Command::cargo_bin("seshat").expect("binary exists");
    cmd.env("HOME", &home_path);
    cmd.env_remove("XDG_DATA_HOME");
    cmd
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
        .env("NO_COLOR", "1")
        .args(["scan", fixture.to_str().expect("valid path")])
        .assert()
        .success()
        .stderr(predicates::str::contains("Scanned"))
        .stderr(predicates::str::contains("Completed in"));
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
    //
    // Run from an isolated tempdir with an isolated HOME so the test does
    // not pick up a pre-existing DB under the real
    // `~/Library/Application Support/seshat/repos/` whose migration
    // history may have been advanced by a different working branch.
    let tmp_home = tempfile::tempdir().expect("create home temp dir");
    let tmp_cwd = tempfile::tempdir().expect("create cwd temp dir");

    // `serve` does not mkdir its repos directory — it expects scan to have
    // run first. Pre-create the platform-specific data dir so the auto-scan
    // path can write the fresh DB.
    #[cfg(target_os = "macos")]
    let repos_dir = tmp_home
        .path()
        .join("Library")
        .join("Application Support")
        .join("seshat")
        .join("repos");
    #[cfg(not(target_os = "macos"))]
    let repos_dir = tmp_home
        .path()
        .join(".local")
        .join("share")
        .join("seshat")
        .join("repos");
    std::fs::create_dir_all(&repos_dir).expect("create repos dir");

    seshat_with_home(tmp_home.path())
        .env("NO_COLOR", "1")
        .current_dir(tmp_cwd.path())
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

// ── Review ───────────────────────────────────────────────────────────

#[test]
fn review_in_git_repo_requires_scan_first() {
    // `seshat review` in a git repo with no DB should fail with a
    // "No database found" message — NOT a "not in a git repository" error.
    let tmp = tempfile::tempdir().expect("create temp dir");

    // Initialize a git repo (no commits needed).
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(tmp.path())
        .output()
        .expect("git init");

    seshat()
        .current_dir(tmp.path())
        .arg("review")
        .assert()
        .failure()
        .stderr(predicates::str::contains("No database found"));
}

#[test]
fn review_without_git_repo_requires_scan_first() {
    // `seshat review` in a non-git directory should NOT fail with
    // "not in a git repository" — it should fallback gracefully and
    // report "No database found" instead.
    let tmp = tempfile::tempdir().expect("create temp dir");

    seshat()
        .current_dir(tmp.path())
        .arg("review")
        .assert()
        .failure()
        .stderr(predicates::str::contains("No database found"));
}

#[test]
fn init_auto_detects_clients() {
    // `seshat init` is now implemented — it should exit successfully.
    // In CI (no AI clients in PATH), it prints a "no clients detected" message.
    // We just verify it exits 0 and doesn't crash.
    seshat().arg("init").assert().success();
}

#[test]
fn init_unknown_client_exits_error() {
    seshat()
        .arg("init")
        .arg("vscode")
        .assert()
        .failure()
        .stderr(predicates::str::contains("Unknown client"));
}

#[test]
fn init_dry_run_flag_accepted() {
    seshat().arg("init").arg("--dry-run").assert().success();
}

// ── US-005: Background update notice ─────────────────────────────────

/// Returns the path where `dirs::data_dir()` would place the version cache
/// when `HOME` is set to `fake_home`.
///
/// - macOS: `<fake_home>/Library/Application Support/seshat/version-check.json`
/// - Linux: `<fake_home>/.local/share/seshat/version-check.json`
fn version_cache_path_for_home(fake_home: &std::path::Path) -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    let data_dir = fake_home.join("Library").join("Application Support");
    #[cfg(not(target_os = "macos"))]
    let data_dir = fake_home.join(".local").join("share");
    data_dir.join("seshat").join("version-check.json")
}

/// Write a version cache with the given version to the platform-appropriate path
/// inside `fake_home`.
fn write_version_cache_for_home(fake_home: &std::path::Path, version: &str) {
    let cache_file = version_cache_path_for_home(fake_home);
    if let Some(parent) = cache_file.parent() {
        std::fs::create_dir_all(parent).expect("create cache dir");
    }
    let now = Utc::now().to_rfc3339();
    let json = format!(r#"{{"latest_version":"{version}","checked_at":"{now}"}}"#);
    std::fs::write(&cache_file, json).expect("write cache file");
}

/// Helper: run seshat with `HOME` overridden to a specific `fake_home` (used
/// when the test needs to pre-populate or inspect files inside that HOME,
/// e.g. version-cache tests). For the default case where HOME just needs to
/// be isolated, prefer `seshat()` which already does that.
fn seshat_with_home(fake_home: &std::path::Path) -> Command {
    let mut cmd = seshat();
    cmd.env("HOME", fake_home);
    cmd
}

#[test]
fn update_notice_printed_for_status_when_newer_version_cached() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    // Cache says version 9999.0.0 — definitely newer than any real build
    write_version_cache_for_home(tmp.path(), "9999.0.0");

    seshat_with_home(tmp.path())
        .arg("status")
        .assert()
        // status may succeed or fail, but we only care that the notice appears
        .stderr(predicates::str::contains("Seshat v9999.0.0 is available"));
}

#[test]
fn update_notice_printed_for_scan_when_newer_version_cached() {
    let tmp_home = tempfile::tempdir().expect("create home temp dir");
    write_version_cache_for_home(tmp_home.path(), "9999.0.0");

    let tmp_scan = tempfile::tempdir().expect("create scan temp dir");
    let _guard = ProjectDbGuard::new(tmp_scan.path());

    seshat_with_home(tmp_home.path())
        .args(["scan", tmp_scan.path().to_str().expect("valid path")])
        .assert()
        .stderr(predicates::str::contains("Seshat v9999.0.0 is available"));
}

#[test]
fn update_notice_suppressed_for_seshat_update_check() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    // Cache says version 9999.0.0 — would normally trigger background notice
    write_version_cache_for_home(tmp.path(), "9999.0.0");

    // `seshat update --check` should NOT print the background notice.
    // The background notice format is "Seshat vX.Y.Z is available ... Run seshat update to upgrade."
    // That exact phrase must not appear for the update subcommand.
    seshat_with_home(tmp.path())
        .args(["update", "--check"])
        .assert()
        .stderr(predicates::str::contains("Run seshat update to upgrade.").not());
}

#[test]
fn update_notice_suppressed_for_seshat_update() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    write_version_cache_for_home(tmp.path(), "9999.0.0");

    // `seshat update` (without --check) should also not show the background notice.
    seshat_with_home(tmp.path())
        .arg("update")
        .assert()
        .stderr(predicates::str::contains("Run seshat update to upgrade.").not());
}

#[test]
fn update_notice_not_printed_when_up_to_date() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    // Cache says version 0.0.1 — older than any real build, so no notice expected
    write_version_cache_for_home(tmp.path(), "0.0.1");

    seshat_with_home(tmp.path())
        .arg("status")
        .assert()
        .stderr(predicates::str::contains("is available").not());
}

#[test]
fn update_notice_network_failure_silent_skip() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    // No cache file → will try network. Network may succeed or fail.
    // Either way, the background notice must NOT print "Could not check for updates"
    // (that message is only for explicit `seshat update --check`).
    seshat_with_home(tmp.path())
        .arg("status")
        .assert()
        .stderr(predicates::str::contains("Could not check for updates").not());
}
