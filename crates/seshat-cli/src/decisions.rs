//! Implementation of the `seshat decisions` subcommands.
//!
//! Exposes `seshat decisions list` (US-013) and `seshat decisions forget`
//! (US-014). Future subcommands — `export` / `import` (US-015) — will slot in
//! here.

use std::fmt::Write as _;
use std::io::Write;

use serde::Serialize;

use seshat_storage::{
    Database, Decision, DecisionRepository, DecisionState, ExampleEvidence,
    SqliteDecisionRepository,
};

use crate::args::{DecisionStateFilter, DecisionsCommand, DecisionsListFormat};
use crate::db;
use crate::error::CliError;

/// Maximum width of the description column in table output.
///
/// Long descriptions are truncated with an ellipsis. JSON output is always
/// full-fidelity.
const TABLE_DESCRIPTION_MAX: usize = 60;

/// Number of leading hash characters shown in the table.
///
/// Eight characters distinguishes most decisions visually while keeping the
/// table inside typical terminal widths. Full hashes are preserved in JSON
/// output and in the underlying `decisions` table.
const TABLE_HASH_LEN: usize = 8;

/// Minimum prefix length accepted by `seshat decisions forget`.
///
/// Hashes are 16 hex characters (`compute_description_hash` returns SHA-256
/// truncated to the first 8 bytes). 4 hex chars = 16 bits ≈ 65k buckets,
/// which is sufficient discrimination for projects with up to a few thousand
/// decisions. Anything shorter is rejected up-front to avoid surfacing
/// "ambiguous prefix" errors that the user can't easily resolve.
const MIN_FORGET_PREFIX_LEN: usize = 4;

/// Dispatch a `seshat decisions <subcommand>` invocation.
pub fn run_decisions(command: DecisionsCommand) -> Result<(), CliError> {
    match command {
        DecisionsCommand::List {
            state,
            branch,
            format,
        } => run_list(state, branch.as_deref(), format),
        DecisionsCommand::Forget { hash, yes } => run_forget(&hash, yes),
    }
}

/// Implement `seshat decisions list`.
fn run_list(
    state_filter: Option<DecisionStateFilter>,
    branch_filter: Option<&str>,
    format: DecisionsListFormat,
) -> Result<(), CliError> {
    let resolved = db::resolve_project(None, "decisions")?;

    if !resolved.db_path.exists() {
        return Err(CliError::CommandFailed {
            command: "decisions".to_owned(),
            reason: "No database found. Run `seshat scan` first.".to_owned(),
        });
    }

    let database = Database::open(&resolved.db_path).map_err(|e| CliError::CommandFailed {
        command: "decisions".to_owned(),
        reason: format!("failed to open database: {e}"),
    })?;

    let decisions = load_decisions(&database, state_filter, branch_filter)?;

    let rendered = match format {
        DecisionsListFormat::Json => format_decisions_json(&decisions)?,
        DecisionsListFormat::Table => format_decisions_table(&decisions),
    };

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    out.write_all(rendered.as_bytes())?;
    Ok(())
}

/// Load decisions from the project DB, applying the optional filters.
///
/// State pushes down into the repository (`list_by_state`) so the lookup is
/// index-supported. The branch filter is applied in-memory because (a) the
/// V12 schema doesn't have a composite (state, branch) index and (b) the
/// total decision count is typically small (tens to low thousands).
fn load_decisions(
    database: &Database,
    state_filter: Option<DecisionStateFilter>,
    branch_filter: Option<&str>,
) -> Result<Vec<Decision>, CliError> {
    let repo = SqliteDecisionRepository::new(database.connection().clone());

    let mut decisions = match state_filter {
        Some(state) => repo.list_by_state(DecisionState::from(state)),
        None => repo.list(),
    }
    .map_err(|e| CliError::CommandFailed {
        command: "decisions".to_owned(),
        reason: format!("failed to read decisions: {e}"),
    })?;

    if let Some(branch) = branch_filter {
        decisions.retain(|d| d.decided_on_branch.0 == branch);
    }

    Ok(decisions)
}

