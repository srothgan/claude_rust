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

use crate::app::{App, AppStatus, MessageRole, SelectionKind, SelectionState};
use crate::ui::message::{self, SpinnerState};
use crate::ui::theme;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Widget, Wrap};

#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::cast_sign_loss)]
pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    // Snapshot spinner state before the loop so we can take &mut msg
    let is_thinking = matches!(app.status, AppStatus::Thinking);
    let msg_count = app.messages.len();

    let spinner = SpinnerState {
        frame: app.spinner_frame,
        is_active: matches!(app.status, AppStatus::Thinking | AppStatus::Running),
        is_last_message: false, // overridden per-message below
        is_thinking_mid_turn: false, // overridden per-message below
    };

    let mut all_lines = Vec::new();

    // Welcome text always at the top
    all_lines.extend(welcome_lines(app));

    for (i, msg) in app.messages.iter_mut().enumerate() {
        let is_last = i + 1 == msg_count;
        // Show trailing "Thinking..." spinner only on the last assistant message
        // when status is Thinking and the message already has content (mid-turn).
        let mid_turn = is_last
            && is_thinking
            && matches!(msg.role, MessageRole::Assistant)
            && !msg.blocks.is_empty();
        let msg_spinner = SpinnerState { is_last_message: is_last, is_thinking_mid_turn: mid_turn, ..spinner };
        // Per-block caching is handled inside render_message -- each text block
        // and tool call maintains its own cache, only re-rendering on mutation.
        all_lines.extend(message::render_message(msg, &msg_spinner, area.width));
    }

    app.rendered_chat_lines = all_lines.iter().map(ToString::to_string).collect();

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
        app.scroll_target = 0;
        app.scroll_pos = 0.0;
        app.auto_scroll = true;
        app.rendered_chat_area = render_area;
        app.rendered_chat_lines = render_lines_from_paragraph(&paragraph, render_area, 0);
        frame.render_widget(paragraph, render_area);
    } else {
        // Long content: scroll within the full viewport
        let max_scroll = content_height - viewport_height;
        if app.auto_scroll {
            app.scroll_target = max_scroll;
        }
        app.scroll_target = app.scroll_target.min(max_scroll);

        let target = app.scroll_target as f32;
        let delta = target - app.scroll_pos;
        if delta.abs() < 0.01 {
            app.scroll_pos = target;
        } else {
            // Smooth over ~2-3 frames at 30fps.
            app.scroll_pos += delta * 0.5;
        }
        app.scroll_offset = app.scroll_pos.round() as usize;
        if app.scroll_offset >= max_scroll {
            app.auto_scroll = true;
        }
        app.rendered_chat_area = area;
        app.rendered_chat_lines = render_lines_from_paragraph(&paragraph, area, app.scroll_offset);
        frame.render_widget(paragraph.scroll((app.scroll_offset as u16, 0)), area);
    }

    if let Some(sel) = app.selection
        && sel.kind == SelectionKind::Chat
    {
        frame.render_widget(SelectionOverlay { selection: sel }, app.rendered_chat_area);
    }
}

struct SelectionOverlay {
    selection: SelectionState,
}

impl Widget for SelectionOverlay {
    #[allow(clippy::cast_possible_truncation)]
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (start, end) =
            crate::app::normalize_selection(self.selection.start, self.selection.end);
        for row in start.row..=end.row {
            let y = area.y.saturating_add(row as u16);
            if y >= area.bottom() {
                break;
            }
            let row_start = if row == start.row { start.col } else { 0 };
            let row_end = if row == end.row { end.col } else { area.width as usize };
            for col in row_start..row_end {
                let x = area.x.saturating_add(col as u16);
                if x >= area.right() {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
                }
            }
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
fn render_lines_from_paragraph(
    paragraph: &Paragraph,
    area: Rect,
    scroll_offset: usize,
) -> Vec<String> {
    let mut buf = Buffer::empty(area);
    let widget = paragraph.clone().scroll((scroll_offset as u16, 0));
    widget.render(area, &mut buf);
    let mut lines = Vec::with_capacity(area.height as usize);
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            if let Some(cell) = buf.cell((area.x + x, area.y + y)) {
                line.push_str(cell.symbol());
            }
        }
        lines.push(line.trim_end().to_owned());
    }
    lines
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
            Style::default().fg(theme::RUST_ORANGE).add_modifier(Modifier::BOLD),
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
