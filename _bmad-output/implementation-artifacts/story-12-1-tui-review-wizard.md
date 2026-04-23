# Story 12.1: TUI Review Wizard — Core Navigation & Actions

**Status:** ready-for-dev

**Epic:** 12 — Interactive Convention Review (TUI)

**FRs covered:** FR16 (confirm/reject/partial conventions), FR43 (`seshat review` TUI wizard)

**NFR covered:** NFR8 — Branch switch <2s (TUI is offline, no DB impact)

---

## Story

As a **developer**,
I want to interactively review conventions in a TUI,
so that I can calibrate Seshat's knowledge graph.

---

## Acceptance Criteria

1. **Given** a scanned project with conventions, **When** `seshat review` runs, **Then** a ratatui TUI renders with a bordered frame, title "Seshat Convention Review", and progress counter (e.g., "1/23").

2. **Given** the TUI is showing a convention, **Then** the convention card displays: name/description, nature (Convention/Fact/Observation), confidence percentage, weight (Strong/Moderate/Weak/Info), code example snippet, and adoption stats (e.g., "47/50 files (94% adoption)").

3. **Given** the TUI is active, **When** the user presses key bindings, **Then**:
   - `y` — Confirm convention → promote to Strong weight
   - `n` — Reject → demote to Observation (or remove)
   - `p` — Partial → mark as partially correct
   - `s` — Skip → no change, move to next
   - `↑` / `↓` — Navigate between conventions
   - `q` — Finish review → show summary

---

## Tasks / Subtasks

### Task 1: Add ratatui + crossterm dependencies

Add to workspace `Cargo.toml` and `crates/seshat-cli/Cargo.toml`:

```toml
# Workspace dependencies (Cargo.toml)
ratatui = "0.29"
crossterm = "0.28"

# seshat-cli/Cargo.toml dev-dependencies or regular dependencies
ratatui = { workspace = true }
crossterm = { workspace = true }
```

Also add `unicode-width` (ratatui transitive, but explicit is better for TUI).

---

### Task 2: Create TUI module structure

```
crates/seshat-cli/src/tui/
├── mod.rs              # Public API: run_review_tui()
├── app.rs              # App state, EventLoop, render dispatch
├── review_wizard.rs    # Main wizard component (convention card + key handling)
└── widgets.rs           # ConventionCard widget, styled borders
```

**`tui/mod.rs`** — Entry point:

```rust
pub mod app;
pub mod review_wizard;
pub mod widgets;

use crate::error::CliError;

/// Launch the interactive convention review TUI.
///
/// Queries conventions from the graph, then enters the ratatui event loop
/// where the user can navigate, confirm, reject, partial, or skip each one.
pub fn run_review_tui(db_path: &std::path::Path) -> Result<(), CliError>;
```

**`tui/app.rs`** — Application state and event loop:

```rust
use ratatui::DefaultTerminal;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};

/// Application state during the TUI session.
pub struct App {
    /// All conventions to review.
    pub conventions: Vec<ConventionItem>,
    /// Index of currently displayed convention.
    pub current_index: usize,
    /// Review results accumulated during the session.
    pub results: Vec<ReviewAction>,
    /// Whether the user wants to exit.
    pub quit: bool,
    /// Whether the TUI has just started (show first convention immediately).
    pub started: bool,
}

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
    Confirm { node_id: i64 },
    Reject { node_id: i64 },
    Partial { node_id: i64 },
    Skip { node_id: i64 },
}

impl App {
    pub fn new(conventions: Vec<ConventionItem>) -> Self {
        Self {
            conventions,
            current_index: 0,
            results: Vec::new(),
            quit: false,
            started: false,
        }
    }

    pub fn current(&self) -> Option<&ConventionItem> {
        self.conventions.get(self.current_index)
    }

    pub fn next(&mut self) {
        if self.current_index < self.conventions.len().saturating_sub(1) {
            self.current_index += 1;
        }
    }

    pub fn previous(&mut self) {
        if self.current_index > 0 {
            self.current_index -= 1;
        }
    }
}
```

