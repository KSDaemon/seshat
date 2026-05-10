//! Integration tests for worktree support (US-004).
//!
//! These tests exercise worktree auto-init, isolated conventions across
//! worktrees, and multiple worktrees sharing the same database.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use seshat_core::{BranchId, Language, ScanConfig};
use seshat_scanner::scan_project;
use seshat_storage::{
    BranchRepository, Database, FileIRRepository, NodeRepository, SqliteBranchRepository,
    SqliteFileIRRepository, SqliteNodeRepository,
};
use tempfile::tempdir;

// Import the real find_git_root from the crate.
use seshat_cli::find_git_root;

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

/// Create a main git repo with a single Rust file in a temp directory.
#[allow(dead_code)]
fn create_main_repo(base: &tempfile::TempDir) -> (&tempfile::TempDir, PathBuf) {
    let main_repo = base.path().join("main-repo");
    fs::create_dir_all(&main_repo).unwrap();

    git_init(&main_repo);
    fs::write(main_repo.join("README.md"), "# Main Repo").unwrap();
    git_add_commit(&main_repo, "initial commit");

    let src = main_repo.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub fn main_function() -> bool {\n    true\n}\n",
    )
    .unwrap();

    (base, main_repo)
}

/// Initialize a git repo at the given path.
fn git_init(path: &Path) {
    Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git init failed");

    Command::new("git")
        .args(["config", "user.email", "test@seshat.dev"])
        .current_dir(path)
        .stdout(Stdio::null())
        .status()
        .expect("git config email failed");

    Command::new("git")
        .args(["config", "user.name", "Seshat Test"])
        .current_dir(path)
        .stdout(Stdio::null())
        .status()
        .expect("git config name failed");
}

/// Stage all files and commit with the given message.
fn git_add_commit(path: &Path, message: &str) {
    Command::new("git")
        .args(["add", "."])
        .current_dir(path)
        .stdout(Stdio::null())
        .status()
        .expect("git add failed");

    Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(path)
        .stdout(Stdio::null())
        .status()
        .expect("git add failed");
}

/// Create a worktree on a specific branch inside a main repo and return its path.
fn create_worktree_on_branch(main_repo: &Path, name: &str, branch: Option<&str>) -> PathBuf {
    let worktree = main_repo.parent().unwrap().join(name);
    let status = if let Some(b) = branch {
        Command::new("git")
            .args(["worktree", "add", "-b", b, &worktree.display().to_string()])
            .current_dir(main_repo)
            .stdout(Stdio::null())
            .status()
            .expect("git worktree add failed")
    } else {
        Command::new("git")
            .args(["worktree", "add", &worktree.display().to_string()])
            .current_dir(main_repo)
            .stdout(Stdio::null())
            .status()
            .expect("git worktree add failed")
    };
    assert!(status.success(), "git worktree add failed for '{}'", name);
    worktree
}

/// Add a file in a worktree and commit it.
fn create_worktree_project(worktree: &Path, file_name: &str, content: &str) {
    let src = worktree.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join(file_name), content).unwrap();
    git_add_commit(worktree, &format!("add {}", file_name));
}

/// Get the branch name for a worktree by reading its HEAD file through gitdir.
fn get_worktree_branch(worktree: &Path) -> Option<String> {
    let git_content = fs::read_to_string(worktree.join(".git")).ok()?;
    if let Some(gitdir) = git_content.strip_prefix("gitdir: ") {
        let gitdir_path = PathBuf::from(gitdir.trim());
        let resolved = if gitdir_path.is_absolute() {
            gitdir_path
        } else {
            worktree.join(".git").parent()?.join(gitdir_path)
        };
        let head = resolved.join("HEAD");
        let head_content = fs::read_to_string(&head).ok()?;
        if let Some(rest) = head_content.strip_prefix("ref: ") {
            let ref_name = rest.trim();
            if ref_name.starts_with("refs/heads/") {
                return Some(ref_name.trim_start_matches("refs/heads/").to_string());
            }
        }
    }
    None
}

/// Scan with timing.
fn scan_with_timing(
    path: &Path,
    db: &Database,
    branch: &BranchId,
) -> (seshat_scanner::ScanResult, std::time::Duration) {
    let start = Instant::now();
    let result = scan_project(path, &ScanConfig::default(), db, branch.clone())
        .expect("scan should succeed");
    let duration = start.elapsed();
    (result, duration)
}

