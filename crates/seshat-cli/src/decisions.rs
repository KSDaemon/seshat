//! Implementation of the `seshat decisions` subcommands.
//!
//! Exposes `seshat decisions list` (US-013), `seshat decisions forget`
//! (US-014), and `seshat decisions export` / `seshat decisions import`
//! (US-015).

use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use seshat_core::BranchId;
use seshat_storage::{
    Database, Decision, DecisionNature, DecisionRepository, DecisionState, DecisionWeight,
    ExampleEvidence, SqliteDecisionRepository,
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
        DecisionsCommand::Export { file } => run_export(&file),
        DecisionsCommand::Import { file, strict } => run_import(&file, strict),
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
    write_tolerating_broken_pipe(&mut out, rendered.as_bytes())?;
    Ok(())
}

/// Write `bytes` to `out`, treating `ErrorKind::BrokenPipe` as a clean
/// exit signal rather than an error.
///
/// P33: a downstream pipeline like `seshat decisions list | head -1`
/// closes the read side after the first line. Pre-fix the resulting
/// `BrokenPipe` from `write_all` propagated up as `CliError::Io` and
/// the process exited non-zero — surprising for what looks like a
/// successful pipeline. The Unix CLI convention is to treat early
/// reader exit as a normal termination, mirroring how `cat` / `seq`
/// behave under `head`.
fn write_tolerating_broken_pipe<W: Write>(out: &mut W, bytes: &[u8]) -> Result<(), CliError> {
    match out.write_all(bytes) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(CliError::Io(e)),
    }
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
    write_tolerating_broken_pipe(&mut stdout, summary.as_bytes())?;

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

    // P25: push the prefix filter into SQL (index-backed range scan)
    // rather than materialising the full table client-side.
    let mut matches: Vec<Decision> =
        repo.find_by_hash_prefix(hash)
            .map_err(|e| CliError::CommandFailed {
                command: "decisions forget".to_owned(),
                reason: format!("failed to read decisions: {e}"),
            })?;

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
    let bytes = input.read_line(&mut response)?;
    // P31: distinguish "user pressed Enter" (1 byte: `\n`) from EOF
    // (0 bytes — happens when stdin is closed, e.g. piped from
    // /dev/null or run under a non-interactive shell). The intent
    // there is "I cannot answer; do not delete by default", which
    // is a refusal — but the caller deserves a clear error so they
    // can pass --yes if they meant the unattended path.
    if bytes == 0 {
        return Err(CliError::CommandFailed {
            command: "decisions forget".to_owned(),
            reason: "stdin closed before confirmation; pass --yes to skip the \
                     prompt for unattended runs"
                .to_owned(),
        });
    }
    let trimmed = response.trim().to_ascii_lowercase();
    Ok(trimmed == "y" || trimmed == "yes")
}

// ══════════════════════════════════════════════════════════════════════
// `seshat decisions export` / `seshat decisions import`
// ══════════════════════════════════════════════════════════════════════

/// Owned mirror of [`DecisionJson`] used for round-trip
/// serialisation/deserialisation.
///
/// [`DecisionJson`] borrows from a [`Decision`] for efficient export, but
/// import needs an owned shape that `serde_json` can deserialise into. Both
/// types share the same field names so the wire format is identical: a JSON
/// array produced by `seshat decisions export` deserialises cleanly into
/// `Vec<DecisionJsonOwned>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DecisionJsonOwned {
    description_hash: String,
    description: String,
    state: String,
    nature: String,
    weight: String,
    category: Option<String>,
    reason: Option<String>,
    examples: Vec<ExampleEvidence>,
    decided_on_branch: String,
    decided_at: i64,
    updated_at: i64,
}