**Main TUI render loop** (in `run_review_tui`):

```rust
pub fn run_review_tui(db_path: &std::path::Path) -> Result<(), CliError> {
    // 1. Query conventions from graph (non-TUI)
    let conventions = query_conventions_for_review(db_path)?;

    if conventions.is_empty() {
        eprintln!("No conventions found to review.");
        return Ok(());
    }

    // 2. Setup ratatui terminal
    let mut terminal = ratatui::init();
    let result = run_app(&mut terminal, conventions);
    ratatui::shutdown();

    result
}

fn run_app(terminal: &mut DefaultTerminal, conventions: Vec<ConventionItem>) -> Result<(), CliError> {
    let mut app = App::new(conventions);

    loop {
        terminal.draw(|frame| render(frame, &mut app))?;

        if app.quit {
            break;
        }

        // Non-blocking event poll
        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    handle_key(key.code, &mut app)?;
                }
            }
        }
    }

    // 3. Apply review actions to the knowledge graph
    apply_review_actions(&app.results)?;

    // 4. Show summary
    show_summary(&app.results);

    Ok(())
}
```

---

### Task 3: Implement ConventionCard widget (`tui/widgets.rs`)

Render the convention card exactly as specified in the UX design:

```
┌─ Seshat Convention Review ───────────────────────── 1/23 ─┐
│                                                            │
│  Import grouping: stdlib → external → internal             │
│                                                            │
│  Nature: Convention    Confidence: 93%    Weight: Strong    │
│                                                            │
│  Example (src/services/auth.ts:1):                         │
│  ┌────────────────────────────────────────────────────┐    │
│  │ import { readFile } from 'fs';                     │    │
│  │ import axios from 'axios';                         │    │
│  │ import { AuthService } from '../services';         │    │
│  └────────────────────────────────────────────────────┘    │
│                                                            │
│  Found in: 47/50 files (94% adoption)                      │
│                                                            │
├────────────────────────────────────────────────────────────┤
│  [y] Confirm   [n] Reject   [p] Partial   [s] Skip        │
│  [↑↓] Navigate   [q] Finish                              │
└────────────────────────────────────────────────────────────┘
```

**Implementation:**

```rust
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};

/// The main convention review card widget.
pub struct ConventionCard<'a> {
    pub convention: &'a ConventionItem,
    pub current: usize,
    pub total: usize,
}

impl Widget for ConventionCard<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Render outer bordered block with title
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(
                " Seshat Convention Review {:>width$}/{:<width$} ",
                self.current + 1,
                self.total,
                width = self.total.to_string().len()
            ))
            .style(Style::default().fg(Color::Cyan))
            .border_style(Style::default().fg(Color::Cyan));

        let inner = block.inner(area);
        block.render(area, buf);

        // Title line (already rendered by Block)

        // Convention description
        Paragraph::new(self.convention.description.clone()).render(inner, buf);

        // Metadata line: Nature | Confidence | Weight
        let meta = format!(
            "Nature: {:<13} Confidence: {:>3}%    Weight: {}",
            self.convention.nature,
            self.convention.confidence_pct,
            self.convention.weight
        );
        Paragraph::new(meta).render(inner, buf);

        // Code example with nested bordered block
        if let Some(example) = self.convention.examples.first() {
            let example_title = format!(" Example ({}:{}) ", example.file, example.line);
            // Render example title + code block
            let code_block = Block::default()
                .borders(Borders::ALL)
                .title(example_title)
                .style(Style::default().fg(Color::Yellow))
                .border_style(Style::default().fg(Color::Yellow));
            // ... render code_block and snippet
        }

        // Adoption stats
        let adoption = format!(
            "Found in: {}/{} files ({}% adoption)",
            self.convention.adoption_count,
            self.convention.total_count,
            self.convention.adoption_rate_pct
        );
        Paragraph::new(adoption).render(inner, buf);

        // Key bindings footer (rendered in a separate bottom area)
    }
}
```

