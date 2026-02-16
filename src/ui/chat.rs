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
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Text};
use ratatui::widgets::{Paragraph, Widget, Wrap};

/// Minimum number of messages to render above/below the visible range as a margin.
/// Heights are now exact (block-level wrapped heights), so no safety margin is needed.
const CULLING_MARGIN: usize = 0;

#[derive(Clone, Copy, Default)]
struct HeightUpdateStats {
    measured_msgs: usize,
    measured_lines: usize,
    reused_msgs: usize,
}

#[derive(Clone, Copy, Default)]
struct CulledRenderStats {
    local_scroll: usize,
    first_visible: usize,
    render_start: usize,
    rendered_msgs: usize,
}

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

/// Ensure every message has an up-to-date height in the viewport at the given width.
/// The last message is always recomputed while streaming (content changes each frame).
///
/// Height is ground truth: each message is rendered into a scratch buffer via
/// `render_message()` and measured with `Paragraph::line_count(width)`. This uses
/// the exact same wrapping algorithm as the actual render path, so heights can
/// never drift from reality.
///
/// Iterates in reverse so we can break early: once we hit a message whose height
/// is already valid at this width, all earlier messages are also valid (content
/// only changes at the tail during streaming). This turns the common case from
/// O(n) to O(1).
fn update_visual_heights(
    app: &mut App,
    base: SpinnerState,
    is_thinking: bool,
    width: u16,
) -> HeightUpdateStats {
    let _t =
        app.perf.as_ref().map(|p| p.start_with("chat::update_heights", "msgs", app.messages.len()));
    let msg_count = app.messages.len();
    let is_streaming = matches!(app.status, AppStatus::Thinking | AppStatus::Running);
    let width_valid = app.viewport.message_heights_width == width;
    let mut stats = HeightUpdateStats::default();
    for i in (0..msg_count).rev() {
        let is_last = i + 1 == msg_count;
        if width_valid && app.viewport.message_height(i) > 0 && !(is_last && is_streaming) {
            stats.reused_msgs = i + 1;
            break;
        }
        let sp = msg_spinner(base, i, msg_count, is_thinking, &app.messages[i]);
        let (h, rendered_lines) = measure_message_height(&mut app.messages[i], &sp, width);
        stats.measured_msgs += 1;
        stats.measured_lines += rendered_lines;

        app.viewport.set_message_height(i, h);
    }
    app.viewport.mark_heights_valid();
    stats
}

/// Measure message height using ground truth: render the message into a scratch
/// buffer and call `Paragraph::line_count(width)`.
///
/// This uses the exact same code path as actual rendering (`render_message()`),
/// so heights can never diverge from what appears on screen. The scratch vec is
/// temporary and discarded after measurement. Block-level caches are still
/// populated as a side effect (via `render_text_cached` / `render_tool_call_cached`),
/// so completed blocks remain O(1) on subsequent calls.
fn measure_message_height(
    msg: &mut crate::app::ChatMessage,
    spinner: &SpinnerState,
    width: u16,
) -> (usize, usize) {
    let _t = crate::perf::start_with("chat::measure_msg", "blocks", msg.blocks.len());
    let (h, wrapped_lines) = message::measure_message_height_cached(msg, spinner, width);
    crate::perf::mark_with("chat::measure_msg_wrapped_lines", "lines", wrapped_lines);
    (h, wrapped_lines)
}

