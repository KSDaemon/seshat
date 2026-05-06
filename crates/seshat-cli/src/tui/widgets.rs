use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

#[cfg(test)]
use super::app::CodeExample;
use super::app::{App, ConventionItem};

pub fn render(frame: &mut ratatui::Frame, app: &App) {
    let area = frame.area();

    let has_examples = app
        .current()
        .map(|c| !c.examples.is_empty())
        .unwrap_or(false);

    let has_filter = app.search_mode || app.filter_locked;

    if let Some(convention) = app.current() {
        let card = ConventionCard {
            convention,
            current: if has_filter {
                app.filtered_current_index()
            } else {
                app.current_index
            },
            total: if has_filter {
                app.filtered_total()
            } else {
                app.total()
            },
            review_complete: app.review_complete,
            has_examples,
            search_mode: app.search_mode,
            search_query: &app.search_query,
            filter_locked: app.filter_locked,
            no_match: false,
        };
        card.render(area, frame.buffer_mut());
    } else if !app.conventions.is_empty()
        && app.filtered_indices.is_empty()
        && (app.search_mode || app.filter_locked)
    {
        let card = ConventionCard {
            convention: &app.conventions[0],
            current: 0,
            total: 0,
            review_complete: false,
            has_examples: false,
            search_mode: app.search_mode,
            search_query: &app.search_query,
            filter_locked: app.filter_locked,
            no_match: true,
        };
        card.render(area, frame.buffer_mut());
    } else {
        Paragraph::new("No convention to display").render(area, frame.buffer_mut());
    }
}

pub struct ConventionCard<'a> {
    pub convention: &'a ConventionItem,
    pub current: usize,
    pub total: usize,
    pub review_complete: bool,
    has_examples: bool,
    search_mode: bool,
    search_query: &'a str,
    filter_locked: bool,
    no_match: bool,
}

impl ConventionCard<'_> {
    /// Build the example panel's title bar text.
    ///
    /// Three states, kept in one place so the previously-nested
    /// `if self.has_examples { if let Some(example) = ... { if N > 1 { ...
    /// } else ... } else ... } else ...` chain inside the `Span::styled`
    /// argument doesn't have to live mid-render.
    fn example_title(&self) -> String {
        if !self.has_examples {
            return "── (no usage examples) ".to_owned();
        }
        let Some(example) = self.convention.examples.get(self.convention.example_index) else {
            return "── (no usage examples) ".to_owned();
        };
        let example_num = self.convention.example_index + 1;
        let examples_count = self.convention.examples.len();
        // Composite evidence (file-level summaries from
        // aggregate_findings — "98 files match this convention" snippet)
        // has empty `file` and `line == 0`. Render a distinct
        // "── Summary " title without the bogus `(…:0)` suffix the
        // per-file branch would produce.
        if example.file.is_empty() && example.line == 0 {
            return if examples_count > 1 {
                format!("── Summary ({example_num}/{examples_count}) ")
            } else {
                "── Summary ".to_owned()
            };
        }
        let file_display = shorten_path(&example.file);
        let line = example.line;
        if examples_count > 1 {
            format!("── Example ({example_num}/{examples_count}): (\u{2026}{file_display}:{line}) ")
        } else {
            format!("── Example: (\u{2026}{file_display}:{line}) ")
        }
    }
}

