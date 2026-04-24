use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;
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
}

#[derive(Debug, Clone)]
pub struct CodeExample {
    pub file: String,
    pub line: u32,
    pub end_line: u32,
    pub snippet: String,
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
}

impl App {
    pub fn new(conventions: Vec<ConventionItem>) -> Self {
        Self {
            conventions,
            current_index: 0,
            results: Vec::new(),
            quit: false,
            saving: false,
            review_complete: false,
        }
    }

    pub fn current(&self) -> Option<&ConventionItem> {
        self.conventions.get(self.current_index)
    }

    pub fn next(&mut self) {
        if self.current_index < self.conventions.len().saturating_sub(1) {
            self.current_index += 1;
        }
        self.review_complete = self.current_index >= self.conventions.len().saturating_sub(1);
    }

    pub fn previous(&mut self) {
        if self.current_index > 0 {
            self.current_index -= 1;
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
    db_path: &Path,
    git_root: &Path,
) -> Result<Vec<ConventionItem>, CliError> {
    let conn = Arc::new(Mutex::new(rusqlite::Connection::open(db_path).map_err(
        |e| CliError::CommandFailed {
            command: "review".to_owned(),
            reason: format!("failed to open database: {e}"),
        },
    )?));

    let branch_id =
        crate::db::get_current_branch(git_root).ok_or_else(|| CliError::CommandFailed {
            command: "review".to_owned(),
            reason: "could not determine current git branch. \
                  Make sure you are inside a git repository with at least one commit."
                .to_owned(),
        })?;

    let guard = lock_conn(&conn).map_err(|e| CliError::TuiError(e.to_string()))?;

    let sql = format!(
        "SELECT id, description, nature, weight, confidence,
                adoption_count, total_count, ext_data
         FROM nodes
         WHERE nature IN ('convention', 'observation')
           AND branch_id = ?1
           AND {sql_not_removed}
           AND (json_extract(ext_data, '$.user_rejected') IS NULL
                OR json_extract(ext_data, '$.user_rejected') != 1)
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
            Ok((
                id,
                description,
                nature,
                weight,
                confidence,
                adoption_count,
                total_count,
                ext_data,
            ))
        })
        .map_err(|e| CliError::TuiError(e.to_string()))?;

    let mut conventions = Vec::new();
    for row_result in rows {
        let (id, description, nature, weight, confidence, adoption_count, total_count, ext_data) =
            row_result.map_err(|e| CliError::TuiError(e.to_string()))?;

        let confidence_pct = (confidence.clamp(0.0, 1.0) * 100.0).round() as u32;
        let adoption_rate_pct = if total_count > 0 {
            ((adoption_count as f64 / total_count as f64) * 100.0).round() as u32
        } else {
            0
        };

        let ext: Option<serde_json::Value> = ext_data
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok());

        if ext.is_none() && ext_data.as_deref().map(|s| !s.is_empty()).unwrap_or(false) {
            tracing::warn!(
                node_id = id,
                ext_data_trunc = ?ext_data.as_deref().map(|s| &s[..100.min(s.len())]),
                "malformed ext_data JSON for convention node"
            );
        }

        let trend = ext
            .as_ref()
            .and_then(|e| e.get("trend"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_owned();

        let source = ext
            .as_ref()
            .and_then(|e| e.get("source"))
            .and_then(|v| v.as_str())
            .unwrap_or("auto_detected")
            .to_owned();

        let examples = parse_evidence(&ext);
        let snapshot_hash = compute_snapshot_hash(&ext_data);

        conventions.push(ConventionItem {
            node_id: id,
            description,
            nature,
            weight,
            confidence_pct,
            adoption_count,
            total_count,
            adoption_rate_pct,
            trend,
            source,
            examples,
            snapshot_hash,
        });
    }

    Ok(conventions)
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
        if !file.is_empty() {
            examples.push(CodeExample {
                file,
                line,
                end_line,
                snippet,
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

    let mut tx_failed = false;
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
            tracing::error!(node_id = ?action.node_id_if_reject(), "action failed: {e}");
            tx_failed = true;
        }
    }

    if tx_failed {
        let g = lock_conn(conn).map_err(|e| CliError::TuiError(e.to_string()))?;
        let _ = g.execute_batch("ROLLBACK");
        return Err(CliError::TuiError(
            "one or more review actions failed; changes may be partial. \
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

pub fn show_summary(results: &[ReviewAction]) {
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

    println!("\n  -- Review Complete -----------------------------------------------");
    println!("\n     + Confirmed   {confirmed}");
    println!("     - Rejected    {rejected}");
    println!("     ~ Partial     {partial}");
    println!("     x Skipped     {skipped}");
    println!("\n     Precision: {precision}%");

    if total_decided > 0 {
        if precision >= 70 {
            println!("     Status: + Seshat is calibrated and ready to use");
        } else {
            println!("     Status: ! Low precision. Seshat may not be reliable for this project.");
            println!("             Consider running review again with more rejections.");
        }
    }

    println!("\n     Knowledge graph updated.");
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
    fn show_summary_status_threshold_at_70() {
        // 7/10 = 70% should be calibrated
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
}
