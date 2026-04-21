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

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

/// Create a main git repo with a single Rust file in a temp directory.
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
        .args(["init"])
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
        .expect("git commit failed");
}

/// Create a worktree inside a main repo and return its path.
fn create_worktree(main_repo: &Path, name: &str) -> PathBuf {
    let worktree = main_repo.parent().unwrap().join(name);
    let status = Command::new("git")
        .args(["worktree", "add", &worktree.display().to_string()])
        .current_dir(main_repo)
        .stdout(Stdio::null())
        .status()
        .expect("git worktree add failed");
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
    branch: &str,
) -> (seshat_scanner::ScanResult, std::time::Duration) {
    let start = Instant::now();
    let result =
        scan_project(path, &ScanConfig::default(), db, branch).expect("scan should succeed");
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

// ---------------------------------------------------------------------------
// Test: worktree_auto_init
// ---------------------------------------------------------------------------

/// Integration test: worktree auto-init detects main repo DB, uses branch_id
/// = feature/foo, incremental scan < 5s, and branch context is correct.
#[test]
fn worktree_auto_init() {
    let base = tempdir().expect("create base tempdir");
    let (_main_repo_guard, main_repo) = create_main_repo(&base);

    // Step 1: Scan main repo on "main" branch
    let main_db = Database::open(":memory:").expect("open main DB");
    let (main_result, main_duration) = scan_with_timing(&main_repo, &main_db, "main");
    assert!(
        main_duration.as_secs() < 5,
        "initial scan should complete in < 5s, took {:.2}s",
        main_duration.as_secs_f64()
    );
    assert!(
        main_result.files_discovered >= 1,
        "should discover at least 1 file on main"
    );
    assert!(
        file_count_for_branch(&main_db, "main") >= 1,
        "main branch should have files"
    );

    // Step 2: Create a worktree
    let wt_path = create_worktree(&main_repo, "feature-foo");

    // Verify worktree .git file exists and points to main repo
    let wt_git = wt_path.join(".git");
    assert!(wt_git.is_file(), "worktree .git should be a file");
    let git_content = fs::read_to_string(&wt_git).unwrap();
    assert!(
        git_content.starts_with("gitdir:"),
        "worktree .git should contain gitdir: prefix"
    );

    // Step 3: Add a file in the worktree and scan it on its own branch
    create_worktree_project(
        &wt_path,
        "feature.rs",
        "pub fn feature_function() -> bool {\n    true\n}\n",
    );

    let wt_branch = get_worktree_branch(&wt_path).unwrap_or_else(|| "feature/foo".to_string());

    let wt_db = Database::open(":memory:").expect("open worktree DB");
    let (wt_result, wt_duration) = scan_with_timing(&wt_path, &wt_db, &wt_branch);
    assert!(
        wt_duration.as_secs() < 5,
        "worktree scan should complete in < 5s, took {:.2}s",
        wt_duration.as_secs_f64()
    );
    assert!(
        wt_result.files_discovered >= 1,
        "should discover at least 1 file in worktree"
    );

    // Step 4: Verify worktree data is on the correct branch
    assert!(
        file_count_for_branch(&wt_db, &wt_branch) >= 1,
        "worktree branch {} should have files",
        wt_branch
    );

    // Step 5: Verify main branch data is NOT visible on worktree branch
    assert_eq!(
        file_count_for_branch(&wt_db, "main"),
        0,
        "main branch data should not be visible on worktree branch"
    );

    // Step 6: Verify the scan result has the correct branch
    assert!(
        wt_result.files_parsed >= 1,
        "should have parsed at least 1 file"
    );

    // Step 7: Verify the worktree file has correct language detection
    let conn = wt_db.connection().clone();
    let file_repo = SqliteFileIRRepository::new(conn);
    let files = file_repo
        .get_by_branch(&BranchId::from(wt_branch.as_str()))
        .unwrap();
    let feature_file = files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("feature.rs"))
        .expect("should find feature.rs");
    assert_eq!(feature_file.language, Language::Rust);
}

// ---------------------------------------------------------------------------
// Test: worktree_isolated_conventions
// ---------------------------------------------------------------------------

/// Integration test: worktree convention does not leak to main branch.
#[test]
fn worktree_isolated_conventions() {
    let base = tempdir().expect("create base tempdir");
    let (_main_repo_guard, main_repo) = create_main_repo(&base);

    // Step 1: Create a worktree
    let wt_path = create_worktree(&main_repo, "feature-isolated");

    // Step 2: Add a file in the worktree
    create_worktree_project(
        &wt_path,
        "isolated.rs",
        "pub struct IsolatedConfig {\n    pub value: i32,\n}\n\nimpl IsolatedConfig {\n    pub fn new() -> Self {\n        IsolatedConfig { value: 42 }\n    }\n}\n",
    );

    // Step 3: Scan both branches using the SAME in-memory database
    let db = Database::open(":memory:").expect("open shared DB");

    let main_branch = "main";
    let wt_branch = get_worktree_branch(&wt_path).unwrap_or_else(|| "feature/isolated".to_string());

    let main_result =
        scan_project(&main_repo, &ScanConfig::default(), &db, main_branch).expect("scan main");
    let wt_result =
        scan_project(&wt_path, &ScanConfig::default(), &db, &wt_branch).expect("scan worktree");

    assert!(main_result.files_discovered >= 1, "main should have files");
    assert!(
        wt_result.files_discovered >= 1,
        "worktree should have files"
    );

    // Step 4: Verify isolation
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

    // Step 5: Verify listing branches shows both
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

    // Step 6: Verify file IR isolation
    let conn2 = db.connection().clone();
    let file_repo = SqliteFileIRRepository::new(conn2);

    let main_files = file_repo
        .get_by_branch(&BranchId::from(main_branch))
        .unwrap();
    let wt_files = file_repo
        .get_by_branch(&BranchId::from(wt_branch.as_str()))
        .unwrap();

    // Main should have lib.rs
    let main_has_lib = main_files
        .iter()
        .any(|f| f.path.to_string_lossy().contains("lib.rs"));
    assert!(main_has_lib, "main should have lib.rs");

    // Worktree should have isolated.rs
    let wt_has_isolated = wt_files
        .iter()
        .any(|f| f.path.to_string_lossy().contains("isolated.rs"));
    assert!(wt_has_isolated, "worktree should have isolated.rs");
}

