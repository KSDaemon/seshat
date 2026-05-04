use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rmcp::schemars;
use rusqlite::Connection;
use seshat_graph::{AffectedSymbol, DiffImpactData, FileStatus};

use crate::envelope::{
    ErrorCode, ErrorEnvelope, ResponseEnvelope, ResponseMetadata, map_graph_error,
    serialize_response,
};

#[derive(Debug, serde::Serialize, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct MapDiffImpactRequest {
    #[schemars(description = "If true, diff index vs HEAD (only staged changes)")]
    pub staged_only: Option<bool>,
    #[schemars(
        description = "Optional base commitish to diff against instead of HEAD. Mutually exclusive with staged_only"
    )]
    pub base: Option<String>,
    #[schemars(description = "Optional path to the git repository root on disk. \
                        Defaults to the project root the server was started in. \
                        Only needed when analysing a different repository (e.g. a submodule).")]
    pub repo_path: Option<String>,
    pub repo: Option<String>,
    pub scope: Option<String>,
    pub file_path: Option<String>,
}

pub fn handle(
    conn: &Arc<Mutex<Connection>>,
    repo_name: &str,
    branch: &str,
    req: MapDiffImpactRequest,
    // Fallback git root — used when `req.repo_path` is not supplied.
    server_project_root: &Path,
) -> String {
    let tool = "map_diff_impact";

    let staged_only = req.staged_only.unwrap_or(false);

    if staged_only && req.base.is_some() {
        let err = ErrorEnvelope::new(
            tool,
            repo_name,
            ErrorCode::InvalidInput,
            "staged_only and base are mutually exclusive",
            "Use either staged_only=true or base=<commitish>, not both",
        );
        return serde_json::to_string(&err).unwrap_or_default();
    }

    // Resolve repo_path: use explicit value if provided, otherwise fall back to the
    // server's own project root (known at startup from the scanned directory).
    let repo_path = match &req.repo_path {
        Some(p) if !p.trim().is_empty() => {
            let path = PathBuf::from(p.trim());
            if !path.join(".git").exists() && !path.join(".git").is_file() {
                let err = ErrorEnvelope::new(
                    tool,
                    repo_name,
                    ErrorCode::InvalidInput,
                    format!("Not a git repository: {}", path.display()),
                    "Provide the path to the root of a git repository",
                );
                return serde_json::to_string(&err).unwrap_or_default();
            }
            path
        }
        _ => server_project_root.to_path_buf(),
    };

    let graph_request = seshat_graph::DiffImpactRequest {
        staged_only,
        base: req.base.clone(),
        repo_path: repo_path.to_string_lossy().to_string(),
    };

    let result = seshat_graph::map_diff_impact(conn, branch, &repo_path, &graph_request);

    match result {
        Ok(data) => {
            let next_steps = generate_next_steps(&data);
            let meta = ResponseMetadata::new(next_steps)
                .with_extra("changed_files_count", data.changed_files.len() as i64)
                .with_extra("affected_symbols_count", data.affected_symbols.len() as i64)
                .with_extra("convention_risks_count", data.convention_risks.len() as i64)
                .with_extra("risk", data.blast_radius_summary.risk.to_string());
            let envelope = ResponseEnvelope::success(tool, repo_name, data, meta);
            serialize_response(tool, repo_name, &envelope)
        }
        Err(e) => map_graph_error(tool, repo_name, e),
    }
}

