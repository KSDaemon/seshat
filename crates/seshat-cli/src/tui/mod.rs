pub mod app;
pub mod review_wizard;
pub mod widgets;

use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::error::CliError;

/// Launch the interactive convention review TUI.
///
/// Opens its own database connection. For use when the caller does not
/// need a shared connection (e.g., standalone invocation).
pub fn run_review_tui(db_path: &Path, git_root: &Path) -> Result<Vec<app::ReviewAction>, CliError> {
    let conventions = app::query_conventions_for_review(db_path, git_root)?;

    if conventions.is_empty() {
        eprintln!("No conventions found to review.");
        return Ok(Vec::new());
    }

    let conn = Arc::new(Mutex::new(rusqlite::Connection::open(db_path).map_err(
        |e| CliError::CommandFailed {
            command: "review".to_owned(),
            reason: format!("failed to open database: {e}"),
        },
    )?));

    let branch_id = crate::db::get_current_branch(git_root).unwrap_or_else(|| "main".to_owned());

    let mut terminal = ratatui::init();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        review_wizard::run_app(&mut terminal, conventions, &conn, &branch_id)
    }));
    ratatui::restore();

    match result {
        Ok(Ok(r)) => {
            app::show_summary(&r);
            Ok(r)
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err(CliError::TuiError(
            "TUI panicked; terminal state has been restored".to_owned(),
        )),
    }
}

/// Launch the TUI using a pre-opened shared connection.
///
/// The connection is also passed to `apply_review_actions` so that all
/// reads and writes happen on the same snapshot inside a transaction.
pub fn run_review_tui_with_conn(
    db_path: &Path,
    git_root: &Path,
    conn: &Arc<Mutex<rusqlite::Connection>>,
    branch_id: &str,
) -> Result<Vec<app::ReviewAction>, CliError> {
    let conventions = app::query_conventions_for_review(db_path, git_root)?;

    if conventions.is_empty() {
        eprintln!("No conventions found to review.");
        return Ok(Vec::new());
    }

    let mut terminal = ratatui::init();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        review_wizard::run_app(&mut terminal, conventions, conn, branch_id)
    }));
    ratatui::restore();

    match result {
        Ok(Ok(r)) => {
            app::show_summary(&r);
            Ok(r)
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err(CliError::TuiError(
            "TUI panicked; terminal state has been restored".to_owned(),
        )),
    }
}