**Key bindings footer** (rendered at the bottom of the screen):

```rust
pub fn render_key_bindings(buf: &mut Buffer, area: Rect) {
    let keys = Line::from(vec![
        Span::styled("[y] Confirm  ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled("[n] Reject   ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled("[p] Partial  ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled("[s] Skip    ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled("[↑↓] Navigate", Style::default().fg(Color::White)),
        Span::raw("  "),
        Span::styled("[q] Finish  ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
    ]);
    Paragraph::new(keys).render(area, buf);
}
```

---

### Task 4: Implement key handling (`tui/app.rs` / `tui/review_wizard.rs`)

```rust
use crossterm::event::KeyCode;
use crate::tui::app::App;
use crate::error::CliError;

fn handle_key(key: KeyCode, app: &mut App) -> Result<(), CliError> {
    match key {
        KeyCode::Char('y') => {
            if let Some(conv) = app.current() {
                app.results.push(ReviewAction::Confirm { node_id: conv.node_id });
                app.next();
            }
        }
        KeyCode::Char('n') => {
            if let Some(conv) = app.current() {
                app.results.push(ReviewAction::Reject { node_id: conv.node_id });
                app.next();
            }
        }
        KeyCode::Char('p') => {
            if let Some(conv) = app.current() {
                app.results.push(ReviewAction::Partial { node_id: conv.node_id });
                app.next();
            }
        }
        KeyCode::Char('s') => {
            if let Some(conv) = app.current() {
                app.results.push(ReviewAction::Skip { node_id: conv.node_id });
                app.next();
            }
        }
        KeyCode::Char('q') | KeyCode::Esc => {
            app.quit = true;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.previous();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.next();
        }
        _ => {}
    }
    Ok(())
}
```

---

### Task 5: Query conventions for review + Apply actions to graph + Persisted Rejection

**Query conventions** — fetch all conventions for the current branch from the graph layer:

```rust
use std::sync::{Arc, Mutex};
use rusqlite::{Connection, params};
use seshat_graph::lock_conn;

fn query_conventions_for_review(db_path: &std::path::Path) -> Result<Vec<tui::app::ConventionItem>, CliError> {
    let conn = Arc::new(Mutex::new(Connection::open(db_path)?));

    // Get current branch
    let branch_id = get_current_branch_from_db(&conn)?;

    // Query all convention nodes for this branch (not removed, not user-rejected)
    // IMPORTANT: exclude user_rejected nodes so they never appear in review again
    let conn_guard = lock_conn(&conn)?;
    let mut stmt = conn_guard.prepare(
        "SELECT id, description, nature, weight, confidence,
                adoption_count, total_count, ext_data
         FROM nodes
         WHERE nature IN ('convention', 'observation')
           AND branch_id = ?1
           AND json_extract(ext_data, '$.removed') IS NULL
           AND json_extract(ext_data, '$.removed') != 1
           AND json_extract(ext_data, '$.user_rejected') IS NULL
           AND json_extract(ext_data, '$.user_rejected') != 1
         ORDER BY confidence DESC"
    )?;

    let convention_iters = stmt.query_map(params![branch_id], |row| {
        Ok(tui::app::ConventionItem {
            node_id: row.get(0)?,
            description: row.get(1)?,
            nature: row.get(2)?,
            weight: row.get(3)?,
            confidence_pct: (row.get::<_, f64>(4)!.clamp(0.0, 1.0) * 100.0).round() as u32,
            adoption_count: row.get(5)?,
            total_count: row.get(6)?,
            adoption_rate_pct: 0, // computed below
            trend: String::from("unknown"), // from ext_data
            source: String::from("auto_detected"), // from ext_data
            examples: Vec::new(), // parsed from ext_data
        })
    })?;

    let mut conventions = Vec::new();
    for conv_result in convention_iters {
        let mut conv = conv_result?;
        if conv.total_count > 0 {
            conv.adoption_rate_pct = ((conv.adoption_count as f64 / conv.total_count as f64) * 100.0).round() as u32;
        }
        // Parse ext_data for trend, source, examples
        parse_ext_data(&mut conv);
        conventions.push(conv);
    }

    Ok(conventions)
}
```