/// Long content: smooth scroll + viewport culling.
#[allow(
    clippy::cast_possible_truncation,
    clippy::too_many_arguments,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn render_scrolled(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    base: SpinnerState,
    is_thinking: bool,
    width: u16,
    content_height: usize,
    viewport_height: usize,
) {
    let _t = app.perf.as_ref().map(|p| p.start("chat::render_scrolled"));
    let vp = &mut app.viewport;
    let max_scroll = content_height.saturating_sub(viewport_height);
    if vp.auto_scroll {
        vp.scroll_target = max_scroll;
        // Auto-scroll should stay pinned to the latest content without easing lag.
        vp.scroll_pos = vp.scroll_target as f32;
    }
    vp.scroll_target = vp.scroll_target.min(max_scroll);

    if !vp.auto_scroll {
        let target = vp.scroll_target as f32;
        let delta = target - vp.scroll_pos;
        if delta.abs() < 0.01 {
            vp.scroll_pos = target;
        } else {
            vp.scroll_pos += delta * 0.3;
        }
    }
    vp.scroll_offset = vp.scroll_pos.round() as usize;
    if vp.scroll_offset >= max_scroll {
        vp.auto_scroll = true;
    }

    let scroll_offset = vp.scroll_offset;
    crate::perf::mark_with("chat::max_scroll", "rows", max_scroll);
    crate::perf::mark_with("chat::scroll_offset", "rows", scroll_offset);

    let mut all_lines = Vec::new();
    let render_stats = {
        let _t = app
            .perf
            .as_ref()
            .map(|p| p.start_with("chat::render_msgs", "msgs", app.messages.len()));
        render_culled_messages(
            app,
            base,
            is_thinking,
            width,
            scroll_offset,
            viewport_height,
            &mut all_lines,
        )
    };
    crate::perf::mark_with("chat::render_scrolled_lines", "lines", all_lines.len());
    crate::perf::mark_with("chat::render_scrolled_msgs", "msgs", render_stats.rendered_msgs);
    crate::perf::mark_with(
        "chat::render_scrolled_first_visible",
        "idx",
        render_stats.first_visible,
    );
    crate::perf::mark_with("chat::render_scrolled_start", "idx", render_stats.render_start);

    let paragraph = {
        let _t = app
            .perf
            .as_ref()
            .map(|p| p.start_with("chat::paragraph_build", "lines", all_lines.len()));
        Paragraph::new(Text::from(all_lines)).wrap(Wrap { trim: false })
    };

    app.rendered_chat_area = area;
    if app.selection.is_some_and(|s| s.dragging) {
        let _t = app.perf.as_ref().map(|p| p.start("chat::selection_capture"));
        app.rendered_chat_lines =
            render_lines_from_paragraph(&paragraph, area, render_stats.local_scroll);
    }
    {
        let _t = app
            .perf
            .as_ref()
            .map(|p| p.start_with("chat::render_widget", "scroll", render_stats.local_scroll));
        frame.render_widget(paragraph.scroll((render_stats.local_scroll as u16, 0)), area);
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
    scroll: usize,
    viewport_height: usize,
    out: &mut Vec<Line<'static>>,
) -> CulledRenderStats {
    let msg_count = app.messages.len();

    // O(log n) binary search via prefix sums to find first visible message.
    let first_visible = app.viewport.find_first_visible(scroll);

    // Apply margin: render a few extra messages above/below for safety
    let render_start = first_visible.saturating_sub(CULLING_MARGIN);

    // O(1) cumulative height lookup via prefix sums
    let height_before_start = app.viewport.cumulative_height_before(render_start);

    // Render messages from render_start onward, stopping when we have enough
    let lines_needed = (scroll - height_before_start) + viewport_height + 100;
    crate::perf::mark_with("chat::cull_lines_needed", "lines", lines_needed);
    let mut rendered_msgs = 0usize;
    let mut local_scroll = scroll.saturating_sub(height_before_start);
    let mut consume_skip_in_messages = true;
    for i in render_start..msg_count {
        let sp = msg_spinner(base, i, msg_count, is_thinking, &app.messages[i]);
        let before = out.len();
        if local_scroll > 0 && consume_skip_in_messages {
            let rem = message::render_message_from_offset(
                &mut app.messages[i],
                &sp,
                width,
                local_scroll,
                out,
            );
            // If we rendered part of this message and still have remaining rows,
            // the remainder is intra-block and must be applied once via
            // `Paragraph::scroll()`, not consumed again by later messages.
            if rem > 0 && out.len() > before {
                consume_skip_in_messages = false;
            }
            local_scroll = rem;
        } else {
            message::render_message(&mut app.messages[i], &sp, width, out);
        }
        if out.len() > before {
            rendered_msgs += 1;
        }
        if out.len() > lines_needed {
            break;
        }
    }

    CulledRenderStats { local_scroll, first_visible, render_start, rendered_msgs }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::cast_sign_loss)]
pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let _t = app.perf.as_ref().map(|p| p.start("chat::render"));
    crate::perf::mark_with("chat::message_count", "msgs", app.messages.len());
    let is_thinking = matches!(app.status, AppStatus::Thinking);
    let width = area.width;

    let base_spinner = SpinnerState {
        frame: app.spinner_frame,
        is_active: matches!(app.status, AppStatus::Thinking | AppStatus::Running),
        is_last_message: false,
        is_thinking_mid_turn: false,
    };

    // Detect width change and invalidate layout caches
    {
        let _t = app.perf.as_ref().map(|p| p.start("chat::on_frame"));
        app.viewport.on_frame(width);
    }

    // Update per-message visual heights
    let height_stats = update_visual_heights(app, base_spinner, is_thinking, width);
    crate::perf::mark_with(
        "chat::update_heights_measured_msgs",
        "msgs",
        height_stats.measured_msgs,
    );
    crate::perf::mark_with("chat::update_heights_reused_msgs", "msgs", height_stats.reused_msgs);
    crate::perf::mark_with(
        "chat::update_heights_measured_lines",
        "lines",
        height_stats.measured_lines,
    );

    // Rebuild prefix sums (O(1) fast path when only last message changed)
    {
        let _t = app.perf.as_ref().map(|p| p.start("chat::prefix_sums"));
        app.viewport.rebuild_prefix_sums();
    }

    // O(1) via prefix sums instead of O(n) sum every frame
    let content_height: usize = app.viewport.total_message_height();
    let viewport_height = area.height as usize;
    crate::perf::mark_with("chat::content_height", "rows", content_height);
    crate::perf::mark_with("chat::viewport_height", "rows", viewport_height);
    crate::perf::mark_with(
        "chat::content_overflow_rows",
        "rows",
        content_height.saturating_sub(viewport_height),
    );

    tracing::trace!(
        "RENDER: width={}, content_height={}, viewport_height={}, scroll_target={}, auto_scroll={}",
        width,
        content_height,
        viewport_height,
        app.viewport.scroll_target,
        app.viewport.auto_scroll
    );

    if content_height <= viewport_height {
        crate::perf::mark_with("chat::path_short", "active", 1);
    } else {
        crate::perf::mark_with("chat::path_scrolled", "active", 1);
    }

    render_scrolled(
        frame,
        area,
        app,
        base_spinner,
        is_thinking,
        width,
        content_height,
        viewport_height,
    );

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
