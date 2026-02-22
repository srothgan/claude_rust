// Claude Code Rust - A native Rust terminal interface for Claude Code
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

mod autocomplete;
mod chat;
mod diff;
mod header;
mod help;
mod input;
mod layout;
mod markdown;
mod message;
mod tables;
pub mod theme;
mod todo;
mod tool_call;

use crate::app::App;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

pub fn render(frame: &mut Frame, app: &mut App) {
    let _t = app.perf.as_ref().map(|p| p.start("ui::render"));
    let frame_area = frame.area();
    app.cached_frame_area = frame_area;
    crate::perf::mark_with("ui::frame_width", "cols", usize::from(frame_area.width));
    crate::perf::mark_with("ui::frame_height", "rows", usize::from(frame_area.height));

    let todo_height = {
        let _t = app.perf.as_ref().map(|p| p.start("ui::todo_height"));
        todo::compute_height(app)
    };
    let help_height = {
        let _t = app.perf.as_ref().map(|p| p.start("ui::help_height"));
        help::compute_height(app, frame_area.width)
    };
    let input_visual_lines = {
        let _t = app.perf.as_ref().map(|p| p.start("ui::input_visual_lines"));
        input::visual_line_count(app, frame_area.width)
    };
    let areas = {
        let _t = app.perf.as_ref().map(|p| p.start("ui::layout"));
        layout::compute(frame_area, input_visual_lines, app.show_header, todo_height, help_height)
    };

    // Header bar (toggleable via Ctrl+H)
    if areas.header.height > 0 {
        let _t = app.perf.as_ref().map(|p| p.start("ui::header"));
        render_separator(frame, areas.header_top_sep);
        header::render(frame, areas.header, app);
        render_separator(frame, areas.header_bot_sep);
    }

    // Body: chat (includes welcome text when no messages yet)
    {
        let _t = app.perf.as_ref().map(|p| p.start("ui::chat"));
        chat::render(frame, areas.body, app);
    }

    // Input separator (above)
    render_separator(frame, areas.input_sep);

    // Todo panel (below input top separator, above input)
    if areas.todo.height > 0 {
        let _t = app.perf.as_ref().map(|p| p.start("ui::todo"));
        todo::render(frame, areas.todo, app);
    }

    // Input
    {
        let _t = app.perf.as_ref().map(|p| p.start("ui::input"));
        input::render(frame, areas.input, app);
    }

    // Autocomplete dropdown (floating overlay above input)
    if autocomplete::is_active(app) {
        let _t = app.perf.as_ref().map(|p| p.start("ui::autocomplete"));
        autocomplete::render(frame, areas.input, app);
    }

    // Input separator (below input)
    render_separator(frame, areas.input_bottom_sep);

    // Help overlay (below input separator)
    if areas.help.height > 0 {
        let _t = app.perf.as_ref().map(|p| p.start("ui::help"));
        help::render(frame, areas.help, app);
    }

    // Footer: mode pill left, command hints right
    if let Some(footer_area) = areas.footer {
        let _t = app.perf.as_ref().map(|p| p.start("ui::footer"));
        render_footer(frame, footer_area, app);
    }

    let fps_y = if areas.header.height > 0 { areas.header.y } else { frame_area.y };
    render_perf_fps_overlay(frame, frame_area, fps_y, app);
}

const FOOTER_PAD: u16 = 2;

fn render_footer(frame: &mut Frame, area: Rect, app: &mut App) {
    let padded = Rect {
        x: area.x + FOOTER_PAD,
        y: area.y,
        width: area.width.saturating_sub(FOOTER_PAD * 2),
        height: area.height,
    };

    if app.cached_footer_line.is_none() {
        let line = if let Some(ref mode) = app.mode {
            let color = mode_color(&mode.current_mode_id);
            Line::from(vec![
                Span::styled("[", Style::default().fg(color)),
                Span::styled(mode.current_mode_name.clone(), Style::default().fg(color)),
                Span::styled("]", Style::default().fg(color)),
                Span::raw("  "),
                Span::styled("?", Style::default().fg(Color::White)),
                Span::styled(" : Shortcuts + Commands", Style::default().fg(theme::DIM)),
            ])
        } else {
            Line::from(vec![
                Span::styled("?", Style::default().fg(Color::White)),
                Span::styled(" : Shortcuts + Commands", Style::default().fg(theme::DIM)),
            ])
        };
        app.cached_footer_line = Some(line);
    }

    if let Some(line) = &app.cached_footer_line {
        frame.render_widget(Paragraph::new(line.clone()), padded);
    }
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

#[cfg(feature = "perf")]
fn render_perf_fps_overlay(frame: &mut Frame, frame_area: Rect, y: u16, app: &App) {
    if app.perf.is_none() || frame_area.height == 0 || y >= frame_area.y + frame_area.height {
        return;
    }
    let Some(fps) = app.frame_fps() else {
        return;
    };

    let color = if fps >= 55.0 {
        Color::Green
    } else if fps >= 45.0 {
        Color::Yellow
    } else {
        Color::Red
    };
    let text = format!("[{fps:>5.1} FPS]");
    let width = u16::try_from(text.len()).unwrap_or(frame_area.width).min(frame_area.width);
    let x = frame_area.x + frame_area.width.saturating_sub(width);
    let area = Rect { x, y, width, height: 1 };
    let line = Line::from(Span::styled(
        text,
        Style::default().fg(color).add_modifier(ratatui::style::Modifier::BOLD),
    ));
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(not(feature = "perf"))]
fn render_perf_fps_overlay(_frame: &mut Frame, _frame_area: Rect, _y: u16, _app: &App) {}