**Apply review actions** — persist user decisions to the graph. This is the critical integration point.

For **Confirm (y)**: Create a user-recorded decision with weight="strong":

```rust
fn confirm_convention(conn: &Arc<Mutex<Connection>>, node_id: i64, description: &str) -> Result<(), CliError> {
    // record_decision creates a new node with source="user", weight="strong".
    // The old auto-detected node remains but the user decision takes precedence
    // in query results (user_confirmed=true in ext_data).
    //
    // Known limitation: old auto-detected node is NOT soft-deleted.
    // It still appears in query_convention but with source="auto_detected"
    // and user_confirmed=false. The user-decision node appears alongside
    // it with source="user" and user_confirmed=true.
    // TODO: in follow-up, soft-delete the old auto-detected node on confirm.
    seshat_graph::decisions::record_decision(
        &conn,
        &get_current_branch(&conn)?,
        seshat_graph::decisions::RecordDecisionParams {
            description: description.to_string(),
            nature: "convention".to_string(),
            weight: "strong".to_string(),
            category: None,
            examples: Vec::new(),
            reason: Some("Confirmed via seshat review TUI".to_string()),
        },
    )?;
    Ok(())
}
```

For **Reject (n)** — **with Persisted Rejection**: Soft-delete AND mark as user_rejected.
The `user_rejected` flag prevents the convention from being recreated on re-scan.

```rust
fn reject_convention(conn: &Arc<Mutex<Connection>>, node_id: i64) -> Result<(), CliError> {
    let conn_guard = lock_conn(&conn)?;

    // Check current source to handle user vs auto-detected nodes differently
    let source: String = conn_guard.query_row(
        "SELECT json_extract(ext_data, '$.source') FROM nodes WHERE id = ?1",
        params![node_id],
    )?;

    if source == "user" {
        // Use existing remove_decision for user nodes (it handles soft-delete + FTS)
        seshat_graph::decisions::remove_decision(
            &conn,
            seshat_graph::decisions::RemoveDecisionParams {
                id: node_id,
                reason: "Rejected via seshat review TUI".to_string(),
            },
        )?;
    } else {
        // For auto-detected nodes: soft-delete + mark user_rejected
        // user_rejected = 1 prevents re-creation during persist_conventions (detection.rs)
        let now = chrono::Utc::now().timestamp();
        conn_guard.execute(
            "UPDATE nodes SET ext_data = json_merge_patch(
                 ext_data,
                 '{\"removed\": 1, \"removed_reason\": \"Rejected via seshat review TUI\", \
                  \"removed_at\": ?, \"user_rejected\": 1}'
               )
             WHERE id = ?2",
            params![now, node_id],
        )?;

        // Remove from FTS5 so it doesn't appear in search results
        seshat_graph::fts::delete_fts_entry(&conn, seshat_core::NodeId(node_id))?;
    }

    Ok(())
}
```

For **Partial (p)**: Create a preference node:

```rust
fn partial_convention(conn: &Arc<Mutex<Connection>>, node_id: i64, description: &str) -> Result<(), CliError> {
    seshat_graph::decisions::record_decision(
        &conn,
        &get_current_branch(&conn)?,
        seshat_graph::decisions::RecordDecisionParams {
            description: format!("Partial: {}", description),
            nature: "preference".to_string(),
            weight: "strong".to_string(),
            category: None,
            examples: Vec::new(),
            reason: Some("Partially confirmed via seshat review TUI".to_string()),
        },
    )?;
    Ok(())
}
```