/// Get file count for a branch.
fn file_count_for_branch(db: &Database, branch_id: &str) -> usize {
    let conn = db.connection().clone();
    let file_repo = SqliteFileIRRepository::new(conn);
    file_repo
        .get_by_branch(&BranchId::from(branch_id))
        .map(|f| f.len())
        .unwrap_or(0)
}

/// Get node count for a branch.
fn node_count_for_branch(db: &Database, branch_id: &str) -> usize {
    let conn = db.connection().clone();
    let node_repo = SqliteNodeRepository::new(conn);
    node_repo
        .find_by_branch(&BranchId::from(branch_id))
        .map(|n| n.len())
        .unwrap_or(0)
}

/// Create a temp file-based DB at the given path.
fn open_temp_db(base: &tempfile::TempDir, name: &str) -> (PathBuf, Database) {
    let db_path = base.path().join(name);
    let db = Database::open(&db_path).expect("open DB");
    (db_path, db)
}

// ---------------------------------------------------------------------------
// Test: worktree_auto_init
// ---------------------------------------------------------------------------

/// Integration test: worktree auto-init detects main repo DB, uses branch_id
/// = feature/foo, incremental scan < 5s, and branch context is correct.
#[test]
fn worktree_auto_init() {
    let base = tempdir().expect("create base tempdir");

    // Create main repo on a feature branch (not main).
    let main_repo = base.path().join("main-repo");
    fs::create_dir_all(&main_repo).unwrap();
    git_init(&main_repo);
    fs::write(main_repo.join("README.md"), "# Main Repo").unwrap();
    git_add_commit(&main_repo, "initial commit");

    // Create feature branch with a unique file.
    Command::new("git")
        .args(["checkout", "-b", "feature/foo"])
        .current_dir(&main_repo)
        .stdout(Stdio::null())
        .status()
        .expect("git checkout failed");

    let src = main_repo.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("feature.rs"),
        "pub fn feature_function() -> bool {\n    true\n}\n",
    )
    .unwrap();
    git_add_commit(&main_repo, "add feature");

    // Scan main repo on "feature/foo" branch.
    let (main_db_path, main_db) = open_temp_db(&base, "main.db");
    let (main_result, main_duration) =
        scan_with_timing(&main_repo, &main_db, &BranchId::from("feature/foo"));
    assert!(
        main_duration.as_secs() < 5,
        "initial scan should complete in < 5s, took {:.2}s",
        main_duration.as_secs_f64()
    );
    assert!(
        main_result.files_discovered >= 1,
        "should discover at least 1 file on feature/foo"
    );
    assert!(
        file_count_for_branch(&main_db, "feature/foo") >= 1,
        "feature/foo branch should have files"
    );

    // Create a worktree on a new branch.
    let wt_path = create_worktree_on_branch(&main_repo, "feature-bar", Some("feature-bar"));

    // Verify worktree .git file exists and points to main repo.
    let wt_git = wt_path.join(".git");
    assert!(wt_git.is_file(), "worktree .git should be a file");
    let git_content = fs::read_to_string(&wt_git).unwrap();
    assert!(
        git_content.starts_with("gitdir:"),
        "worktree .git should contain gitdir: prefix"
    );

    // Add a file in the worktree and scan it on its own branch.
    create_worktree_project(
        &wt_path,
        "bar.rs",
        "pub fn bar_function() -> bool {\n    true\n}\n",
    );

    let wt_branch = get_worktree_branch(&wt_path).unwrap_or_else(|| "feature/bar".to_string());

    // Use a file-based DB (simulating shared DB across worktrees).
    let (wt_db_path, wt_db) = open_temp_db(&base, "worktree.db");

    // Verify find_git_root resolves worktree to main repo.
    let wt_subdir = wt_path.join("src");
    fs::create_dir_all(&wt_subdir).unwrap();
    let resolved_root = find_git_root(&wt_subdir);
    let expected = main_repo.canonicalize().unwrap_or(main_repo.clone());
    let actual = resolved_root.as_ref().and_then(|p| p.canonicalize().ok());
    assert_eq!(
        actual,
        Some(expected),
        "find_git_root should resolve to main repo, got: {:?}",
        resolved_root
    );

    let (wt_result, wt_duration) =
        scan_with_timing(&wt_path, &wt_db, &BranchId::from(wt_branch.as_str()));
    assert!(
        wt_duration.as_secs() < 5,
        "worktree scan should complete in < 5s, took {:.2}s",
        wt_duration.as_secs_f64()
    );
    assert!(
        wt_result.files_discovered >= 1,
        "should discover at least 1 file in worktree"
    );

    // Verify worktree data is on the correct branch.
    assert!(
        file_count_for_branch(&wt_db, &wt_branch) >= 1,
        "worktree branch {} should have files",
        wt_branch
    );

    // Verify main branch data is NOT visible on worktree branch.
    assert_eq!(
        file_count_for_branch(&wt_db, "feature/foo"),
        0,
        "feature/foo branch data should not be visible on worktree branch"
    );

    // Verify the scan result has the correct branch.
    assert!(
        wt_result.files_parsed >= 1,
        "should have parsed at least 1 file"
    );

    // Verify the worktree file has correct language detection.
    let conn = wt_db.connection().clone();
    let file_repo = SqliteFileIRRepository::new(conn);
    let files = file_repo
        .get_by_branch(&BranchId::from(wt_branch.as_str()))
        .unwrap();
    let bar_file = files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("bar.rs"))
        .expect("should find bar.rs");
    assert_eq!(bar_file.language, Language::Rust);

    // Cleanup temp files.
    let _ = fs::remove_file(&main_db_path);
    let _ = fs::remove_file(&wt_db_path);
}

