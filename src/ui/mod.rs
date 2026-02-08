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

mod chat;
mod input;
mod layout;
mod message;
mod permission_dialog;
pub mod theme;
mod welcome;

use crate::app::App;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

pub fn render(frame: &mut Frame, app: &mut App) {
    let perm_height = permission_dialog::required_height(app);
    let areas = layout::compute(frame.area(), app.input.line_count(), perm_height);

    // Body: welcome screen or chat
    if app.messages.is_empty() {
        welcome::render(frame, areas.body, app);
    } else {
        chat::render(frame, areas.body, app);
    }

    // Input separator
    render_separator(frame, areas.input_sep);

    // Permission dialog (inline, pushes messages up)
    if let Some(perm_area) = areas.permission {
        permission_dialog::render(frame, perm_area, app);
    }

    // Input
    input::render(frame, areas.input, app);

    // Footer: token stats left, command hints right
    if let Some(footer_area) = areas.footer {
        render_footer(frame, footer_area, app);
    }
}

const FOOTER_PAD: u16 = 2;

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    // Inset area with padding on both sides
    let padded = Rect {
        x: area.x + FOOTER_PAD,
        y: area.y,
        width: area.width.saturating_sub(FOOTER_PAD * 2),
        height: area.height,
    };

    // Left side: token usage stats
    let (input_tokens, output_tokens) = app.tokens_used;
    let total = input_tokens + output_tokens;
    let left = Line::from(vec![
        Span::styled(
            format!("{}k", total / 1000),
            Style::default().fg(theme::DIM),
        ),
        Span::styled(" tokens", Style::default().fg(theme::DIM)),
        Span::styled("  \u{2502}  ", Style::default().fg(theme::DIM)),
        Span::styled(
            format!("${:.2}", cost_estimate(input_tokens, output_tokens)),
            Style::default().fg(theme::DIM),
        ),
    ]);

    // Right side: command hints in "key: action" format
    let dot = Span::styled("  \u{00b7}  ", Style::default().fg(theme::DIM));
    let mut hints = vec![
        Span::styled("enter", Style::default().fg(Color::White)),
        Span::styled(": send", Style::default().fg(theme::DIM)),
        dot.clone(),
        Span::styled("shift+enter", Style::default().fg(Color::White)),
        Span::styled(": newline", Style::default().fg(theme::DIM)),
    ];
    if matches!(app.status, crate::app::AppStatus::Thinking | crate::app::AppStatus::Running(_)) {
        hints.push(dot.clone());
        hints.push(Span::styled("esc", Style::default().fg(Color::White)));
        hints.push(Span::styled(": cancel", Style::default().fg(theme::DIM)));
    }
    hints.push(dot);
    hints.push(Span::styled("ctrl+c", Style::default().fg(Color::White)));
    hints.push(Span::styled(": quit", Style::default().fg(theme::DIM)));
    let right = Line::from(hints);

    let right_width = right.width() as u16;
    let [left_area, right_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(right_width),
    ])
    .areas(padded);

    frame.render_widget(Paragraph::new(left), left_area);
    frame.render_widget(
        Paragraph::new(right).alignment(ratatui::layout::Alignment::Right),
        right_area,
    );
}

fn cost_estimate(input_tokens: u64, output_tokens: u64) -> f64 {
    // Rough: $3/M input, $15/M output (Sonnet pricing)
    (input_tokens as f64 * 3.0 / 1_000_000.0) + (output_tokens as f64 * 15.0 / 1_000_000.0)
}

fn render_separator(frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let sep_str = theme::SEPARATOR_CHAR.repeat(area.width as usize);
    let line = Line::from(Span::styled(sep_str, Style::default().fg(theme::DIM)));
    frame.render_widget(Paragraph::new(line), area);
}