**Batch apply function** — called after TUI exits, applies all accumulated actions:

```rust
fn apply_review_actions(results: &[ReviewAction]) -> Result<(), CliError> {
    if results.is_empty() {
        return Ok(());
    }

    let conn = Arc::new(Mutex::new(Connection::open(get_db_path()?)?));

    for action in results {
        match action {
            ReviewAction::Confirm { node_id, .. } => {
                // Need description — fetch from DB
                let desc: String = conn.lock().unwrap().query_row(
                    "SELECT description FROM nodes WHERE id = ?1",
                    params![node_id],
                )?;
                confirm_convention(&conn, *node_id, &desc)?;
            }
            ReviewAction::Reject { node_id } => {
                reject_convention(&conn, *node_id)?;
            }
            ReviewAction::Partial { node_id, .. } => {
                let desc: String = conn.lock().unwrap().query_row(
                    "SELECT description FROM nodes WHERE id = ?1",
                    params![node_id],
                )?;
                partial_convention(&conn, *node_id, &desc)?;
            }
            ReviewAction::Skip { .. } => {}
        }
    }

    // Rebuild FTS5 index after batch changes
    seshat_graph::fts::rebuild_fts_index(&conn)?;

    Ok(())
}
```

**Persisted Rejection in `persist_conventions`** (detection.rs) — CRITICAL:
Modify the DELETE query in `persist_conventions` to NOT delete nodes with `user_rejected = 1`:

```rust
// In crates/seshat-graph/src/detection.rs, change the DELETE query:

// BEFORE (line 236-241):
guard.execute(
    "DELETE FROM nodes
     WHERE branch_id = ?1
       AND json_extract(ext_data, '$.source') = 'auto_detected'",
    rusqlite::params![branch_id.0],
)?;

// AFTER — add user_rejected filter:
guard.execute(
    "DELETE FROM nodes
     WHERE branch_id = ?1
       AND json_extract(ext_data, '$.source') = 'auto_detected'
       AND (json_extract(ext_data, '$.user_rejected') IS NULL
            OR json_extract(ext_data, '$.user_rejected') != 1)",
    rusqlite::params![branch_id.0],
)?;
```

This ensures that when `run_detection_cycle` runs on re-scan, rejected auto-detected nodes survive the DELETE phase and are never recreated.

**Summary display** (after review completes):

```rust
fn show_summary(results: &[ReviewAction]) {
    let confirmed = results.iter().filter(|r| matches!(r, ReviewAction::Confirm { .. })).count();
    let rejected = results.iter().filter(|r| matches!(r, ReviewAction::Reject { .. })).count();
    let partial = results.iter().filter(|r| matches!(r, ReviewAction::Partial { .. })).count();
    let skipped = results.iter().filter(|r| matches!(r, ReviewAction::Skip { .. })).count();

    let total_decided = confirmed + rejected + partial;
    let precision = if total_decided > 0 {
        (confirmed as f64 / total_decided as f64 * 100.0).round() as u32
    } else {
        0
    };

    println!("\n  ── Review Complete ───────────────────────────────────────────");
    println!("\n     ✓ Confirmed   {}", confirmed);
    println!("     ✗ Rejected     {}", rejected);
    println!("     ~ Partial      {}", partial);
    println!("     ⊘ Skipped      {}", skipped);
    println!("\n     Precision: {}%", precision);

    if total_decided > 0 {
        if precision >= 70 {
            println!("     Status: ✓ Seshat is calibrated and ready to use");
        } else {
            println!("     Status: ⚠ Low precision. Seshat may not be reliable for this project.");
            println!("             Consider running review again with more rejections.");
        }
    }

    println!("\n     Knowledge graph updated.");
}
```

---

### Task 6: Wire `seshat review` command

**`crates/seshat-cli/src/commands/review.rs`** (new file):