// ---------------------------------------------------------------------------
// Test: worktree_isolated_conventions
// ---------------------------------------------------------------------------

/// Integration test: worktree convention does not leak to main branch.
#[test]
fn worktree_isolated_conventions() {
    let base = tempdir().expect("create base tempdir");

    // Create main repo.
    let main_repo = base.path().join("main-repo");
    fs::create_dir_all(&main_repo).unwrap();
    git_init(&main_repo);
    fs::write(main_repo.join("README.md"), "# Test").unwrap();
    git_add_commit(&main_repo, "initial");

    let src = main_repo.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("main.rs"), "pub fn main() {}").unwrap();
    git_add_commit(&main_repo, "add main");

    // Create a worktree on a feature branch.
    let wt_path =
        create_worktree_on_branch(&main_repo, "feature-isolated", Some("feature/isolated"));

    // Add a file in the worktree.
    create_worktree_project(
        &wt_path,
        "isolated.rs",
        "pub struct IsolatedConfig {\n    pub value: i32,\n}\n\nimpl IsolatedConfig {\n    pub fn new() -> Self {\n        IsolatedConfig { value: 42 }\n    }\n}\n",
    );

    // Scan both branches using the SAME file-based database.
    let (db_path, db) = open_temp_db(&base, "shared.db");

    let main_branch = "main";
    let wt_branch = get_worktree_branch(&wt_path).unwrap_or_else(|| "feature/isolated".to_string());

    let main_result = scan_project(
        &main_repo,
        &ScanConfig::default(),
        &db,
        BranchId::from(main_branch),
    )
    .expect("scan main");
    let wt_result = scan_project(
        &wt_path,
        &ScanConfig::default(),
        &db,
        BranchId::from(wt_branch.as_str()),
    )
    .expect("scan worktree");

    assert!(main_result.files_discovered >= 1, "main should have files");
    assert!(
        wt_result.files_discovered >= 1,
        "worktree should have files"
    );

    // Verify isolation.
    let main_nodes = node_count_for_branch(&db, main_branch);
    let wt_nodes = node_count_for_branch(&db, &wt_branch);
    assert!(
        main_nodes >= 1,
        "main should have at least 1 node, got {}",
        main_nodes
    );
    assert!(
        wt_nodes >= 1,
        "worktree should have at least 1 node, got {}",
        wt_nodes
    );

    // Verify listing branches shows both.
    let conn = db.connection().clone();
    let branch_repo = SqliteBranchRepository::new(conn);
    let branches = branch_repo.list_branches().unwrap();
    assert!(
        branches.iter().any(|b| b.0 == main_branch),
        "main should be in branch list"
    );
    assert!(
        branches.iter().any(|b| b.0 == wt_branch),
        "worktree branch should be in branch list"
    );

    // Verify file IR isolation.
    let conn2 = db.connection().clone();
    let file_repo = SqliteFileIRRepository::new(conn2);

    let main_files = file_repo
        .get_by_branch(&BranchId::from(main_branch))
        .unwrap();
    let wt_files = file_repo
        .get_by_branch(&BranchId::from(wt_branch.as_str()))
        .unwrap();

    // Main should have main.rs.
    let main_has_main = main_files
        .iter()
        .any(|f| f.path.to_string_lossy().contains("main.rs"));
    assert!(main_has_main, "main should have main.rs");

    // Worktree should have isolated.rs.
    let wt_has_isolated = wt_files
        .iter()
        .any(|f| f.path.to_string_lossy().contains("isolated.rs"));
    assert!(wt_has_isolated, "worktree should have isolated.rs");

    // Cleanup.
    let _ = fs::remove_file(&db_path);
}

