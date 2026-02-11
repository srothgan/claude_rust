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

/// Minimum number of messages to render above/below the visible range as a margin.
/// Absorbs approximation error from the visual height estimation.
const CULLING_MARGIN: usize = 2;

/// Build a `SpinnerState` for a specific message index.
fn msg_spinner(
    base: SpinnerState,
    index: usize,
    msg_count: usize,
    is_thinking: bool,
    msg: &crate::app::ChatMessage,
) -> SpinnerState {
    let is_last = index + 1 == msg_count;
    let mid_turn = is_last
        && is_thinking
        && matches!(msg.role, MessageRole::Assistant)
        && !msg.blocks.is_empty();
    SpinnerState { is_last_message: is_last, is_thinking_mid_turn: mid_turn, ..base }
}

/// Ensure every message has an up-to-date `cached_visual_height` at the given width.
/// The last message is always recomputed while streaming (content changes each frame).
fn update_visual_heights(app: &mut App, base: SpinnerState, is_thinking: bool, width: u16) {
    let msg_count = app.messages.len();
    let is_streaming = matches!(app.status, AppStatus::Thinking | AppStatus::Running);
    for i in 0..msg_count {
        let is_last = i + 1 == msg_count;
        if app.messages[i].cached_visual_width == width
            && app.messages[i].cached_visual_height > 0
            && !(is_last && is_streaming)
        {
            continue;
        }
        let sp = msg_spinner(base, i, msg_count, is_thinking, &app.messages[i]);
        let mut tmp = Vec::new();
        message::render_message(&mut app.messages[i], &sp, width, &mut tmp);
        let h = Paragraph::new(Text::from(tmp)).wrap(Wrap { trim: false }).line_count(width);
        app.messages[i].cached_visual_height = h;
        app.messages[i].cached_visual_width = width;
    }
}

/// Render all messages into `out` (no culling). Used when content fits in the viewport.
fn render_all_messages(
    app: &mut App,
    base: SpinnerState,
    is_thinking: bool,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    if let Some(cached) = &app.cached_welcome_lines {
        out.extend(cached.iter().cloned());
    }
    let msg_count = app.messages.len();
    for i in 0..msg_count {
        let sp = msg_spinner(base, i, msg_count, is_thinking, &app.messages[i]);
        message::render_message(&mut app.messages[i], &sp, width, out);
    }
}

/// Render only the visible message range into `out` (viewport culling).
/// Returns the local scroll offset to pass to `Paragraph::scroll()`.
#[allow(clippy::cast_possible_truncation, clippy::too_many_arguments)]
fn render_culled_messages(
    app: &mut App,
    base: SpinnerState,
    is_thinking: bool,
    width: u16,
    welcome_height: usize,
    scroll: usize,
    viewport_height: usize,
    out: &mut Vec<Line<'static>>,
) -> usize {
    let msg_count = app.messages.len();

    // Walk cumulative heights to find the first message overlapping scroll_offset.
    let mut cumulative = welcome_height;
    let mut first_visible = 0;
    for (i, msg) in app.messages.iter().enumerate() {
        if cumulative + msg.cached_visual_height > scroll {
            first_visible = i;
            break;
        }
        cumulative += msg.cached_visual_height;
        first_visible = i + 1;
    }
    first_visible = first_visible.min(msg_count.saturating_sub(1));

    // Apply margin: render a few extra messages above/below for safety
    let render_start = first_visible.saturating_sub(CULLING_MARGIN);

    // Compute cumulative height before render_start (for local scroll offset)
    let mut height_before_start = welcome_height;
    for msg in &app.messages[..render_start] {
        height_before_start += msg.cached_visual_height;
    }

    // Include welcome text only if render_start is 0 (top is visible)
    if render_start == 0 {
        if let Some(cached) = &app.cached_welcome_lines {
            out.extend(cached.iter().cloned());
        }
        height_before_start = 0;
    }

    // Render messages from render_start onward, stopping when we have enough
    let lines_needed = (scroll - height_before_start) + viewport_height + 100;
    for i in render_start..msg_count {
        let sp = msg_spinner(base, i, msg_count, is_thinking, &app.messages[i]);
        message::render_message(&mut app.messages[i], &sp, width, out);
        if out.len() > lines_needed {
            break;
        }
    }

    scroll.saturating_sub(height_before_start)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::cast_sign_loss)]
pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let is_thinking = matches!(app.status, AppStatus::Thinking);
    let width = area.width;

    let base_spinner = SpinnerState {
        frame: app.spinner_frame,
        is_active: matches!(app.status, AppStatus::Thinking | AppStatus::Running),
        is_last_message: false,
        is_thinking_mid_turn: false,
    };

    // Welcome text (cached once)
    if app.cached_welcome_lines.is_none() {
        app.cached_welcome_lines = Some(welcome_lines(app));
    }

    // Update per-message visual heights
    update_visual_heights(app, base_spinner, is_thinking, width);

    let welcome_height = app.cached_welcome_lines.as_ref().map_or(0, |lines| {
        Paragraph::new(Text::from(lines.clone())).wrap(Wrap { trim: false }).line_count(width)
    });
    let content_height: usize =
        welcome_height + app.messages.iter().map(|m| m.cached_visual_height).sum::<usize>();
    let viewport_height = area.height as usize;

    if content_height <= viewport_height {
        // Short content: render everything, bottom-aligned
        let mut all_lines = Vec::new();
        render_all_messages(app, base_spinner, is_thinking, width, &mut all_lines);

        let paragraph = Paragraph::new(Text::from(all_lines)).wrap(Wrap { trim: false });
        let real_height = paragraph.line_count(width);
        let offset = (viewport_height - real_height) as u16;
        let render_area =
            Rect { x: area.x, y: area.y + offset, width: area.width, height: real_height as u16 };
        app.scroll_offset = 0;
        app.scroll_target = 0;
        app.scroll_pos = 0.0;
        app.auto_scroll = true;
        app.rendered_chat_area = render_area;
        if app.selection.is_some() {
            app.rendered_chat_lines = render_lines_from_paragraph(&paragraph, render_area, 0);
        }
        frame.render_widget(paragraph, render_area);
    } else {
        // Long content: smooth scroll + viewport culling
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
            app.scroll_pos += delta * 0.3;
        }
        app.scroll_offset = app.scroll_pos.round() as usize;
        if app.scroll_offset >= max_scroll {
            app.auto_scroll = true;
        }

        let mut all_lines = Vec::new();
        let local_scroll = render_culled_messages(
            app,
            base_spinner,
            is_thinking,
            width,
            welcome_height,
            app.scroll_offset,
            viewport_height,
            &mut all_lines,
        );
        let paragraph = Paragraph::new(Text::from(all_lines)).wrap(Wrap { trim: false });

        app.rendered_chat_area = area;
        if app.selection.is_some() {
            app.rendered_chat_lines = render_lines_from_paragraph(&paragraph, area, local_scroll);
        }
        frame.render_widget(paragraph.scroll((local_scroll as u16, 0)), area);
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
    r" --------------------------------- ",
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
