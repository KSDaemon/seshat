use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};

use rusqlite::params;
use seshat_core::NodeId;
use seshat_graph::{SQL_NOT_REMOVED, lock_conn};

use crate::error::CliError;

#[derive(Debug, Clone)]
pub struct ConventionItem {
    pub node_id: i64,
    pub description: String,
    pub nature: String,
    pub weight: String,
    pub confidence_pct: u32,
    pub adoption_count: u32,
    pub total_count: u32,
    pub adoption_rate_pct: u32,
    pub trend: String,
    pub source: String,
    pub examples: Vec<CodeExample>,
    /// SHA256-style snapshot hash of ext_data at query time.
    /// Used for optimistic concurrency check on reject.
    pub snapshot_hash: u64,
    /// Index into `examples` vector for left/right cycling.
    pub example_index: usize,
    /// SHA256 hash of normalized description for deduplication.
    pub description_hash: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CodeExample {
    pub file: String,
    pub line: u32,
    pub end_line: u32,
    pub snippet: String,
    pub snippet_start_line: u32,
}

#[derive(Debug, Clone)]
pub enum ReviewAction {
    Confirm {
        node_id: i64,
        description: String,
        examples: Vec<CodeExample>,
    },
    Reject {
        node_id: i64,
        snapshot_hash: u64,
    },
    Partial {
        node_id: i64,
        description: String,
        original_node_id: i64,
    },
    Skip {
        node_id: i64,
    },
}

pub struct App {
    pub conventions: Vec<ConventionItem>,
    pub current_index: usize,
    pub results: Vec<ReviewAction>,
    pub quit: bool,
    pub saving: bool,
    pub review_complete: bool,
    /// Tracks which convention indices have already been acted on (y/n/p/s).
    acted_on: Vec<bool>,
    pub search_mode: bool,
    pub search_query: String,
    pub filter_locked: bool,
    pub filtered_indices: Vec<usize>,
}

impl App {
    pub fn new(conventions: Vec<ConventionItem>) -> Self {
        let len = conventions.len();
        let filtered: Vec<usize> = (0..len).collect();
        Self {
            conventions,
            current_index: 0,
            results: Vec::new(),
            quit: false,
            saving: false,
            review_complete: false,
            acted_on: vec![false; len],
            search_mode: false,
            search_query: String::new(),
            filter_locked: false,
            filtered_indices: filtered,
        }
    }

    pub fn filtered_current_index(&self) -> usize {
        self.filtered_indices
            .iter()
            .position(|&i| i == self.current_index)
            .unwrap_or(0)
    }

    pub fn filtered_total(&self) -> usize {
        self.filtered_indices.len()
    }

    pub fn filtered_current(&self) -> Option<&ConventionItem> {
        self.current()
    }

    pub fn filtered_next(&mut self) {
        if let Some(pos) = self
            .filtered_indices
            .iter()
            .position(|&i| i == self.current_index)
        {
            if pos + 1 < self.filtered_indices.len() {
                self.current_index = self.filtered_indices[pos + 1];
            }
        }
    }

    pub fn filtered_previous(&mut self) {
        if let Some(pos) = self
            .filtered_indices
            .iter()
            .position(|&i| i == self.current_index)
        {
            if pos > 0 {
                self.current_index = self.filtered_indices[pos - 1];
            }
        }
    }

    fn rebuild_filtered_indices(&mut self) {
        let query = self.search_query.to_lowercase();
        let previous = self.current_index;
        self.filtered_indices = (0..self.conventions.len())
            .filter(|&i| {
                self.conventions
                    .get(i)
                    .map(|c| c.description.to_lowercase())
                    .map(|desc| fuzzy_match(&query, &desc))
                    .unwrap_or(false)
            })
            .collect();

        if self.filtered_indices.contains(&previous) {
            return;
        }
        if let Some(first_match) = self.filtered_indices.first().copied() {
            self.current_index = first_match;
        }
    }

    pub fn push_search_char(&mut self, ch: char) {
        self.search_query.push(ch);
        self.rebuild_filtered_indices();
    }

    pub fn pop_search_char(&mut self) {
        self.search_query.pop();
        if self.search_query.is_empty() {
            self.cancel_search();
        } else {
            self.rebuild_filtered_indices();
        }
    }

    pub fn lock_filter(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }
        self.filter_locked = true;
        self.search_mode = false;
    }

    pub fn cancel_search(&mut self) {
        self.search_query.clear();
        self.search_mode = false;
        self.filter_locked = false;
        self.filtered_indices = (0..self.conventions.len()).collect();
        if !self.filtered_indices.is_empty() {
            self.current_index = self.filtered_indices[0];
        }
    }

    pub fn mark_acted_on(&mut self, index: usize) {
        if index < self.acted_on.len() {
            self.acted_on[index] = true;
        }
    }

    pub fn is_acted_on(&self, index: usize) -> bool {
        self.acted_on.get(index).copied().unwrap_or(true)
    }

    pub fn all_acted_on(&self) -> bool {
        self.acted_on.iter().all(|&b| b)
    }

    /// Advance to the next un-reviewed convention.
    /// Searches forward from current position, then wraps to the start.
    /// If all conventions have been reviewed, sets `quit = true`.
    pub fn advance_to_next_unreviewed(&mut self) {
        let total = self.conventions.len();
        if total == 0 {
            self.quit = true;
            return;
        }

        for offset in 1..=total {
            let idx = (self.current_index + offset) % total;
            if !self.acted_on[idx] {
                self.current_index = idx;
                if let Some(conv) = self.conventions.get_mut(self.current_index) {
                    conv.example_index = 0;
                }
                self.review_complete = false;
                return;
            }
        }

        self.quit = true;
    }