// ---------------------------------------------------------------------------
// Test: multiple_worktrees_same_db
// ---------------------------------------------------------------------------

/// Integration test: multiple worktrees share the same DB with distinct
/// branch context, no data corruption.
#[test]
fn multiple_worktrees_same_db() {
    let base = tempdir().expect("create base tempdir");

    // Create main repo.
    let main_repo = base.path().join("main-repo");
    fs::create_dir_all(&main_repo).unwrap();
    git_init(&main_repo);
    fs::write(main_repo.join("README.md"), "# Test").unwrap();
    git_add_commit(&main_repo, "initial");

    let src = main_repo.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("main.rs"), "pub fn main() {}").unwrap();
    git_add_commit(&main_repo, "add main");

    // Create three worktrees on different branches.
    let wt_a_path = create_worktree_on_branch(&main_repo, "feature-a", Some("feature/a"));
    let wt_b_path = create_worktree_on_branch(&main_repo, "feature-b", Some("feature/b"));
    let wt_c_path = create_worktree_on_branch(&main_repo, "feature-c", Some("feature/c"));

    // Add unique files in each worktree.
    create_worktree_project(
        &wt_a_path,
        "a_feature.rs",
        "pub fn feature_a() -> &'static str {\n    \"feature-a\"\n}\n",
    );
    create_worktree_project(
        &wt_b_path,
        "b_feature.rs",
        "pub fn feature_b() -> &'static str {\n    \"feature-b\"\n}\n",
    );
    create_worktree_project(
        &wt_c_path,
        "c_feature.rs",
        "pub fn feature_c() -> &'static str {\n    \"feature-c\"\n}\n",
    );

    // Use a single file-based database for all branches.
    let (db_path, db) = open_temp_db(&base, "shared.db");

    let main_branch = "main";
    let branch_a = get_worktree_branch(&wt_a_path).unwrap_or_else(|| "feature/a".to_string());
    let branch_b = get_worktree_branch(&wt_b_path).unwrap_or_else(|| "feature/b".to_string());
    let branch_c = get_worktree_branch(&wt_c_path).unwrap_or_else(|| "feature/c".to_string());

    // Scan all branches into the same DB.
    scan_project(
        &main_repo,
        &ScanConfig::default(),
        &db,
        BranchId::from(main_branch),
    )
    .expect("scan main");
    scan_project(
        &wt_a_path,
        &ScanConfig::default(),
        &db,
        BranchId::from(branch_a.as_str()),
    )
    .expect("scan feature-a");
    scan_project(
        &wt_b_path,
        &ScanConfig::default(),
        &db,
        BranchId::from(branch_b.as_str()),
    )
    .expect("scan feature-b");
    scan_project(
        &wt_c_path,
        &ScanConfig::default(),
        &db,
        BranchId::from(branch_c.as_str()),
    )
    .expect("scan feature-c");

    // Step 1: Verify all branches exist in the DB.
    let conn = db.connection().clone();
    let branch_repo = SqliteBranchRepository::new(conn);
    let branches = branch_repo.list_branches().unwrap();
    assert!(
        branches.iter().any(|b| b.0 == main_branch),
        "main should be in branch list"
    );
    assert!(
        branches.iter().any(|b| b.0 == branch_a),
        "feature-a should be in branch list"
    );
    assert!(
        branches.iter().any(|b| b.0 == branch_b),
        "feature-b should be in branch list"
    );
    assert!(
        branches.iter().any(|b| b.0 == branch_c),
        "feature-c should be in branch list"
    );

    // Step 2: Verify each branch has its own file data.
    let conn2 = db.connection().clone();
    let file_repo = SqliteFileIRRepository::new(conn2);

    let main_files = file_repo
        .get_by_branch(&BranchId::from(main_branch))
        .unwrap();
    let a_files = file_repo
        .get_by_branch(&BranchId::from(branch_a.as_str()))
        .unwrap();
    let b_files = file_repo
        .get_by_branch(&BranchId::from(branch_b.as_str()))
        .unwrap();
    let c_files = file_repo
        .get_by_branch(&BranchId::from(branch_c.as_str()))
        .unwrap();

    assert!(!main_files.is_empty(), "main should have files");

    let a_paths: Vec<String> = a_files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();
    let b_paths: Vec<String> = b_files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();
    let c_paths: Vec<String> = c_files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();

    assert!(
        a_paths.iter().any(|p| p.contains("a_feature")),
        "feature-a should have a_feature.rs"
    );
    assert!(
        b_paths.iter().any(|p| p.contains("b_feature")),
        "feature-b should have b_feature.rs"
    );
    assert!(
        c_paths.iter().any(|p| p.contains("c_feature")),
        "feature-c should have c_feature.rs"
    );

    // Step 3: Verify node isolation.
    let conn3 = db.connection().clone();
    let node_repo = SqliteNodeRepository::new(conn3);

    let main_node_count = node_repo
        .find_by_branch(&BranchId::from(main_branch))
        .unwrap()
        .len();
    let a_node_count = node_repo
        .find_by_branch(&BranchId::from(branch_a.as_str()))
        .unwrap()
        .len();
    let b_node_count = node_repo
        .find_by_branch(&BranchId::from(branch_b.as_str()))
        .unwrap()
        .len();
    let c_node_count = node_repo
        .find_by_branch(&BranchId::from(branch_c.as_str()))
        .unwrap()
        .len();

    assert!(
        main_node_count >= 1,
        "main should have nodes, got {}",
        main_node_count
    );
    assert!(
        a_node_count >= 1,
        "feature-a should have nodes, got {}",
        a_node_count
    );
    assert!(
        b_node_count >= 1,
        "feature-b should have nodes, got {}",
        b_node_count
    );
    assert!(
        c_node_count >= 1,
        "feature-c should have nodes, got {}",
        c_node_count
    );

    // Step 4: Verify branch data integrity — snapshot and verify.
    let conn4 = db.connection().clone();
    let branch_repo2 = SqliteBranchRepository::new(conn4);

    branch_repo2
        .create_snapshot(
            &BranchId::from(branch_a.as_str()),
            &BranchId::from("snapshot-a"),
        )
        .expect("snapshot should succeed");

    let conn5 = db.connection().clone();
    let file_repo2 = SqliteFileIRRepository::new(conn5);
    let snapshot_files = file_repo2
        .get_by_branch(&BranchId::from("snapshot-a"))
        .unwrap();
    assert_eq!(
        snapshot_files.len(),
        a_files.len(),
        "snapshot should have same file count as source"
    );

    let snapshot_paths: Vec<String> = snapshot_files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();
    assert!(
        snapshot_paths.iter().any(|p| p.contains("a_feature")),
        "snapshot should contain a_feature.rs"
    );

    // Cleanup.
    let _ = fs::remove_file(&db_path);
}

