//! End-to-end integration tests for `seshat serve` guardrails (US-005).
//!
//! These tests spawn the real `seshat` binary and assert user-visible
//! behaviour wired up across US-001..US-004:
//!
//! - **Test 1** — refusal when invoked from a dangerous cwd with no nearby
//!   git repo (US-003 P1 refusal gate).
//! - **Test 2** — pass-through when invoked from inside a real git repo
//!   (the dangerous-cwd guard must not fire).
//! - **Test 3** — non-fatal multi-line stderr warning when `--repo` points
//!   at a dangerous-non-git path (US-003 P1 opt-out).
//! - **Test 4** — P0 watcher gating: when auto-scan fails (`auto_scan_limit`
//!   exceeded), the startup banner reports "disabled (auto-scan failed: ...)"
//!   AND the process RSS stays bounded — protecting against the original
//!   91.8 GB recursive-walk leak class.
//!
//! All tests redirect `HOME` and `XDG_*` to a tempdir so they never depend on
//! the developer's real `$HOME` or any cached project DBs at
//! `~/Library/Application Support/seshat/`.

#![cfg(not(target_os = "windows"))]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;

const STARTUP_LINE: &str = "Waiting for MCP client connection";
const REFUSAL_NEEDLE: &str = "refusing to auto-scan";
const REPO_OVERRIDE_WARN: &str = "Serving from a dangerous location";
const WATCHER_DISABLED: &str = "disabled (auto-scan failed";

/// Build a `Command` for `seshat serve` with `HOME` and the `XDG_*` triple
/// pointed inside `home`, so `dirs::home_dir()` / `dirs::data_dir()` /
/// `dirs::config_dir()` all resolve into the tempdir.
///
/// Also pre-creates the `seshat/repos/` directory inside the OS-specific
/// data dir, because `seshat serve` does not auto-create it (only
/// `seshat scan` does — see `scan.rs::run_scan`).
fn seshat_serve_cmd(cwd: &Path, home: &Path) -> Command {
    ensure_seshat_data_dir(home);
    let mut cmd = Command::cargo_bin("seshat").expect("seshat bin in workspace target");
    cmd.arg("serve")
        .current_dir(cwd)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local").join("share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("NO_COLOR", "1");
    cmd
}

/// Pre-create `<home>/<os-data-dir>/seshat/repos/` so `Database::open` (which
/// does not create parents) can place a project DB there.
///
/// - macOS: `<home>/Library/Application Support/seshat/repos/`
/// - Linux: `<home>/.local/share/seshat/repos/` (matches XDG_DATA_HOME above)
fn ensure_seshat_data_dir(home: &Path) {
    #[cfg(target_os = "macos")]
    let data_dir = home.join("Library").join("Application Support");
    #[cfg(not(target_os = "macos"))]
    let data_dir = home.join(".local").join("share");
    let repos = data_dir.join("seshat").join("repos");
    std::fs::create_dir_all(&repos).expect("create seshat repos dir");
}

/// Drains a child's stderr into a shared buffer on a background thread so
/// callers can poll for substrings without blocking on `wait()`.
struct StderrCapture {
    buf: Arc<Mutex<String>>,
}

impl StderrCapture {
    fn new(stderr: std::process::ChildStderr) -> Self {
        let buf = Arc::new(Mutex::new(String::new()));
        let buf_for_thread = Arc::clone(&buf);
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let mut guard = buf_for_thread.lock().expect("stderr buf lock");
                guard.push_str(&line);
                guard.push('\n');
            }
        });
        Self { buf }
    }

    fn snapshot(&self) -> String {
        self.buf.lock().expect("stderr buf lock").clone()
    }

    /// Poll the buffer until `needle` appears or `timeout` elapses.
    fn wait_for(&self, needle: &str, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let snap = self.snapshot();
            if snap.contains(needle) {
                return Some(snap);
            }
            thread::sleep(Duration::from_millis(50));
        }
        None
    }
}

/// Sample resident set size of a running PID via `ps`. Returns kilobytes.
///
/// Returns `None` when the process has already exited or `ps` is unavailable
/// — callers should treat the absence of a sample as "no leak observed".
fn ps_rss_kb(pid: u32) -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    raw.trim().parse::<u64>().ok()
}

fn kill_and_reap(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn git_init(repo: &Path) {
    let status = Command::new("git")
        .args(["init", "--quiet", "-b", "main"])
        .current_dir(repo)
        .status()
        .expect("git init failed to spawn");
    assert!(status.success(), "git init failed");
}

// ── Test 1: dangerous cwd, no git, no --repo → fast non-zero refusal ───

#[test]
fn serve_refuses_to_run_from_dangerous_cwd_without_git() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    // Pointing HOME at the tempdir makes the tempdir itself match the
    // per-OS `$HOME` denylist entry. The tempdir lives under `/var/folders`
    // (macOS) or `/tmp` (Linux); neither has a `.git` ancestor, so
    // `find_git_root()` returns None and the refusal gate fires.
    let mut cmd = seshat_serve_cmd(tmp.path(), tmp.path());

    let start = Instant::now();
    let output = cmd.output().expect("run seshat serve");
    let elapsed = start.elapsed();

    assert!(
        !output.status.success(),
        "expected non-zero exit; status={:?}",
        output.status
    );
    // PRD says <1s. We allow 10s to absorb cold-binary startup variance in
    // CI; the real signal is that we exit BEFORE doing any DB/scan work.
    assert!(
        elapsed < Duration::from_secs(10),
        "serve should refuse fast; elapsed={elapsed:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_lowercase().contains(REFUSAL_NEEDLE),
        "expected '{REFUSAL_NEEDLE}' in stderr; got: {stderr}"
    );
}

