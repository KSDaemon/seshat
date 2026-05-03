use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use ratatui::prelude::Widget;
use rusqlite::Connection;

use crate::error::CliError;

use super::app::{App, ReviewAction};
use super::{app, widgets};

pub fn run_app(
    terminal: &mut DefaultTerminal,
    conventions: Vec<super::app::ConventionItem>,
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
) -> Result<Vec<ReviewAction>, CliError> {
    let mut app = App::new(conventions);

    loop {
        terminal
            .draw(|frame| widgets::render(frame, &app))
            .map_err(|e| CliError::TuiError(e.to_string()))?;

        if app.quit {
            break;
        }

        if event::poll(Duration::from_millis(50)).unwrap_or(false) {
            match event::read() {
                Ok(Event::Key(k)) => {
                    if k.kind == KeyEventKind::Press || k.kind == KeyEventKind::Repeat {
                        if k.code == KeyCode::Char('c') && k.modifiers == KeyModifiers::CONTROL {
                            app.quit = true;
                        } else if k.code != KeyCode::Char('c')
                            || k.modifiers != KeyModifiers::CONTROL
                        {
                            let _ = handle_key(k.code, &mut app);
                        }
                    }
                }
                Ok(_) => { /* non-key event (resize, mouse) silently discarded */ }
                Err(_) => { /* read error — continue to next iteration */ }
            }
        }
    }

    // Apply review actions (same connection, same transaction)
    if !app.results.is_empty() {
        terminal
            .draw(|frame| {
                let area = frame.area();
                let msg = ratatui::widgets::Paragraph::new("  Saving...").block(
                    ratatui::widgets::Block::default()
                        .title(" Seshat Convention Review ")
                        .borders(ratatui::widgets::Borders::ALL),
                );
                msg.render(area, frame.buffer_mut());
            })
            .map_err(|e| CliError::TuiError(e.to_string()))?;

        app::apply_review_actions(conn, branch_id, &app.results)?;
    }

    Ok(app.results)
}