impl DecisionJsonOwned {
    fn into_decision(self) -> Result<Decision, CliError> {
        let state =
            DecisionState::from_sql_str(&self.state).map_err(|e| CliError::CommandFailed {
                command: "decisions import".to_owned(),
                reason: format!("invalid state for hash '{}': {e}", self.description_hash),
            })?;
        let nature =
            DecisionNature::from_sql_str(&self.nature).map_err(|e| CliError::CommandFailed {
                command: "decisions import".to_owned(),
                reason: format!("invalid nature for hash '{}': {e}", self.description_hash),
            })?;
        let weight =
            DecisionWeight::from_sql_str(&self.weight).map_err(|e| CliError::CommandFailed {
                command: "decisions import".to_owned(),
                reason: format!("invalid weight for hash '{}': {e}", self.description_hash),
            })?;
        Ok(Decision {
            description_hash: self.description_hash,
            description: self.description,
            state,
            nature,
            weight,
            category: self.category,
            reason: self.reason,
            examples: self.examples,
            decided_on_branch: BranchId(self.decided_on_branch),
            decided_at: self.decided_at,
            updated_at: self.updated_at,
        })
    }
}

/// Outcome of an import operation.
///
/// Returned by [`import_decisions_from_str`] so the caller (CLI or test)
/// can render or assert against the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSummary {
    /// Total rows in the import payload.
    pub total: usize,
    /// Rows newly inserted (no prior row with this hash).
    pub inserted: usize,
    /// Rows that updated an existing row because the imported `decided_at`
    /// was strictly greater than the existing one.
    pub updated: usize,
    /// Rows skipped because an existing row with a `decided_at` ≥ the
    /// imported one was already present (incumbent kept).
    pub skipped: usize,
}

/// Atomically write `bytes` to `path` using a temp-file-and-rename
/// pattern. P32: pre-fix `std::fs::write` left a truncated file behind
/// if the process was killed mid-write, and a subsequent import would
/// fail with a JSON parse error against half a payload.
///
/// The temp file is named `.path.<pid>.tmp` next to the target so the
/// rename happens within the same filesystem. Rename is atomic on
/// POSIX/NTFS for files on the same volume.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    use std::io::Write;

    let parent = path.parent().unwrap_or(Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("decisions-export");
    let tmp_name = format!(".{file_name}.{}.tmp", std::process::id());
    let tmp_path = parent.join(tmp_name);

    {
        let mut tmp = std::fs::File::create(&tmp_path).map_err(|e| CliError::IoWithPath {
            message: format!("failed to create export temp file: {e}"),
            path: tmp_path.clone(),
        })?;
        tmp.write_all(bytes).map_err(|e| CliError::IoWithPath {
            message: format!("failed to write decisions export: {e}"),
            path: tmp_path.clone(),
        })?;
        tmp.sync_all().map_err(|e| CliError::IoWithPath {
            message: format!("failed to fsync export temp file: {e}"),
            path: tmp_path.clone(),
        })?;
    }

    std::fs::rename(&tmp_path, path).map_err(|e| {
        // Best-effort cleanup so we don't leave the temp file behind.
        let _ = std::fs::remove_file(&tmp_path);
        CliError::IoWithPath {
            message: format!("failed to atomically rename export to target: {e}"),
            path: path.to_owned(),
        }
    })?;
    Ok(())
}

/// Implement `seshat decisions export <file>`.
fn run_export(file: &Path) -> Result<(), CliError> {
    let resolved = db::resolve_project(None, "decisions")?;

    if !resolved.db_path.exists() {
        return Err(CliError::CommandFailed {
            command: "decisions export".to_owned(),
            reason: "No database found. Run `seshat scan` first.".to_owned(),
        });
    }

    let database = Database::open(&resolved.db_path).map_err(|e| CliError::CommandFailed {
        command: "decisions export".to_owned(),
        reason: format!("failed to open database: {e}"),
    })?;

    let json = export_decisions_to_string(&database)?;
    write_atomic(file, json.as_bytes())?;

    let count = export_count(&database)?;
    let mut stdout = std::io::stdout().lock();
    writeln!(
        stdout,
        "Exported {count} decision{plural} to {path}",
        plural = if count == 1 { "" } else { "s" },
        path = file.display(),
    )?;
    Ok(())
}

