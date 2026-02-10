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
mod header;
mod help;
mod input;
mod layout;
mod message;
pub mod theme;
mod tables;
mod todo;

use crate::app::App;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

pub fn render(frame: &mut Frame, app: &mut App) {
    app.cached_frame_area = frame.area();
    let todo_height = todo::compute_height(app);
    let help_height = help::compute_height(app, frame.area().width);
    let areas = layout::compute(
        frame.area(),
        input::visual_line_count(app, frame.area().width),
        true,
        todo_height,
        help_height,
    );

    // Header bar (always visible)
    if areas.header.height > 0 {
        header::render(frame, areas.header, app);
        render_separator(frame, areas.header_sep);
    }

    // Body: chat (includes welcome text when no messages yet)
    chat::render(frame, areas.body, app);

    // Todo panel (between chat and input)
    if areas.todo.height > 0 {
        todo::render(frame, areas.todo, app);
    }

    // Input separator (above)
    render_separator(frame, areas.input_sep);

    // Input
    input::render(frame, areas.input, app);

    // Input separator (below input)
    render_separator(frame, areas.input_bottom_sep);

    // Help overlay (below input separator)
    if areas.help.height > 0 {
        help::render(frame, areas.help, app);
    }

    // Footer: mode pill left, command hints right
    if let Some(footer_area) = areas.footer {
        render_footer(frame, footer_area, app);
    }
}

const FOOTER_PAD: u16 = 2;

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let padded = Rect {
        x: area.x + FOOTER_PAD,
        y: area.y,
        width: area.width.saturating_sub(FOOTER_PAD * 2),
        height: area.height,
    };

    // Left side: mode pill
    let left = if let Some(ref mode) = app.mode {
        let color = mode_color(&mode.current_mode_id);
        Line::from(vec![
            Span::styled("[", Style::default().fg(color)),
            Span::styled(&mode.current_mode_name, Style::default().fg(color)),
            Span::styled("]", Style::default().fg(color)),
            Span::raw("  "),
            Span::styled("?", Style::default().fg(Color::White)),
            Span::styled(" : help", Style::default().fg(theme::DIM)),
        ])
    } else {
        Line::from(vec![
            Span::styled("?", Style::default().fg(Color::White)),
            Span::styled(" : help", Style::default().fg(theme::DIM)),
        ])
    };

    frame.render_widget(Paragraph::new(left), padded);
}

/// Returns a color for the given mode ID.
fn mode_color(mode_id: &str) -> Color {
    match mode_id {
        "default" => theme::DIM,
        "plan" => Color::Blue,
        "acceptEdits" => Color::Yellow,
        "bypassPermissions" | "dontAsk" => Color::Red,
        _ => Color::Magenta,
    }
}

fn render_separator(frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let sep_str = theme::SEPARATOR_CHAR.repeat(area.width as usize);
    let line = Line::from(Span::styled(sep_str, Style::default().fg(theme::DIM)));
    frame.render_widget(Paragraph::new(line), area);
}