fn handle_key(key: KeyCode, app: &mut App) -> Result<(), CliError> {
    let has_filter = app.search_mode || app.filter_locked;

    if app.search_mode {
        match key {
            KeyCode::Enter => {
                app.lock_filter();
                return Ok(());
            }
            KeyCode::Esc => {
                app.cancel_search();
                return Ok(());
            }
            KeyCode::Backspace => {
                app.pop_search_char();
                return Ok(());
            }
            KeyCode::Char('/') => {
                return Ok(());
            }
            KeyCode::Char('y') | KeyCode::Char('n') | KeyCode::Char('p') | KeyCode::Char('s') => {
                app.push_search_char(match key {
                    KeyCode::Char(c) => c,
                    _ => unreachable!(),
                });
                return Ok(());
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.filtered_previous();
                return Ok(());
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.filtered_next();
                return Ok(());
            }
            KeyCode::Left | KeyCode::Char('a') if app.example_total() > 1 => {
                app.previous_example();
                return Ok(());
            }
            KeyCode::Right | KeyCode::Char('d') if app.example_total() > 1 => {
                app.next_example();
                return Ok(());
            }
            KeyCode::Char('q') => {
                app.push_search_char('q');
                return Ok(());
            }
            KeyCode::Char(c) => {
                app.push_search_char(c);
                return Ok(());
            }
            _ => return Ok(()),
        }
    }

    match key {
        KeyCode::Char('/') => {
            if app.filter_locked {
                app.cancel_search();
            }
            app.search_mode = true;
        }
        KeyCode::Char('y') if !has_filter => {
            if let Some(conv) = app.current() {
                app.results.push(ReviewAction::Confirm {
                    node_id: conv.node_id,
                    description: conv.description.clone(),
                    examples: conv.examples.clone(),
                });
                app.mark_acted_on(app.current_index);
                app.advance_to_next_unreviewed();
            }
        }
        KeyCode::Char('n') if !has_filter => {
            if let Some(conv) = app.current() {
                app.results.push(ReviewAction::Reject {
                    node_id: conv.node_id,
                    snapshot_hash: conv.snapshot_hash,
                });
                app.mark_acted_on(app.current_index);
                app.advance_to_next_unreviewed();
            }
        }
        KeyCode::Char('p') if !has_filter => {
            if let Some(conv) = app.current() {
                app.results.push(ReviewAction::Partial {
                    node_id: conv.node_id,
                    description: conv.description.clone(),
                    original_node_id: conv.node_id,
                });
                app.mark_acted_on(app.current_index);
                app.advance_to_next_unreviewed();
            }
        }
        KeyCode::Char('s') if !has_filter => {
            if let Some(conv) = app.current() {
                app.results.push(ReviewAction::Skip {
                    node_id: conv.node_id,
                });
                app.mark_acted_on(app.current_index);
                app.advance_to_next_unreviewed();
            }
        }
        KeyCode::Char('q') => {
            app.quit = true;
        }
        KeyCode::Esc if app.filter_locked => {
            app.cancel_search();
        }
        KeyCode::Esc => {
            app.quit = true;
        }
        KeyCode::Up | KeyCode::Char('k') if has_filter => {
            app.filtered_previous();
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.previous();
        }
        KeyCode::Down | KeyCode::Char('j') if has_filter => {
            app.filtered_next();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.next();
        }
        KeyCode::Left | KeyCode::Char('a') if app.example_total() > 1 => {
            app.previous_example();
        }
        KeyCode::Right | KeyCode::Char('d') if app.example_total() > 1 => {
            app.next_example();
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::ConventionItem;

    fn make_convention(id: i64, desc: &str) -> ConventionItem {
        ConventionItem {
            node_id: id,
            description: desc.to_owned(),
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
        }
    }

    #[test]
    fn handle_key_y_confirms() {
        let conventions = vec![make_convention(1, "test"), make_convention(2, "test2")];
        let mut app = App::new(conventions);
        handle_key(KeyCode::Char('y'), &mut app).unwrap();
        assert!(matches!(
            &app.results[0],
            ReviewAction::Confirm { node_id: 1, .. }
        ));
        assert_eq!(app.current_index, 1);
        assert!(app.is_acted_on(0));
        assert!(!app.is_acted_on(1));
    }

    #[test]
    fn handle_key_y_caches_description() {
        let conventions = vec![make_convention(1, "import grouping")];
        let mut app = App::new(conventions);
        handle_key(KeyCode::Char('y'), &mut app).unwrap();
        if let ReviewAction::Confirm { description, .. } = &app.results[0] {
            assert_eq!(description, "import grouping");
        } else {
            panic!("expected Confirm");
        }
    }

    #[test]
    fn handle_key_n_rejects() {
        let conventions = vec![make_convention(1, "test")];
        let mut app = App::new(conventions);
        handle_key(KeyCode::Char('n'), &mut app).unwrap();
        assert!(matches!(
            &app.results[0],
            ReviewAction::Reject { node_id: 1, .. }
        ));
    }

    #[test]
    fn handle_key_n_rejects_last_convention_auto_quits() {
        let conventions = vec![make_convention(1, "test")];
        let mut app = App::new(conventions);
        assert!(!app.quit);
        handle_key(KeyCode::Char('n'), &mut app).unwrap();
        assert!(app.quit);
        assert!(app.is_acted_on(0));
    }

    #[test]
    fn handle_key_n_caches_snapshot_hash() {
        let conventions = vec![make_convention(1, "test")];
        let mut app = App::new(conventions);
        app.conventions[0].snapshot_hash = 42;
        handle_key(KeyCode::Char('n'), &mut app).unwrap();
        if let ReviewAction::Reject { snapshot_hash, .. } = &app.results[0] {
            assert_eq!(*snapshot_hash, 42);
        } else {
            panic!("expected Reject");
        }
    }

    #[test]
    fn handle_key_p_partial() {
        let conventions = vec![make_convention(1, "test")];
        let mut app = App::new(conventions);
        handle_key(KeyCode::Char('p'), &mut app).unwrap();
        assert!(matches!(
            &app.results[0],
            ReviewAction::Partial { node_id: 1, .. }
        ));
    }

    #[test]
    fn handle_key_p_caches_original_node_id() {
        let conventions = vec![make_convention(42, "test")];
        let mut app = App::new(conventions);
        handle_key(KeyCode::Char('p'), &mut app).unwrap();
        if let ReviewAction::Partial {
            original_node_id, ..
        } = &app.results[0]
        {
            assert_eq!(*original_node_id, 42);
        } else {
            panic!("expected Partial");
        }
    }

    #[test]
    fn handle_key_s_skips() {
        let conventions = vec![make_convention(1, "test")];
        let mut app = App::new(conventions);
        handle_key(KeyCode::Char('s'), &mut app).unwrap();
        assert!(matches!(&app.results[0], ReviewAction::Skip { node_id: 1 }));
    }

    #[test]
    fn handle_key_q_quits() {
        let conventions = vec![make_convention(1, "test")];
        let mut app = App::new(conventions);
        assert!(!app.quit);
        handle_key(KeyCode::Char('q'), &mut app).unwrap();
        assert!(app.quit);
    }

    #[test]
    fn handle_key_esc_quits() {
        let conventions = vec![make_convention(1, "test")];
        let mut app = App::new(conventions);
        handle_key(KeyCode::Esc, &mut app).unwrap();
        assert!(app.quit);
    }

    #[test]
    fn handle_key_up_down_navigates() {
        let conventions = vec![make_convention(1, "a"), make_convention(2, "b")];
        let mut app = App::new(conventions);
        handle_key(KeyCode::Down, &mut app).unwrap();
        assert_eq!(app.current_index, 1);
        handle_key(KeyCode::Up, &mut app).unwrap();
        assert_eq!(app.current_index, 0);
    }

    #[test]
    fn handle_key_j_k_navigates() {
        let conventions = vec![make_convention(1, "a"), make_convention(2, "b")];
        let mut app = App::new(conventions);
        handle_key(KeyCode::Char('j'), &mut app).unwrap();
        assert_eq!(app.current_index, 1);
        handle_key(KeyCode::Char('k'), &mut app).unwrap();
        assert_eq!(app.current_index, 0);
    }

    #[test]
    fn review_complete_flag_set_at_last_item() {
        let conventions = vec![make_convention(1, "a"), make_convention(2, "b")];
        let mut app = App::new(conventions);
        assert!(!app.review_complete);

        handle_key(KeyCode::Down, &mut app).unwrap();
        assert!(app.review_complete);

        // Confirming at last item — since item at index 0 isn't acted on,
        // advance_to_next_unreviewed wraps to it (not auto-quit)
        handle_key(KeyCode::Char('y'), &mut app).unwrap();
        assert_eq!(app.current_index, 0);
        assert!(!app.quit);
    }

    #[test]
    fn handle_key_repeat_allowed() {
        let conventions = vec![make_convention(1, "a"), make_convention(2, "b")];
        let mut app = App::new(conventions);
        handle_key(KeyCode::Down, &mut app).unwrap();
        assert_eq!(app.current_index, 1);
    }

    #[test]
    fn advance_wraps_to_first_unreviewed() {
        let conventions = vec![
            make_convention(1, "a"),
            make_convention(2, "b"),
            make_convention(3, "c"),
        ];
        let mut app = App::new(conventions);
        assert_eq!(app.current_index, 0);

        // Skip item 0
        handle_key(KeyCode::Char('s'), &mut app).unwrap();
        assert_eq!(app.current_index, 1);
        assert!(app.is_acted_on(0));

        // Reject item 1
        handle_key(KeyCode::Char('n'), &mut app).unwrap();
        assert_eq!(app.current_index, 2);
        assert!(app.is_acted_on(1));

        // Confirm item 2 — wraps back to item 0 which was skipped, but 0 is already acted-on,
        // and so is 1, and so is 2 → auto quit
        handle_key(KeyCode::Char('y'), &mut app).unwrap();
        assert!(app.quit);
    }

    #[test]
    fn all_acted_on_triggers_quit() {
        let conventions = vec![make_convention(1, "a"), make_convention(2, "b")];
        let mut app = App::new(conventions);

        handle_key(KeyCode::Char('y'), &mut app).unwrap();
        assert!(!app.quit);
        assert_eq!(app.current_index, 1);

        handle_key(KeyCode::Char('n'), &mut app).unwrap();
        assert!(app.quit);
    }
}