    pub fn current(&self) -> Option<&ConventionItem> {
        self.conventions.get(self.current_index)
    }

    pub fn example_total(&self) -> usize {
        self.current().map(|c| c.examples.len()).unwrap_or(0)
    }

    pub fn next_example(&mut self) {
        let total = self.example_total();
        if total <= 1 {
            return;
        }
        if let Some(c) = self.current() {
            let idx = c.example_index;
            let new_idx = (idx + 1) % total;
            if let Some(conv) = self.conventions.get_mut(self.current_index) {
                conv.example_index = new_idx;
            }
        }
    }

    pub fn previous_example(&mut self) {
        let total = self.example_total();
        if total <= 1 {
            return;
        }
        if let Some(c) = self.current() {
            let idx = c.example_index;
            let new_idx = if idx == 0 { total - 1 } else { idx - 1 };
            if let Some(conv) = self.conventions.get_mut(self.current_index) {
                conv.example_index = new_idx;
            }
        }
    }

    pub fn next(&mut self) {
        if self.current_index < self.conventions.len().saturating_sub(1) {
            self.current_index += 1;
            if let Some(conv) = self.conventions.get_mut(self.current_index) {
                conv.example_index = 0;
            }
        }
        self.review_complete = self.current_index >= self.conventions.len().saturating_sub(1);
    }

    pub fn previous(&mut self) {
        if self.current_index > 0 {
            self.current_index -= 1;
            if let Some(conv) = self.conventions.get_mut(self.current_index) {
                conv.example_index = 0;
            }
        }
        self.review_complete = self.current_index >= self.conventions.len().saturating_sub(1);
    }

    pub fn total(&self) -> usize {
        self.conventions.len()
    }
}

fn compute_snapshot_hash(ext_data: &Option<String>) -> u64 {
    let mut hasher = DefaultHasher::default();
    ext_data.as_deref().unwrap_or("").hash(&mut hasher);
    hasher.finish()
}

pub fn query_conventions_for_review(
    conn: &Arc<Mutex<rusqlite::Connection>>,
    branch_id: &str,
) -> Result<(Vec<ConventionItem>, String), CliError> {
    let guard = lock_conn(conn).map_err(|e| CliError::TuiError(e.to_string()))?;

    let sql = format!(
        "SELECT id, description, nature, weight, confidence,
                adoption_count, total_count, ext_data, description_hash
         FROM nodes
         WHERE nature IN ('convention', 'observation')
           AND branch_id = ?1
           AND {sql_not_removed}
           AND (json_extract(ext_data, '$.user_rejected') IS NULL
                OR json_extract(ext_data, '$.user_rejected') != 1)
           AND (json_extract(ext_data, '$.source') IS NULL
                OR json_extract(ext_data, '$.source') != 'user')
           AND (description_hash IS NULL
                OR description_hash NOT IN (
                    SELECT description_hash FROM nodes
                    WHERE branch_id = ?1
                      AND description_hash IS NOT NULL
                      AND json_extract(ext_data, '$.source') = 'user'
                      AND {sql_not_removed}
                ))
         ORDER BY confidence DESC",
        sql_not_removed = SQL_NOT_REMOVED
    );

    let mut stmt = guard
        .prepare(&sql)
        .map_err(|e| CliError::TuiError(e.to_string()))?;

    let rows = stmt
        .query_map(params![branch_id], |row| {
            let id: i64 = row.get(0)?;
            let description: String = row.get(1)?;
            let nature: String = row.get(2)?;
            let weight: String = row.get(3)?;
            let confidence: f64 = row.get(4)?;
            let adoption_count: u32 = row.get(5)?;
            let total_count: u32 = row.get(6)?;
            let ext_data: Option<String> = row.get(7)?;
            let description_hash: Option<String> = row.get(8)?;
            Ok((
                id,
                description,
                nature,
                weight,
                confidence,
                adoption_count,
                total_count,
                ext_data,
                description_hash,
            ))
        })
        .map_err(|e| CliError::TuiError(e.to_string()))?;

    let mut conventions = Vec::new();

    for row_result in rows {
        let (
            id,
            description,
            nature,
            weight,
            confidence,
            adoption_count,
            total_count,
            ext_data,
            description_hash,
        ) = row_result.map_err(|e| CliError::TuiError(e.to_string()))?;

        let ext: Option<serde_json::Value> = ext_data
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok());

        let source = ext
            .as_ref()
            .and_then(|e| e.get("source"))
            .and_then(|v| v.as_str())
            .unwrap_or("auto_detected")
            .to_owned();
        let trend = ext
            .as_ref()
            .and_then(|e| e.get("trend"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_owned();
        let examples = parse_evidence(&ext);

        conventions.push(ConventionItem {
            node_id: id,
            description,
            nature,
            weight,
            confidence_pct: (confidence.clamp(0.0, 1.0) * 100.0).round() as u32,
            adoption_count,
            total_count,
            adoption_rate_pct: if total_count > 0 {
                ((adoption_count as f64 / total_count as f64) * 100.0).round() as u32
            } else {
                0
            },
            trend,
            source: source.clone(),
            examples,
            snapshot_hash: compute_snapshot_hash(&ext_data),
            description_hash,
            example_index: 0,
        });
    }

    Ok((conventions, branch_id.to_string()))
}

/// Count user-confirmed conventions on the current branch that exist in the DB.
/// These are nodes with source='user' that have not been removed.
pub fn count_confirmed_conventions(
    conn: &Arc<Mutex<rusqlite::Connection>>,
    branch_id: &str,
) -> usize {
    let guard = match lock_conn(conn) {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!("failed to lock connection for count_confirmed_conventions: {e}");
            return 0;
        }
    };
    let sql = format!(
        "SELECT COUNT(*) FROM nodes
          WHERE branch_id = ?1
            AND {sql_not_removed}
            AND json_extract(ext_data, '$.source') = 'user'",
        sql_not_removed = SQL_NOT_REMOVED
    );
    guard
        .query_row(&sql, params![branch_id], |row| row.get::<_, i64>(0))
        .unwrap_or(0) as usize
}

