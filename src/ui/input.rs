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

// TODO: Replace custom InputState with tui-textarea when it supports ratatui 0.30
// Track: https://github.com/rhysd/tui-textarea/pull/118

use crate::app::App;
use crate::ui::theme;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

/// Prompt prefix width: "❯ " = 2 columns
const PROMPT_WIDTH: u16 = 2;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    if app.input.is_empty() {
        // Placeholder
        let line = Line::from(vec![
            Span::styled(
                format!("{} ", theme::PROMPT_CHAR),
                Style::default().fg(theme::RUST_ORANGE),
            ),
            Span::styled("Type a message...", Style::default().fg(theme::DIM)),
        ]);
        frame.render_widget(Paragraph::new(line), area);

        // Cursor after prompt char
        frame.set_cursor_position((area.x + PROMPT_WIDTH, area.y));
        return;
    }

    // Build lines with prompt on first line, indent on continuation lines
    let lines: Vec<Line> = app
        .input
        .lines
        .iter()
        .enumerate()
        .map(|(row, text)| {
            let prefix = if row == 0 {
                Span::styled(
                    format!("{} ", theme::PROMPT_CHAR),
                    Style::default().fg(theme::RUST_ORANGE),
                )
            } else {
                // Indent continuation lines to align with content after "❯ "
                Span::raw("  ")
            };
            Line::from(vec![prefix, Span::raw(text.clone())])
        })
        .collect();

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);

    // Place terminal cursor
    let cursor_x = area.x + PROMPT_WIDTH + app.input.cursor_col as u16;
    let cursor_y = area.y + app.input.cursor_row as u16;
    if cursor_x < area.right() && cursor_y < area.bottom() {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}