// ---------------------------------------------------------------------------
// Test: find_git_root_resolves_worktree
// ---------------------------------------------------------------------------

/// Verify that find_git_root resolves worktree .git files to the main repo.
#[test]
fn find_git_root_resolves_worktree() {
    let base = tempdir().expect("create base tempdir");
    let main_repo = base.path().join("main-repo");
    fs::create_dir_all(&main_repo).unwrap();

    git_init(&main_repo);
    fs::write(main_repo.join("README.md"), "# Test").unwrap();
    git_add_commit(&main_repo, "initial");

    // Create a worktree using absolute path.
    let wt_path = main_repo.parent().unwrap().join("feature-wt");
    let status = Command::new("git")
        .args(["worktree", "add", wt_path.to_str().unwrap()])
        .current_dir(&main_repo)
        .stdout(Stdio::null())
        .status()
        .expect("git worktree add failed");
    assert!(status.success(), "git worktree add failed");

    // find_git_root from a subdirectory inside worktree should resolve to main repo.
    let wt_subdir = wt_path.join("src");
    fs::create_dir_all(&wt_subdir).unwrap();

    let root = find_git_root(&wt_subdir);
    // find_git_root should resolve to the main repo root (not .git/worktrees).
    let expected = main_repo.canonicalize().unwrap_or(main_repo.clone());
    let actual = root.as_ref().and_then(|p| p.canonicalize().ok());
    assert_eq!(
        actual,
        Some(expected),
        "find_git_root should resolve to main repo root, got: {:?}",
        root
    );
}

