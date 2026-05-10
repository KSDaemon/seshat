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

/// Render a symbol's `changed_lines` as a compact phrase to splice into a
/// next-step sentence.
///
/// Returns `" at lines 42-58"` for `[(42, 58)]`, `" at lines 42-58, 70-72"`
/// for two ranges, and the empty string for an empty input (so the calling
/// `format!` can interpolate it without producing an awkward dangling
/// preposition). Single-line ranges collapse to a bare line number
/// (`" at line 42"` for `[(42, 42)]`) — readers and downstream agents
/// see the natural form instead of the noisy `"42-42"`.
fn format_changed_lines(changed_lines: &[(usize, usize)]) -> String {
    if changed_lines.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = changed_lines
        .iter()
        .map(|(start, end)| {
            if start == end {
                start.to_string()
            } else {
                format!("{start}-{end}")
            }
        })
        .collect();
    // Use the singular "line" when the only range is a single line; the
    // plural "lines" otherwise (multiple ranges, or any multi-line range).
    let label = if changed_lines.len() == 1 && changed_lines[0].0 == changed_lines[0].1 {
        "line"
    } else {
        "lines"
    };
    format!(" at {label} {}", parts.join(", "))
}

/// Generate actionable next steps from the diff impact result.
///
/// Follows the same pattern as other MCP tool handlers: advice lives here,
/// not in the graph layer. Symbols are deduplicated by name (same symbol may
/// appear as both `export` and `type`) and `kind` is omitted — the full
/// detail is already available in `data.affected_symbols`. Per-symbol advice
/// surfaces the content-level granularity introduced in US-009: which line
/// ranges were touched and how many of the dependents are direct (import the
/// symbol by name) versus transitive (reachable via 2nd/3rd-order import
/// chains).
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
            let lines_phrase = format_changed_lines(&sym.changed_lines);
            // When the transitive count equals the direct count there are
            // no 2nd/3rd-order dependents to call out — skip the
            // "(N direct)" parenthetical so the message reads cleanly
            // ("with 7 dependents" instead of "with 7 transitive (7 direct)
            // dependents").
            let dep_phrase = if sym.dependent_count == sym.direct_dependent_count {
                format!("{} dependents", sym.dependent_count)
            } else {
                format!(
                    "{} transitive ({} direct) dependents",
                    sym.dependent_count, sym.direct_dependent_count
                )
            };
            steps.push(format!(
                "{} touched{} with {} in {} — check for breaking changes",
                sym.name, lines_phrase, dep_phrase, dep_list
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

    // ── generate_next_steps ─────────────────────────────────────────

    use seshat_graph::{
        AdoptionSummary, BlastRadius, BlastRadiusSummary, ChangedFile, ConventionRisk,
        DependentRef, DiffImpactData, ImpactMetadata,
    };

    fn empty_data() -> DiffImpactData {
        DiffImpactData {
            changed_files: Vec::new(),
            affected_symbols: Vec::new(),
            convention_risks: Vec::new(),
            blast_radius_summary: BlastRadiusSummary {
                total_dependents: 0,
                total_affected_symbols: 0,
                total_changed_files: 0,
                risk: BlastRadius::None,
            },
            total_hunks: 0,
            metadata: ImpactMetadata {
                branch: "main".to_owned(),
            },
        }
    }

    fn modified(path: &str) -> ChangedFile {
        ChangedFile {
            path: path.to_owned(),
            status: FileStatus::Modified,
        }
    }

    fn affected(name: &str, file: &str, dependent_count: usize) -> AffectedSymbol {
        affected_split(name, file, dependent_count, dependent_count, vec![(1, 1)])
    }

    /// Build an `AffectedSymbol` with explicit direct/transitive split and
    /// `changed_lines` so per-symbol next-step formatting can be exercised.
    fn affected_split(
        name: &str,
        file: &str,
        dependent_count: usize,
        direct_dependent_count: usize,
        changed_lines: Vec<(usize, usize)>,
    ) -> AffectedSymbol {
        AffectedSymbol {
            name: name.to_owned(),
            file: file.to_owned(),
            kind: "function".to_owned(),
            dependent_count,
            direct_dependent_count,
            dependents: (0..direct_dependent_count.min(5))
                .map(|i| DependentRef {
                    file: format!("dep_{i}.rs"),
                    line: 1,
                })
                .collect(),
            changed_lines,
            blast_radius: if dependent_count >= 10 {
                BlastRadius::High
            } else if dependent_count >= 3 {
                BlastRadius::Medium
            } else {
                BlastRadius::Low
            },
        }
    }

    fn risk(topic: &str, file: &str, golden: bool) -> ConventionRisk {
        ConventionRisk {
            topic: topic.to_owned(),
            description: "desc".to_owned(),
            affected_file: file.to_owned(),
            confidence_pct: 95.0,
            weight: "strong".to_owned(),
            adoption: AdoptionSummary {
                count: 9,
                total: 10,
                rate_pct: 90.0,
            },
            is_golden_file: golden,
            note: "n".to_owned(),
        }
    }

    #[test]
    fn next_steps_empty_diff_returns_nothing_to_review() {
        let steps = generate_next_steps(&empty_data());
        assert_eq!(steps, vec!["nothing to review".to_owned()]);
    }

    #[test]
    fn next_steps_low_dependent_count_does_not_emit_high_impact_advice() {
        let mut data = empty_data();
        data.changed_files.push(modified("a.rs"));
        // Only one symbol with 2 dependents — below the threshold of 3.
        data.affected_symbols.push(affected("foo", "a.rs", 2));
        data.blast_radius_summary.total_changed_files = 1;
        data.blast_radius_summary.total_affected_symbols = 1;
        data.blast_radius_summary.total_dependents = 0;

        let steps = generate_next_steps(&data);
        assert!(!steps.iter().any(|s| s.contains("affected_symbols")));
        // No total_dependents either — must not suggest running tests.
        assert!(!steps.iter().any(|s| s.contains("test suite")));
    }

    #[test]
    fn next_steps_high_impact_emits_review_and_per_symbol_advice() {
        let mut data = empty_data();
        data.changed_files.push(modified("a.rs"));
        data.affected_symbols.push(affected("foo", "a.rs", 5));
        data.affected_symbols.push(affected("bar", "a.rs", 3));
        data.affected_symbols.push(affected("baz", "a.rs", 2)); // below threshold
        data.blast_radius_summary.total_dependents = 8;

        let steps = generate_next_steps(&data);
        assert!(steps.iter().any(|s| s.contains("dependent_count >= 3")));
        // Per-symbol advice for the two ≥3 symbols (sorted descending).
        let foo_idx = steps
            .iter()
            .position(|s| s.contains("foo touched"))
            .unwrap();
        let bar_idx = steps
            .iter()
            .position(|s| s.contains("bar touched"))
            .unwrap();
        assert!(
            foo_idx < bar_idx,
            "foo (5 deps) must come before bar (3 deps)"
        );
        // baz with 2 deps must not appear.
        assert!(!steps.iter().any(|s| s.contains("baz")));
    }

    #[test]
    fn next_steps_dedupes_symbols_by_name_keeping_max_dependent_count() {
        let mut data = empty_data();
        data.changed_files.push(modified("a.rs"));
        // Same symbol name appears twice (e.g. as both `export` and `type`).
        // Higher count must be kept.
        data.affected_symbols.push(affected("foo", "a.rs", 3));
        data.affected_symbols.push(affected("foo", "a.rs", 7));

        let steps = generate_next_steps(&data);
        let foo_lines: Vec<_> = steps.iter().filter(|s| s.contains("foo touched")).collect();
        assert_eq!(foo_lines.len(), 1, "duplicate symbol must collapse");
        // When transitive == direct (no 2nd-order dependents), the
        // "(N direct)" parenthetical is suppressed so the line reads
        // cleanly. Locks that contraction.
        assert!(
            foo_lines[0].contains("with 7 dependents"),
            "expected collapsed phrasing when transitive == direct, got: {}",
            foo_lines[0]
        );
        assert!(
            !foo_lines[0].contains("transitive"),
            "expected the 'transitive' qualifier to be dropped when counts match, got: {}",
            foo_lines[0]
        );
    }

    #[test]
    fn next_steps_per_symbol_advice_surfaces_changed_lines_and_split_counts() {
        // Locks the AC example shape:
        //   "foo touched at lines 42-58 with 12 transitive (4 direct) dependents in ..."
        let mut data = empty_data();
        data.changed_files.push(modified("a.rs"));
        data.affected_symbols
            .push(affected_split("foo", "a.rs", 12, 4, vec![(42, 58)]));

        let steps = generate_next_steps(&data);
        let foo_step = steps
            .iter()
            .find(|s| s.contains("foo touched"))
            .expect("foo per-symbol advice should be emitted");
        assert!(
            foo_step.contains("at lines 42-58"),
            "expected ' at lines 42-58' in: {foo_step}"
        );
        assert!(
            foo_step.contains("12 transitive (4 direct) dependents"),
            "expected '12 transitive (4 direct) dependents' in: {foo_step}"
        );
    }

    #[test]
    fn next_steps_per_symbol_advice_renders_multiple_changed_line_ranges() {
        let mut data = empty_data();
        data.changed_files.push(modified("a.rs"));
        data.affected_symbols.push(affected_split(
            "foo",
            "a.rs",
            6,
            6,
            vec![(10, 12), (40, 40)],
        ));

        let steps = generate_next_steps(&data);
        let foo_step = steps
            .iter()
            .find(|s| s.contains("foo touched"))
            .expect("foo per-symbol advice should be emitted");
        // Mixed ranges: the multi-line range is rendered as "10-12";
        // the single-line range collapses to a bare "40" instead of
        // the redundant "40-40". The outer label stays plural ("lines")
        // because there is more than one range.
        assert!(
            foo_step.contains("at lines 10-12, 40"),
            "expected mixed range list with single-line collapse in: {foo_step}"
        );
        assert!(
            !foo_step.contains("40-40"),
            "expected single-line range to collapse to bare line number: {foo_step}"
        );
    }

    #[test]
    fn next_steps_per_symbol_advice_uses_singular_label_for_one_single_line_range() {
        let mut data = empty_data();
        data.changed_files.push(modified("a.rs"));
        data.affected_symbols
            .push(affected_split("foo", "a.rs", 4, 4, vec![(42, 42)]));

        let steps = generate_next_steps(&data);
        let foo_step = steps
            .iter()
            .find(|s| s.contains("foo touched"))
            .expect("foo per-symbol advice should be emitted");
        assert!(
            foo_step.contains(" at line 42 "),
            "expected ' at line 42 ' singular form in: {foo_step}"
        );
        assert!(
            !foo_step.contains("42-42"),
            "single-line range must not render as '42-42' in: {foo_step}"
        );
    }

    #[test]
    fn next_steps_per_symbol_advice_omits_lines_clause_when_changed_lines_empty() {
        // Defence-in-depth: graph filters out symbols whose body wasn't touched,
        // but if a future change leaks one through, the formatter must not emit
        // a dangling 'at lines' phrase.
        let mut data = empty_data();
        data.changed_files.push(modified("a.rs"));
        data.affected_symbols
            .push(affected_split("foo", "a.rs", 5, 5, vec![]));

        let steps = generate_next_steps(&data);
        let foo_step = steps
            .iter()
            .find(|s| s.contains("foo touched"))
            .expect("foo per-symbol advice should be emitted");
        assert!(
            !foo_step.contains(" at line"),
            "expected no ' at line(s)' clause when changed_lines is empty: {foo_step}"
        );
        assert!(
            foo_step.contains("with 5 dependents"),
            "expected collapsed phrasing when transitive == direct, got: {foo_step}",
        );
    }

    #[test]
    fn next_steps_takes_at_most_5_symbols() {
        let mut data = empty_data();
        data.changed_files.push(modified("a.rs"));
        for i in 0..8 {
            data.affected_symbols
                .push(affected(&format!("sym_{i}"), "a.rs", 10 - i));
        }

        let steps = generate_next_steps(&data);
        let symbol_lines = steps.iter().filter(|s| s.contains("touched")).count();
        assert_eq!(symbol_lines, 5, "must cap at 5 per-symbol lines");
    }

    #[test]
    fn next_steps_golden_file_change_emits_record_decision_advice() {
        let mut data = empty_data();
        data.changed_files.push(modified("src/lib.rs"));
        data.convention_risks
            .push(risk("error_handling", "src/lib.rs", true));
        data.convention_risks
            .push(risk("naming", "src/other.rs", false));

        let steps = generate_next_steps(&data);
        let golden_step = steps
            .iter()
            .find(|s| s.contains("golden file"))
            .expect("golden file advice should be emitted");
        assert!(golden_step.contains("error_handling"));
        assert!(golden_step.contains("record_decision"));
        // Non-golden risk must NOT appear.
        assert!(!steps.iter().any(|s| s.contains("naming")));
    }

    #[test]
    fn next_steps_deleted_files_emit_remaining_imports_warning() {
        let mut data = empty_data();
        data.changed_files.push(modified("kept.rs"));
        data.changed_files.push(ChangedFile {
            path: "old.rs".into(),
            status: FileStatus::Deleted,
        });

        let steps = generate_next_steps(&data);
        assert!(steps.iter().any(|s| s.contains("deleted file old.rs")));
        // Modified file must NOT trigger the deletion warning.
        assert!(!steps.iter().any(|s| s.contains("deleted file kept.rs")));
    }

    #[test]
    fn next_steps_total_dependents_advises_test_run() {
        let mut data = empty_data();
        data.changed_files.push(modified("a.rs"));
        data.blast_radius_summary.total_dependents = 12;

        let steps = generate_next_steps(&data);
        let test_step = steps
            .iter()
            .find(|s| s.contains("test suite"))
            .expect("test-suite advice should be emitted");
        assert!(test_step.contains("12 dependents"));
    }

    #[test]
    fn next_steps_zero_dependents_omits_test_advice() {
        let mut data = empty_data();
        data.changed_files.push(modified("a.rs"));
        data.blast_radius_summary.total_dependents = 0;

        let steps = generate_next_steps(&data);
        assert!(!steps.iter().any(|s| s.contains("test suite")));
    }

    #[test]
    fn next_steps_high_impact_with_no_dependents_uses_unknown_locations() {
        let mut data = empty_data();
        data.changed_files.push(modified("a.rs"));
        // High dependent_count but empty `dependents` list — output should
        // fall back to "unknown locations" rather than panic.
        let mut sym = affected("foo", "a.rs", 3);
        sym.dependents = Vec::new();
        data.affected_symbols.push(sym);

        let steps = generate_next_steps(&data);
        assert!(steps.iter().any(|s| s.contains("unknown locations")));
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