/// JSON DTO mirroring the row shape of the `decisions` table.
///
/// Local to this module so the storage crate stays free of `serde::Serialize`
/// derives on `Decision` — and so the CLI can pin the wire shape (snake_case
/// enum strings, full hash) independently of internal types.
#[derive(Debug, Serialize)]
struct DecisionJson<'a> {
    description_hash: &'a str,
    description: &'a str,
    state: &'a str,
    nature: &'a str,
    weight: &'a str,
    category: Option<&'a str>,
    reason: Option<&'a str>,
    examples: &'a [ExampleEvidence],
    decided_on_branch: &'a str,
    decided_at: i64,
    updated_at: i64,
}

impl<'a> From<&'a Decision> for DecisionJson<'a> {
    fn from(d: &'a Decision) -> Self {
        Self {
            description_hash: &d.description_hash,
            description: &d.description,
            state: d.state.as_sql_str(),
            nature: d.nature.as_sql_str(),
            weight: d.weight.as_sql_str(),
            category: d.category.as_deref(),
            reason: d.reason.as_deref(),
            examples: &d.examples,
            decided_on_branch: &d.decided_on_branch.0,
            decided_at: d.decided_at,
            updated_at: d.updated_at,
        }
    }
}

fn format_decisions_json(decisions: &[Decision]) -> Result<String, CliError> {
    let dtos: Vec<DecisionJson<'_>> = decisions.iter().map(DecisionJson::from).collect();
    let mut json = serde_json::to_string_pretty(&dtos).map_err(|e| CliError::CommandFailed {
        command: "decisions".to_owned(),
        reason: format!("failed to serialise decisions to JSON: {e}"),
    })?;
    json.push('\n');
    Ok(json)
}

fn format_decisions_table(decisions: &[Decision]) -> String {
    if decisions.is_empty() {
        return "No decisions recorded.\n".to_owned();
    }

    // Column headers — match the AC literally:
    // "state | hash | description | decided_on_branch | decided_at".
    const H_STATE: &str = "state";
    const H_HASH: &str = "hash";
    const H_DESCRIPTION: &str = "description";
    const H_BRANCH: &str = "decided_on_branch";
    const H_DECIDED_AT: &str = "decided_at";

    // Pre-compute per-row formatted values so column widths are dimensioned
    // off the actually-rendered strings (truncated description, fixed-prefix
    // hash, formatted timestamp).
    let rows: Vec<[String; 5]> = decisions
        .iter()
        .map(|d| {
            [
                d.state.as_sql_str().to_owned(),
                short_hash(&d.description_hash),
                truncate_chars(&d.description, TABLE_DESCRIPTION_MAX),
                d.decided_on_branch.0.clone(),
                format_decided_at(d.decided_at),
            ]
        })
        .collect();

    let widths = [
        column_width(H_STATE, &rows, 0),
        column_width(H_HASH, &rows, 1),
        column_width(H_DESCRIPTION, &rows, 2),
        column_width(H_BRANCH, &rows, 3),
        column_width(H_DECIDED_AT, &rows, 4),
    ];

    let mut out = String::new();
    write_row(
        &mut out,
        &[H_STATE, H_HASH, H_DESCRIPTION, H_BRANCH, H_DECIDED_AT],
        &widths,
    );
    for row in &rows {
        let cells = [
            row[0].as_str(),
            row[1].as_str(),
            row[2].as_str(),
            row[3].as_str(),
            row[4].as_str(),
        ];
        write_row(&mut out, &cells, &widths);
    }
    out
}

fn column_width(header: &str, rows: &[[String; 5]], idx: usize) -> usize {
    let header_w = header.chars().count();
    rows.iter()
        .map(|r| r[idx].chars().count())
        .max()
        .map(|w| w.max(header_w))
        .unwrap_or(header_w)
}

fn write_row(out: &mut String, cells: &[&str; 5], widths: &[usize; 5]) {
    // Two-space gutter between columns; trailing column is unpadded so users
    // can copy-paste lines without trailing whitespace.
    writeln!(
        out,
        "{state:<state_w$}  {hash:<hash_w$}  {desc:<desc_w$}  {branch:<branch_w$}  {decided}",
        state = cells[0],
        state_w = widths[0],
        hash = cells[1],
        hash_w = widths[1],
        desc = cells[2],
        desc_w = widths[2],
        branch = cells[3],
        branch_w = widths[3],
        decided = cells[4],
    )
    .expect("writes to String are infallible");
}