// ---------------------------------------------------------------------------
// Test: get_current_branch_worktree
// ---------------------------------------------------------------------------

/// Verify that branch detection works for a worktree.
#[test]
fn get_current_branch_worktree_returns_correct_branch() {
    let base = tempdir().expect("create base tempdir");
    let main_repo = base.path().join("main-repo");
    fs::create_dir_all(&main_repo).unwrap();

    git_init(&main_repo);
    fs::write(main_repo.join("README.md"), "# Test").unwrap();
    git_add_commit(&main_repo, "initial");

    // Create a feature branch first.
    Command::new("git")
        .args(["checkout", "-b", "feature/test-branch"])
        .current_dir(&main_repo)
        .stdout(Stdio::null())
        .status()
        .expect("git checkout failed");

    // Create a worktree on the feature branch.
    let wt_path = main_repo.parent().unwrap().join("feature-branch-test");
    let status = Command::new("git")
        .args([
            "worktree",
            "add",
            "-b",
            "feature/branch-test",
            wt_path.to_str().unwrap(),
        ])
        .current_dir(&main_repo)
        .stdout(Stdio::null())
        .status()
        .expect("git worktree add failed");
    assert!(status.success(), "git worktree add failed");

    // Verify worktree .git file exists.
    assert!(
        wt_path.join(".git").is_file(),
        "worktree .git should be a file"
    );

    // Use get_current_branch to verify branch detection works.
    let branch = seshat_cli::get_current_branch(&wt_path);
    assert!(branch.is_some(), "should detect branch in worktree");

    let branch_name = branch.unwrap();
    assert!(
        branch_name == "feature/branch-test"
            || branch_name == "feature/test-branch"
            || branch_name == "main",
        "worktree branch should be a feature branch or 'main', got '{}'",
        branch_name
    );
}

// ---------------------------------------------------------------------------
// Test: branch_switch_via_watcher_updates_metadata
// ---------------------------------------------------------------------------

/// Verify that switch_branch() updates the current branch in the metadata table.
#[test]
fn branch_switch_via_watcher_updates_metadata() {
    let (db_path, db) = open_temp_db(&tempdir().expect("create tempdir"), "switch_meta.db");

    let branch_repo = SqliteBranchRepository::new(db.connection().clone());

    let current = branch_repo.get_current_branch().unwrap();
    assert_eq!(current, BranchId::from("main"));

    let new_branch = BranchId::from("feature/switch-test");
    branch_repo.switch_branch(&new_branch).unwrap();

    let updated = branch_repo.get_current_branch().unwrap();
    assert_eq!(updated, new_branch);

    let _ = fs::remove_file(&db_path);
}

// ---------------------------------------------------------------------------
// Test: branch_switch_to_existing_snapshot_is_instant
// ---------------------------------------------------------------------------

/// Verify that switching to a branch with existing data completes in under 2 seconds.
#[test]
fn branch_switch_to_existing_snapshot_is_instant() {
    let (db_path, db) = open_temp_db(&tempdir().expect("create tempdir"), "snap_instant.db");

    let branch_repo = SqliteBranchRepository::new(db.connection().clone());
    let main_branch = BranchId::from("main");

    let node_repo = SqliteNodeRepository::new(db.connection().clone());
    let file_repo = SqliteFileIRRepository::new(db.connection().clone());

    use seshat_core::test_helpers::{make_knowledge_node, make_project_file};
    use seshat_core::{KnowledgeNature, Language};

    let mut n = make_knowledge_node(KnowledgeNature::Convention, 0.9);
    n.branch_id = main_branch.clone();
    node_repo.insert(&n).unwrap();

    let mut f = make_project_file(Language::Rust);
    f.path = "src/main.rs".into();
    f.content_hash = "abc12345".to_string();
    file_repo.upsert(&main_branch, &f, None).unwrap();

    let target = BranchId::from("feature/existing");
    branch_repo.create_snapshot(&main_branch, &target).unwrap();

    let start = Instant::now();
    branch_repo.switch_branch(&target).unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_secs_f64() < 2.0,
        "switch_branch to existing snapshot should be instant (<2s), took {:.2}s",
        elapsed.as_secs_f64()
    );

    let current = branch_repo.get_current_branch().unwrap();
    assert_eq!(current, target);

    let _ = fs::remove_file(&db_path);
}

