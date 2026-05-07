//! Implementation of the `seshat decisions` subcommands.
//!
//! Currently exposes `seshat decisions list` (US-013). Future subcommands —
//! `forget` (US-014), `export` / `import` (US-015) — will slot in here.

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

/// Dispatch a `seshat decisions <subcommand>` invocation.
pub fn run_decisions(command: DecisionsCommand) -> Result<(), CliError> {
    match command {
        DecisionsCommand::List {
            state,
            branch,
            format,
        } => run_list(state, branch.as_deref(), format),
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
}
