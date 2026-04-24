use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::error::CliError;

pub fn run_review(db_path: Option<PathBuf>) -> Result<(), CliError> {
    let git_root = crate::db::find_git_root(std::path::Path::new(".")).ok_or_else(|| {
        CliError::CommandFailed {
            command: "review".to_owned(),
            reason: "not in a git repository".to_owned(),
        }
    })?;

    let db_path = match db_path {
        Some(path) => path,
        None => crate::db::resolve_db_path(&git_root)?,
    };

    if !db_path.exists() {
        return Err(CliError::CommandFailed {
            command: "review".to_owned(),
            reason: "No database found. Run `seshat scan` first.".to_owned(),
        });
    }

    // Open a single connection shared between TUI read and batch-write.
    // This ensures the write-back uses the same snapshot as the read,
    // and allows wrapping all actions in a single transaction.
    let conn = Arc::new(Mutex::new(rusqlite::Connection::open(&db_path).map_err(
        |e| CliError::CommandFailed {
            command: "review".to_owned(),
            reason: format!("failed to open database: {e}"),
        },
    )?));

    let branch_id = crate::db::get_current_branch(&git_root).unwrap_or_else(|| "main".to_owned());

    let results = crate::tui::run_review_tui_with_conn(&db_path, &git_root, &conn, &branch_id)?;

    if !results.is_empty() {
        crate::tui::app::show_summary(&results);
    }

    Ok(())
}