impl Widget for ConventionCard<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let border_style = Style::default().fg(Color::Blue);
        let divider_set = symbols::border::Set {
            top_left: "├",
            top_right: "┤",
            ..symbols::border::PLAIN
        };
        let outer_block = Block::default()
            .borders(Borders::ALL)
            .title("── Seshat Convention Review ")
            .style(Style::default().fg(Color::Cyan))
            .border_style(border_style);
        let inner = outer_block.inner(area);
        outer_block.render(area, buf);

        if self.no_match {
            Paragraph::new("  No matching conventions")
                .style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
                .render(inner, buf);
            render_key_bindings(
                buf,
                Rect {
                    x: area.x,
                    y: area.height.saturating_sub(1),
                    width: area.width,
                    height: 1,
                },
                0,
            );
            return;
        }

        let has_search_bar = self.search_mode;

        // Fixed: header(1), div(1), info(3). Example fills rest. Fixed: div(1), ctrl(1). Optional: search_bar(1).
        let [header_height, info_height] = if self.filter_locked {
            [Constraint::Length(2), Constraint::Length(2)]
        } else {
            [Constraint::Length(1), Constraint::Length(3)]
        };

        let constraints: Vec<Constraint> = {
            let mut v = vec![header_height, Constraint::Length(1), info_height];
            v.push(Constraint::Min(2));
            v.push(Constraint::Length(1));
            v.push(Constraint::Length(1));
            v
        };

        let areas = Layout::vertical(&constraints).split(inner);
        let header_area = areas[0];
        let div1_area = areas[1];
        let info_area = areas[2];
        let example_area = areas[3];
        let div2_area = areas[4];
        let ctrl_area = areas[5];

        // Header: "    1/53: description" or "[filter: 'keyword']"
        if self.filter_locked {
            let filter_text = format!("  [filter: '{}']", self.search_query);
            Paragraph::new(filter_text)
                .style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .render(header_area, buf);

            let desc_text = format!(
                "  {}/{}: {}",
                self.current + 1,
                self.total,
                self.convention.description
            );
            Paragraph::new(desc_text)
                .style(
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
                .wrap(Wrap { trim: false })
                .render(
                    Rect {
                        y: header_area.y + 1,
                        height: 1,
                        ..header_area
                    },
                    buf,
                );
        } else {
            let desc_text = format!(
                "  {}/{}: {}",
                self.current + 1,
                self.total,
                self.convention.description
            );
            Paragraph::new(desc_text)
                .style(
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
                .wrap(Wrap { trim: false })
                .render(header_area, buf);
        }

        // ├────────────────────────────────────────────────────────────────────┤
        Block::default()
            .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT)
            .border_set(divider_set)
            .border_style(border_style)
            .render(
                Rect {
                    x: area.x,
                    y: div1_area.y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );

        // Info section: metadata + adoption (2-space indent)
        let weight_display = match self.convention.weight.as_str() {
            "rule" => "Rule",
            "strong" => "Strong",
            "moderate" => "Moderate",
            "weak" => "Weak",
            "info" => "Info",
            other => other,
        };
        let nature_display = match self.convention.nature.as_str() {
            "convention" => "Convention",
            "observation" => "Observation",
            other => other,
        };

        let meta = Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("Nature: {nature_display}"),
                Style::default().fg(Color::Green),
            ),
            Span::raw("       "),
            Span::styled(
                format!("Confidence: {}%", self.convention.confidence_pct),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw("       "),
            Span::styled(
                format!("Weight: {weight_display}"),
                Style::default().fg(Color::Magenta),
            ),
        ]);

        let adoption = format!(
            "  Found in: {}/{} files ({}% adoption)",
            self.convention.adoption_count,
            self.convention.total_count,
            self.convention.adoption_rate_pct
        );

        Paragraph::new(vec![meta, Line::from(adoption), Line::default()]).render(info_area, buf);

        // Example section: collapsed-border title + code lines filling remaining space
        // ├── Example (n/m): (…path:line) ─────────────────────────────┤
        let example_title = self.example_title();
        Block::default()
            .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT)
            .border_set(divider_set)
            .border_style(border_style)
            .title(Span::styled(example_title, border_style))
            .render(
                Rect {
                    x: area.x,
                    y: example_area.y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );

        // Code lines fill the rest of example_area
        let code_area = Rect {
            y: example_area.y + 1,
            height: example_area.height.saturating_sub(1),
            ..example_area
        };

        if self.has_examples {
            if let Some(example) = self.convention.examples.get(self.convention.example_index) {
                let max_lines = code_area.height as usize;
                let max_chars = code_area.width.saturating_sub(8).max(1) as usize;

                let is_composite = example.file.is_empty() && example.line == 0;

                let snippet_lines: Vec<Line> = if is_composite {
                    // Composite/synthetic summary: no real source lines
                    // to anchor at — render the snippet text as-is, no
                    // line-number gutter, no green highlight.
                    example
                        .snippet
                        .lines()
                        .take(max_lines)
                        .map(|line_text| {
                            Line::from(Span::styled(
                                truncate_str(line_text, max_chars),
                                Style::default().fg(Color::Cyan),
                            ))
                        })
                        .collect()
                } else {
                    let snippet_start = if example.snippet_start_line > 0 {
                        example.snippet_start_line
                    } else {
                        example.line
                    };
                    example
                        .snippet
                        .lines()
                        .take(max_lines)
                        .enumerate()
                        .map(|(i, line_text)| {
                            let line_num = snippet_start + i as u32;
                            let is_highlight = line_num >= example.line
                                && line_num <= example.end_line.max(example.line);
                            let display = truncate_str(line_text, max_chars);
                            let text_style = if is_highlight {
                                Style::default()
                                    .fg(Color::Green)
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(Color::Yellow)
                            };
                            Line::from(vec![
                                Span::styled(
                                    format!("{:>5}  ", line_num),
                                    Style::default().fg(Color::DarkGray),
                                ),
                                Span::styled(display, text_style),
                            ])
                        })
                        .collect()
                };

                if snippet_lines.is_empty() {
                    Paragraph::new("(no snippet available)")
                        .style(Style::default().fg(Color::DarkGray))
                        .render(code_area, buf);
                } else {
                    Paragraph::new(snippet_lines).render(code_area, buf);
                }
            }
        } else {
            Paragraph::new("(no usage examples found for this convention)")
                .style(Style::default().fg(Color::DarkGray))
                .render(code_area, buf);
        }

        // ├────────────────────────────────────────────────────────────────────┤
        Block::default()
            .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT)
            .border_set(divider_set)
            .border_style(border_style)
            .render(
                Rect {
                    x: area.x,
                    y: div2_area.y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );

        let examples_count = self.convention.examples.len();

        if has_search_bar {
            let hint = "Press Enter to keep or Esc to clear";
            let prompt = format!("  Filter: {}", self.search_query);
            let prompt_width = prompt.chars().count();
            let hint_width = hint.chars().count();
            let gap = ctrl_area.width as usize;

            if prompt_width + hint_width + 2 < gap {
                let pad = gap - prompt_width - hint_width - 2;
                let full = format!("{prompt}{}{hint}", " ".repeat(pad));
                Paragraph::new(full)
                    .style(Style::default().fg(Color::Yellow))
                    .render(ctrl_area, buf);
            } else {
                Paragraph::new(prompt)
                    .style(Style::default().fg(Color::Yellow))
                    .render(ctrl_area, buf);
            }

            let cursor_pos = 10 + self.search_query.len();
            if cursor_pos < ctrl_area.width as usize {
                if let Some(c) = buf.cell_mut((ctrl_area.x + cursor_pos as u16, ctrl_area.y)) {
                    c.set_style(
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::REVERSED),
                    );
                }
            }
        } else {
            render_key_bindings(buf, ctrl_area, examples_count);
        }
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        return s.to_owned();
    }
    let truncated: String = s.chars().take(max_len).collect();
    format!("{}\u{2026}", truncated)
}