fn short_hash(hash: &str) -> String {
    hash.chars().take(TABLE_HASH_LEN).collect()
}

fn truncate_chars(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_owned()
    } else if max == 0 {
        String::new()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

fn format_decided_at(epoch: i64) -> String {
    chrono::DateTime::from_timestamp(epoch, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| epoch.to_string())
}

// ══════════════════════════════════════════════════════════════════════
// `seshat decisions forget`
// ══════════════════════════════════════════════════════════════════════

/// Implement `seshat decisions forget <hash> [--yes]`.
///
/// Resolves `hash` (full description_hash or unambiguous prefix ≥ 4 chars)
/// against the project's decisions table, prints the matched decision, then
/// prompts the user for confirmation unless `--yes` was passed. On
/// confirmation the decision row is hard-deleted; the next `seshat scan`
/// will re-emit the convention into the review queue (per US-008's bulk
/// decision lookup, removing the row removes the dedup signal).
fn run_forget(hash: &str, yes: bool) -> Result<(), CliError> {
    let resolved = db::resolve_project(None, "decisions")?;

    if !resolved.db_path.exists() {
        return Err(CliError::CommandFailed {
            command: "decisions forget".to_owned(),
            reason: "No database found. Run `seshat scan` first.".to_owned(),
        });
    }

    let database = Database::open(&resolved.db_path).map_err(|e| CliError::CommandFailed {
        command: "decisions forget".to_owned(),
        reason: format!("failed to open database: {e}"),
    })?;
    let repo = SqliteDecisionRepository::new(database.connection().clone());

    let decision = resolve_decision_for_forget(&repo, hash)?;

    let mut stdout = std::io::stdout().lock();
    let summary = format_decision_summary(&decision);
    stdout.write_all(summary.as_bytes())?;

    if !yes && !prompt_for_confirmation(&mut stdout, &mut std::io::stdin().lock())? {
        writeln!(stdout, "Aborted; decision not removed.")?;
        return Ok(());
    }

    repo.delete(&decision.description_hash)
        .map_err(|e| CliError::CommandFailed {
            command: "decisions forget".to_owned(),
            reason: format!("failed to delete decision: {e}"),
        })?;

    writeln!(
        stdout,
        "Removed decision {}.",
        short_hash(&decision.description_hash)
    )?;
    Ok(())
}

/// Look up a decision by full hash or prefix, returning the unique match.
///
/// Errors for prefixes shorter than [`MIN_FORGET_PREFIX_LEN`], for prefixes
/// that match no decisions, and for prefixes that match more than one. The
/// ambiguous-match error message lists the short forms of the matched
/// hashes so the user can disambiguate by lengthening the prefix.
fn resolve_decision_for_forget<R: DecisionRepository>(
    repo: &R,
    hash: &str,
) -> Result<Decision, CliError> {
    if hash.len() < MIN_FORGET_PREFIX_LEN {
        return Err(CliError::InvalidArgument(format!(
            "decision hash prefix '{hash}' is too short; need at least \
             {MIN_FORGET_PREFIX_LEN} characters"
        )));
    }

    let mut matches: Vec<Decision> = repo
        .list()
        .map_err(|e| CliError::CommandFailed {
            command: "decisions forget".to_owned(),
            reason: format!("failed to read decisions: {e}"),
        })?
        .into_iter()
        .filter(|d| d.description_hash.starts_with(hash))
        .collect();

    match matches.len() {
        0 => Err(CliError::CommandFailed {
            command: "decisions forget".to_owned(),
            reason: format!("no decision matches hash '{hash}'"),
        }),
        1 => Ok(matches.swap_remove(0)),
        _ => {
            let listed = matches
                .iter()
                .map(|d| short_hash(&d.description_hash))
                .collect::<Vec<_>>()
                .join(", ");
            Err(CliError::CommandFailed {
                command: "decisions forget".to_owned(),
                reason: format!(
                    "prefix '{hash}' is ambiguous; matches {} decisions: {listed}",
                    matches.len()
                ),
            })
        }
    }
}

/// Render a decision as a multi-line key:value summary suitable for the
/// confirmation prompt. The full hash is shown (not truncated) so the user
/// can verify the exact row that's about to be deleted.
fn format_decision_summary(decision: &Decision) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Found decision:");
    let _ = writeln!(out, "  hash:        {}", decision.description_hash);
    let _ = writeln!(out, "  state:       {}", decision.state.as_sql_str());
    let _ = writeln!(out, "  nature:      {}", decision.nature.as_sql_str());
    let _ = writeln!(out, "  weight:      {}", decision.weight.as_sql_str());
    let _ = writeln!(out, "  description: {}", decision.description);
    let _ = writeln!(out, "  branch:      {}", decision.decided_on_branch.0);
    let _ = writeln!(
        out,
        "  decided_at:  {}",
        format_decided_at(decision.decided_at)
    );
    out
}

