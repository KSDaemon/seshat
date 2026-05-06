//! `seshat debug-snippets` command — dumps every convention from the DB
//! with full evidence for offline inspection.
//!
//! Hidden CLI command (registered with `#[command(hide = true)]`); no
//! stable user-facing contract. Used to bisect snippet-extraction
//! regressions on real repositories.

use std::path::Path;

use serde::Deserialize;

use crate::error::CliError;

/// One evidence row as deserialised from `nodes.ext_data` JSON.
///
/// `snippet` lives under two historical shapes:
/// - bare string (legacy)
/// - `{ "content": "..." }` object (current `CodeSnippet`)
///
/// `Snippet`'s custom Deserialize collapses both into a single String.
#[derive(Debug, Deserialize)]
struct EvidenceRow {
    #[serde(default)]
    file: String,
    #[serde(default)]
    line: u64,
    #[serde(default)]
    end_line: u64,
    #[serde(default)]
    snippet: Snippet,
    #[serde(default)]
    snippet_start_line: u64,
}

/// Wrapper that accepts both the bare-string and the
/// `{"content": "..."}` snippet shapes the DB has carried at various
/// schema versions.
#[derive(Debug, Default)]
struct Snippet(String);

impl Snippet {
    fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Snippet {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Either {
            Bare(String),
            Object { content: String },
        }
        match Either::deserialize(d)? {
            Either::Bare(s) => Ok(Snippet(s)),
            Either::Object { content } => Ok(Snippet(content)),
        }
    }
}

/// Top-level shape of `nodes.ext_data` we care about — only the
/// evidence array. Other fields are ignored.
#[derive(Debug, Default, Deserialize)]
struct ExtData {
    #[serde(default)]
    evidence: Vec<EvidenceRow>,
}

/// One DB row that survives the SELECT projection.
struct NodeRow {
    description: String,
    nature: String,
    weight: String,
    confidence: f64,
    adoption_count: u32,
    total_count: u32,
    ext_data: Option<String>,
}

pub fn run_debug(db_path: &Path, branch_id: &str) -> Result<(), CliError> {
    let conn = rusqlite::Connection::open(db_path).map_err(|e| CliError::CommandFailed {
        command: "debug".to_owned(),
        reason: format!("failed to open database: {e}"),
    })?;

    let sql = "
        SELECT description, nature, weight, confidence,
               adoption_count, total_count, ext_data
        FROM nodes
        WHERE nature IN ('convention', 'observation')
          AND branch_id = ?1
          AND (json_extract(ext_data, '$.user_rejected') IS NULL
               OR json_extract(ext_data, '$.user_rejected') != 1)
          AND (json_extract(ext_data, '$.source') IS NULL
               OR json_extract(ext_data, '$.source') != 'user')
        ORDER BY confidence DESC
    ";

    let mut stmt = conn.prepare(sql).map_err(|e| CliError::CommandFailed {
        command: "debug".to_owned(),
        reason: e.to_string(),
    })?;

    let rows = stmt
        .query_map(rusqlite::params![branch_id], |row| {
            Ok(NodeRow {
                description: row.get(0)?,
                nature: row.get(1)?,
                weight: row.get(2)?,
                confidence: row.get(3)?,
                adoption_count: row.get(4)?,
                total_count: row.get(5)?,
                ext_data: row.get(6)?,
            })
        })
        .map_err(|e| CliError::CommandFailed {
            command: "debug".to_owned(),
            reason: e.to_string(),
        })?;

    // A single malformed row used to abort the entire dump via `?`.
    // The whole point of debug-snippets is "show what's there"; one
    // bad ext_data shouldn't blank the rest of the report.
    for (idx, row_result) in rows.enumerate() {
        let row = match row_result {
            Ok(r) => r,
            Err(e) => {
                eprintln!("  [warn] row {} skipped: {e}", idx + 1);
                continue;
            }
        };
        print_node(idx + 1, &row);
    }

    Ok(())
}

fn print_node(idx: usize, row: &NodeRow) {
    let conf_pct = (row.confidence.clamp(0.0, 1.0) * 100.0).round() as u32;
    let adoption_pct = if row.total_count > 0 {
        ((f64::from(row.adoption_count) / f64::from(row.total_count)) * 100.0).round() as u32
    } else {
        0
    };

    println!(
        "═══ {idx}/─ {desc} ═══ {nature}|{weight}|{conf_pct}% | {adopt}/{total} ({adoption_pct}%)",
        desc = row.description,
        nature = row.nature,
        weight = row.weight,
        adopt = row.adoption_count,
        total = row.total_count,
    );

    let ext: ExtData = row
        .ext_data
        .as_deref()
        .and_then(|s| match serde_json::from_str(s) {
            Ok(d) => Some(d),
            Err(e) => {
                eprintln!("  [warn] malformed ext_data: {e}");
                None
            }
        })
        .unwrap_or_default();

    if ext.evidence.is_empty() {
        println!("  [no evidence]");
        return;
    }

    for (ei, item) in ext.evidence.iter().enumerate() {
        print_evidence(ei, item);
    }
}

fn print_evidence(ei: usize, item: &EvidenceRow) {
    let file = if item.file.is_empty() {
        "?"
    } else {
        item.file.as_str()
    };
    let line = u32::try_from(item.line).unwrap_or(u32::MAX);
    let end_line = u32::try_from(if item.end_line == 0 {
        item.line
    } else {
        item.end_line
    })
    .unwrap_or(u32::MAX);
    let snippet_start_line = u32::try_from(item.snippet_start_line).unwrap_or(0);
    let snippet = item.snippet.as_str();

    println!(
        "  [{ei}] {file}  line={line}..{end_line}  ssl={snippet_start_line}  snippet_len={}",
        snippet.len(),
    );
    if snippet.is_empty() {
        return;
    }
    for (li, l) in snippet.lines().enumerate() {
        let actual_line = if snippet_start_line > 0 {
            snippet_start_line as usize + li
        } else {
            line as usize + li
        };
        let marker = if actual_line >= line as usize && actual_line <= end_line as usize {
            ">>>"
        } else {
            "   "
        };
        let numbered_line = actual_line + 1;
        println!("    {marker} {numbered_line:>4} | {l}");
    }
}