```rust
use std::path::PathBuf;
use crate::error::CliError;
use crate::db::{find_git_root, get_db_path};

/// Run the interactive convention review TUI.
pub fn run_review(db_path: Option<PathBuf>) -> Result<(), CliError> {
    // Resolve database path: use provided path or auto-detect from git root
    let db_path = match db_path {
        Some(path) => path,
        None => {
            let git_root = find_git_root()?;
            get_db_path(&git_root)?
        }
    };

    // Launch the TUI
    crate::tui::run_review_tui(&db_path)
}
```

**`crates/seshat-cli/src/lib.rs`** — Add tui module and wire the Review command:

```rust
// Add at top with other modules:
/// TUI components for interactive convention review.
pub mod tui;

// In run() function, replace the Review stub:
Command::Review => {
    review::run_review(None)
}
```

**`crates/seshat-cli/src/lib.rs`** — Add review module:

```rust
/// Implementation of the `seshat review` command.
pub mod review;
```

---

## Dev Notes

### Architecture Context

**What exists:**
- `Command::Review` variant in `args.rs:76` — **already defined**, no changes needed
- Stub handler in `lib.rs:82-85` — prints error and exits, **needs replacement**
- `NodeRepository::find_by_nature()` — **exists**, can query all convention nodes
- `NodeRepository::update()` — **exists**, for updating nodes
- `NodeRepository::delete()` — **exists**, for soft-deleting nodes
- `seshat_graph::decisions::record_decision()` — **exists**, creates user decisions
- `seshat_graph::decisions::update_decision()` — **exists**, updates user decisions
- `seshat_graph::decisions::remove_decision()` — **exists**, soft-deletes user decisions
- `seshat_graph::lock_conn()` — **exists**, for safe mutex access
- `seshat_graph::fts::delete_fts_entry()` — **exists**, for FTS cleanup
- `seshat_graph::fts::rebuild_fts_index()` — **exists**, call after batch changes
- `seshat_core::NodeId` — **exists**
- `seshat_core::truncate_snippet()` — **exists**
- `seshat_core::CodeSnippet` — **exists**

**What needs to be created:**
- `crates/seshat-cli/src/tui/` — entire TUI module (mod.rs, app.rs, review_wizard.rs, widgets.rs)
- `crates/seshat-cli/src/review.rs` — command handler
- `reject_convention()` — helper in review.rs or tui (handles both user and auto-detected nodes, sets user_rejected flag)
- `apply_review_actions()` — batch apply function (called after TUI exits)
- `query_conventions_for_review()` — fetch conventions for TUI (filters out user_rejected nodes)
- `persist_conventions` modification in `detection.rs` — add `user_rejected` filter to DELETE query

**Dependency note:** ratatui 0.29 + crossterm 0.28 need to be added to both workspace Cargo.toml and seshat-cli/Cargo.toml.

### Key design decisions

1. **TUI is a separate binary concern** — it runs entirely in seshat-cli, queries the graph layer for data, and writes back via graph layer functions. No direct SQL from TUI code.

2. **Confirm creates a new user-decision node** — `record_decision` creates a new node with `source="user"`, `weight="strong"`. The old auto-detected node remains but the user decision takes precedence (it appears in query results too). **Known limitation:** old auto-detected node is NOT soft-deleted on confirm. Both appear in query results. The user-decision has `user_confirmed=true` in ext_data which signals it was confirmed.

3. **Reject uses soft-delete + Persisted Rejection** — For user-recorded nodes: `remove_decision`. For auto-detected nodes: set `ext_data.removed=1` AND `ext_data.user_rejected=1`, then call `delete_fts_entry`. The `user_rejected` flag is checked in `persist_conventions` (detection.rs) — nodes with `user_rejected=1` are NOT deleted during the auto-detect cycle, preventing re-creation on re-scan.

4. **Partial creates a "preference" node** — A new user-decision with nature="preference" and a "Partial: " prefix on the description. The original convention node is preserved.

