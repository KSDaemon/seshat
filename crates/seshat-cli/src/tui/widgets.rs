use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
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
        Paragraph::new("No convention to display")
            .block(Block::default().borders(Borders::ALL))
            .render(area, frame.buffer_mut());
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
        let total_width = self.total.to_string().len().max(1);
        let title = format!(
            " Seshat Convention Review {:>width$}/{:<width$} ",
            self.current + 1,
            self.total,
            width = total_width
        );

        // Single outer cyan border (PRD FR-1 design)
        let outer_block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .style(Style::default().fg(Color::Cyan))
            .border_style(Style::default().fg(Color::Cyan));
        let inner = outer_block.inner(area);
        outer_block.render(area, buf);

        // Layout: header(1), info(2), divider(1), example(min 3, takes rest), bottom(3)
        let [header_area, info_area, divider1, example_area, bottom_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Length(1),
            if self.has_examples {
                Constraint::Min(3)
            } else {
                Constraint::Length(0)
            },
            Constraint::Length(3),
        ])
        .areas(inner);

        // Header: "  1/53: Import grouping: stdlib → external → internal"
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
            .wrap(Wrap { trim: true })
            .render(header_area, buf);

        // Info section: metadata line + adoption stats
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
            Span::styled(
                format!("Nature: {nature_display}"),
                Style::default().fg(Color::Green),
            ),
            Span::raw("     "),
            Span::styled(
                format!("Confidence: {}%", self.convention.confidence_pct),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw("     "),
            Span::styled(
                format!("Weight: {weight_display}"),
                Style::default().fg(Color::Magenta),
            ),
        ]);

        let adoption = format!(
            "Found in: {}/{} files ({}% adoption)",
            self.convention.adoption_count,
            self.convention.total_count,
            self.convention.adoption_rate_pct
        );

        let info_lines = vec![meta, Line::from(adoption)];
        Paragraph::new(info_lines).render(info_area, buf);

        // Divider line
        let divider_line = Line::from(vec![Span::styled(
            "─".repeat(divider1.width as usize),
            Style::default().fg(Color::DarkGray),
        )]);
        Paragraph::new(divider_line).render(divider1, buf);

        // Example section: code block with filename:line title
        // Data comes ONLY from DB — snippets are stored during scan.
        // Display only what fits in the code block area.
        if self.has_examples {
            if let Some(example) = self.convention.examples.first() {
                let file_display = shorten_path(&example.file);
                let example_title = if example.line > 0 {
                    format!(" Example: ({file_display}:{}) ", example.line)
                } else {
                    format!(" Example: ({file_display}) ")
                };
                let example_block = Block::default()
                    .borders(Borders::ALL)
                    .title(example_title)
                    .border_style(Style::default().fg(Color::Yellow));
                let code_inner = example_block.inner(example_area);
                example_block.render(example_area, buf);

                // Limit display to code_inner dimensions.
                // Only show what's in the DB snippet — no file I/O.
                let max_lines = (code_inner.height.max(3)).saturating_sub(2) as usize;
                let max_chars = (code_inner.width.max(10)).saturating_sub(6) as usize;

                let snippet_lines: Vec<Line> = example
                    .snippet
                    .lines()
                    .take(max_lines)
                    .enumerate()
                    .map(|(i, line_text)| {
                        let line_num = example.line + i as u32;
                        let is_highlight = line_num >= example.line
                            && line_num <= example.end_line.max(example.line);

                        // Truncate line to fit within the code block width
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
                                format!("{:>4} ", line_num),
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::styled(display, text_style),
                        ])
                    })
                    .collect();

                if snippet_lines.is_empty() {
                    Paragraph::new("(no snippet available)")
                        .style(Style::default().fg(Color::DarkGray))
                        .render(code_inner, buf);
                } else {
                    Paragraph::new(snippet_lines)
                        .wrap(Wrap { trim: true })
                        .render(code_inner, buf);
                }
            }
        } else {
            // Empty example area (no examples to show)
            let empty_line = Line::from(vec![Span::styled(
                " (no examples) ".repeat(example_area.width as usize),
                Style::default().fg(Color::DarkGray),
            )]);
            Paragraph::new(empty_line).render(example_area, buf);
        }

        // Bottom bar: all controls in one line (PRD FR-1 design)
        render_key_bindings(buf, bottom_area);
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        return s.to_owned();
    }
    let truncated: String = s.chars().take(max_len).collect();
    format!("{}…", truncated)
}

fn shorten_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 4 {
        return path.to_owned();
    }
    let tail = &parts[parts.len() - 4..];
    format!("…/{}", tail.join("/"))
}

fn render_key_bindings(buf: &mut Buffer, area: Rect) {
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    let inner_width = inner.width as usize;

    // All controls on a single line
    let parts: &[(&str, Style)] = &[
        (
            "[y] Confirm",
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
            "[q/Esc] Finish",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    let mut spans = Vec::new();
    for (text, style) in parts.iter() {
        if !spans.is_empty() {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(text.to_string(), *style));
    }

    // Truncate rendered line if it exceeds available width
    let rendered_text: String = Line::from(spans.clone()).to_string();
    if rendered_text.chars().count() > inner_width {
        let take = inner_width.saturating_sub(3);
        let truncated: String = rendered_text.chars().take(take).collect();
        spans = vec![Span::styled(truncated + "...", parts.last().unwrap().1)];
    }

    Paragraph::new(Line::from(spans)).render(inner, buf);
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
        assert!(result.starts_with("…/"));
        assert!(result.contains("file.rs"));
    }

    #[test]
    fn shorten_path_exact_four_parts_not_truncated() {
        assert_eq!(shorten_path("a/b/c/d"), "a/b/c/d");
    }

    #[test]
    fn shorten_path_five_parts_truncated() {
        let result = shorten_path("a/b/c/d/e.rs");
        assert!(result.starts_with("…/"));
        assert!(result.contains("d/e.rs"));
    }

    #[test]
    fn layout_constraints_produce_valid_areas() {
        let area = Rect::new(0, 0, 120, 40);
        let areas: [Rect; 5] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .areas(area);

        assert!(areas[3].height >= 3);
        assert_eq!(areas[4].height, 3);
    }

    #[test]
    fn layout_with_examples_provides_five_areas() {
        let inner = Rect::new(0, 0, 120, 30);
        let areas: [Rect; 5] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .areas(inner);

        assert_eq!(areas.len(), 5);
        assert!(areas[3].height >= 3);
    }

    #[test]
    fn layout_without_examples_provides_zero_height_for_code() {
        let inner = Rect::new(0, 0, 120, 30);
        let areas: [Rect; 5] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(0),
            Constraint::Length(3),
        ])
        .areas(inner);

        assert_eq!(areas.len(), 5);
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
        assert!(result.ends_with("…"));
        assert_eq!(result.chars().count(), 8); // 7 + "…"
    }
}
