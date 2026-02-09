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

use crate::app::{App, AppStatus};
use crate::ui::message::{self, SpinnerState};
use crate::ui::theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Wrap};

pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    // Snapshot spinner state before the loop so we can take &mut msg
    let spinner = SpinnerState {
        frame: app.spinner_frame,
        is_active: matches!(app.status, AppStatus::Thinking | AppStatus::Running),
    };

    let mut all_lines = Vec::new();

    // Welcome text always at the top
    all_lines.extend(welcome_lines(app));

    for msg in &mut app.messages {
        // Per-block caching is handled inside render_message — each text block
        // and tool call maintains its own cache, only re-rendering on mutation.
        all_lines.extend(message::render_message(msg, &spinner, area.width));
    }

    // Build paragraph once — line_count gives the real wrapped height
    let paragraph = Paragraph::new(Text::from(all_lines)).wrap(Wrap { trim: false });
    let content_height = paragraph.line_count(area.width);
    let viewport_height = area.height as usize;

    if content_height <= viewport_height {
        // Short content: render in a bottom-aligned sub-rect (stacks above input)
        let offset = (viewport_height - content_height) as u16;
        let render_area = Rect {
            x: area.x,
            y: area.y + offset,
            width: area.width,
            height: content_height as u16,
        };
        app.scroll_offset = 0;
        app.auto_scroll = true;
        frame.render_widget(paragraph, render_area);
    } else {
        // Long content: scroll within the full viewport
        let max_scroll = content_height - viewport_height;
        app.scroll_offset = app.scroll_offset.min(max_scroll);
        if app.auto_scroll {
            app.scroll_offset = max_scroll;
        }
        if app.scroll_offset >= max_scroll {
            app.auto_scroll = true;
        }
        frame.render_widget(paragraph.scroll((app.scroll_offset as u16, 0)), area);
    }
}

const FERRIS_SAYS: &[&str] = &[
    r" _________________________________ ",
    r"< Welcome back to Claude, in Rust! >",
    r" --------------------------------- ",
    r"        \             ",
    r"         \            ",
    r"            _~^~^~_  ",
    r"        \) /  o o  \ (/",
    r"          '_   -   _' ",
    r"          / '-----' \ ",
];

fn welcome_lines(app: &App) -> Vec<Line<'static>> {
    let pad = "  ";
    let mut lines = Vec::new();

    // Ferris with speech bubble
    for art_line in FERRIS_SAYS {
        lines.push(Line::from(Span::styled(
            format!("{pad}{art_line}"),
            Style::default().fg(theme::RUST_ORANGE),
        )));
    }

    lines.push(Line::default());
    lines.push(Line::default());

    // Model and cwd
    lines.push(Line::from(vec![
        Span::styled(format!("{pad}Model: "), Style::default().fg(theme::DIM)),
        Span::styled(
            app.model_name.clone(),
            Style::default()
                .fg(theme::RUST_ORANGE)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        format!("{pad}cwd:   {}", app.cwd),
        Style::default().fg(theme::DIM),
    )));

    lines.push(Line::default());

    // Tips
    lines.push(Line::from(Span::styled(
        format!("{pad}Tips: Enter to send, Shift+Enter for newline, Ctrl+C to quit"),
        Style::default().fg(theme::DIM),
    )));
    lines.push(Line::default());

    lines
}
