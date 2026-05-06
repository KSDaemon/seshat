pub mod app;
pub mod review_wizard;
pub mod widgets;

use std::sync::{Arc, Mutex};

use crate::error::CliError;

/// Launch the interactive convention review TUI using a shared connection.
///
/// The connection is also passed to `apply_review_actions` so that all
/// reads and writes happen on the same snapshot inside a transaction.
pub fn run_review_tui_with_conn(
    branch_id: &str,
    conn: &Arc<Mutex<rusqlite::Connection>>,
) -> Result<Vec<app::ReviewAction>, CliError> {
    let (conventions, _queried_branch_id) = app::query_conventions_for_review(conn, branch_id)?;

    if conventions.is_empty() {
        eprintln!("No conventions found to review.");
        return Ok(Vec::new());
    }

    let convention_count = conventions.len();
    let mut terminal = ratatui::init();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        review_wizard::run_app(&mut terminal, conventions, conn, branch_id)
    }));
    ratatui::restore();

    let already_confirmed = app::count_confirmed_conventions(conn, branch_id);

    match result {
        Ok(Ok(r)) => {
            app::show_summary(
                &r,
                &app::SummaryContext {
                    total_in_scope: convention_count,
                    already_confirmed,
                },
            );
            Ok(r)
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err(CliError::TuiError(
            "TUI panicked; terminal state has been restored".to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_empty_db() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        // The decisions table mirrors V12's primary-key column so the LEFT
        // JOIN in `query_conventions_for_review` resolves cleanly.
        conn.execute_batch(
            "CREATE TABLE nodes (
                id INTEGER PRIMARY KEY,
                description TEXT,
                nature TEXT,
                weight TEXT,
                confidence REAL,
                adoption_count INTEGER,
                total_count INTEGER,
                ext_data TEXT,
                description_hash TEXT,
                branch_id TEXT,
                removed INTEGER DEFAULT 0
            );
            CREATE TABLE edges (
                id INTEGER PRIMARY KEY,
                source_node_id INTEGER,
                target_node_id INTEGER,
                edge_type TEXT,
                ext_data TEXT,
                branch_id TEXT,
                removed INTEGER DEFAULT 0
            );
            CREATE TABLE fts_index (
                content TEXT,
                node_id INTEGER,
                branch_id TEXT
            );
            CREATE TABLE decisions (
                description_hash TEXT NOT NULL PRIMARY KEY,
                description TEXT NOT NULL,
                state TEXT NOT NULL,
                nature TEXT NOT NULL,
                weight TEXT NOT NULL,
                category TEXT,
                reason TEXT,
                examples TEXT,
                decided_on_branch TEXT NOT NULL,
                decided_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );",
        )
        .unwrap();
        Arc::new(Mutex::new(conn))
    }

    #[test]
    fn empty_conventions_returns_empty_vec() {
        let conn = setup_empty_db();
        let result = run_review_tui_with_conn("main", &conn);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn error_from_query_is_propagated() {
        let conn = setup_empty_db();
        // Drop the table so the query fails
        {
            let guard = conn.lock().unwrap();
            guard.execute_batch("DROP TABLE nodes").unwrap();
        }
        let result = run_review_tui_with_conn("main", &conn);
        assert!(result.is_err());
    }
}