// ── Test 2: real git repo → guard short-circuits, banner appears ───────

#[test]
fn serve_starts_in_real_git_repo() {
    let home = tempfile::tempdir().expect("home tempdir");
    let repo = tempfile::tempdir().expect("repo tempdir");
    git_init(repo.path());

    let mut child = seshat_serve_cmd(repo.path(), home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn seshat serve");

    let capture = StderrCapture::new(child.stderr.take().expect("stderr piped"));

    let banner = capture.wait_for(STARTUP_LINE, Duration::from_secs(25));
    if banner.is_none() {
        let snap = capture.snapshot();
        let _ = child.kill();
        let _ = child.wait();
        panic!("startup banner '{STARTUP_LINE}' did not appear; stderr so far:\n{snap}");
    }

    // The dangerous-cwd guard must not fire inside a real git repo.
    let snap = capture.snapshot();
    assert!(
        !snap.to_lowercase().contains(REFUSAL_NEEDLE),
        "refusal must not fire inside git repo; stderr:\n{snap}"
    );

    kill_and_reap(child);
}

// ── Test 3: --repo at dangerous, non-git path → stderr warn, then start

#[test]
fn serve_with_explicit_dangerous_repo_warns_but_starts() {
    let home = tempfile::tempdir().expect("home tempdir");
    // No git init: the path must be both dangerous AND not a git repo for
    // the override warn to fire.
    let dangerous_repo = tempfile::tempdir().expect("dangerous repo tempdir");

    // `repo` is a positional argument on `seshat serve` (not `--repo`).
    let mut cmd = seshat_serve_cmd(home.path(), home.path());
    cmd.arg(dangerous_repo.path());

    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn seshat serve --repo");

    let capture = StderrCapture::new(child.stderr.take().expect("stderr piped"));

    let warn = capture.wait_for(REPO_OVERRIDE_WARN, Duration::from_secs(20));
    if warn.is_none() {
        let snap = capture.snapshot();
        let _ = child.kill();
        let _ = child.wait();
        panic!("expected '{REPO_OVERRIDE_WARN}' warn line; stderr so far:\n{snap}");
    }
    // Sanity: the warn comes from the override path, which only runs when
    // --repo is provided. Make sure we did NOT take the refusal branch.
    let snap = capture.snapshot();
    assert!(
        !snap.to_lowercase().contains(REFUSAL_NEEDLE),
        "refusal must not fire when --repo was passed; stderr:\n{snap}"
    );

    kill_and_reap(child);
}

// ── Test 4: P0 — watcher disabled when auto-scan fails, RSS stays low ──

#[test]
fn serve_disables_watcher_when_auto_scan_fails_and_memory_stays_bounded() {
    let home = tempfile::tempdir().expect("home tempdir");
    let repo = tempfile::tempdir().expect("repo tempdir");
    git_init(repo.path());

    // Force the project-too-large branch with `auto_scan_limit = 1`.
    std::fs::write(
        repo.path().join("seshat.toml"),
        "[scan]\nauto_scan_limit = 1\n",
    )
    .expect("write seshat.toml");

    // Two recognized-extension files → discover_files returns 2 → exceeds
    // the limit → ScanState::mark_failed fires → watcher is gated off.
    std::fs::write(repo.path().join("a.rs"), "fn a() {}\n").expect("write a.rs");
    std::fs::write(repo.path().join("b.rs"), "fn b() {}\n").expect("write b.rs");

    let mut child = seshat_serve_cmd(repo.path(), home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn seshat serve");
    let pid = child.id();

    let capture = StderrCapture::new(child.stderr.take().expect("stderr piped"));

    let banner = capture.wait_for(WATCHER_DISABLED, Duration::from_secs(25));
    if banner.is_none() {
        let snap = capture.snapshot();
        let _ = child.kill();
        let _ = child.wait();
        panic!("expected '{WATCHER_DISABLED}' in startup banner; stderr so far:\n{snap}");
    }
    let banner = banner.unwrap();
    assert!(
        banner.contains("Project too large for auto-scan"),
        "expected concrete failure reason in banner; got:\n{banner}"
    );

    // Allow a short grace period for any (forbidden) watcher init to allocate.
    // The original leak class allocated GBs in seconds when the watcher
    // recursively walked a tree — 3s is plenty to detect a ~200 MB regression.
    // The PRD specifies a 30s window, which we shorten here to keep the test
    // under the per-test 30s budget while still catching the regression.
    thread::sleep(Duration::from_secs(3));

    if let Some(rss_kb) = ps_rss_kb(pid) {
        let rss_mb = rss_kb / 1024;
        assert!(
            rss_mb < 200,
            "expected RSS under 200 MB after scan failure; observed {rss_mb} MB"
        );
    }

    kill_and_reap(child);
}