5. **Skip does nothing** — The convention stays as-is in the graph.

6. **Summary is CLI output, not TUI** — After the TUI exits, the summary is printed to stdout via `println!`. This avoids needing a TUI summary screen.

7. **Conventions sorted by confidence DESC** — Highest confidence first, so the user reviews the most impactful conventions first.

8. **Persisted Rejection mechanism** — When a user rejects a convention in the TUI, the node gets `ext_data.user_rejected=1`. The `persist_conventions` function in `detection.rs` has been modified to skip nodes with `user_rejected=1` during the DELETE phase. This means:
   - User rejects convention → node marked with `user_rejected=1`
   - Next scan runs `run_detection_cycle` → `persist_conventions` DELETE skips `user_rejected=1` nodes
   - Convention is NOT recreated → user never sees it again
   - FTS5 index updated → convention not found in search

9. **Only Convention and Observation are reviewable** — Fact, Decision, and Preference are NOT shown in TUI. Facts are objective (no review needed), Decision/Preference are user-created (already decided).

### What NOT to touch

- `crates/seshat-core/src/knowledge.rs` — **no changes needed** (all fields already exist)
- `crates/seshat-storage/src/repository/node_repository.rs` — **no changes needed**
- `crates/seshat-graph/src/conventions.rs` — **no changes needed** (query_convention already works)
- `crates/seshat-graph/src/decisions.rs` — **no changes needed** (record/update/remove_decision all work)
- `crates/seshat-graph/src/fts.rs` — **no changes needed** (delete_fts_entry + rebuild_fts_index already work)
- `crates/seshat-mcp/` — **no changes needed** (TUI calls graph functions directly, not via MCP)
- `crates/seshat-detectors/` — **no changes needed** (detection logic unchanged)
- `crates/seshat-scanner/` — **no changes needed**
- `args.rs` — **no changes needed** (Review variant already exists)
- Database migrations — **no changes needed** (uses existing nodes table + ext_data JSON)

### Edge cases

1. **No conventions to review** — If the project has no scanned conventions, print a message and exit cleanly (no TUI).

2. **Single convention** — TUI should work fine with 1 convention. User confirms/rejects and then quits.

3. **DB path resolution** — If no DB exists yet (project not scanned), show an error: "No database found. Run `seshat scan` first."

4. **Multi-branch** — Only show conventions for the current branch. Use the same branch detection as other commands.

5. **TUI not available (no terminal)** — If `crossterm` cannot initialize the terminal (e.g., piped output, CI), fall back to a simple text-based review or print an error.

6. **Very large convention sets (100+)** — The current design loads all conventions into memory. For now this is fine (typical projects have <50 conventions). If it becomes an issue, add pagination.

7. **Concurrent DB access** — The TUI reads from the DB, then writes back at the end. If another process modifies the DB during the TUI session, the write-back should handle conflicts gracefully (rusqlite will return an error).

8. **Reject → re-scan → NOT recreated (Persisted Rejection)** — This is the core mechanism. When user rejects:
   - Node gets `ext_data.user_rejected=1`
   - `persist_conventions` (detection.rs) DELETE query filters out `user_rejected=1` nodes
   - Convention is never recreated on re-scan
   - FTS5 updated so convention doesn't appear in search
   - **Test this!** This is the #1 regression risk.

9. **Confirm → old auto-detected node remains** — `record_decision` creates a new user-decision node. The old auto-detected node stays in DB. Both appear in `query_convention` results. The user-decision has `user_confirmed=true` which signals it was confirmed. **Known limitation, not a bug for 12.1.**

10. **Partial creates preference + original preserved** — Original convention node stays unchanged. New preference node is created with "Partial: " prefix. Both appear in query results.

11. **Observation shown in TUI** — Observations are auto-detected conventions with low confidence. They appear in the TUI with their nature field showing "observation". User can confirm (→ new user-decision with nature="convention", weight="strong"), reject (→ soft-delete + user_rejected), partial (→ preference), or skip.