/// Prompt the user for confirmation on `out`, then read a line from `input`
/// and return whether the response is an affirmative.
///
/// Accepts `y` or `yes` (case-insensitive) as positive; everything else —
/// including the empty default response — is treated as decline. Mirrors the
/// `[y/N]` style that `git` and other CLI tools use, with the lowercase `n`
/// signalling that "no" is the safe default.
fn prompt_for_confirmation<W: Write, R: std::io::BufRead>(
    out: &mut W,
    input: &mut R,
) -> Result<bool, CliError> {
    write!(out, "Forget this decision? [y/N]: ")?;
    out.flush()?;
    let mut response = String::new();
    input.read_line(&mut response)?;
    let trimmed = response.trim().to_ascii_lowercase();
    Ok(trimmed == "y" || trimmed == "yes")
}

// ══════════════════════════════════════════════════════════════════════
// Test-only seam for the `forget` integration test
// ══════════════════════════════════════════════════════════════════════

/// Resolve and hard-delete a decision by hash or prefix, returning the
/// removed [`Decision`]. Public seam for the integration test
/// (`tests/decisions_forget.rs`) that exercises the full
/// "scan → confirm → forget → rescan re-emits" flow without involving stdin.
///
/// This bypasses both project resolution and the interactive prompt: the
/// caller supplies an already-open [`Database`], and the helper performs
/// the same resolve-then-delete sequence the `--yes` path uses.
pub fn forget_decision_with_database(
    database: &Database,
    hash: &str,
) -> Result<Decision, CliError> {
    let repo = SqliteDecisionRepository::new(database.connection().clone());
    let decision = resolve_decision_for_forget(&repo, hash)?;
    repo.delete(&decision.description_hash)
        .map_err(|e| CliError::CommandFailed {
            command: "decisions forget".to_owned(),
            reason: format!("failed to delete decision: {e}"),
        })?;
    Ok(decision)
}

