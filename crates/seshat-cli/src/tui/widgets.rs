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
    let areas = build_areas(area, has_examples);

    let [content_area, footer_area] =
        Layout::vertical([Constraint::Min(10), Constraint::Length(3)]).areas(area);

    if let Some(convention) = app.current() {
        let card = ConventionCard {
            convention,
            current: app.current_index,
            total: app.total(),
            review_complete: app.review_complete,
            areas,
        };
        card.render(content_area, frame.buffer_mut());
    } else {
        Paragraph::new("No convention to display")
            .block(Block::default().borders(Borders::ALL))
            .render(content_area, frame.buffer_mut());
    }

    render_key_bindings(frame.buffer_mut(), footer_area);
}

fn build_areas(area: Rect, has_examples: bool) -> [Rect; 5] {
    if has_examples {
        Layout::vertical([
            Constraint::Min(2),
            Constraint::Length(2),
            Constraint::Min(4),
            Constraint::Length(2),
            Constraint::Min(0),
        ])
        .areas(area)
    } else {
        Layout::vertical([
            Constraint::Min(2),
            Constraint::Length(2),
            Constraint::Length(0),
            Constraint::Length(2),
            Constraint::Min(0),
        ])
        .areas(area)
    }
}

pub struct ConventionCard<'a> {
    pub convention: &'a ConventionItem,
    pub current: usize,
    pub total: usize,
    pub review_complete: bool,
    areas: [Rect; 5],
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

        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .style(Style::default().fg(Color::Cyan))
            .border_style(Style::default().fg(Color::Cyan));

        block.render(area, buf);

        let [desc_area, meta_area, example_area, stats_area, spacer_area] = self.areas;

        let desc_style = Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD);
        Paragraph::new(self.convention.description.clone())
            .style(desc_style)
            .wrap(Wrap { trim: true })
            .render(desc_area, buf);

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
            Span::raw("    "),
            Span::styled(
                format!("Confidence: {}%", self.convention.confidence_pct),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw("    "),
            Span::styled(
                format!("Weight: {weight_display}"),
                Style::default().fg(Color::Magenta),
            ),
        ]);
        Paragraph::new(meta).render(meta_area, buf);

        if !self.convention.examples.is_empty() {
            if let Some(example) = self.convention.examples.first() {
                let file_display = shorten_path(&example.file);
                let example_title = if example.line > 0 {
                    format!(" Example ({file_display}:{}) ", example.line)
                } else {
                    format!(" Example ({file_display}) ")
                };
                let code_block = Block::default()
                    .borders(Borders::ALL)
                    .title(example_title)
                    .border_style(Style::default().fg(Color::DarkGray));
                let code_inner = code_block.inner(example_area);
                code_block.render(example_area, buf);

                let snippet_lines: Vec<Line> = example
                    .snippet
                    .lines()
                    .enumerate()
                    .map(|(i, line_text)| {
                        let line_num = example.line + i as u32;
                        let is_highlight = line_num >= example.line
                            && line_num <= example.end_line.max(example.line);
                        if is_highlight {
                            Line::from(vec![
                                Span::styled(
                                    format!("{:>4} ", line_num),
                                    Style::default().fg(Color::Green),
                                ),
                                Span::styled(
                                    line_text.to_owned(),
                                    Style::default()
                                        .fg(Color::Green)
                                        .add_modifier(Modifier::BOLD),
                                ),
                            ])
                        } else {
                            Line::from(vec![
                                Span::styled(
                                    format!("{:>4} ", line_num),
                                    Style::default().fg(Color::DarkGray),
                                ),
                                Span::styled(
                                    line_text.to_owned(),
                                    Style::default().fg(Color::Yellow),
                                ),
                            ])
                        }
                    })
                    .collect();

                if snippet_lines.is_empty() {
                    Paragraph::new("(no snippet available)")
                        .style(Style::default().fg(Color::DarkGray))
                        .render(code_inner, buf);
                } else {
                    Paragraph::new(snippet_lines)
                        .wrap(Wrap { trim: false })
                        .render(code_inner, buf);
                }
            }
        }

        let adoption = format!(
            "Found in: {}/{} files ({}% adoption)",
            self.convention.adoption_count,
            self.convention.total_count,
            self.convention.adoption_rate_pct
        );
        Paragraph::new(adoption)
            .style(Style::default().fg(Color::White))
            .render(stats_area, buf);

        // Show "review complete" indicator when at last convention
        if self.review_complete {
            let complete_text = Line::from(Span::styled(
                " [Last convention] — press y/n/p/s to act, q to finish",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
            Paragraph::new(complete_text).render(spacer_area, buf);
        }
    }
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
    let keys = Line::from(vec![
        Span::styled(
            "[y] Confirm",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(
            "[n] Reject",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(
            "[p] Partial",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(
            "[s] Skip",
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled("[↑↓] Navigate", Style::default().fg(Color::White)),
        Span::raw("   "),
        Span::styled(
            "[q] Finish",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    block.render(area, buf);
    Paragraph::new(keys).render(inner, buf);
}