/// Read all decisions from `database` and serialise them as a pretty-printed
/// JSON array. Matches the wire shape used by `seshat decisions list
/// --format json` so a round-trip via `decisions import` is lossless.
///
/// Public seam for the integration / round-trip test.
pub fn export_decisions_to_string(database: &Database) -> Result<String, CliError> {
    let repo = SqliteDecisionRepository::new(database.connection().clone());
    let decisions = repo.list().map_err(|e| CliError::CommandFailed {
        command: "decisions export".to_owned(),
        reason: format!("failed to read decisions: {e}"),
    })?;

    let dtos: Vec<DecisionJson<'_>> = decisions.iter().map(DecisionJson::from).collect();
    let mut json = serde_json::to_string_pretty(&dtos).map_err(|e| CliError::CommandFailed {
        command: "decisions export".to_owned(),
        reason: format!("failed to serialise decisions to JSON: {e}"),
    })?;
    json.push('\n');
    Ok(json)
}

fn export_count(database: &Database) -> Result<usize, CliError> {
    let repo = SqliteDecisionRepository::new(database.connection().clone());
    repo.list()
        .map(|v| v.len())
        .map_err(|e| CliError::CommandFailed {
            command: "decisions export".to_owned(),
            reason: format!("failed to read decisions: {e}"),
        })
}

/// Implement `seshat decisions import <file> [--strict]`.
fn run_import(file: &Path, strict: bool) -> Result<(), CliError> {
    let resolved = db::resolve_project(None, "decisions")?;

    if !resolved.db_path.exists() {
        return Err(CliError::CommandFailed {
            command: "decisions import".to_owned(),
            reason: "No database found. Run `seshat scan` first.".to_owned(),
        });
    }

    let database = Database::open(&resolved.db_path).map_err(|e| CliError::CommandFailed {
        command: "decisions import".to_owned(),
        reason: format!("failed to open database: {e}"),
    })?;

    let json = std::fs::read_to_string(file).map_err(|e| CliError::IoWithPath {
        message: format!("failed to read decisions import file: {e}"),
        path: file.to_owned(),
    })?;

    let summary = import_decisions_from_str(&database, &json, strict)?;

    let mut stdout = std::io::stdout().lock();
    writeln!(
        stdout,
        "Imported {} decision{plural} ({} new, {} updated, {} skipped).",
        summary.inserted + summary.updated,
        summary.inserted,
        summary.updated,
        summary.skipped,
        plural = if summary.inserted + summary.updated == 1 {
            ""
        } else {
            "s"
        },
    )?;
    Ok(())
}