// ══════════════════════════════════════════════════════════════════════
// Tests
// ══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::BranchId;
    use seshat_storage::{DecisionNature, DecisionWeight};

    fn make_db() -> Database {
        Database::open(":memory:").expect("in-memory DB")
    }

    fn make_decision(
        hash: &str,
        description: &str,
        state: DecisionState,
        branch: &str,
        decided_at: i64,
    ) -> Decision {
        Decision {
            description_hash: hash.to_owned(),
            description: description.to_owned(),
            state,
            nature: DecisionNature::Convention,
            weight: DecisionWeight::Rule,
            category: Some("logging".to_owned()),
            reason: Some("because tests".to_owned()),
            examples: vec![ExampleEvidence {
                file: "src/lib.rs".to_owned(),
                line: 1,
                end_line: 3,
                snippet: "tracing::info!()".to_owned(),
            }],
            decided_on_branch: BranchId(branch.to_owned()),
            decided_at,
            updated_at: decided_at,
        }
    }

    fn populate(db: &Database) {
        let repo = SqliteDecisionRepository::new(db.connection().clone());
        repo.upsert(&make_decision(
            "aaaaaaaa1111",
            "Use anyhow for error propagation",
            DecisionState::Approved,
            "main",
            1_700_000_100,
        ))
        .unwrap();
        repo.upsert(&make_decision(
            "bbbbbbbb2222",
            "Allow unwrap() in production",
            DecisionState::Rejected,
            "feature/x",
            1_700_000_200,
        ))
        .unwrap();
        repo.upsert(&make_decision(
            "cccccccc3333",
            "Partial: tracing::info for hot paths only",
            DecisionState::Partial,
            "main",
            1_700_000_300,
        ))
        .unwrap();
        repo.upsert(&make_decision(
            "dddddddd4444",
            "Recorded decision via MCP",
            DecisionState::Recorded,
            "main",
            1_700_000_400,
        ))
        .unwrap();
    }

    // ── format_decisions_table ───────────────────────────────────────

    #[test]
    fn format_decisions_table_empty_returns_friendly_message() {
        let out = format_decisions_table(&[]);
        assert_eq!(out, "No decisions recorded.\n");
    }

    #[test]
    fn format_decisions_table_populated_includes_header_and_rows() {
        let db = make_db();
        populate(&db);
        let decisions = load_decisions(&db, None, None).unwrap();

        let table = format_decisions_table(&decisions);

        // Header row.
        assert!(table.contains("state"), "missing state header: {table}");
        assert!(table.contains("hash"), "missing hash header: {table}");
        assert!(
            table.contains("description"),
            "missing description header: {table}"
        );
        assert!(
            table.contains("decided_on_branch"),
            "missing branch header: {table}"
        );
        assert!(
            table.contains("decided_at"),
            "missing decided_at header: {table}"
        );

        // Each state value appears at least once.
        for state in ["approved", "rejected", "partial", "recorded"] {
            assert!(table.contains(state), "missing state {state}: {table}");
        }

        // Hash prefix appears (TABLE_HASH_LEN = 8).
        assert!(table.contains("aaaaaaaa"));
        assert!(table.contains("bbbbbbbb"));

        // Branches appear.
        assert!(table.contains("main"));
        assert!(table.contains("feature/x"));

        // Description text appears (un-truncated since it fits in 60 chars).
        assert!(table.contains("Use anyhow for error propagation"));
    }

    #[test]
    fn format_decisions_table_truncates_long_description() {
        let long = "x".repeat(200);
        let d = make_decision("h", &long, DecisionState::Approved, "main", 1_700_000_000);
        let table = format_decisions_table(std::slice::from_ref(&d));

        // Should NOT contain the full 200-char string.
        assert!(!table.contains(&long));
        // Should contain ellipsis indicating truncation.
        assert!(table.contains('…'), "expected ellipsis: {table}");
    }

    // ── format_decisions_json ────────────────────────────────────────

    #[test]
    fn format_decisions_json_empty_is_valid_json_array() {
        let out = format_decisions_json(&[]).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert!(parsed.is_array());
        assert_eq!(parsed.as_array().unwrap().len(), 0);
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn format_decisions_json_populated_is_valid_json_array() {
        let db = make_db();
        populate(&db);
        let decisions = load_decisions(&db, None, None).unwrap();
        assert_eq!(decisions.len(), 4);

        let out = format_decisions_json(&decisions).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        let arr = parsed.as_array().expect("top-level array");
        assert_eq!(arr.len(), 4);

        // Each item has the required Decision shape.
        for item in arr {
            let obj = item.as_object().expect("object");
            for key in [
                "description_hash",
                "description",
                "state",
                "nature",
                "weight",
                "category",
                "reason",
                "examples",
                "decided_on_branch",
                "decided_at",
                "updated_at",
            ] {
                assert!(obj.contains_key(key), "missing key {key} in {item}");
            }
        }
    }

    #[test]
    fn format_decisions_json_uses_sql_state_strings() {
        // Pin the wire shape: enum values render as the same lowercase strings
        // used in the SQL CHECK constraints, not as PascalCase Rust variants.
        let d = make_decision("h", "x", DecisionState::Approved, "main", 1_700_000_000);
        let out = format_decisions_json(std::slice::from_ref(&d)).unwrap();
        assert!(out.contains("\"state\": \"approved\""), "got: {out}");
        assert!(out.contains("\"nature\": \"convention\""));
        assert!(out.contains("\"weight\": \"rule\""));
    }

    // ── load_decisions filters ───────────────────────────────────────

    #[test]
    fn load_decisions_empty_db_returns_empty_vec() {
        let db = make_db();
        let result = load_decisions(&db, None, None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn load_decisions_no_filter_returns_all() {
        let db = make_db();
        populate(&db);
        let result = load_decisions(&db, None, None).unwrap();
        assert_eq!(result.len(), 4);
    }

    #[test]
    fn load_decisions_filters_by_state() {
        let db = make_db();
        populate(&db);

        let approved = load_decisions(&db, Some(DecisionStateFilter::Approved), None).unwrap();
        assert_eq!(approved.len(), 1);
        assert_eq!(approved[0].state, DecisionState::Approved);

        let rejected = load_decisions(&db, Some(DecisionStateFilter::Rejected), None).unwrap();
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].state, DecisionState::Rejected);

        let partial = load_decisions(&db, Some(DecisionStateFilter::Partial), None).unwrap();
        assert_eq!(partial.len(), 1);

        let recorded = load_decisions(&db, Some(DecisionStateFilter::Recorded), None).unwrap();
        assert_eq!(recorded.len(), 1);
    }

    #[test]
    fn load_decisions_filters_by_branch() {
        let db = make_db();
        populate(&db);

        let main_only = load_decisions(&db, None, Some("main")).unwrap();
        assert_eq!(main_only.len(), 3);
        assert!(main_only.iter().all(|d| d.decided_on_branch.0 == "main"));

        let feature = load_decisions(&db, None, Some("feature/x")).unwrap();
        assert_eq!(feature.len(), 1);
        assert_eq!(feature[0].decided_on_branch.0, "feature/x");

        let unknown = load_decisions(&db, None, Some("does-not-exist")).unwrap();
        assert!(unknown.is_empty());
    }

    #[test]
    fn load_decisions_combined_state_and_branch_filter() {
        let db = make_db();
        populate(&db);

        // Approved on main → exactly one (the "Use anyhow…" one).
        let result =
            load_decisions(&db, Some(DecisionStateFilter::Approved), Some("main")).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].description_hash, "aaaaaaaa1111");

        // Rejected on main → none (the rejected row is on feature/x).
        let result =
            load_decisions(&db, Some(DecisionStateFilter::Rejected), Some("main")).unwrap();
        assert!(result.is_empty());
    }

    // ── helpers ──────────────────────────────────────────────────────

    #[test]
    fn short_hash_truncates_to_eight_chars() {
        assert_eq!(short_hash("abcdef0123456789"), "abcdef01");
        // Short inputs are returned as-is.
        assert_eq!(short_hash("abc"), "abc");
        // Exactly TABLE_HASH_LEN is returned unchanged.
        assert_eq!(short_hash("abcdefgh"), "abcdefgh");
    }

    #[test]
    fn truncate_chars_returns_input_when_short_enough() {
        assert_eq!(truncate_chars("hello", 10), "hello");
        // Boundary: equal length is unchanged (no ellipsis).
        assert_eq!(truncate_chars("hello", 5), "hello");
    }

    #[test]
    fn truncate_chars_appends_ellipsis_when_too_long() {
        let out = truncate_chars("0123456789", 6);
        // 5 chars + ellipsis = 6 visible glyphs.
        assert_eq!(out, "01234…");
    }

    #[test]
    fn format_decided_at_formats_unix_timestamp() {
        // 1_700_000_000 is 2023-11-14 22:13:20 UTC.
        let out = format_decided_at(1_700_000_000);
        assert_eq!(out, "2023-11-14 22:13:20");
    }

    // ── arg conversion ───────────────────────────────────────────────

    #[test]
    fn decision_state_filter_converts_to_storage_enum() {
        assert_eq!(
            DecisionState::from(DecisionStateFilter::Approved),
            DecisionState::Approved
        );
        assert_eq!(
            DecisionState::from(DecisionStateFilter::Rejected),
            DecisionState::Rejected
        );
        assert_eq!(
            DecisionState::from(DecisionStateFilter::Partial),
            DecisionState::Partial
        );
        assert_eq!(
            DecisionState::from(DecisionStateFilter::Recorded),
            DecisionState::Recorded
        );
    }

    // ── resolve_decision_for_forget ──────────────────────────────────

    #[test]
    fn resolve_decision_for_forget_returns_exact_match_for_full_hash() {
        let db = make_db();
        populate(&db);
        let repo = SqliteDecisionRepository::new(db.connection().clone());

        let resolved = resolve_decision_for_forget(&repo, "aaaaaaaa1111").unwrap();
        assert_eq!(resolved.description_hash, "aaaaaaaa1111");
        assert_eq!(resolved.state, DecisionState::Approved);
    }

    #[test]
    fn resolve_decision_for_forget_returns_unique_match_for_prefix() {
        let db = make_db();
        populate(&db);
        let repo = SqliteDecisionRepository::new(db.connection().clone());

        // 4-char prefix uniquely identifying the "Use anyhow…" row.
        let resolved = resolve_decision_for_forget(&repo, "aaaa").unwrap();
        assert_eq!(resolved.description_hash, "aaaaaaaa1111");
    }

    #[test]
    fn resolve_decision_for_forget_rejects_short_prefix() {
        let db = make_db();
        populate(&db);
        let repo = SqliteDecisionRepository::new(db.connection().clone());

        let err = resolve_decision_for_forget(&repo, "abc").unwrap_err();
        let msg = err.to_string();
        // The min-length guard fires BEFORE the lookup, so the error must
        // mention the minimum-length contract regardless of whether a
        // matching prefix exists.
        assert!(msg.contains("too short"), "got: {msg}");
        assert!(msg.contains("4"), "must mention the 4-char minimum: {msg}");
    }

    #[test]
    fn resolve_decision_for_forget_rejects_short_prefix_even_when_unique() {
        // The min-length guard is a CLI-level safety rail, not just a
        // disambiguation aid. Even when "abc" would uniquely match a row
        // (DB has only one decision starting with "abc"), the rule applies.
        let db = make_db();
        let repo = SqliteDecisionRepository::new(db.connection().clone());
        repo.upsert(&make_decision(
            "abc",
            "test",
            DecisionState::Approved,
            "main",
            1,
        ))
        .unwrap();

        let err = resolve_decision_for_forget(&repo, "abc").unwrap_err();
        assert!(err.to_string().contains("too short"));
    }

    #[test]
    fn resolve_decision_for_forget_returns_not_found_for_unmatched_prefix() {
        let db = make_db();
        populate(&db);
        let repo = SqliteDecisionRepository::new(db.connection().clone());

        let err = resolve_decision_for_forget(&repo, "ffff0000").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no decision matches"), "got: {msg}");
        assert!(msg.contains("ffff0000"), "must echo the input: {msg}");
    }

    #[test]
    fn resolve_decision_for_forget_returns_ambiguous_for_multiple_matches() {
        let db = make_db();
        let repo = SqliteDecisionRepository::new(db.connection().clone());
        // Two decisions sharing a 4-char prefix.
        repo.upsert(&make_decision(
            "aaaa1111",
            "first",
            DecisionState::Approved,
            "main",
            1,
        ))
        .unwrap();
        repo.upsert(&make_decision(
            "aaaa2222",
            "second",
            DecisionState::Rejected,
            "main",
            2,
        ))
        .unwrap();

        let err = resolve_decision_for_forget(&repo, "aaaa").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ambiguous"), "got: {msg}");
        // Should list both matched (short) hashes so the user can lengthen.
        assert!(msg.contains("aaaa1111"), "missing first hash: {msg}");
        assert!(msg.contains("aaaa2222"), "missing second hash: {msg}");
    }

    // ── format_decision_summary ──────────────────────────────────────

    #[test]
    fn format_decision_summary_includes_full_hash_and_key_fields() {
        let d = make_decision(
            "aaaaaaaa1111",
            "Use anyhow for error propagation",
            DecisionState::Approved,
            "main",
            1_700_000_000,
        );
        let summary = format_decision_summary(&d);
        // Full hash, not truncated, so the user can confirm the exact row.
        assert!(summary.contains("aaaaaaaa1111"));
        assert!(summary.contains("approved"));
        assert!(summary.contains("convention"));
        assert!(summary.contains("rule"));
        assert!(summary.contains("Use anyhow for error propagation"));
        assert!(summary.contains("main"));
        // Formatted timestamp, not raw epoch.
        assert!(summary.contains("2023-11-14 22:13:20"));
    }

    // ── prompt_for_confirmation ──────────────────────────────────────

    #[test]
    fn prompt_for_confirmation_treats_y_as_affirmative() {
        let mut out: Vec<u8> = Vec::new();
        let mut input = std::io::Cursor::new(b"y\n".to_vec());
        assert!(prompt_for_confirmation(&mut out, &mut input).unwrap());
        let prompt = String::from_utf8(out).unwrap();
        assert!(prompt.contains("Forget this decision?"));
        assert!(prompt.contains("[y/N]"), "must show the [y/N] hint");
    }

    #[test]
    fn prompt_for_confirmation_accepts_uppercase_yes() {
        let mut out: Vec<u8> = Vec::new();
        let mut input = std::io::Cursor::new(b"YES\n".to_vec());
        assert!(prompt_for_confirmation(&mut out, &mut input).unwrap());
    }

    #[test]
    fn prompt_for_confirmation_treats_n_as_decline() {
        let mut out: Vec<u8> = Vec::new();
        let mut input = std::io::Cursor::new(b"n\n".to_vec());
        assert!(!prompt_for_confirmation(&mut out, &mut input).unwrap());
    }

    #[test]
    fn prompt_for_confirmation_treats_empty_default_as_decline() {
        // Pressing Enter with no input must NOT delete — the [y/N] convention
        // is "lowercase n is the default, deletions are explicit only".
        let mut out: Vec<u8> = Vec::new();
        let mut input = std::io::Cursor::new(b"\n".to_vec());
        assert!(!prompt_for_confirmation(&mut out, &mut input).unwrap());
    }

    #[test]
    fn prompt_for_confirmation_treats_unrelated_input_as_decline() {
        // Anything that isn't y/yes is a decline — preserves the safe default.
        let mut out: Vec<u8> = Vec::new();
        let mut input = std::io::Cursor::new(b"maybe\n".to_vec());
        assert!(!prompt_for_confirmation(&mut out, &mut input).unwrap());
    }

    // ── forget_decision_with_database ────────────────────────────────

    #[test]
    fn forget_decision_with_database_deletes_by_full_hash() {
        let db = make_db();
        populate(&db);
        // Sanity: row exists pre-delete.
        let repo = SqliteDecisionRepository::new(db.connection().clone());
        assert!(repo.get_by_hash("aaaaaaaa1111").unwrap().is_some());

        let removed = forget_decision_with_database(&db, "aaaaaaaa1111").unwrap();
        assert_eq!(removed.description_hash, "aaaaaaaa1111");
        assert_eq!(removed.state, DecisionState::Approved);
        // Row is hard-deleted; no soft-delete column to set.
        assert!(repo.get_by_hash("aaaaaaaa1111").unwrap().is_none());
    }

    #[test]
    fn forget_decision_with_database_deletes_by_prefix() {
        let db = make_db();
        populate(&db);
        let repo = SqliteDecisionRepository::new(db.connection().clone());

        let removed = forget_decision_with_database(&db, "bbbb").unwrap();
        assert_eq!(removed.description_hash, "bbbbbbbb2222");
        assert!(repo.get_by_hash("bbbbbbbb2222").unwrap().is_none());
    }

    #[test]
    fn forget_decision_with_database_propagates_resolution_errors() {
        let db = make_db();
        populate(&db);

        // Not found.
        let err = forget_decision_with_database(&db, "ffff0000").unwrap_err();
        assert!(err.to_string().contains("no decision matches"));

        // Too short — never even hits the DB.
        let err = forget_decision_with_database(&db, "ab").unwrap_err();
        assert!(err.to_string().contains("too short"));
    }
}