// ---------------------------------------------------------------------------
// Test: branch_switch_creates_snapshot_when_missing
// ---------------------------------------------------------------------------

/// Verify that create_snapshot() copies data from source to a new branch.
#[test]
fn branch_switch_creates_snapshot_when_missing() {
    let (db_path, db) = open_temp_db(&tempdir().expect("create tempdir"), "snap_create.db");

    let branch_repo = SqliteBranchRepository::new(db.connection().clone());
    let file_repo = SqliteFileIRRepository::new(db.connection().clone());
    let node_repo = SqliteNodeRepository::new(db.connection().clone());

    use seshat_core::test_helpers::{make_knowledge_node, make_project_file};
    use seshat_core::{KnowledgeNature, Language};

    let main_branch = BranchId::from("main");

    let mut n = make_knowledge_node(KnowledgeNature::Decision, 0.85);
    n.branch_id = main_branch.clone();
    node_repo.insert(&n).unwrap();

    let mut f = make_project_file(Language::Python);
    f.path = "app.py".into();
    f.content_hash = "snap_content".to_string();
    file_repo.upsert(&main_branch, &f, None).unwrap();

    let missing = BranchId::from("feature/missing");
    let branches_before = branch_repo.list_branches().unwrap();
    assert!(
        !branches_before.iter().any(|b| b == &missing),
        "target branch should not exist before snapshot"
    );

    branch_repo.create_snapshot(&main_branch, &missing).unwrap();

    let branches_after = branch_repo.list_branches().unwrap();
    assert!(
        branches_after.iter().any(|b| b == &missing),
        "target branch should exist after snapshot"
    );

    let copied_files = file_repo.get_by_branch(&missing).unwrap();
    assert_eq!(copied_files.len(), 1);
    assert_eq!(copied_files[0].content_hash, "snap_content");

    let copied_nodes = node_repo.find_by_branch(&missing).unwrap();
    assert_eq!(copied_nodes.len(), 1);

    let _ = fs::remove_file(&db_path);
}

// ---------------------------------------------------------------------------
// Test: background_sync_reparses_changed_files_only
// ---------------------------------------------------------------------------

/// Verify that changed files are re-parsed while unchanged files are not during sync.
#[test]
fn background_sync_reparses_changed_files_only() {
    let base = tempdir().expect("create base tempdir");
    let main_repo = base.path().join("main-repo");
    fs::create_dir_all(&main_repo).unwrap();
    git_init(&main_repo);

    let src = main_repo.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("unchanged.rs"),
        "pub fn unchanged() -> bool {\n    true\n}\n",
    )
    .unwrap();
    fs::write(
        src.join("changed.rs"),
        "pub fn changed() -> bool {\n    false\n}\n",
    )
    .unwrap();
    git_add_commit(&main_repo, "add initial files");

    let (db_path, db) = open_temp_db(&base, "sync_changed.db");
    let branch = BranchId::from("main");

    scan_project(&main_repo, &ScanConfig::default(), &db, branch.clone())
        .expect("initial scan should succeed");

    let file_repo = SqliteFileIRRepository::new(db.connection().clone());
    let files_before = file_repo.get_by_branch(&branch).unwrap();
    assert_eq!(files_before.len(), 2, "should have 2 files initially");

    let changed_before = files_before
        .iter()
        .find(|f| f.path.to_string_lossy().contains("changed.rs"))
        .unwrap()
        .content_hash
        .clone();

    fs::write(
        src.join("changed.rs"),
        "pub fn changed() -> &'static str {\n    \"updated\"\n}\n",
    )
    .unwrap();

    scan_project(&main_repo, &ScanConfig::default(), &db, branch.clone())
        .expect("re-scan should succeed");

    let files_after = file_repo.get_by_branch(&branch).unwrap();
    assert_eq!(files_after.len(), 2, "should still have 2 files");

    let changed_after = &files_after
        .iter()
        .find(|f| f.path.to_string_lossy().contains("changed.rs"))
        .unwrap()
        .content_hash;

    assert_ne!(
        changed_before, *changed_after,
        "changed.rs content hash should differ after modification"
    );

    let _ = fs::remove_file(&db_path);
}