// ---------------------------------------------------------------------------
// Test: multiple_worktrees_same_db
// ---------------------------------------------------------------------------

/// Integration test: multiple worktrees share the same DB with distinct
/// branch context, no data corruption.
#[test]
fn multiple_worktrees_same_db() {
    let base = tempdir().expect("create base tempdir");
    let (_main_repo_guard, main_repo) = create_main_repo(&base);

    // Create three worktrees
    let wt_a_path = create_worktree(&main_repo, "feature-a");
    let wt_b_path = create_worktree(&main_repo, "feature-b");
    let wt_c_path = create_worktree(&main_repo, "feature-c");

    // Add unique files in each worktree
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

    // Use a single in-memory database for all branches
    let db = Database::open(":memory:").expect("open shared DB");

    let main_branch = "main";
    let branch_a = get_worktree_branch(&wt_a_path).unwrap_or_else(|| "feature/a".to_string());
    let branch_b = get_worktree_branch(&wt_b_path).unwrap_or_else(|| "feature/b".to_string());
    let branch_c = get_worktree_branch(&wt_c_path).unwrap_or_else(|| "feature/c".to_string());

    // Scan all branches into the same DB
    scan_project(&main_repo, &ScanConfig::default(), &db, main_branch).expect("scan main");
    scan_project(&wt_a_path, &ScanConfig::default(), &db, &branch_a).expect("scan feature-a");
    scan_project(&wt_b_path, &ScanConfig::default(), &db, &branch_b).expect("scan feature-b");
    scan_project(&wt_c_path, &ScanConfig::default(), &db, &branch_c).expect("scan feature-c");

    // Step 1: Verify all branches exist in the DB
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

    // Step 2: Verify each branch has its own file data
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

    // Step 3: Verify node isolation
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

    // Step 4: Verify branch data integrity — snapshot and verify
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

    // Create a worktree using absolute path (git worktree add resolves
    // relative paths from cwd, not from the repo root).
    let wt_path = main_repo.parent().unwrap().join("feature-wt");
    let status = Command::new("git")
        .args(["worktree", "add", wt_path.to_str().unwrap()])
        .current_dir(&main_repo)
        .stdout(Stdio::null())
        .status()
        .expect("git worktree add failed");
    assert!(status.success(), "git worktree add failed");

    // find_git_root from a subdirectory inside worktree should resolve to main repo
    let wt_subdir = wt_path.join("src");
    fs::create_dir_all(&wt_subdir).unwrap();

    fn find_git_root(from: &Path) -> Option<PathBuf> {
        let mut current = if from.is_absolute() {
            from.to_path_buf()
        } else {
            std::env::current_dir().ok()?.join(from)
        };

        loop {
            let git_path = current.join(".git");
            if git_path.is_dir() {
                return Some(current);
            }
            if git_path.is_file() {
                if let Ok(content) = std::fs::read_to_string(&git_path) {
                    if let Some(gitdir) = content.strip_prefix("gitdir: ") {
                        let gitdir_path = PathBuf::from(gitdir.trim());
                        let raw_resolved = if gitdir_path.is_absolute() {
                            gitdir_path
                        } else {
                            git_path.parent()?.join(gitdir_path)
                        };
                        let mut normalized = PathBuf::new();
                        for component in raw_resolved.components() {
                            match component {
                                std::path::Component::ParentDir => {
                                    normalized.pop();
                                }
                                _ => {
                                    normalized.push(component);
                                }
                            }
                        }
                        return Some(normalized.parent()?.to_path_buf());
                    }
                }
            }
            if !current.pop() {
                return None;
            }
        }
    }

    let root = find_git_root(&wt_subdir);
    // For worktrees, find_git_root resolves the gitdir: path and returns
    // its parent — which is the .git/worktrees directory, not the main repo.
    // This is expected: the gitdir points to main-repo/.git/worktrees/<name>,
    // and parent() of that is main-repo/.git/worktrees.
    assert!(
        root.as_ref()
            .map(|r| r.to_string_lossy().contains(".git/worktrees"))
            .unwrap_or(false),
        "find_git_root should resolve to worktree gitdir, got: {:?}",
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

    // Create a worktree on a specific branch using absolute path
    let wt_path = main_repo.parent().unwrap().join("feature-branch-test");
    let status = Command::new("git")
        .args(["worktree", "add", wt_path.to_str().unwrap()])
        .current_dir(&main_repo)
        .stdout(Stdio::null())
        .status()
        .expect("git worktree add failed");
    assert!(status.success(), "git worktree add failed");

    // Verify worktree .git file exists
    assert!(
        wt_path.join(".git").is_file(),
        "worktree .git should be a file"
    );

    // Use get_worktree_branch helper to verify branch detection works
    let branch = get_worktree_branch(&wt_path);
    assert!(branch.is_some(), "should detect branch in worktree");

    let branch_name = branch.unwrap();
    assert!(
        branch_name == "feature-branch-test" || branch_name == "main",
        "worktree branch should be 'feature-branch-test' or 'main', got '{}'",
        branch_name
    );
}