/// Parse `json` as a `Vec<DecisionJsonOwned>` and apply each row against
/// `database` per the conflict policy:
///
/// - `strict = false` (default): for each incoming row, compare its
///   `decided_at` against the existing row's `decided_at` (if any). If the
///   incoming row is strictly newer, UPSERT it; otherwise leave the existing
///   row untouched. New rows (no existing match) are always inserted.
/// - `strict = true`: any incoming hash that already has a row in the DB
///   aborts the import with [`CliError::CommandFailed`]; no writes happen.
///
/// Errors:
/// - Malformed JSON → `CommandFailed`.
/// - Invalid enum string in any row → `CommandFailed`.
/// - DB read/write failure → `CommandFailed`.
///
/// Public seam for unit and round-trip tests so they can drive the import
/// without going through `db::resolve_project` and the project-wide DB
/// path resolution.
pub fn import_decisions_from_str(
    database: &Database,
    json: &str,
    strict: bool,
) -> Result<ImportSummary, CliError> {
    let parsed: Vec<DecisionJsonOwned> =
        serde_json::from_str(json).map_err(|e| CliError::CommandFailed {
            command: "decisions import".to_owned(),
            reason: format!("failed to parse decisions JSON: {e}"),
        })?;

    let total = parsed.len();
    let repo = SqliteDecisionRepository::new(database.connection().clone());

    // Strict mode: pre-flight all hashes BEFORE any writes so a single
    // conflict aborts the entire import. We collect every conflicting hash
    // (not just the first) so the error message is actionable.
    if strict {
        let hash_refs: Vec<&str> = parsed.iter().map(|d| d.description_hash.as_str()).collect();
        let existing = repo
            .get_by_hashes(&hash_refs)
            .map_err(|e| CliError::CommandFailed {
                command: "decisions import".to_owned(),
                reason: format!("failed to look up existing decisions: {e}"),
            })?;
        if !existing.is_empty() {
            let mut conflicts: Vec<&str> = existing.keys().map(String::as_str).collect();
            conflicts.sort_unstable();
            return Err(CliError::CommandFailed {
                command: "decisions import".to_owned(),
                reason: format!(
                    "strict mode: {} hash conflict{} detected; aborting import: {}",
                    conflicts.len(),
                    if conflicts.len() == 1 { "" } else { "s" },
                    conflicts.join(", "),
                ),
            });
        }
    }

    let mut summary = ImportSummary {
        total,
        inserted: 0,
        updated: 0,
        skipped: 0,
    };

    // P27: wrap the whole import in one transaction. Pre-fix every
    // repo.upsert ran in its own implicit transaction, so a failure
    // halfway through left the DB partially populated — bad for both
    // atomicity (caller sees partial state) and performance (per-row
    // commit overhead). With BEGIN IMMEDIATE the loop becomes a single
    // unit: full success → COMMIT; any failure → ROLLBACK and the user
    // can re-run the import with the original payload unchanged.
    //
    // The connection's transaction state is per-connection, so the
    // BEGIN here covers every subsequent repo.upsert / repo.get_by_hash
    // (each re-acquires the same Mutex<Connection>) until the closing
    // COMMIT/ROLLBACK below.
    {
        let guard = database
            .connection()
            .lock()
            .map_err(|e| CliError::CommandFailed {
                command: "decisions import".to_owned(),
                reason: format!("failed to acquire DB lock for transaction: {e}"),
            })?;
        guard
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| CliError::CommandFailed {
                command: "decisions import".to_owned(),
                reason: format!("failed to begin transaction: {e}"),
            })?;
    }

    // P26: bulk-fetch existing rows in ONE SELECT and look them up
    // in-memory, instead of issuing one repo.get_by_hash per entry
    // (the pre-fix N+1: ~5k SELECTs for a 5k-row import). The HashMap
    // returned by get_by_hashes mirrors what the per-row path
    // produced; the rest of the loop's branching stays identical.
    // Note: strict-mode does its own get_by_hashes pre-flight above
    // and returns early on conflict, so this fetch is non-strict only.
    let existing_map = {
        let hash_refs: Vec<&str> = parsed.iter().map(|d| d.description_hash.as_str()).collect();
        repo.get_by_hashes(&hash_refs)
            .map_err(|e| CliError::CommandFailed {
                command: "decisions import".to_owned(),
                reason: format!("failed to bulk-look up existing decisions: {e}"),
            })?
    };

    let txn_result: Result<ImportSummary, CliError> = (|| {
        for entry in parsed {
            let decision = entry.into_decision()?;
            match existing_map.get(&decision.description_hash).cloned() {
                None => {
                    repo.upsert(&decision)
                        .map_err(|e| CliError::CommandFailed {
                            command: "decisions import".to_owned(),
                            reason: format!(
                                "failed to insert decision '{}': {e}",
                                decision.description_hash
                            ),
                        })?;
                    summary.inserted += 1;
                }
                Some(existing) => {
                    // "Latest decided_at wins" — strict greater-than so equal
                    // timestamps preserve the incumbent (deterministic, avoids
                    // churn on round-trips of unchanged rows).
                    if decision.decided_at > existing.decided_at {
                        repo.upsert(&decision)
                            .map_err(|e| CliError::CommandFailed {
                                command: "decisions import".to_owned(),
                                reason: format!(
                                    "failed to update decision '{}': {e}",
                                    decision.description_hash
                                ),
                            })?;
                        summary.updated += 1;
                    } else {
                        summary.skipped += 1;
                    }
                }
            }
        }
        Ok(summary)
    })();

    // Commit or rollback the transaction opened above. A best-effort
    // ROLLBACK on failure keeps the DB at its pre-import state.
    {
        let guard = database
            .connection()
            .lock()
            .map_err(|e| CliError::CommandFailed {
                command: "decisions import".to_owned(),
                reason: format!("failed to re-acquire DB lock for COMMIT: {e}"),
            })?;
        match &txn_result {
            Ok(_) => guard
                .execute_batch("COMMIT")
                .map_err(|e| CliError::CommandFailed {
                    command: "decisions import".to_owned(),
                    reason: format!("failed to commit transaction: {e}"),
                })?,
            Err(_) => {
                // Errors during ROLLBACK are logged but never overwrite the
                // primary error — the caller cares about the original cause.
                if let Err(rb) = guard.execute_batch("ROLLBACK") {
                    tracing::warn!("decisions import: ROLLBACK after error failed: {rb}");
                }
            }
        }
    }

    txn_result
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

    #[test]
    fn prompt_for_confirmation_returns_error_on_eof_before_input() {
        // P31: stdin closed (0-byte read) is distinguishable from "user
        // pressed Enter" (1-byte `\n`). The 0-byte case must surface a
        // typed CommandFailed error pointing the user at --yes for
        // unattended use, not silently treat it as decline.
        let mut out: Vec<u8> = Vec::new();
        let mut input = std::io::Cursor::new(Vec::<u8>::new());
        let result = prompt_for_confirmation(&mut out, &mut input);
        match result {
            Err(CliError::CommandFailed { reason, .. }) => {
                assert!(
                    reason.contains("--yes"),
                    "EOF error must hint at --yes for unattended runs; got: {reason}"
                );
                assert!(
                    reason.contains("stdin"),
                    "EOF error must mention stdin so the user can debug; got: {reason}"
                );
            }
            other => panic!("expected CommandFailed on EOF, got: {other:?}"),
        }
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

    // ══════════════════════════════════════════════════════════════════
    // export_decisions_to_string / import_decisions_from_str (US-015)
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn export_decisions_to_string_empty_db_returns_empty_array() {
        let db = make_db();
        let json = export_decisions_to_string(&db).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert!(parsed.is_array());
        assert_eq!(parsed.as_array().unwrap().len(), 0);
        assert!(json.ends_with('\n'));
    }

    #[test]
    fn export_decisions_to_string_populated_db_returns_all_rows() {
        let db = make_db();
        populate(&db);
        let json = export_decisions_to_string(&db).unwrap();
        let parsed: Vec<DecisionJsonOwned> =
            serde_json::from_str(&json).expect("parses back into owned DTOs");
        assert_eq!(parsed.len(), 4);

        // Each row carries the wire-format enum strings (lowercase, SQL form).
        let states: Vec<&str> = parsed.iter().map(|d| d.state.as_str()).collect();
        for expected in ["approved", "rejected", "partial", "recorded"] {
            assert!(
                states.contains(&expected),
                "missing state {expected} in {states:?}"
            );
        }
    }

    #[test]
    fn import_decisions_from_str_inserts_into_empty_db() {
        let db_src = make_db();
        populate(&db_src);
        let json = export_decisions_to_string(&db_src).unwrap();

        let db_dst = make_db();
        let summary = import_decisions_from_str(&db_dst, &json, false).unwrap();
        assert_eq!(summary.total, 4);
        assert_eq!(summary.inserted, 4);
        assert_eq!(summary.updated, 0);
        assert_eq!(summary.skipped, 0);

        // All four rows landed in the destination DB.
        let dst_repo = SqliteDecisionRepository::new(db_dst.connection().clone());
        assert_eq!(dst_repo.list().unwrap().len(), 4);
    }

    #[test]
    fn import_decisions_from_str_empty_array_is_no_op() {
        let db = make_db();
        populate(&db);
        let summary = import_decisions_from_str(&db, "[]", false).unwrap();
        assert_eq!(summary.total, 0);
        assert_eq!(summary.inserted, 0);
        assert_eq!(summary.updated, 0);
        assert_eq!(summary.skipped, 0);

        // Existing rows untouched.
        let repo = SqliteDecisionRepository::new(db.connection().clone());
        assert_eq!(repo.list().unwrap().len(), 4);
    }

    #[test]
    fn import_decisions_from_str_updates_when_imported_is_newer() {
        let db = make_db();
        // Existing row at decided_at = 1_700_000_100.
        populate(&db);
        let repo = SqliteDecisionRepository::new(db.connection().clone());
        let before = repo.get_by_hash("aaaaaaaa1111").unwrap().unwrap();
        assert_eq!(before.state, DecisionState::Approved);

        // Build an import that flips the state and bumps decided_at.
        let newer = make_decision(
            "aaaaaaaa1111",
            "Use anyhow for error propagation (revised)",
            DecisionState::Rejected,
            "feature/x",
            1_800_000_000,
        );
        let json = serde_json::to_string(&[DecisionJson::from(&newer)]).unwrap();

        let summary = import_decisions_from_str(&db, &json, false).unwrap();
        assert_eq!(summary.total, 1);
        assert_eq!(summary.inserted, 0);
        assert_eq!(summary.updated, 1);
        assert_eq!(summary.skipped, 0);

        let after = repo.get_by_hash("aaaaaaaa1111").unwrap().unwrap();
        assert_eq!(after.state, DecisionState::Rejected);
        assert_eq!(after.decided_at, 1_800_000_000);
        assert_eq!(
            after.description,
            "Use anyhow for error propagation (revised)"
        );
    }

    #[test]
    fn import_decisions_from_str_skips_when_existing_is_newer() {
        let db = make_db();
        populate(&db);
        let repo = SqliteDecisionRepository::new(db.connection().clone());
        let before = repo.get_by_hash("aaaaaaaa1111").unwrap().unwrap();
        assert_eq!(before.decided_at, 1_700_000_100);
        assert_eq!(before.state, DecisionState::Approved);

        // Older imported row — must be silently skipped (incumbent kept).
        let older = make_decision(
            "aaaaaaaa1111",
            "STALE",
            DecisionState::Rejected,
            "old-branch",
            1_600_000_000, // older than the existing 1_700_000_100
        );
        let json = serde_json::to_string(&[DecisionJson::from(&older)]).unwrap();

        let summary = import_decisions_from_str(&db, &json, false).unwrap();
        assert_eq!(summary.total, 1);
        assert_eq!(summary.inserted, 0);
        assert_eq!(summary.updated, 0);
        assert_eq!(summary.skipped, 1);

        // Existing row unchanged.
        let after = repo.get_by_hash("aaaaaaaa1111").unwrap().unwrap();
        assert_eq!(after.decided_at, before.decided_at);
        assert_eq!(after.state, before.state);
        assert_eq!(after.description, before.description);
    }

    #[test]
    fn import_decisions_from_str_skips_on_equal_decided_at() {
        // Defensive: equal decided_at means neither row is "later" — keep the
        // incumbent so a round-trip of unchanged data doesn't churn updated_at
        // timestamps or trigger spurious writes.
        let db = make_db();
        populate(&db);
        let repo = SqliteDecisionRepository::new(db.connection().clone());
        let before = repo.get_by_hash("aaaaaaaa1111").unwrap().unwrap();

        let same = make_decision(
            "aaaaaaaa1111",
            "DIFFERENT",
            DecisionState::Rejected,
            "main",
            before.decided_at, // exactly equal
        );
        let json = serde_json::to_string(&[DecisionJson::from(&same)]).unwrap();

        let summary = import_decisions_from_str(&db, &json, false).unwrap();
        assert_eq!(summary.skipped, 1);
        assert_eq!(summary.updated, 0);
        let after = repo.get_by_hash("aaaaaaaa1111").unwrap().unwrap();
        assert_eq!(after.description, before.description);
        assert_eq!(after.state, before.state);
    }

    #[test]
    fn import_decisions_from_str_strict_fails_on_conflict() {
        let db = make_db();
        populate(&db); // includes hash aaaaaaaa1111

        let conflicting = make_decision(
            "aaaaaaaa1111",
            "newer description",
            DecisionState::Rejected,
            "main",
            1_900_000_000,
        );
        let json = serde_json::to_string(&[DecisionJson::from(&conflicting)]).unwrap();

        let err = import_decisions_from_str(&db, &json, true).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("strict mode"), "got: {msg}");
        assert!(
            msg.contains("aaaaaaaa1111"),
            "must list conflicting hash: {msg}"
        );

        // No writes happened: existing row is untouched.
        let repo = SqliteDecisionRepository::new(db.connection().clone());
        let after = repo.get_by_hash("aaaaaaaa1111").unwrap().unwrap();
        assert_eq!(after.state, DecisionState::Approved); // original
        assert_eq!(after.decided_at, 1_700_000_100); // original
    }

    #[test]
    fn import_decisions_from_str_strict_succeeds_when_no_conflict() {
        // A clean import on an empty target should succeed regardless of
        // --strict — strict only fires on hash collisions.
        let db_src = make_db();
        populate(&db_src);
        let json = export_decisions_to_string(&db_src).unwrap();

        let db_dst = make_db();
        let summary = import_decisions_from_str(&db_dst, &json, true).unwrap();
        assert_eq!(summary.inserted, 4);
        assert_eq!(summary.updated, 0);
        assert_eq!(summary.skipped, 0);
    }

    #[test]
    fn import_decisions_from_str_strict_lists_all_conflicts() {
        let db = make_db();
        populate(&db);

        // Build a payload where two of the four hashes conflict and one is new.
        let conflict_a = make_decision(
            "aaaaaaaa1111",
            "x",
            DecisionState::Approved,
            "main",
            1_900_000_000,
        );
        let conflict_b = make_decision(
            "bbbbbbbb2222",
            "y",
            DecisionState::Rejected,
            "feature/x",
            1_900_000_000,
        );
        let new_one = make_decision(
            "ffffffff9999",
            "new",
            DecisionState::Recorded,
            "main",
            1_900_000_000,
        );
        let dtos = vec![
            DecisionJson::from(&conflict_a),
            DecisionJson::from(&conflict_b),
            DecisionJson::from(&new_one),
        ];
        let json = serde_json::to_string(&dtos).unwrap();

        let err = import_decisions_from_str(&db, &json, true).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("aaaaaaaa1111"),
            "missing first conflict: {msg}"
        );
        assert!(
            msg.contains("bbbbbbbb2222"),
            "missing second conflict: {msg}"
        );
        // The non-conflicting hash must NOT trigger an alarm.
        assert!(
            !msg.contains("ffffffff9999"),
            "non-conflicting hash leaked: {msg}"
        );

        // No partial writes — even the non-conflicting row is NOT inserted.
        let repo = SqliteDecisionRepository::new(db.connection().clone());
        assert!(repo.get_by_hash("ffffffff9999").unwrap().is_none());
    }

    #[test]
    fn import_decisions_from_str_invalid_json_returns_error() {
        let db = make_db();
        let err = import_decisions_from_str(&db, "{not json", false).unwrap_err();
        assert!(err.to_string().contains("failed to parse"), "{err}");
    }

    #[test]
    fn import_decisions_from_str_invalid_state_returns_error() {
        let db = make_db();
        // Hand-rolled JSON with a state value the V12 CHECK rejects.
        let json = r#"[{
            "description_hash": "abc",
            "description": "x",
            "state": "BOGUS",
            "nature": "convention",
            "weight": "rule",
            "category": null,
            "reason": null,
            "examples": [],
            "decided_on_branch": "main",
            "decided_at": 1,
            "updated_at": 1
        }]"#;

        let err = import_decisions_from_str(&db, json, false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid state"), "got: {msg}");
        assert!(msg.contains("abc"), "must mention offending hash: {msg}");
    }

    #[test]
    fn round_trip_export_then_import_yields_identical_table() {
        // AC #3: export → wipe → import → table identical.
        let db_src = make_db();
        populate(&db_src);
        let src_repo = SqliteDecisionRepository::new(db_src.connection().clone());
        let mut before = src_repo.list().unwrap();
        before.sort_by(|a, b| a.description_hash.cmp(&b.description_hash));

        // Export.
        let json = export_decisions_to_string(&db_src).unwrap();

        // "Wipe" by importing into a fresh DB — equivalent to deleting all rows
        // and re-importing in-place, but cleaner to assert against.
        let db_dst = make_db();
        let summary = import_decisions_from_str(&db_dst, &json, false).unwrap();
        assert_eq!(summary.total, 4);
        assert_eq!(summary.inserted, 4);

        // Read back and compare row-by-row, sorted on hash for stable order.
        let dst_repo = SqliteDecisionRepository::new(db_dst.connection().clone());
        let mut after = dst_repo.list().unwrap();
        after.sort_by(|a, b| a.description_hash.cmp(&b.description_hash));

        assert_eq!(before.len(), after.len());
        for (b, a) in before.iter().zip(after.iter()) {
            // PartialEq on Decision compares every field including timestamps,
            // so this is the strongest "table identical" assertion available.
            assert_eq!(b, a, "round-trip mismatch on hash {}", b.description_hash);
        }
    }

    #[test]
    fn round_trip_in_place_wipe_then_import_yields_identical_table() {
        // Stronger variant of the AC: do the wipe in-place (delete all rows
        // from the same DB) so the only thing remaining is what import wrote.
        let db = make_db();
        populate(&db);
        let repo = SqliteDecisionRepository::new(db.connection().clone());
        let mut before = repo.list().unwrap();
        before.sort_by(|a, b| a.description_hash.cmp(&b.description_hash));

        let json = export_decisions_to_string(&db).unwrap();

        // Delete every row.
        for d in &before {
            repo.delete(&d.description_hash).unwrap();
        }
        assert!(
            repo.list().unwrap().is_empty(),
            "wipe should clear the table"
        );

        // Import — every row is "new" again because we deleted everything.
        let summary = import_decisions_from_str(&db, &json, false).unwrap();
        assert_eq!(summary.inserted, before.len());
        assert_eq!(summary.skipped, 0);
        assert_eq!(summary.updated, 0);

        let mut after = repo.list().unwrap();
        after.sort_by(|a, b| a.description_hash.cmp(&b.description_hash));
        assert_eq!(before, after);
    }

    #[test]
    fn decision_json_owned_into_decision_round_trips_via_export_format() {
        // Defensive: confirm DecisionJsonOwned is a faithful inverse of
        // DecisionJson. If a new field is added to one but not the other,
        // the round-trip equality breaks first here.
        let original = make_decision(
            "h1",
            "Use anyhow",
            DecisionState::Approved,
            "main",
            1_700_000_000,
        );
        let json = serde_json::to_string(&DecisionJson::from(&original)).unwrap();
        let parsed: DecisionJsonOwned = serde_json::from_str(&json).unwrap();
        let restored = parsed.into_decision().unwrap();
        assert_eq!(original, restored);
    }
}