fn shorten_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 4 {
        return path.to_owned();
    }
    let tail = &parts[parts.len() - 4..];
    format!("\u{2026}/{}", tail.join("/"))
}

fn render_key_bindings(buf: &mut Buffer, area: Rect, examples_count: usize) {
    let inner_width = area.width as usize;

    let mut parts: Vec<(&str, Style)> = vec![
        (
            " [y] Confirm",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        (
            "[n] Reject",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        (
            "[p] Partial",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        (
            "[s] Skip",
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        ),
        (
            "[\u{2191}\u{2193}/jk] Navigate",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    if examples_count > 1 {
        parts.push((
            "[\u{2190}\u{2192}/ad] Examples",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));
    }

    parts.push((
        "[q/Esc] Finish",
        Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    ));

    let mut spans = Vec::new();
    for (text, style) in &parts {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(text.to_string(), *style));
    }

    let rendered_text: String = Line::from(spans.clone()).to_string();
    if rendered_text.chars().count() > inner_width {
        let take = inner_width.saturating_sub(3);
        let truncated: String = rendered_text.chars().take(take).collect();
        spans = vec![Span::styled(truncated + "...", parts.last().unwrap().1)];
    }

    Paragraph::new(Line::from(spans)).render(area, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorten_path_keeps_short_paths() {
        assert_eq!(shorten_path("src/main.rs"), "src/main.rs");
        assert_eq!(shorten_path("a/b/c/d.rs"), "a/b/c/d.rs");
    }

    #[test]
    fn shorten_path_truncates_long_paths() {
        let result = shorten_path("very/long/path/that/has/many/segments/file.rs");
        assert!(result.starts_with("\u{2026}/"));
        assert!(result.contains("file.rs"));
    }

    #[test]
    fn shorten_path_exact_four_parts_not_truncated() {
        assert_eq!(shorten_path("a/b/c/d"), "a/b/c/d");
    }

    #[test]
    fn shorten_path_five_parts_truncated() {
        let result = shorten_path("a/b/c/d/e.rs");
        assert!(result.starts_with("\u{2026}/"));
        assert!(result.contains("d/e.rs"));
    }

    #[test]
    fn layout_constraints_produce_valid_areas() {
        let area = Rect::new(0, 0, 120, 40);
        let areas: [Rect; 6] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(2),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);

        assert!(areas[3].height >= 2);
        assert_eq!(areas[5].height, 1);
    }

    #[test]
    fn layout_with_examples_provides_six_areas() {
        let inner = Rect::new(0, 0, 120, 30);
        let areas: [Rect; 6] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(2),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);

        assert_eq!(areas.len(), 6);
        assert!(areas[3].height >= 2);
    }

    #[test]
    fn layout_without_examples_provides_zero_height_for_code() {
        let inner = Rect::new(0, 0, 120, 30);
        let areas: [Rect; 6] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Length(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);

        assert_eq!(areas.len(), 6);
        assert_eq!(areas[3].height, 0);
    }

    #[test]
    fn progress_title_format_single_digit() {
        let total_width = 9.to_string().len().max(1);
        let title = format!(
            " Seshat Convention Review {:>width$}/{:<width$} ",
            1,
            9,
            width = total_width
        );
        assert!(title.contains("1/9"));
    }

    #[test]
    fn progress_title_format_double_digit() {
        let total_width = 10.to_string().len().max(1);
        let title = format!(
            " Seshat Convention Review {:>width$}/{:<width$} ",
            5,
            10,
            width = total_width
        );
        assert!(title.contains(" 5/10"));
    }

    #[test]
    fn progress_title_format_triple_digit() {
        let total_width = 100.to_string().len().max(1);
        let title = format!(
            " Seshat Convention Review {:>width$}/{:<width$} ",
            50,
            100,
            width = total_width
        );
        assert!(title.contains(" 50/100"));
    }

    #[test]
    fn truncate_str_short_string_no_change() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn truncate_str_long_string_truncates() {
        let result = truncate_str("hello world", 7);
        assert!(result.ends_with("\u{2026}"));
        assert_eq!(result.chars().count(), 8); // 7 + "\u{2026}"
    }

    #[test]
    fn truncate_str_empty_string() {
        assert_eq!(truncate_str("", 0), "");
        assert_eq!(truncate_str("", 5), "");
    }

    #[test]
    fn truncate_str_exact_length() {
        assert_eq!(truncate_str("abc", 3), "abc");
    }

    #[test]
    fn render_key_bindings_single_example() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, 1));
        render_key_bindings(&mut buf, Rect::new(0, 0, 120, 1), 1);
        let text = buf.content().iter().map(|c| c.symbol()).collect::<String>();
        assert!(text.contains("[y] Confirm"));
        assert!(text.contains("[n] Reject"));
        assert!(!text.contains("Examples"));
    }

    #[test]
    fn render_key_bindings_multiple_examples() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, 1));
        render_key_bindings(&mut buf, Rect::new(0, 0, 120, 1), 3);
        let text = buf.content().iter().map(|c| c.symbol()).collect::<String>();
        assert!(text.contains("Examples"));
    }

    #[test]
    fn render_key_bindings_zero_examples() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, 1));
        render_key_bindings(&mut buf, Rect::new(0, 0, 120, 1), 0);
        let text = buf.content().iter().map(|c| c.symbol()).collect::<String>();
        assert!(!text.contains("Examples"));
    }

    fn make_conv_item(desc: &str, examples: Vec<CodeExample>) -> ConventionItem {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::default();
        desc.hash(&mut hasher);
        let hash = hasher.finish();

        ConventionItem {
            node_id: 1,
            description: desc.to_owned(),
            nature: "convention".to_owned(),
            weight: "strong".to_owned(),
            confidence_pct: 85,
            adoption_count: 10,
            total_count: 12,
            adoption_rate_pct: 83,
            trend: "stable".to_owned(),
            source: "auto".to_owned(),
            examples,
            snapshot_hash: hash,
            example_index: 0,
            description_hash: None,
        }
    }

    #[test]
    fn convention_card_fills_buffer() {
        let examples = vec![CodeExample {
            file: "src/main.rs".to_owned(),
            line: 10,
            end_line: 12,
            snippet: "fn main() {\n    println!(\"hi\");\n}".to_owned(),
            snippet_start_line: 10,
        }];
        let item = make_conv_item("Use snake_case", examples);
        let card = ConventionCard {
            convention: &item,
            current: 0,
            total: 1,
            review_complete: false,
            has_examples: true,
            search_mode: false,
            search_query: "",
            filter_locked: false,
            no_match: false,
        };
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, 30));
        card.render(Rect::new(0, 0, 120, 30), &mut buf);
        let text = buf.content().iter().map(|c| c.symbol()).collect::<String>();
        assert!(text.contains("Use snake_case"));
        assert!(text.contains("Nature: Convention"));
        assert!(text.contains("Confidence: 85%"));
    }

    #[test]
    fn convention_card_no_examples_fills_buffer() {
        let item = make_conv_item("Use camelCase", vec![]);
        let card = ConventionCard {
            convention: &item,
            current: 0,
            total: 1,
            review_complete: true,
            has_examples: false,
            search_mode: false,
            search_query: "",
            filter_locked: false,
            no_match: false,
        };
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, 30));
        card.render(Rect::new(0, 0, 120, 30), &mut buf);
        let text = buf.content().iter().map(|c| c.symbol()).collect::<String>();
        assert!(text.contains("Use camelCase"));
    }

    #[test]
    fn convention_card_tiny_area_does_not_panic() {
        let examples = vec![CodeExample {
            file: "src/lib.rs".to_owned(),
            line: 1,
            end_line: 1,
            snippet: "pub fn add(a: i32, b: i32) -> i32 { a + b }".to_owned(),
            snippet_start_line: 1,
        }];
        let item = make_conv_item("Prefer explicit types", examples);
        let card = ConventionCard {
            convention: &item,
            current: 0,
            total: 1,
            review_complete: false,
            has_examples: true,
            search_mode: false,
            search_query: "",
            filter_locked: false,
            no_match: false,
        };
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 3));
        card.render(Rect::new(0, 0, 10, 3), &mut buf);
    }
}