fn parse_evidence(ext: &Option<serde_json::Value>) -> Vec<CodeExample> {
    let evidence = match ext
        .as_ref()
        .and_then(|e| e.get("evidence"))
        .and_then(|v| v.as_array())
    {
        Some(arr) => arr,
        None => return Vec::new(),
    };

    let mut examples = Vec::new();
    for item in evidence {
        let file = item
            .get("file")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let line = item.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let end_line = item
            .get("end_line")
            .and_then(|v| v.as_u64())
            .unwrap_or(line as u64) as u32;
        let snippet = item
            .get("snippet")
            .and_then(|v| {
                v.get("content")
                    .and_then(|c| c.as_str())
                    .or_else(|| v.as_str())
            })
            .unwrap_or("")
            .to_owned();
        let snippet_start_line = item
            .get("snippet_start_line")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        if !file.is_empty() {
            examples.push(CodeExample {
                file,
                line,
                end_line,
                snippet,
                snippet_start_line,
            });
        }
    }
    examples
}

pub fn apply_review_actions(
    conn: &Arc<Mutex<rusqlite::Connection>>,
    branch_id: &str,
    results: &[ReviewAction],
) -> Result<(), CliError> {
    if results.is_empty() {
        return Ok(());
    }

    {
        let guard = lock_conn(conn).map_err(|e| CliError::TuiError(e.to_string()))?;
        guard
            .execute_batch("BEGIN")
            .map_err(|e| CliError::TuiError(format!("BEGIN transaction: {e}")))?;
    }

    let mut fail_count = 0usize;
    for action in results {
        if let Err(e) = match action {
            ReviewAction::Confirm {
                node_id,
                description,
                examples,
            } => confirm_convention(conn, *node_id, branch_id, description, examples),
            ReviewAction::Reject {
                node_id,
                snapshot_hash,
            } => reject_convention(conn, *node_id, *snapshot_hash),
            ReviewAction::Partial {
                node_id,
                description,
                original_node_id,
            } => partial_convention(conn, *node_id, branch_id, description, *original_node_id),
            ReviewAction::Skip { .. } => Ok(()),
        } {
            tracing::warn!(node_id = ?action.node_id_if_reject(), "action skipped: {e}");
            fail_count += 1;
        }
    }

    if fail_count > 0 && fail_count == results.len() {
        let g = lock_conn(conn).map_err(|e| CliError::TuiError(e.to_string()))?;
        let _ = g.execute_batch("ROLLBACK");
        return Err(CliError::TuiError(
            "all review actions failed; no changes applied. \
             Run `seshat review` again to retry."
                .to_owned(),
        ));
    }

    seshat_graph::rebuild_fts_index(conn).map_err(|e| CliError::TuiError(e.to_string()))?;

    {
        let g = lock_conn(conn).map_err(|e| CliError::TuiError(e.to_string()))?;
        g.execute_batch("COMMIT")
            .map_err(|e| CliError::TuiError(format!("COMMIT transaction: {e}")))?;
    }

    if fail_count > 0 {
        tracing::info!(
            fail_count,
            success_count = results.len() - fail_count,
            "some actions skipped, rest committed"
        );
    }

    Ok(())
}

trait ReviewActionDebug {
    fn node_id_if_reject(&self) -> Option<i64>;
}

impl ReviewActionDebug for ReviewAction {
    fn node_id_if_reject(&self) -> Option<i64> {
        match self {
            ReviewAction::Confirm { node_id, .. }
            | ReviewAction::Reject { node_id, .. }
            | ReviewAction::Partial { node_id, .. }
            | ReviewAction::Skip { node_id } => Some(*node_id),
        }
    }
}

fn confirm_convention(
    conn: &Arc<Mutex<rusqlite::Connection>>,
    _node_id: i64,
    branch_id: &str,
    description: &str,
    examples: &[CodeExample],
) -> Result<(), CliError> {
    let converted_examples: Vec<seshat_graph::decisions::ExampleInput> = examples
        .iter()
        .map(|e| seshat_graph::decisions::ExampleInput {
            file: e.file.clone(),
            line: e.line,
            end_line: e.end_line,
            snippet: e.snippet.clone(),
        })
        .collect();

    seshat_graph::record_decision(
        conn,
        branch_id,
        seshat_graph::RecordDecisionParams {
            description: description.to_owned(),
            nature: "convention".to_owned(),
            weight: "strong".to_owned(),
            category: None,
            examples: converted_examples,
            reason: Some("Confirmed via seshat review TUI".to_owned()),
        },
    )
    .map_err(|e| CliError::TuiError(e.to_string()))?;
    Ok(())
}