/// Generate actionable next steps from the diff impact result.
///
/// Follows the same pattern as other MCP tool handlers: advice lives here,
/// not in the graph layer. Symbols are deduplicated by name (same symbol may
/// appear as both `export` and `type`) and `kind` is omitted — the full
/// detail is already available in `data.affected_symbols`.
fn generate_next_steps(data: &DiffImpactData) -> Vec<String> {
    let mut steps = Vec::new();

    if data.changed_files.is_empty() {
        steps.push("nothing to review".to_owned());
        return steps;
    }

    // Deduplicate symbols by name, keeping the one with the highest dependent_count.
    let mut by_name: HashMap<&str, &AffectedSymbol> = HashMap::new();
    for sym in &data.affected_symbols {
        by_name
            .entry(&sym.name)
            .and_modify(|e| {
                if sym.dependent_count > e.dependent_count {
                    *e = sym;
                }
            })
            .or_insert(sym);
    }

    let mut high_impact: Vec<&AffectedSymbol> = by_name
        .values()
        .copied()
        .filter(|s| s.dependent_count >= 3)
        .collect();
    high_impact.sort_by(|a, b| b.dependent_count.cmp(&a.dependent_count));

    if !high_impact.is_empty() {
        steps
            .push("review affected_symbols with dependent_count >= 3 before committing".to_owned());

        for sym in high_impact.iter().take(5) {
            let dep_files: Vec<&str> = sym.dependents.iter().map(|d| d.file.as_str()).collect();
            let dep_list = if dep_files.is_empty() {
                "unknown locations".to_owned()
            } else {
                dep_files.join(", ")
            };
            steps.push(format!(
                "{} touched with {} dependents in {} — check for breaking changes",
                sym.name, sym.dependent_count, dep_list
            ));
        }
    }

    for risk in data.convention_risks.iter().filter(|r| r.is_golden_file) {
        steps.push(format!(
            "{} is a golden file for '{}' — if intentionally changing the pattern, call record_decision to capture the new expectation",
            risk.affected_file, risk.topic
        ));
    }

    for deleted in data
        .changed_files
        .iter()
        .filter(|c| c.status == FileStatus::Deleted)
    {
        steps.push(format!(
            "deleted file {} — verify no remaining imports",
            deleted.path
        ));
    }

    // Use total_dependents from summary (accurate, group-by-file count).
    if data.blast_radius_summary.total_dependents > 0 {
        steps.push(format!(
            "run test suite: the {} dependents may break",
            data.blast_radius_summary.total_dependents
        ));
    }

    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::test_conn;
    use std::process::Command;

    fn init_git_repo(dir: &std::path::Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir)
            .output()
            .expect("git config email");
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(dir)
            .output()
            .expect("git config name");
    }

    fn git_commit_all(dir: &std::path::Path, msg: &str) {
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", msg])
            .current_dir(dir)
            .output()
            .expect("git commit");
    }

    // Helper: call handle with an explicit repo_path override.
    fn call(
        conn: &Arc<Mutex<rusqlite::Connection>>,
        req: MapDiffImpactRequest,
        fallback: &Path,
    ) -> serde_json::Value {
        let result = handle(conn, "test-project", "main", req, fallback);
        serde_json::from_str(&result).unwrap()
    }

    #[test]
    fn staged_only_and_base_together_returns_error() {
        let conn = test_conn();
        let parsed = call(
            &conn,
            MapDiffImpactRequest {
                staged_only: Some(true),
                base: Some("main".to_owned()),
                repo_path: None,
                repo: None,
                scope: None,
                file_path: None,
            },
            Path::new("/tmp"),
        );
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["tool"], "map_diff_impact");
        assert_eq!(parsed["repo"], "test-project");
        assert!(
            parsed["error"]["message"]
                .as_str()
                .unwrap()
                .contains("mutually exclusive")
        );
    }

    #[test]
    fn explicit_repo_path_not_a_git_repo_returns_error() {
        let conn = test_conn();
        let dir = tempfile::tempdir().expect("tempdir");
        let fake_path = dir.path().join("not-a-repo");
        std::fs::create_dir_all(&fake_path).expect("create dir");

        let parsed = call(
            &conn,
            MapDiffImpactRequest {
                staged_only: Some(false),
                base: None,
                repo_path: Some(fake_path.to_string_lossy().to_string()),
                repo: None,
                scope: None,
                file_path: None,
            },
            Path::new("/tmp"),
        );
        assert_eq!(parsed["status"], "error");
        assert!(
            parsed["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Not a git repository")
        );
    }

    #[test]
    fn whitespace_repo_path_falls_back_to_server_root() {
        // A whitespace-only repo_path is treated as absent and the server
        // project_root (a real git repo) is used instead.
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);
        std::fs::write(repo.join("hello.txt"), "hello").expect("write");
        git_commit_all(&repo, "initial");

        let conn = test_conn();
        let parsed = call(
            &conn,
            MapDiffImpactRequest {
                staged_only: Some(false),
                base: None,
                repo_path: Some("   ".to_owned()),
                repo: None,
                scope: None,
                file_path: None,
            },
            &repo,
        );
        // Falls back to the valid repo root — should succeed.
        assert_eq!(parsed["status"], "success");
    }

    #[test]
    fn no_repo_path_falls_back_to_server_root() {
        // When repo_path is absent the server project_root is used.
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);
        std::fs::write(repo.join("hello.txt"), "hello").expect("write");
        git_commit_all(&repo, "initial");

        let conn = test_conn();
        let parsed = call(
            &conn,
            MapDiffImpactRequest {
                staged_only: Some(false),
                base: None,
                repo_path: None,
                repo: None,
                scope: None,
                file_path: None,
            },
            &repo,
        );
        assert_eq!(parsed["status"], "success");
        assert!(parsed["data"]["changed_files"].is_array());
    }

    #[test]
    fn explicit_repo_path_overrides_server_root() {
        // Explicit repo_path takes priority over the server project_root.
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);
        std::fs::write(repo.join("hello.txt"), "hello").expect("write");
        git_commit_all(&repo, "initial");

        let conn = test_conn();
        // Pass a bogus fallback — it must NOT be used because repo_path is explicit.
        let parsed = call(
            &conn,
            MapDiffImpactRequest {
                staged_only: Some(false),
                base: None,
                repo_path: Some(repo.to_string_lossy().to_string()),
                repo: None,
                scope: None,
                file_path: None,
            },
            Path::new("/tmp/bogus-fallback"),
        );
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "map_diff_impact");
        assert_eq!(parsed["repo"], "test-project");
        assert!(parsed["data"]["changed_files"].is_array());
        assert!(parsed["data"]["affected_symbols"].is_array());
        assert!(parsed["data"]["convention_risks"].is_array());
    }

    #[test]
    fn detached_head_not_an_error_in_mcp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);
        std::fs::write(repo.join("hello.txt"), "hello").expect("write");
        git_commit_all(&repo, "initial");

        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repo)
            .output()
            .expect("rev-parse");
        let commit_hash = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        Command::new("git")
            .args(["checkout", &commit_hash])
            .current_dir(&repo)
            .output()
            .expect("git checkout commit hash");

        let conn = test_conn();
        let result = handle(
            &conn,
            "test-project",
            &commit_hash,
            MapDiffImpactRequest {
                staged_only: Some(false),
                base: None,
                repo_path: None,
                repo: None,
                scope: None,
                file_path: None,
            },
            &repo,
        );
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            parsed["status"], "success",
            "Detached HEAD should not be an error, got: {parsed:?}"
        );
    }
}
