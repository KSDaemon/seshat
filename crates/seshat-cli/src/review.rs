use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::error::CliError;

pub fn run_review(project_path: Option<PathBuf>) -> Result<(), CliError> {
    // Resolve the project — shared resolver also used by serve/status.
    let explicit = project_path.as_deref();
    let resolved = crate::db::resolve_project(explicit, "review")?;

    // Check that the database actually exists.
    if !resolved.db_path.exists() {
        return Err(CliError::CommandFailed {
            command: "review".to_owned(),
            reason: "No database found. Run `seshat scan` first.".to_owned(),
        });
    }

    // Determine branch once and pass it through to all downstream calls.
    let branch_id = crate::db::get_current_branch(&resolved.project_root).unwrap_or_else(|| {
        tracing::debug!(
           path = %resolved.project_root.display(),
            "Could not detect git branch, defaulting to 'main'"
        );
        "main".to_string()
    });

    // Open a single connection shared between TUI read and batch-write.
    // This ensures the write-back uses the same snapshot as the read,
    // and allows wrapping all actions in a single transaction.
    let conn = Arc::new(Mutex::new(
        rusqlite::Connection::open(&resolved.db_path).map_err(|e| CliError::CommandFailed {
            command: "review".to_owned(),
            reason: format!("failed to open database: {e}"),
        })?,
    ));

    let conventions =
        crate::tui::app::query_conventions_for_review(&conn, &branch_id).map_err(|e| {
            CliError::CommandFailed {
                command: "review".to_owned(),
                reason: format!("failed to query conventions: {e}"),
            }
        })?;
    let convention_count = conventions.0.len();

    let results = crate::tui::run_review_tui_with_conn(&branch_id, &conn)?;

    if !results.is_empty() {
        let already_confirmed = crate::tui::app::count_confirmed_conventions(&conn, &branch_id);
        crate::tui::app::show_summary(
            &results,
            &crate::tui::app::SummaryContext {
                total_in_scope: convention_count,
                already_confirmed,
            },
        );
    }

    Ok(())
}