fn reject_convention(
    conn: &Arc<Mutex<rusqlite::Connection>>,
    node_id: i64,
    expected_hash: u64,
) -> Result<(), CliError> {
    let (source, ext_data): (String, Option<String>) = {
        let guard = lock_conn(conn).map_err(|e| CliError::TuiError(e.to_string()))?;
        guard
            .query_row(
                "SELECT json_extract(ext_data, '$.source'), ext_data
                 FROM nodes WHERE id = ?1",
                params![node_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| CliError::TuiError(e.to_string()))?
    };

    // Optimistic concurrency: verify ext_data hasn't changed since we read it.
    let current_hash = compute_snapshot_hash(&ext_data);
    if current_hash != expected_hash {
        return Err(CliError::TuiError(format!(
            "convention {node_id} was modified during review; please retry"
        )));
    }

    if source == "user" {
        seshat_graph::remove_decision(
            conn,
            seshat_graph::RemoveDecisionParams {
                id: node_id,
                reason: "Rejected via seshat review TUI".to_owned(),
            },
        )
        .map_err(|e| CliError::TuiError(e.to_string()))?;
    } else {
        let now = chrono::Utc::now().timestamp();
        let mut ext: serde_json::Value = ext_data
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(serde_json::json!({}));
        ext["removed"] = serde_json::json!(1);
        ext["removed_reason"] = serde_json::json!("Rejected via seshat review TUI");
        ext["removed_at"] = serde_json::json!(now);
        ext["user_rejected"] = serde_json::json!(1);

        {
            let guard = lock_conn(conn).map_err(|e| CliError::TuiError(e.to_string()))?;
            guard
                .execute(
                    "UPDATE nodes SET ext_data = ?1 WHERE id = ?2",
                    params![ext.to_string(), node_id],
                )
                .map_err(|e| CliError::TuiError(e.to_string()))?;
        }
        seshat_graph::delete_fts_entry(conn, NodeId(node_id))
            .map_err(|e| CliError::TuiError(e.to_string()))?;
    }

    Ok(())
}

fn partial_convention(
    conn: &Arc<Mutex<rusqlite::Connection>>,
    _node_id: i64,
    branch_id: &str,
    description: &str,
    original_node_id: i64,
) -> Result<(), CliError> {
    seshat_graph::record_decision(
        conn,
        branch_id,
        seshat_graph::RecordDecisionParams {
            description: format!("Partial: {description} (refers to node {original_node_id})"),
            nature: "preference".to_owned(),
            weight: "strong".to_owned(),
            category: None,
            examples: Vec::new(),
            reason: Some("Partially confirmed via seshat review TUI".to_owned()),
        },
    )
    .map_err(|e| CliError::TuiError(e.to_string()))?;
    Ok(())
}

pub struct SummaryContext {
    /// Total conventions in the scope returned by the query (excludes already-confirmed and rejected).
    pub total_in_scope: usize,
    /// Number of conventions already confirmed on this branch before this session (from DB).
    pub already_confirmed: usize,
}

/// Display a rich summary with full session context: totals, per-session counts,
/// session precision, and overall coverage including already-confirmed from DB.
///
/// When the user presses q immediately: all session counts are 0, pending = total_in_scope,
/// precision = 0%, coverage = already_confirmed / (total_in_scope + already_confirmed) * 100.
pub fn show_summary(results: &[ReviewAction], context: &SummaryContext) {
    let confirmed = results
        .iter()
        .filter(|r| matches!(r, ReviewAction::Confirm { .. }))
        .count();
    let rejected = results
        .iter()
        .filter(|r| matches!(r, ReviewAction::Reject { .. }))
        .count();
    let partial = results
        .iter()
        .filter(|r| matches!(r, ReviewAction::Partial { .. }))
        .count();
    let skipped = results
        .iter()
        .filter(|r| matches!(r, ReviewAction::Skip { .. }))
        .count();

    let total_decided = confirmed.saturating_add(rejected).saturating_add(partial);

    let still_pending = context
        .total_in_scope
        .saturating_sub(total_decided)
        .saturating_sub(skipped);

    let precision_denom = total_decided.max(1);
    let session_precision = (confirmed as f64 / precision_denom as f64 * 100.0).round() as u32;

    let total_with_db = context
        .total_in_scope
        .saturating_add(context.already_confirmed);
    let overall_coverage = if total_with_db > 0 {
        let val = (context.already_confirmed.saturating_add(confirmed)) as f64
            / total_with_db as f64
            * 100.0;
        val.round() as u32
    } else {
        0
    };

    println!("\n   -- Review Complete ----------------------------------------------------------");
    println!(
        "   {:<24}  {:>4}",
        "Conventions in scope:", context.total_in_scope
    );
    println!(
        "   {:<24}  {:>4}",
        "Already confirmed (DB):", context.already_confirmed
    );
    println!();
    println!("   {:<24}  {:>4}", "+ Confirmed", confirmed);
    println!("   {:<24}  {:>4}", "- Rejected", rejected);
    println!("   {:<24}  {:>4}", "~ Partial", partial);
    println!("   {:<24}  {:>4}", "x Skipped", skipped);
    println!();
    println!("   {:<24}  {:>4}", "Still pending:", still_pending);
    println!("   {:<24}  {:>3}%", "Session precision:", session_precision);

    if session_precision >= 70 {
        println!(
            "   {:<24}  calibrated — detected conventions are well-aligned",
            "Precision diagnostic:"
        );
    } else {
        println!(
            "   {:<24}  low precision — consider re-reviewing flagged conventions",
            "Precision diagnostic:"
        );
    }

    println!("   {:<24}  {:>3}%", "Overall coverage:", overall_coverage);

    if context.already_confirmed > 0 || total_decided > 0 {
        println!("\n   Knowledge graph updated.");
    } else {
        println!("\n   No actions; graph unchanged.");
    }
}

fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let a_len = a_chars.len();
    let b_len = b_chars.len();

    if a_len == 0 {
        return b_len;
    }
    if b_len == 0 {
        return a_len;
    }

    let mut prev: Vec<usize> = (0..=b_len).collect();
    let mut curr = vec![0usize; b_len + 1];

    for i in 1..=a_len {
        curr[0] = i;
        for j in 1..=b_len {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b_len]
}

pub fn fuzzy_match(query: &str, candidate: &str) -> bool {
    if query.is_empty() {
        return true;
    }

    if candidate.contains(query) {
        return true;
    }

    let candidate_chars: Vec<char> = candidate.chars().collect();
    let query_len = query.chars().count();

    for window_len in query_len.saturating_sub(2)..=(query_len + 2).min(candidate_chars.len()) {
        if window_len == 0 {
            continue;
        }
        for i in 0..=candidate_chars.len().saturating_sub(window_len) {
            let window: String = candidate_chars[i..i + window_len].iter().collect();
            let dist = levenshtein_distance(query, &window);
            if dist <= 2 {
                return true;
            }
        }
    }

    candidate.to_lowercase().contains(&query.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compute_summary_stats(results: &[ReviewAction]) -> (usize, usize, usize, usize, u32) {
        let confirmed = results
            .iter()
            .filter(|r| matches!(r, ReviewAction::Confirm { .. }))
            .count();
        let rejected = results
            .iter()
            .filter(|r| matches!(r, ReviewAction::Reject { .. }))
            .count();
        let partial = results
            .iter()
            .filter(|r| matches!(r, ReviewAction::Partial { .. }))
            .count();
        let skipped = results
            .iter()
            .filter(|r| matches!(r, ReviewAction::Skip { .. }))
            .count();
        let total_decided = confirmed.saturating_add(rejected).saturating_add(partial);
        let precision = if total_decided > 0 {
            (confirmed as f64 / total_decided as f64 * 100.0).round() as u32
        } else {
            0
        };
        (confirmed, rejected, partial, skipped, precision)
    }

    #[test]
    fn code_example_uses_snippet_start_line_for_line_numbers() {
        // When snippet_start_line is non-zero it should be read from JSON and
        // stored on CodeExample so widgets can compute correct line numbers.
        let ext = Some(serde_json::json!({
            "evidence": [
                {
                    "file": "src/lib.rs",
                    "line": 10,
                    "end_line": 12,
                    "snippet": "fn context_line() {}\nfn target_fn() {\n    do_thing();\n}",
                    "snippet_start_line": 8
                }
            ]
        }));

        let examples = parse_evidence(&ext);
        assert_eq!(examples.len(), 1);
        let ex = &examples[0];
        assert_eq!(ex.snippet_start_line, 8);
        assert_eq!(ex.line, 10);

        // Verify fallback: when snippet_start_line is absent it defaults to 0
        let ext_no_start = Some(serde_json::json!({
            "evidence": [
                {
                    "file": "src/lib.rs",
                    "line": 5,
                    "end_line": 5,
                    "snippet": "let x = 1;"
                }
            ]
        }));
        let examples2 = parse_evidence(&ext_no_start);
        assert_eq!(examples2.len(), 1);
        assert_eq!(examples2[0].snippet_start_line, 0);
    }

    #[test]
    fn app_next_previous_bounds() {
        let conventions = vec![
            ConventionItem {
                node_id: 1,
                description: "A".to_owned(),
                nature: "convention".to_owned(),
                weight: "strong".to_owned(),
                confidence_pct: 90,
                adoption_count: 10,
                total_count: 10,
                adoption_rate_pct: 100,
                trend: "stable".to_owned(),
                source: "auto_detected".to_owned(),
                examples: Vec::new(),
                snapshot_hash: 0,
                description_hash: None,
                example_index: 0,
            },
            ConventionItem {
                node_id: 2,
                description: "B".to_owned(),
                nature: "convention".to_owned(),
                weight: "strong".to_owned(),
                confidence_pct: 80,
                adoption_count: 8,
                total_count: 10,
                adoption_rate_pct: 80,
                trend: "rising".to_owned(),
                source: "auto_detected".to_owned(),
                examples: Vec::new(),
                snapshot_hash: 0,
                description_hash: None,
                example_index: 0,
            },
        ];
        let mut app = App::new(conventions);

        assert_eq!(app.current_index, 0);
        assert!(!app.review_complete);

        app.previous();
        assert_eq!(app.current_index, 0);

        app.next();
        assert_eq!(app.current_index, 1);
        assert!(app.review_complete);

        app.next();
        assert_eq!(app.current_index, 1);
        assert!(app.review_complete);

        app.previous();
        assert_eq!(app.current_index, 0);
        assert!(!app.review_complete);
    }

    #[test]
    fn app_current_returns_none_when_empty() {
        let app = App::new(Vec::new());
        assert!(app.current().is_none());
        assert_eq!(app.total(), 0);
    }

    #[test]
    fn app_acted_on_tracking() {
        let conventions = vec![
            ConventionItem {
                node_id: 1,
                description: "A".to_owned(),
                nature: "convention".to_owned(),
                weight: "strong".to_owned(),
                confidence_pct: 90,
                adoption_count: 10,
                total_count: 10,
                adoption_rate_pct: 100,
                trend: "stable".to_owned(),
                source: "auto_detected".to_owned(),
                examples: Vec::new(),
                snapshot_hash: 0,
                description_hash: None,
                example_index: 0,
            },
            ConventionItem {
                node_id: 2,
                description: "B".to_owned(),
                nature: "convention".to_owned(),
                weight: "strong".to_owned(),
                confidence_pct: 80,
                adoption_count: 8,
                total_count: 10,
                adoption_rate_pct: 80,
                trend: "rising".to_owned(),
                source: "auto_detected".to_owned(),
                examples: Vec::new(),
                snapshot_hash: 0,
                description_hash: None,
                example_index: 0,
            },
            ConventionItem {
                node_id: 3,
                description: "C".to_owned(),
                nature: "convention".to_owned(),
                weight: "strong".to_owned(),
                confidence_pct: 70,
                adoption_count: 7,
                total_count: 10,
                adoption_rate_pct: 70,
                trend: "rising".to_owned(),
                source: "auto_detected".to_owned(),
                examples: Vec::new(),
                snapshot_hash: 0,
                description_hash: None,
                example_index: 0,
            },
        ];
        let mut app = App::new(conventions);

        assert!(!app.is_acted_on(0));
        assert!(!app.is_acted_on(1));
        assert!(!app.all_acted_on());

        app.mark_acted_on(0);
        assert!(app.is_acted_on(0));
        assert!(!app.is_acted_on(1));
        assert!(!app.all_acted_on());

        app.mark_acted_on(1);
        app.mark_acted_on(2);
        assert!(app.all_acted_on());
    }

    #[test]
    fn app_advance_to_next_unreviewed() {
        let conventions = vec![
            ConventionItem {
                node_id: 1,
                description: "A".to_owned(),
                nature: "convention".to_owned(),
                weight: "strong".to_owned(),
                confidence_pct: 90,
                adoption_count: 10,
                total_count: 10,
                adoption_rate_pct: 100,
                trend: "stable".to_owned(),
                source: "auto_detected".to_owned(),
                examples: Vec::new(),
                snapshot_hash: 0,
                description_hash: None,
                example_index: 0,
            },
            ConventionItem {
                node_id: 2,
                description: "B".to_owned(),
                nature: "convention".to_owned(),
                weight: "strong".to_owned(),
                confidence_pct: 80,
                adoption_count: 8,
                total_count: 10,
                adoption_rate_pct: 80,
                trend: "rising".to_owned(),
                source: "auto_detected".to_owned(),
                examples: Vec::new(),
                snapshot_hash: 0,
                description_hash: None,
                example_index: 0,
            },
            ConventionItem {
                node_id: 3,
                description: "C".to_owned(),
                nature: "convention".to_owned(),
                weight: "strong".to_owned(),
                confidence_pct: 70,
                adoption_count: 7,
                total_count: 10,
                adoption_rate_pct: 70,
                trend: "rising".to_owned(),
                source: "auto_detected".to_owned(),
                examples: Vec::new(),
                snapshot_hash: 0,
                description_hash: None,
                example_index: 0,
            },
        ];
        let mut app = App::new(conventions);

        // Start at index 0, advance wraps to 1
        app.mark_acted_on(0);
        app.advance_to_next_unreviewed();
        assert_eq!(app.current_index, 1);
        assert!(!app.quit);

        // Mark 1 as acted and advance wraps to 2
        app.mark_acted_on(1);
        app.advance_to_next_unreviewed();
        assert_eq!(app.current_index, 2);
        assert!(!app.quit);

        // Mark 2 as acted and advance wraps back to find 0, but 0 is also acted → all acted → quit
        app.mark_acted_on(2);
        app.advance_to_next_unreviewed();
        assert!(app.quit);
    }

    #[test]
    fn app_advance_skips_acted_items() {
        let conventions = vec![
            ConventionItem {
                node_id: 1,
                description: "A".to_owned(),
                nature: "convention".to_owned(),
                weight: "strong".to_owned(),
                confidence_pct: 90,
                adoption_count: 10,
                total_count: 10,
                adoption_rate_pct: 100,
                trend: "stable".to_owned(),
                source: "auto_detected".to_owned(),
                examples: Vec::new(),
                snapshot_hash: 0,
                description_hash: None,
                example_index: 0,
            },
            ConventionItem {
                node_id: 2,
                description: "B".to_owned(),
                nature: "convention".to_owned(),
                weight: "strong".to_owned(),
                confidence_pct: 80,
                adoption_count: 8,
                total_count: 10,
                adoption_rate_pct: 80,
                trend: "rising".to_owned(),
                source: "auto_detected".to_owned(),
                examples: Vec::new(),
                snapshot_hash: 0,
                description_hash: None,
                example_index: 0,
            },
            ConventionItem {
                node_id: 3,
                description: "C".to_owned(),
                nature: "convention".to_owned(),
                weight: "strong".to_owned(),
                confidence_pct: 70,
                adoption_count: 7,
                total_count: 10,
                adoption_rate_pct: 70,
                trend: "rising".to_owned(),
                source: "auto_detected".to_owned(),
                examples: Vec::new(),
                snapshot_hash: 0,
                description_hash: None,
                example_index: 0,
            },
        ];
        let mut app = App::new(conventions);

        // Act on 0, skip to 1 — but mark 1 as already acted. Should go to 2.
        app.mark_acted_on(0);
        app.mark_acted_on(1);
        app.current_index = 0;
        app.advance_to_next_unreviewed();
        assert_eq!(app.current_index, 2);
        assert!(!app.quit);
    }

    #[test]
    fn review_action_confirm() {
        let action = ReviewAction::Confirm {
            node_id: 42,
            description: "test".to_owned(),
            examples: Vec::new(),
        };
        assert!(matches!(action, ReviewAction::Confirm { node_id: 42, .. }));
    }

    #[test]
    fn review_action_reject() {
        let action = ReviewAction::Reject {
            node_id: 7,
            snapshot_hash: 12345,
        };
        assert!(matches!(action, ReviewAction::Reject { node_id: 7, .. }));
    }

    #[test]
    fn review_action_partial() {
        let action = ReviewAction::Partial {
            node_id: 3,
            description: "test".to_owned(),
            original_node_id: 3,
        };
        assert!(matches!(action, ReviewAction::Partial { node_id: 3, .. }));
    }

    #[test]
    fn review_action_skip() {
        let action = ReviewAction::Skip { node_id: 1 };
        assert!(matches!(action, ReviewAction::Skip { node_id: 1 }));
    }

    #[test]
    fn compute_snapshot_hash_consistent() {
        let ext = Some(r#"{"source":"auto_detected","trend":"stable"}"#.to_owned());
        let h1 = compute_snapshot_hash(&ext);
        let h2 = compute_snapshot_hash(&ext);
        assert_eq!(h1, h2);

        let ext2 = Some(r#"{"source":"auto_detected","trend":"rising"}"#.to_owned());
        let h3 = compute_snapshot_hash(&ext2);
        assert_ne!(h1, h3);
    }

    #[test]
    fn compute_snapshot_hash_null_is_consistent() {
        let h1 = compute_snapshot_hash(&None);
        let h2 = compute_snapshot_hash(&None);
        assert_eq!(h1, h2);
    }

    #[test]
    fn show_summary_empty_results() {
        let results: Vec<ReviewAction> = vec![];
        let (_confirmed, _rejected, _partial, _skipped, precision) =
            compute_summary_stats(&results);
        assert_eq!(precision, 0);
    }

    #[test]
    fn show_summary_all_confirmed() {
        let results = vec![
            ReviewAction::Confirm {
                node_id: 1,
                description: "A".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 2,
                description: "B".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 3,
                description: "C".to_owned(),
                examples: Vec::new(),
            },
        ];
        let (confirmed, rejected, partial, skipped, precision) = compute_summary_stats(&results);
        assert_eq!(confirmed, 3);
        assert_eq!(rejected, 0);
        assert_eq!(partial, 0);
        assert_eq!(skipped, 0);
        assert_eq!(precision, 100);
    }

    #[test]
    fn show_summary_mixed_decisions() {
        let results = vec![
            ReviewAction::Confirm {
                node_id: 1,
                description: "A".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Reject {
                node_id: 2,
                snapshot_hash: 0,
            },
            ReviewAction::Partial {
                node_id: 3,
                description: "C".to_owned(),
                original_node_id: 3,
            },
            ReviewAction::Skip { node_id: 4 },
        ];
        let (confirmed, rejected, partial, skipped, precision) = compute_summary_stats(&results);
        assert_eq!(confirmed, 1);
        assert_eq!(rejected, 1);
        assert_eq!(partial, 1);
        assert_eq!(skipped, 1);
        assert_eq!(precision, 33);
    }

    #[test]
    fn show_summary_high_precision_status() {
        let results = vec![
            ReviewAction::Confirm {
                node_id: 1,
                description: "A".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 2,
                description: "B".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Reject {
                node_id: 3,
                snapshot_hash: 0,
            },
        ];
        let (_confirmed, _rejected, _partial, _skipped, precision) =
            compute_summary_stats(&results);
        assert_eq!(precision, 67);
    }

    #[test]
    fn show_summary_low_precision_status() {
        let results = vec![
            ReviewAction::Confirm {
                node_id: 1,
                description: "A".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Reject {
                node_id: 2,
                snapshot_hash: 0,
            },
            ReviewAction::Reject {
                node_id: 3,
                snapshot_hash: 0,
            },
            ReviewAction::Reject {
                node_id: 4,
                snapshot_hash: 0,
            },
        ];
        let (confirmed, rejected, _partial, _skipped, precision) = compute_summary_stats(&results);
        assert_eq!(confirmed, 1);
        assert_eq!(rejected, 3);
        assert_eq!(precision, 25);
        assert!(precision < 70);
    }

    #[test]
    fn show_summary_only_skipped() {
        let results = vec![
            ReviewAction::Skip { node_id: 1 },
            ReviewAction::Skip { node_id: 2 },
        ];
        let (confirmed, rejected, partial, skipped, precision) = compute_summary_stats(&results);
        assert_eq!(confirmed, 0);
        assert_eq!(rejected, 0);
        assert_eq!(partial, 0);
        assert_eq!(skipped, 2);
        assert_eq!(precision, 0);
    }

    #[test]
    fn show_summary_all_rejected() {
        let results = vec![
            ReviewAction::Reject {
                node_id: 1,
                snapshot_hash: 0,
            },
            ReviewAction::Reject {
                node_id: 2,
                snapshot_hash: 0,
            },
        ];
        let (confirmed, rejected, _partial, _skipped, precision) = compute_summary_stats(&results);
        assert_eq!(confirmed, 0);
        assert_eq!(rejected, 2);
        assert_eq!(precision, 0);
    }

    #[test]
    fn show_summary_precision_rounding() {
        let results = vec![
            ReviewAction::Confirm {
                node_id: 1,
                description: "A".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 2,
                description: "B".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Reject {
                node_id: 3,
                snapshot_hash: 0,
            },
            ReviewAction::Reject {
                node_id: 4,
                snapshot_hash: 0,
            },
            ReviewAction::Reject {
                node_id: 5,
                snapshot_hash: 0,
            },
        ];
        let (confirmed, rejected, _partial, _skipped, precision) = compute_summary_stats(&results);
        assert_eq!(confirmed, 2);
        assert_eq!(rejected, 3);
        assert_eq!(precision, 40);
    }

    #[test]
    fn show_summary_status_threshold_below_70() {
        // 7/12 = 58.3% -> 58% should be low precision
        let results = vec![
            ReviewAction::Confirm {
                node_id: 1,
                description: "A".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 2,
                description: "B".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 3,
                description: "C".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 4,
                description: "D".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 5,
                description: "E".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 6,
                description: "F".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 7,
                description: "G".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Reject {
                node_id: 8,
                snapshot_hash: 0,
            },
            ReviewAction::Reject {
                node_id: 9,
                snapshot_hash: 0,
            },
            ReviewAction::Reject {
                node_id: 10,
                snapshot_hash: 0,
            },
            ReviewAction::Reject {
                node_id: 11,
                snapshot_hash: 0,
            },
            ReviewAction::Reject {
                node_id: 12,
                snapshot_hash: 0,
            },
        ];
        let (confirmed, rejected, _, _, precision) = compute_summary_stats(&results);
        // 7/12 = 58.3% -> 58%
        assert_eq!(confirmed, 7);
        assert_eq!(rejected, 5);
        assert_eq!(precision, 58);
        assert!(precision < 70);
    }

    #[test]
    fn show_summary_status_below_70() {
        // 6/9 = 66.7% -> 67% should be below calibrated
        let results = vec![
            ReviewAction::Confirm {
                node_id: 1,
                description: "A".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 2,
                description: "B".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 3,
                description: "C".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 4,
                description: "D".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 5,
                description: "E".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 6,
                description: "F".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Reject {
                node_id: 7,
                snapshot_hash: 0,
            },
            ReviewAction::Reject {
                node_id: 8,
                snapshot_hash: 0,
            },
            ReviewAction::Reject {
                node_id: 9,
                snapshot_hash: 0,
            },
        ];
        let (confirmed, rejected, _, _, precision) = compute_summary_stats(&results);
        // 6/9 = 66.7% -> 67%
        assert_eq!(confirmed, 6);
        assert_eq!(rejected, 3);
        assert_eq!(precision, 67);
        assert!(precision < 70);
    }

    #[test]
    fn fuzzy_match_exact_substring() {
        assert!(fuzzy_match("error", "error handling"));
        assert!(fuzzy_match("ERROR", "error handling"));
        assert!(fuzzy_match("log", "logging is done via tracing"));
    }

    #[test]
    fn fuzzy_match_fuzzy_typo() {
        assert!(fuzzy_match("err", "error handling"));
        assert!(fuzzy_match("loging", "logging"));
        assert!(fuzzy_match("handlng", "error handling"));
    }

    #[test]
    fn fuzzy_match_no_match() {
        assert!(!fuzzy_match("xyzzy", "error handling"));
        assert!(!fuzzy_match("completelydifferent", "error handling"));
    }

    #[test]
    fn fuzzy_match_empty_query_matches_all() {
        assert!(fuzzy_match("", "anything"));
        assert!(fuzzy_match("", ""));
    }

    #[test]
    fn levenshtein_distance_identical() {
        assert_eq!(levenshtein_distance("abc", "abc"), 0);
    }

    #[test]
    fn levenshtein_distance_one_substitution() {
        assert_eq!(levenshtein_distance("abc", "adc"), 1);
    }

    #[test]
    fn levenshtein_distance_empty() {
        assert_eq!(levenshtein_distance("", "abc"), 3);
        assert_eq!(levenshtein_distance("abc", ""), 3);
    }

    #[test]
    fn precision_all_confirmed() {
        let results: Vec<ReviewAction> = (0..10)
            .map(|i| ReviewAction::Confirm {
                node_id: i,
                description: "ok".to_owned(),
                examples: Vec::new(),
            })
            .collect();
        let (confirmed, rejected, _, _, precision) = compute_summary_stats(&results);
        assert_eq!(confirmed, 10);
        assert_eq!(rejected, 0);
        assert_eq!(precision, 100);
    }

    #[test]
    fn precision_all_rejected() {
        let results: Vec<ReviewAction> = (0..5)
            .map(|i| ReviewAction::Reject {
                node_id: i,
                snapshot_hash: 0,
            })
            .collect();
        let (confirmed, rejected, _, _, precision) = compute_summary_stats(&results);
        assert_eq!(confirmed, 0);
        assert_eq!(rejected, 5);
        assert_eq!(precision, 0);
    }

    #[test]
    fn precision_all_skipped() {
        let results: Vec<ReviewAction> =
            (0..5).map(|i| ReviewAction::Skip { node_id: i }).collect();
        let (confirmed, rejected, _, skipped, precision) = compute_summary_stats(&results);
        assert_eq!(confirmed, 0);
        assert_eq!(rejected, 0);
        assert_eq!(skipped, 5);
        assert_eq!(precision, 0);
    }

    #[test]
    fn show_summary_status_threshold_at_exactly_70() {
        let results = vec![
            ReviewAction::Confirm {
                node_id: 1,
                description: "A".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 2,
                description: "B".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 3,
                description: "C".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 4,
                description: "D".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 5,
                description: "E".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 6,
                description: "F".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 7,
                description: "G".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Reject {
                node_id: 8,
                snapshot_hash: 0,
            },
            ReviewAction::Reject {
                node_id: 9,
                snapshot_hash: 0,
            },
            ReviewAction::Reject {
                node_id: 10,
                snapshot_hash: 0,
            },
        ];
        let (confirmed, rejected, _, _, precision) = compute_summary_stats(&results);
        assert_eq!(confirmed, 7);
        assert_eq!(rejected, 3);
        assert_eq!(precision, 70);
    }

    #[test]
    fn show_summary_status_threshold_at_69() {
        let results = vec![
            ReviewAction::Confirm {
                node_id: 1,
                description: "A".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 2,
                description: "B".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 3,
                description: "C".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 4,
                description: "D".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 5,
                description: "E".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 6,
                description: "F".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 7,
                description: "G".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 8,
                description: "H".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: 9,
                description: "I".to_owned(),
                examples: Vec::new(),
            },
            ReviewAction::Reject {
                node_id: 10,
                snapshot_hash: 0,
            },
            ReviewAction::Reject {
                node_id: 11,
                snapshot_hash: 0,
            },
            ReviewAction::Reject {
                node_id: 12,
                snapshot_hash: 0,
            },
            ReviewAction::Reject {
                node_id: 13,
                snapshot_hash: 0,
            },
        ];
        let (confirmed, rejected, _, _, precision) = compute_summary_stats(&results);
        assert_eq!(confirmed, 9);
        assert_eq!(rejected, 4);
        assert_eq!(precision, 69);
        assert!(precision < 70);
    }
}
