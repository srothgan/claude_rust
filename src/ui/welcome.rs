// claude_rust — A native Rust terminal interface for Claude Code
// Copyright (C) 2025  Simon Peter Rothgang
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

use crate::app::App;
use crate::ui::theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

// Ferris with speech bubble (ferris-says style)
const FERRIS_SAYS: &[&str] = &[
    r" ____________________ ",
    r"< Welcome back!      >",
    r" -------------------- ",
    r"        \             ",
    r"         \            ",
    r"            _~^~^~_  ",
    r"        \) /  o o  \ (/",
    r"          '_   -   _' ",
    r"          / '-----' \ ",
];

/// Min/max height for the welcome box (including borders)
const MIN_BOX_HEIGHT: u16 = 14;
const MAX_BOX_HEIGHT: u16 = 18;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    // Clamp the welcome box height
    let box_height = area.height.clamp(MIN_BOX_HEIGHT, MAX_BOX_HEIGHT);

    // Place the box at the top, remaining space is empty
    let [box_area, _rest] =
        Layout::vertical([Constraint::Length(box_height), Constraint::Min(0)]).areas(area);

    // Title embedded in the top border
    let title = format!(" claude-rust v{} ", env!("CARGO_PKG_VERSION"));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::RUST_ORANGE))
        .title(title)
        .title_style(
            Style::default()
                .fg(theme::RUST_ORANGE)
                .add_modifier(Modifier::BOLD),
        );

    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    // Split inner area: left (ferris + info) | divider | right (tips + recent)
    let left_width = inner.width * 2 / 5;
    let [left_area, divider_area, right_area] = Layout::horizontal([
        Constraint::Length(left_width),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(inner);

    render_left_panel(frame, left_area, app);
    render_divider(frame, divider_area);
    render_right_panel(frame, right_area);
}

fn render_left_panel(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Content: ferris art + blank + model + cwd
    let ferris_height = FERRIS_SAYS.len() as u16;
    let content_height = ferris_height + 1 + 2; // ferris + blank + model + cwd
    let top_pad = area.height.saturating_sub(content_height) / 2;

    for _ in 0..top_pad {
        lines.push(Line::default());
    }

    // Ferris with speech bubble in Rust orange, centered horizontally
    let max_art_width = FERRIS_SAYS.iter().map(|l| l.len()).max().unwrap_or(0);
    let art_pad = area.width.saturating_sub(max_art_width as u16) / 2;
    let pad_str: String = " ".repeat(art_pad as usize);

    for ferris_line in FERRIS_SAYS {
        lines.push(Line::from(Span::styled(
            format!("{pad_str}{ferris_line}"),
            Style::default().fg(theme::RUST_ORANGE),
        )));
    }

    lines.push(Line::default());

    // Model name centered, in Rust orange bold
    let model_text = app.model_name.clone();
    let model_line = Line::from(Span::styled(
        model_text,
        Style::default()
            .fg(theme::RUST_ORANGE)
            .add_modifier(Modifier::BOLD),
    ))
    .centered();
    lines.push(model_line);

    // Current directory centered, in dim
    let cwd_line = Line::from(Span::styled(
        app.cwd.clone(),
        Style::default().fg(theme::DIM),
    ))
    .centered();
    lines.push(cwd_line);

    frame.render_widget(Paragraph::new(lines), area);
}

fn render_divider(frame: &mut Frame, area: Rect) {
    let lines: Vec<Line<'static>> = (0..area.height)
        .map(|_| {
            Line::from(Span::styled(
                "\u{2502}",
                Style::default().fg(theme::RUST_ORANGE),
            ))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_right_panel(frame: &mut Frame, area: Rect) {
    let sep_width = area.width.saturating_sub(2) as usize;
    let lines: Vec<Line<'static>> = vec![
        // Tips section
        Line::from(Span::styled(
            " Tips for getting started",
            Style::default()
                .fg(theme::RUST_ORANGE)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            " Enter to send, Shift+Enter for newline",
            Style::default().fg(Color::White),
        )),
        Line::from(Span::styled(
            " Ctrl+C to quit, Ctrl+\u{2191}/\u{2193} to scroll",
            Style::default().fg(Color::White),
        )),
        // Separator
        Line::default(),
        Line::from(Span::styled(
            format!(" {}", theme::SEPARATOR_CHAR.repeat(sep_width)),
            Style::default().fg(theme::RUST_ORANGE),
        )),
        Line::default(),
        // Recent activity section
        Line::from(Span::styled(
            " Recent activity",
            Style::default()
                .fg(theme::RUST_ORANGE)
                .add_modifier(Modifier::BOLD),
        )),
        // TODO: Populate from session persistence (Phase 6)
        Line::from(Span::styled(
            " No recent activity",
            Style::default().fg(theme::DIM),
        )),
    ];

    frame.render_widget(Paragraph::new(lines), area);
}
