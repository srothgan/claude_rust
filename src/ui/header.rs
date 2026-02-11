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
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

const HEADER_PAD: u16 = 2;

pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let padded = Rect {
        x: area.x + HEADER_PAD,
        y: area.y,
        width: area.width.saturating_sub(HEADER_PAD * 2),
        height: area.height,
    };

    if app.cached_header_line.is_none() {
        let sep = Span::styled("  \u{2502}  ", Style::default().fg(theme::DIM));
        app.cached_header_line = Some(Line::from(vec![
            Span::styled("\u{1F980} ", Style::default().fg(theme::RUST_ORANGE)),
            Span::styled(
                "claude-rust",
                Style::default().fg(theme::RUST_ORANGE).add_modifier(Modifier::BOLD),
            ),
            sep,
            Span::styled("Model: ", Style::default().fg(theme::DIM)),
            Span::styled(app.model_name.clone(), Style::default().fg(ratatui::style::Color::White)),
        ]));
    }

    if let Some(line) = &app.cached_header_line {
        frame.render_widget(Paragraph::new(line.clone()), padded);
    }
}