12. **Double-confirm same convention** — Each confirm creates a new user-decision node. No deduplication. Multiple user-decision nodes for the same description will appear in query results. **Known limitation.**

13. **Reject then change mind** — No "unreject" mechanism. User must re-scan to recreate the convention (but with Persisted Rejection, it won't be recreated). To "unreject", the user needs to manually remove `user_rejected` from ext_data or we add an "unreject" action in a future story.

### File List

```
Cargo.toml                                      ← MODIFY: add ratatui, crossterm workspace deps
crates/seshat-cli/Cargo.toml                    ← MODIFY: add ratatui, crossterm dependencies
crates/seshat-cli/src/lib.rs                    ← MODIFY: add tui + review modules, wire Review command
crates/seshat-cli/src/review.rs                 ← CREATE: run_review() command handler
crates/seshat-cli/src/tui/mod.rs                ← CREATE: TUI module entry point
crates/seshat-cli/src/tui/app.rs                ← CREATE: App state, event loop, key handling
crates/seshat-cli/src/tui/widgets.rs            ← CREATE: ConventionCard widget, key bindings footer
crates/seshat-cli/src/tui/review_wizard.rs      ← CREATE: Wizard component (or merge into app.rs)
crates/seshat-graph/src/detection.rs            ← MODIFY: persist_conventions DELETE query (add user_rejected filter)
```

### Unit tests required

**In `crates/seshat-graph/src/detection.rs`**:
1. `persist_conventions_skips_user_rejected` — insert auto-detected node with `user_rejected=1`, run persist_conventions, verify node still exists
2. `persist_conventions_deletes_normal_auto_detected` — insert auto-detected node without `user_rejected`, run persist_conventions, verify node deleted

**In `crates/seshat-graph/src/decisions.rs`** (or new test file for TUI actions):
3. `reject_convention_marks_user_rejected` — call reject_convention on auto-detected node, verify `ext_data.user_rejected=1`
4. `reject_convention_removes_from_fts5` — call reject_convention, verify FTS5 search returns empty
5. `confirm_convention_creates_user_decision` — call confirm_convention, verify new user-decision node created
6. `partial_convention_creates_preference` — call partial_convention, verify new preference node created

**In TUI module tests** (`crates/seshat-cli/src/tui/`):
7. `app_next_previous_bounds` — navigate at boundaries, verify no panic
8. `handle_key_y_confirms` — press y, verify ReviewAction::Confirm pushed
9. `handle_key_n_rejects` — press n, verify ReviewAction::Reject pushed
10. `handle_key_q_quits` — press q, verify quit=true
11. `handle_key_up_down_navigates` — press ↑↓, verify current_index changes
12. `run_review_tui_empty_conventions_exits_gracefully` — no conventions, verify clean exit

### Integration test

13. **Reject → re-scan → NOT recreated** — This is the #1 integration test:
    - Insert auto-detected convention node with `user_rejected=1`
    - Run `run_detection_cycle`
    - Verify the convention node still exists (was NOT recreated, was NOT deleted)
    - Verify FTS5 does not contain the convention

### References

- UX Design: `_bmad-output/planning-artifacts/ux-design-specification.md#L139-L217` — `seshat review` TUI layout, key bindings, search mode, summary
- Architecture: `_bmad-output/planning-artifacts/architecture.md#L149` — ratatui + crossterm tech choice
- Architecture: `_bmad-output/planning-artifacts/architecture.md#L741-L743` — TUI module structure
- PRD: `_bmad-output/planning-artifacts/prd.md#L650` — FR16
- PRD: `_bmad-output/planning-artifacts/prd.md#L686` — FR43
- Epics: `_bmad-output/planning-artifacts/epics.md#L1571-L1591` — Epic 12, Story 12.1

---

## Dev Agent Record

### Agent Model Used

### Debug Log References

### Completion Notes List

### File List
