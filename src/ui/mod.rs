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
mod input;
mod layout;
mod message;
pub mod theme;

use crate::app::App;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

pub fn render(frame: &mut Frame, app: &mut App) {
    let areas = layout::compute(
        frame.area(),
        input::visual_line_count(app, frame.area().width),
        true,
    );

    // Header bar (always visible)
    if areas.header.height > 0 {
        header::render(frame, areas.header, app);
        render_separator(frame, areas.header_sep);
    }

    // Body: chat (includes welcome text when no messages yet)
    chat::render(frame, areas.body, app);

    // Input separator (above)
    render_separator(frame, areas.input_sep);

    // Input
    input::render(frame, areas.input, app);

    // Input separator (below)
    render_separator(frame, areas.input_bottom_sep);

    // Footer: mode pill left, command hints right
    if let Some(footer_area) = areas.footer {
        render_footer(frame, footer_area, app);
    }
}

const FOOTER_PAD: u16 = 2;

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
        ])
    } else {
        Line::default()
    };

    // Right side: only novel keybindings (not enter/quit/etc)
    let dot = Span::styled("  \u{00b7}  ", Style::default().fg(theme::DIM));
    let mut hints: Vec<Span> = Vec::new();
    if matches!(
        app.status,
        crate::app::AppStatus::Thinking | crate::app::AppStatus::Running
    ) {
        hints.push(Span::styled("esc", Style::default().fg(Color::White)));
        hints.push(Span::styled(": cancel", Style::default().fg(theme::DIM)));
    }
    // Mode cycling hint (only if multiple modes available)
    if app
        .mode
        .as_ref()
        .is_some_and(|m| m.available_modes.len() > 1)
    {
        if !hints.is_empty() {
            hints.push(dot.clone());
        }
        hints.push(Span::styled("shift+tab", Style::default().fg(Color::White)));
        hints.push(Span::styled(": mode", Style::default().fg(theme::DIM)));
    }
    if !hints.is_empty() {
        hints.push(dot.clone());
    }
    hints.push(Span::styled("ctrl+o", Style::default().fg(Color::White)));
    let tool_hint = if app.tools_collapsed {
        ": expand tools"
    } else {
        ": collapse tools"
    };
    hints.push(Span::styled(tool_hint, Style::default().fg(theme::DIM)));
    let right = Line::from(hints);

    let left_width = left.width() as u16;
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Length(left_width), Constraint::Fill(1)]).areas(padded);

    frame.render_widget(Paragraph::new(left), left_area);
    frame.render_widget(
        Paragraph::new(right).alignment(ratatui::layout::Alignment::Right),
        right_area,
    );
}

fn render_separator(frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let sep_str = theme::SEPARATOR_CHAR.repeat(area.width as usize);
    let line = Line::from(Span::styled(sep_str, Style::default().fg(theme::DIM)));
    frame.render_widget(Paragraph::new(line), area);
}
