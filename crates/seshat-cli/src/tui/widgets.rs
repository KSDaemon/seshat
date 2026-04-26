use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use super::app::{App, ConventionItem};

pub fn render(frame: &mut ratatui::Frame, app: &App) {
    let area = frame.area();

    let has_examples = app
        .current()
        .map(|c| !c.examples.is_empty())
        .unwrap_or(false);

    if let Some(convention) = app.current() {
        let card = ConventionCard {
            convention,
            current: app.current_index,
            total: app.total(),
            review_complete: app.review_complete,
            has_examples,
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

        // Fixed: header(1), div(1), info(2). Example fills rest. Fixed: div(1), ctrl(1).
        let [
            header_area,
            div1_area,
            info_area,
            example_area,
            div2_area,
            ctrl_area,
        ] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(3),
            if self.has_examples {
                Constraint::Min(2)
            } else {
                Constraint::Length(0)
            },
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);

        // Header: "    1/53: description"
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
        if self.has_examples {
            if let Some(example) = self.convention.examples.get(self.convention.example_index) {
                let file_display = shorten_path(&example.file);
                let example_num = self.convention.example_index + 1;
                let examples_count = self.convention.examples.len();
                let example_title = if examples_count > 1 {
                    format!(
                        "Example ({example_num}/{examples_count}): (\u{2026}{file_display}:{})",
                        example.line
                    )
                } else {
                    format!("Example: (\u{2026}{file_display}:{})", example.line)
                };

                // ├── Example (n/m): (…path:line) ─────────────────────────────┤
                Block::default()
                    .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT)
                    .border_set(divider_set)
                    .border_style(border_style)
                    .title(Span::styled(format!("── {example_title} "), border_style))
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
                let max_lines = code_area.height as usize;
                let max_chars = code_area.width.saturating_sub(8).max(1) as usize;

                let snippet_lines: Vec<Line> = example
                    .snippet
                    .lines()
                    .take(max_lines)
                    .enumerate()
                    .map(|(i, line_text)| {
                        let line_num = example.line + i as u32;
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
                    .collect();

                if snippet_lines.is_empty() {
                    Paragraph::new("(no snippet available)")
                        .style(Style::default().fg(Color::DarkGray))
                        .render(code_area, buf);
                } else {
                    Paragraph::new(snippet_lines).render(code_area, buf);
                }
            }
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

        // Controls pinned to bottom
        let examples_count = self.convention.examples.len();
        render_key_bindings(buf, ctrl_area, examples_count);
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
            "[\u{2190}\u{2192}] Examples",
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
}