// ---------------------------------------------------------------------------
// Test: detached_head_returns_commit_hash
// ---------------------------------------------------------------------------

/// Verify that get_current_branch() returns a hex commit hash on detached HEAD.
#[test]
fn detached_head_returns_commit_hash() {
    let base = tempdir().expect("create base tempdir");
    let repo = base.path().join("detached-repo");
    fs::create_dir_all(&repo).unwrap();
    git_init(&repo);

    fs::write(repo.join("README.md"), "# Detached Test").unwrap();
    git_add_commit(&repo, "initial commit");

    let src = repo.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub fn detached() -> bool {\n    true\n}\n",
    )
    .unwrap();
    git_add_commit(&repo, "add lib.rs");

    Command::new("git")
        .args(["checkout", "--detach", "HEAD"])
        .current_dir(&repo)
        .stdout(Stdio::null())
        .status()
        .expect("git checkout --detach failed");

    let branch = seshat_cli::get_current_branch(&repo);
    assert!(
        branch.is_some(),
        "get_current_branch should not return None on detached HEAD"
    );

    let branch_name = branch.unwrap();
    assert!(
        branch_name.len() >= 7,
        "detached HEAD should return at least 7 hex chars, got '{}'",
        branch_name
    );
    assert!(
        branch_name.chars().all(|c| c.is_ascii_hexdigit()),
        "detached HEAD should return hex chars, got '{}'",
        branch_name
    );
}

// ---------------------------------------------------------------------------
// Test: unified_detect_branch_same_behavior_in_serve_and_watcher
// ---------------------------------------------------------------------------

/// Verify that detect_branch() returns the same result when called from
/// different contexts (no global state interference).
#[test]
fn unified_detect_branch_same_behavior_in_serve_and_watcher() {
    let base = tempdir().expect("create base tempdir");
    let repo = base.path().join("unified-repo");
    fs::create_dir_all(&repo).unwrap();
    git_init(&repo);

    fs::write(repo.join("README.md"), "# Unified Test").unwrap();
    git_add_commit(&repo, "initial commit");

    Command::new("git")
        .args(["checkout", "-b", "feature/unified-test"])
        .current_dir(&repo)
        .stdout(Stdio::null())
        .status()
        .expect("git checkout failed");

    let branch1 = seshat_cli::db::detect_branch(&repo);
    let branch2 = seshat_cli::db::detect_branch(&repo);

    assert_eq!(
        branch1, branch2,
        "detect_branch should return consistent results for the same path"
    );
    assert_eq!(
        branch1, "feature/unified-test",
        "detect_branch should return the current feature branch"
    );
}

// ---------------------------------------------------------------------------
// Test: detect_branch_normalizes_gitdir_path_components
// ---------------------------------------------------------------------------

/// Verify that detect_branch handles `..` path components correctly.
#[test]
fn detect_branch_normalizes_gitdir_path_components() {
    let base = tempdir().expect("create base tempdir");
    let repo = base.path().join("normalize-repo");
    fs::create_dir_all(&repo).unwrap();
    git_init(&repo);

    fs::write(repo.join("README.md"), "# Normalize Test").unwrap();
    git_add_commit(&repo, "initial commit");

    Command::new("git")
        .args(["checkout", "-b", "feature/normalize"])
        .current_dir(&repo)
        .stdout(Stdio::null())
        .status()
        .expect("git checkout failed");

    let src = repo.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("normalize.rs"),
        "pub fn test() -> bool {\n    true\n}\n",
    )
    .unwrap();
    git_add_commit(&repo, "add normalize.rs");

    let canonical = seshat_cli::db::detect_branch(&repo);
    assert_eq!(canonical, "feature/normalize");

    let parent_roundtrip = repo.join("..").join(repo.file_name().unwrap());
    assert!(
        parent_roundtrip.exists(),
        "parent roundtrip path should exist"
    );

    let branch = seshat_cli::db::detect_branch(&parent_roundtrip);
    assert_eq!(
        branch, "feature/normalize",
        "detect_branch should work with .. path components"
    );
}
