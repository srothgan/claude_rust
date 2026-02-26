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

use crate::app::{
    BlockCache, ChatMessage, IncrementalMarkdown, MessageBlock, MessageRole, WelcomeBlock,
};
use crate::ui::tables;
use crate::ui::theme;
use crate::ui::tool_call;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Wrap};
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_width::UnicodeWidthStr;

const SPINNER_FRAMES: &[char] = &[
    '\u{280B}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283C}', '\u{2834}', '\u{2826}', '\u{2827}',
    '\u{2807}', '\u{280F}',
];

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

/// Snapshot of the app state needed by the spinner -- extracted before
/// the message loop so we don't need `&App` (which conflicts with `&mut msg`).
#[derive(Clone, Copy)]
#[allow(clippy::struct_excessive_bools)]
pub struct SpinnerState {
    pub frame: usize,
    pub is_active: bool,
    /// True when this is the last message in the conversation.
    /// Thinking spinners only render on the last assistant message.
    pub is_last_message: bool,
    /// True when the agent is thinking mid-turn (all tool calls finished,
    /// waiting for next action). Shows a trailing spinner after existing blocks.
    pub is_thinking_mid_turn: bool,
    /// True while the SDK reports active compaction.
    pub is_compacting: bool,
}

fn format_scaled_count(value: u64, divisor: u64, suffix: char) -> String {
    // Integer arithmetic avoids precision-loss casts and keeps stable output.
    let scaled_tenths = (u128::from(value) * 10 + u128::from(divisor / 2)) / u128::from(divisor);
    let whole = scaled_tenths / 10;
    let frac = scaled_tenths % 10;
    if frac == 0 { format!("{whole}{suffix}") } else { format!("{whole}.{frac}{suffix}") }
}

fn format_token_count(value: u64) -> String {
    if value >= 1_000_000 {
        format_scaled_count(value, 1_000_000, 'M')
    } else if value >= 1_000 {
        format_scaled_count(value, 1_000, 'k')
    } else {
        value.to_string()
    }
}

fn format_turn_usage(usage: &crate::app::MessageUsage) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let turn_tokens =
        usage.input_tokens.unwrap_or(0).saturating_add(usage.output_tokens.unwrap_or(0));
    if turn_tokens > 0 {
        parts.push(format!("{} tok", format_token_count(turn_tokens)));
    }
    if let Some(cost) = usage.turn_cost_usd {
        parts.push(format!("${cost:.2}"));
    }
    if parts.is_empty() { None } else { Some(parts.join(" / ")) }
}

fn assistant_role_label_line(msg: &ChatMessage, spinner: &SpinnerState) -> Line<'static> {
    let mut spans = vec![Span::styled(
        "Claude",
        Style::default().fg(theme::ROLE_ASSISTANT).add_modifier(Modifier::BOLD),
    )];

    if let Some(usage) = msg.usage.as_ref()
        && let Some(text) = format_turn_usage(usage)
    {
        spans.push(Span::styled(
            format!("  ({text})"),
            Style::default().fg(theme::DIM).add_modifier(Modifier::ITALIC),
        ));
    }

    if spinner.is_compacting && spinner.is_last_message {
        let ch = SPINNER_FRAMES[spinner.frame % SPINNER_FRAMES.len()];
        spans.push(Span::styled(format!("  {ch} Compacting..."), Style::default().fg(theme::DIM)));
    }

    Line::from(spans)
}

/// Render a single chat message into a `Vec<Line>`, using per-block caches.
/// Takes `&mut` so block caches can be updated.
/// `spinner` is only used for the "Thinking..." animation on empty assistant messages.
pub fn render_message(
    msg: &mut ChatMessage,
    spinner: &SpinnerState,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    match msg.role {
        MessageRole::Welcome => {
            out.push(role_label_line(&msg.role));
            for block in &mut msg.blocks {
                if let MessageBlock::Welcome(welcome) = block {
                    render_welcome_cached(welcome, width, out);
                }
            }
        }
        MessageRole::User => {
            // "User" label in gray bold
            out.push(Line::from(Span::styled(
                "User",
                Style::default().fg(theme::DIM).add_modifier(Modifier::BOLD),
            )));

            // User message: markdown-rendered with background overlay
            for block in &mut msg.blocks {
                if let MessageBlock::Text(text, cache, incr) = block {
                    render_text_cached(
                        text,
                        cache,
                        incr,
                        width,
                        Some(theme::USER_MSG_BG),
                        true,
                        out,
                    );
                }
            }
        }
        MessageRole::Assistant => {
            out.push(assistant_role_label_line(msg, spinner));

            // Empty blocks + thinking = show spinner (only on the last message)
            if msg.blocks.is_empty() && spinner.is_active && spinner.is_last_message {
                let ch = SPINNER_FRAMES[spinner.frame % SPINNER_FRAMES.len()];
                out.push(Line::from(Span::styled(
                    format!("{ch} Thinking..."),
                    Style::default().fg(theme::DIM),
                )));
                out.push(Line::default());
                return;
            }

            // Render blocks in order with spacing at text<->tool transitions
            let mut prev_was_tool = false;
            for block in &mut msg.blocks {
                match block {
                    MessageBlock::Text(text, cache, incr) => {
                        // Add half-spacing when transitioning from tools back to text
                        if prev_was_tool {
                            out.push(Line::default());
                        }
                        render_text_cached(text, cache, incr, width, None, false, out);
                        prev_was_tool = false;
                    }
                    MessageBlock::ToolCall(tc) => {
                        let tc = tc.as_mut();
                        // Skip hidden tool calls (subagent children)
                        if tc.hidden {
                            continue;
                        }
                        // Add half-spacing when transitioning from text to tools
                        if !prev_was_tool && out.len() > 1 {
                            out.push(Line::default());
                        }
                        tool_call::render_tool_call_cached(tc, width, spinner.frame, out);
                        prev_was_tool = true;
                    }
                    MessageBlock::Welcome(_) => {}
                }
            }

            // Trailing "Thinking..." spinner when all tool calls finished mid-turn
            if spinner.is_thinking_mid_turn {
                out.push(Line::default());
                let ch = SPINNER_FRAMES[spinner.frame % SPINNER_FRAMES.len()];
                out.push(Line::from(Span::styled(
                    format!("{ch} Thinking..."),
                    Style::default().fg(theme::DIM),
                )));
            }
        }
        MessageRole::System => {
            out.push(Line::from(Span::styled(
                "System",
                Style::default().fg(theme::STATUS_ERROR).add_modifier(Modifier::BOLD),
            )));

            for block in &mut msg.blocks {
                if let MessageBlock::Text(text, cache, incr) = block {
                    let mut lines = Vec::new();
                    render_text_cached(text, cache, incr, width, None, false, &mut lines);
                    tint_lines(&mut lines, theme::STATUS_ERROR);
                    out.extend(lines);
                }
            }
        }
    }

    // Blank separator between messages
    out.push(Line::default());
}

/// Measure message height from block caches + width-aware wrapped heights.
/// Returns `(visual_height_rows, lines_wrapped_for_height_updates)`.
///
/// Accuracy is preserved because each block height is computed with
/// `Paragraph::line_count(width)` on the exact rendered `Vec<Line>`.
pub fn measure_message_height_cached(
    msg: &mut ChatMessage,
    spinner: &SpinnerState,
    width: u16,
) -> (usize, usize) {
    let mut height = 1usize; // role label
    let mut wrapped_lines = 0usize;

    match msg.role {
        MessageRole::User => {
            for block in &mut msg.blocks {
                if let MessageBlock::Text(text, cache, incr) = block {
                    let (h, lines) = text_block_height_cached(
                        text,
                        cache,
                        incr,
                        width,
                        Some(theme::USER_MSG_BG),
                        true,
                    );
                    height += h;
                    wrapped_lines += lines;
                }
            }
        }
        MessageRole::Welcome => {
            for block in &mut msg.blocks {
                if let MessageBlock::Welcome(welcome) = block {
                    let (h, lines) = welcome_block_height_cached(welcome, width);
                    height += h;
                    wrapped_lines += lines;
                }
            }
        }
        MessageRole::Assistant => {
            if msg.blocks.is_empty() && spinner.is_active && spinner.is_last_message {
                // "Thinking..." line + trailing message separator
                return (height + 2, wrapped_lines);
            }

            let mut prev_was_tool = false;
            let mut lines_after_label = 0usize;
            for block in &mut msg.blocks {
                match block {
                    MessageBlock::Text(text, cache, incr) => {
                        if prev_was_tool {
                            height += 1;
                            lines_after_label += 1;
                        }
                        let (h, lines) =
                            text_block_height_cached(text, cache, incr, width, None, false);
                        height += h;
                        lines_after_label += h;
                        wrapped_lines += lines;
                        prev_was_tool = false;
                    }
                    MessageBlock::ToolCall(tc) => {
                        let tc = tc.as_mut();
                        if tc.hidden {
                            continue;
                        }
                        if !prev_was_tool && lines_after_label > 0 {
                            height += 1;
                            lines_after_label += 1;
                        }
                        let (h, lines) =
                            tool_call::measure_tool_call_height_cached(tc, width, spinner.frame);
                        height += h;
                        lines_after_label += h;
                        wrapped_lines += lines;
                        prev_was_tool = true;
                    }
                    MessageBlock::Welcome(_) => {}
                }
            }

            if spinner.is_thinking_mid_turn {
                // Blank line + "Thinking..."
                height += 2;
            }
        }
        MessageRole::System => {
            for block in &mut msg.blocks {
                if let MessageBlock::Text(text, cache, incr) = block {
                    let (h, lines) =
                        text_block_height_cached(text, cache, incr, width, None, false);
                    height += h;
                    wrapped_lines += lines;
                }
            }
        }
    }

    // Blank separator between messages
    (height + 1, wrapped_lines)
}

/// Render a message while consuming as many whole leading rows as possible.
///
/// `skip_rows` is measured in wrapped visual rows. We skip entire structural parts
/// (label/separators/full blocks) without rendering them. If skipping lands inside
/// a block, that block is rendered in full and the remaining skip is returned so
/// the caller can apply `Paragraph::scroll()` for exact intra-block offset.
#[allow(clippy::too_many_lines)]
pub fn render_message_from_offset(
    msg: &mut ChatMessage,
    spinner: &SpinnerState,
    width: u16,
    skip_rows: usize,
    out: &mut Vec<Line<'static>>,
) -> usize {
    let mut remaining_skip = skip_rows;
    let mut can_consume_skip = true;

    let role_line = if matches!(msg.role, MessageRole::Assistant) {
        assistant_role_label_line(msg, spinner)
    } else {
        role_label_line(&msg.role)
    };
    emit_line_with_skip(role_line, out, &mut remaining_skip, can_consume_skip);

    match msg.role {
        MessageRole::Welcome => {
            for block in &mut msg.blocks {
                if let MessageBlock::Welcome(welcome) = block {
                    let (h, _) = welcome_block_height_cached(welcome, width);
                    let mut render = |dst: &mut Vec<Line<'static>>| {
                        render_welcome_cached(welcome, width, dst);
                    };
                    if should_skip_whole_block(h, &mut remaining_skip, &mut can_consume_skip) {
                        continue;
                    }
                    render(out);
                }
            }
        }
        MessageRole::User => {
            for block in &mut msg.blocks {
                if let MessageBlock::Text(text, cache, incr) = block {
                    let (h, _) = text_block_height_cached(
                        text,
                        cache,
                        incr,
                        width,
                        Some(theme::USER_MSG_BG),
                        true,
                    );
                    let mut render = |dst: &mut Vec<Line<'static>>| {
                        render_text_cached(
                            text,
                            cache,
                            incr,
                            width,
                            Some(theme::USER_MSG_BG),
                            true,
                            dst,
                        );
                    };
                    if should_skip_whole_block(h, &mut remaining_skip, &mut can_consume_skip) {
                        continue;
                    }
                    render(out);
                }
            }
        }
        MessageRole::Assistant => {
            if msg.blocks.is_empty() && spinner.is_active && spinner.is_last_message {
                emit_line_with_skip(
                    thinking_line(spinner.frame),
                    out,
                    &mut remaining_skip,
                    can_consume_skip,
                );
                emit_line_with_skip(Line::default(), out, &mut remaining_skip, can_consume_skip);
                return remaining_skip;
            }

            let mut prev_was_tool = false;
            let mut lines_after_label = 0usize;
            for block in &mut msg.blocks {
                match block {
                    MessageBlock::Text(text, cache, incr) => {
                        if prev_was_tool {
                            emit_line_with_skip(
                                Line::default(),
                                out,
                                &mut remaining_skip,
                                can_consume_skip,
                            );
                            lines_after_label += 1;
                        }
                        let (h, _) =
                            text_block_height_cached(text, cache, incr, width, None, false);
                        let mut render = |dst: &mut Vec<Line<'static>>| {
                            render_text_cached(text, cache, incr, width, None, false, dst);
                        };
                        if !should_skip_whole_block(h, &mut remaining_skip, &mut can_consume_skip) {
                            render(out);
                        }
                        lines_after_label += h;
                        prev_was_tool = false;
                    }
                    MessageBlock::ToolCall(tc) => {
                        let tc = tc.as_mut();
                        if tc.hidden {
                            continue;
                        }
                        if !prev_was_tool && lines_after_label > 0 {
                            emit_line_with_skip(
                                Line::default(),
                                out,
                                &mut remaining_skip,
                                can_consume_skip,
                            );
                            lines_after_label += 1;
                        }
                        let (h, _) =
                            tool_call::measure_tool_call_height_cached(tc, width, spinner.frame);
                        let mut render = |dst: &mut Vec<Line<'static>>| {
                            tool_call::render_tool_call_cached(tc, width, spinner.frame, dst);
                        };
                        if !should_skip_whole_block(h, &mut remaining_skip, &mut can_consume_skip) {
                            render(out);
                        }
                        lines_after_label += h;
                        prev_was_tool = true;
                    }
                    MessageBlock::Welcome(_) => {}
                }
            }

            if spinner.is_thinking_mid_turn {
                emit_line_with_skip(Line::default(), out, &mut remaining_skip, can_consume_skip);
                emit_line_with_skip(
                    thinking_line(spinner.frame),
                    out,
                    &mut remaining_skip,
                    can_consume_skip,
                );
            }
        }
        MessageRole::System => {
            for block in &mut msg.blocks {
                if let MessageBlock::Text(text, cache, incr) = block {
                    let (h, _) = text_block_height_cached(text, cache, incr, width, None, false);
                    let mut render = |dst: &mut Vec<Line<'static>>| {
                        let mut lines = Vec::new();
                        render_text_cached(text, cache, incr, width, None, false, &mut lines);
                        tint_lines(&mut lines, theme::STATUS_ERROR);
                        dst.extend(lines);
                    };
                    if !should_skip_whole_block(h, &mut remaining_skip, &mut can_consume_skip) {
                        render(out);
                    }
                }
            }
        }
    }

    emit_line_with_skip(Line::default(), out, &mut remaining_skip, can_consume_skip);
    remaining_skip
}

fn emit_line_with_skip(
    line: Line<'static>,
    out: &mut Vec<Line<'static>>,
    remaining_skip: &mut usize,
    can_consume_skip: bool,
) {
    if can_consume_skip && *remaining_skip > 0 {
        *remaining_skip -= 1;
    } else {
        out.push(line);
    }
}

fn should_skip_whole_block(
    block_h: usize,
    remaining_skip: &mut usize,
    can_consume_skip: &mut bool,
) -> bool {
    if !*can_consume_skip {
        return false;
    }
    if *remaining_skip >= block_h {
        *remaining_skip -= block_h;
        return true;
    }
    if *remaining_skip > 0 {
        // We have to render this block, but keep the remaining intra-block skip
        // for Paragraph::scroll().
        *can_consume_skip = false;
    }
    false
}

fn role_label_line(role: &MessageRole) -> Line<'static> {
    match role {
        MessageRole::Welcome => Line::from(Span::styled(
            "Overview",
            Style::default().fg(theme::RUST_ORANGE).add_modifier(Modifier::BOLD),
        )),
        MessageRole::User => Line::from(Span::styled(
            "User",
            Style::default().fg(theme::DIM).add_modifier(Modifier::BOLD),
        )),
        MessageRole::Assistant => Line::from(Span::styled(
            "Claude",
            Style::default().fg(theme::ROLE_ASSISTANT).add_modifier(Modifier::BOLD),
        )),
        MessageRole::System => Line::from(Span::styled(
            "System",
            Style::default().fg(theme::STATUS_ERROR).add_modifier(Modifier::BOLD),
        )),
    }
}

fn thinking_line(frame: usize) -> Line<'static> {
    let ch = SPINNER_FRAMES[frame % SPINNER_FRAMES.len()];
    Line::from(Span::styled(format!("{ch} Thinking..."), Style::default().fg(theme::DIM)))
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

fn parse_date_ymd(raw: &str) -> Option<(i32, u32, u32)> {
    let mut parts = raw.split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) {
        return None;
    }
    let max_day = days_in_month(year, month);
    if day == 0 || day > max_day {
        return None;
    }
    Some((year, month, day))
}

fn parse_time_hms(raw: &str) -> Option<(u32, u32, u32)> {
    let mut parts = raw.split(':');
    let hour: u32 = parts.next()?.parse().ok()?;
    let minute: u32 = parts.next()?.parse().ok()?;
    let sec_part = parts.next().unwrap_or("0");
    if parts.next().is_some() {
        return None;
    }
    let second_str = sec_part.split('.').next().unwrap_or(sec_part);
    let second: u32 = second_str.parse().ok()?;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some((hour, minute, second))
}

fn parse_timezone_offset_seconds(raw: &str) -> Option<i64> {
    if raw.eq_ignore_ascii_case("Z") {
        return Some(0);
    }
    if raw.len() != 6 || (!raw.starts_with('+') && !raw.starts_with('-')) {
        return None;
    }
    let bytes = raw.as_bytes();
    if bytes.get(3).copied() != Some(b':') {
        return None;
    }
    let hours: i64 = raw[1..3].parse().ok()?;
    let minutes: i64 = raw[4..6].parse().ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    let sign = if raw.starts_with('-') { -1 } else { 1 };
    Some(sign * (hours * 3600 + minutes * 60))
}

fn days_since_unix_epoch(year: i32, month: u32, day: u32) -> Option<i64> {
    let month_i32 = i32::try_from(month).ok()?;
    let day_i32 = i32::try_from(day).ok()?;
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = month_i32 + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day_i32 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(i64::from(era) * 146_097 + i64::from(doe) - 719_468)
}

fn parse_timestamp_epoch_seconds(raw: &str) -> Option<i64> {
    let trimmed = raw.trim();
    let (date_raw, time_and_zone_raw) =
        trimmed.split_once('T').or_else(|| trimmed.split_once(' '))?;
    let (year, month, day) = parse_date_ymd(date_raw)?;

    let tz_split = time_and_zone_raw
        .char_indices()
        .find(|(idx, ch)| *idx >= 5 && matches!(ch, 'Z' | 'z' | '+' | '-'))
        .map(|(idx, _)| idx);

    let (time_raw, tz_raw) = match tz_split {
        Some(idx) => (&time_and_zone_raw[..idx], Some(&time_and_zone_raw[idx..])),
        None => (time_and_zone_raw, None),
    };
    let (hour, minute, second) = parse_time_hms(time_raw)?;
    let tz_offset = tz_raw.map_or(Some(0), parse_timezone_offset_seconds)?;

    let days = days_since_unix_epoch(year, month, day)?;
    let seconds_in_day = i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second);
    days.checked_mul(86_400)?.checked_add(seconds_in_day)?.checked_sub(tz_offset)
}

fn now_epoch_seconds() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}

fn format_relative_age(epoch_seconds: i64, now_seconds: i64) -> String {
    let delta_seconds = if now_seconds >= epoch_seconds {
        now_seconds - epoch_seconds
    } else {
        epoch_seconds - now_seconds
    };

    if delta_seconds < 5 * 60 {
        return "<5m".to_owned();
    }
    if delta_seconds < 60 * 60 {
        let minutes = delta_seconds / 60;
        return format!("{minutes}m");
    }
    if delta_seconds < 24 * 60 * 60 {
        let hours = delta_seconds / (60 * 60);
        return format!("{hours}h");
    }

    let total_hours = delta_seconds / (60 * 60);
    let days = total_hours / 24;
    let hours = total_hours % 24;
    format!("{days}d {hours}h")
}

fn format_recent_updated_at(raw: Option<&str>) -> String {
    let Some(ts) = raw else {
        return "--".to_owned();
    };
    let Some(epoch_seconds) = parse_timestamp_epoch_seconds(ts) else {
        return ts.to_owned();
    };
    format_relative_age(epoch_seconds, now_epoch_seconds())
}

fn truncate_recent_title(title: &str, max_chars: usize) -> String {
    let count = title.chars().count();
    if count <= max_chars {
        return title.to_owned();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    let keep = max_chars - 3;
    let mut out: String = title.chars().take(keep).collect();
    out.push_str("...");
    out
}

fn welcome_recent_lines(block: &WelcomeBlock) -> Vec<String> {
    if block.recent_sessions.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::with_capacity(block.recent_sessions.len().min(5) * 2 + 1);
    lines.push("Recent sessions:".to_owned());
    for session in block.recent_sessions.iter().take(5) {
        let time = format_recent_updated_at(session.updated_at.as_deref());
        let title = session.title.as_deref().map_or("", str::trim);
        let title = if title.is_empty() {
            "(no message)".to_owned()
        } else {
            truncate_recent_title(title, 56)
        };
        lines.push(format!("{time} - {title}"));
        lines.push(session.session_id.clone());
    }
    lines
}

fn two_column_line(
    left: String,
    left_style: Style,
    right: Option<String>,
    right_style: Style,
    right_start_col: usize,
) -> Line<'static> {
    let Some(right_text) = right else {
        return Line::from(Span::styled(left, left_style));
    };
    let left_w = UnicodeWidthStr::width(left.as_str());
    if left_w + 2 >= right_start_col {
        return Line::from(Span::styled(left, left_style));
    }
    let gap = " ".repeat(right_start_col - left_w);
    Line::from(vec![
        Span::styled(left, left_style),
        Span::raw(gap),
        Span::styled(right_text, right_style),
    ])
}

fn welcome_lines(block: &WelcomeBlock, width: u16) -> Vec<Line<'static>> {
    let pad = "  ";
    let mut lines = Vec::new();
    let right_lines = welcome_recent_lines(block);
    let can_render_right = !right_lines.is_empty() && width >= 90;
    let right_column_start = ((width as usize) * 56 / 100).clamp(50, 72);
    let recent_style = Style::default().fg(Color::Rgb(205, 205, 205));
    let recent_header_style = recent_style.add_modifier(Modifier::BOLD);

    for (idx, art_line) in FERRIS_SAYS.iter().enumerate() {
        let right = if can_render_right { right_lines.get(idx).cloned() } else { None };
        let right_style = if idx == 0 { recent_header_style } else { recent_style };
        lines.push(two_column_line(
            format!("{pad}{art_line}"),
            Style::default().fg(theme::RUST_ORANGE),
            right,
            right_style,
            right_column_start,
        ));
    }
    if can_render_right && right_lines.len() > FERRIS_SAYS.len() {
        for row in right_lines.iter().skip(FERRIS_SAYS.len()) {
            lines.push(two_column_line(
                pad.to_owned(),
                Style::default(),
                Some(row.clone()),
                recent_style,
                right_column_start,
            ));
        }
    }

    lines.push(Line::default());
    lines.push(Line::default());

    lines.push(Line::from(vec![
        Span::styled(format!("{pad}Model: "), Style::default().fg(theme::DIM)),
        Span::styled(
            block.model_name.clone(),
            Style::default().fg(theme::RUST_ORANGE).add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        format!("{pad}cwd:   {}", block.cwd),
        Style::default().fg(theme::DIM),
    )));

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        format!(
            "{pad}Tips: Enter to send, Shift+Enter for newline, Ctrl+C copies selection or quits"
        ),
        Style::default().fg(theme::DIM),
    )));

    if !can_render_right && !right_lines.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(format!("{pad}Recent sessions:"), recent_header_style)));
        for row in right_lines.into_iter().skip(1).take(10) {
            lines.push(Line::from(Span::styled(format!("{pad}{row}"), recent_style)));
        }
    }
    lines.push(Line::default());

    lines
}

fn render_welcome_cached(block: &mut WelcomeBlock, width: u16, out: &mut Vec<Line<'static>>) {
    if let Some(cached_lines) = block.cache.get() {
        out.extend_from_slice(cached_lines);
        return;
    }

    let fresh = welcome_lines(block, width);
    let h = {
        let _t = crate::perf::start_with("msg::wrap_height", "lines", fresh.len());
        Paragraph::new(Text::from(fresh.clone())).wrap(Wrap { trim: false }).line_count(width)
    };
    block.cache.store(fresh);
    block.cache.set_height(h, width);
    if let Some(stored) = block.cache.get() {
        out.extend_from_slice(stored);
    }
}

fn welcome_block_height_cached(block: &mut WelcomeBlock, width: u16) -> (usize, usize) {
    if let Some(h) = block.cache.height_at(width) {
        return (h, 0);
    }

    if let Some(cached_lines) = block.cache.get().cloned() {
        let h = Paragraph::new(Text::from(cached_lines.clone()))
            .wrap(Wrap { trim: false })
            .line_count(width);
        block.cache.set_height(h, width);
        return (h, cached_lines.len());
    }

    let fresh = welcome_lines(block, width);
    let lines = fresh.len();
    let h = Paragraph::new(Text::from(fresh.clone())).wrap(Wrap { trim: false }).line_count(width);
    block.cache.store(fresh);
    block.cache.set_height(h, width);
    (h, lines)
}

fn text_block_height_cached(
    text: &str,
    cache: &mut BlockCache,
    incr: &mut IncrementalMarkdown,
    width: u16,
    bg: Option<Color>,
    preserve_newlines: bool,
) -> (usize, usize) {
    if let Some(h) = cache.height_at(width) {
        return (h, 0);
    }

    if let Some(cached_lines) = cache.get().cloned() {
        let h = Paragraph::new(Text::from(cached_lines.clone()))
            .wrap(Wrap { trim: false })
            .line_count(width);
        cache.set_height(h, width);
        return (h, cached_lines.len());
    }

    let mut scratch = Vec::new();
    render_text_cached(text, cache, incr, width, bg, preserve_newlines, &mut scratch);

    if let Some(h) = cache.height_at(width) {
        return (h, scratch.len());
    }

    let h =
        Paragraph::new(Text::from(scratch.clone())).wrap(Wrap { trim: false }).line_count(width);
    cache.set_height(h, width);
    (h, scratch.len())
}

fn tint_lines(lines: &mut [Line<'static>], color: Color) {
    for line in lines {
        for span in &mut line.spans {
            span.style = span.style.fg(color);
        }
    }
}

/// Preprocess markdown that `tui_markdown` doesn't handle well.
/// Headings (`# Title`) become `**Title**` (bold) with a blank line before.
/// Handles variations: `#Title`, `#  Title`, `  ## Title  `, etc.
/// Links are left as-is -- `tui_markdown` handles `[title](url)` natively.
fn preprocess_markdown(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            // Strip all leading '#' characters
            let after_hashes = trimmed.trim_start_matches('#');
            // Extract heading content (trim spaces between # and text, and trailing)
            let content = after_hashes.trim();
            if !content.is_empty() {
                // Blank line before heading for visual separation
                if !result.is_empty() && !result.ends_with("\n\n") {
                    result.push('\n');
                }
                result.push_str("**");
                result.push_str(content);
                result.push_str("**\n");
                continue;
            }
        }
        result.push_str(line);
        result.push('\n');
    }
    if !text.ends_with('\n') {
        result.pop();
    }
    result
}

/// Render a text block with caching. Uses paragraph-level incremental markdown
/// during streaming to avoid re-parsing the entire text every frame.
///
/// Cache hierarchy:
/// 1. `BlockCache` (full block) -- hit for completed messages (no changes).
/// 2. `IncrementalMarkdown` (per-paragraph) -- only tail paragraph re-parsed during streaming.
pub(super) fn render_text_cached(
    _text: &str,
    cache: &mut BlockCache,
    incr: &mut IncrementalMarkdown,
    width: u16,
    bg: Option<Color>,
    preserve_newlines: bool,
    out: &mut Vec<Line<'static>>,
) {
    // Fast path: full block cache is valid (completed message, no changes)
    if let Some(cached_lines) = cache.get() {
        crate::perf::mark_with("msg::cache_hit", "lines", cached_lines.len());
        out.extend_from_slice(cached_lines);
        return;
    }
    crate::perf::mark("msg::cache_miss");

    let _t = crate::perf::start("msg::render_text");

    // Build a render function that handles preprocessing + tui_markdown
    let render_fn = |src: &str| -> Vec<Line<'static>> {
        let mut preprocessed = preprocess_markdown(src);
        if preserve_newlines {
            preprocessed = force_markdown_line_breaks(&preprocessed);
        }
        tables::render_markdown_with_tables(&preprocessed, width, bg)
    };

    // Ensure any previously invalidated paragraph caches are re-rendered
    incr.ensure_rendered(&render_fn);

    // Render: cached paragraphs + fresh tail
    let fresh = incr.lines(&render_fn);

    // Store in the full block cache with wrapped height.
    // For streaming messages this will be invalidated on the next chunk,
    // but for completed messages it persists.
    let h = {
        let _t = crate::perf::start_with("msg::wrap_height", "lines", fresh.len());
        Paragraph::new(Text::from(fresh.clone())).wrap(Wrap { trim: false }).line_count(width)
    };
    cache.store(fresh);
    cache.set_height(h, width);
    if let Some(stored) = cache.get() {
        out.extend_from_slice(stored);
    }
}

/// Convert single line breaks into hard breaks so user-entered newlines persist.
fn force_markdown_line_breaks(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = String::with_capacity(text.len());
    for (i, line) in lines.iter().enumerate() {
        if !line.is_empty() {
            out.push_str(line);
            out.push_str("  ");
        }
        if i + 1 < lines.len() || text.ends_with('\n') {
            out.push('\n');
        }
    }
    if text.ends_with('\n') {
        // preserve trailing newline
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{
        ChatMessage, IncrementalMarkdown, MessageBlock, RecentSessionInfo, WelcomeBlock,
    };
    use pretty_assertions::assert_eq;
    use ratatui::widgets::{Paragraph, Wrap};

    // preprocess_markdown

    #[test]
    fn preprocess_h1_heading() {
        let result = preprocess_markdown("# Hello");
        assert!(result.contains("**Hello**"));
        assert!(!result.contains('#'));
    }

    #[test]
    fn preprocess_h3_heading() {
        let result = preprocess_markdown("### Deeply Nested");
        assert!(result.contains("**Deeply Nested**"));
    }

    #[test]
    fn preprocess_non_heading_passthrough() {
        let input = "Just normal text\nwith multiple lines";
        let result = preprocess_markdown(input);
        assert_eq!(result, input);
    }

    #[test]
    fn preprocess_mixed_headings_and_text() {
        let input = "# Title\nSome text\n## Subtitle\nMore text";
        let result = preprocess_markdown(input);
        assert!(result.contains("**Title**"));
        assert!(result.contains("Some text"));
        assert!(result.contains("**Subtitle**"));
        assert!(result.contains("More text"));
    }

    #[test]
    fn preprocess_heading_no_space() {
        let result = preprocess_markdown("#Title");
        assert!(result.contains("**Title**"));
    }

    #[test]
    fn preprocess_heading_extra_spaces() {
        let result = preprocess_markdown("#   Spaced Out   ");
        assert!(result.contains("**Spaced Out**"));
    }

    #[test]
    fn preprocess_indented_heading() {
        let result = preprocess_markdown("  ## Indented");
        assert!(result.contains("**Indented**"));
    }

    #[test]
    fn preprocess_empty_heading() {
        let result = preprocess_markdown("# ");
        assert_eq!(result, "# ");
    }

    #[test]
    fn preprocess_empty_string() {
        assert_eq!(preprocess_markdown(""), "");
    }

    #[test]
    fn preprocess_preserves_trailing_newline() {
        let result = preprocess_markdown("hello\n");
        assert!(result.ends_with('\n'));
    }

    #[test]
    fn preprocess_no_trailing_newline() {
        let result = preprocess_markdown("hello");
        assert!(!result.ends_with('\n'));
    }

    #[test]
    fn preprocess_blank_line_before_heading() {
        let input = "text\n\n# Heading";
        let result = preprocess_markdown(input);
        assert!(!result.contains("\n\n\n"));
        assert!(result.contains("**Heading**"));
    }

    #[test]
    fn preprocess_consecutive_headings() {
        let input = "# First\n# Second";
        let result = preprocess_markdown(input);
        assert!(result.contains("**First**"));
        assert!(result.contains("**Second**"));
    }

    #[test]
    fn preprocess_hash_in_code_not_heading() {
        let result = preprocess_markdown("# actual heading");
        assert!(result.contains("**actual heading**"));
    }

    /// H6 heading (6 `#` chars).
    #[test]
    fn preprocess_h6_heading() {
        let result = preprocess_markdown("###### Deep H6");
        assert!(result.contains("**Deep H6**"));
        assert!(!result.contains('#'));
    }

    /// Heading with markdown formatting inside.
    #[test]
    fn preprocess_heading_with_bold_inside() {
        let result = preprocess_markdown("# **bold** and *italic*");
        assert!(result.contains("****bold** and *italic***"));
    }

    /// Heading at end of file with no trailing newline.
    #[test]
    fn preprocess_heading_at_eof_no_newline() {
        let result = preprocess_markdown("text\n# Final");
        assert!(result.contains("**Final**"));
        assert!(!result.ends_with('\n'));
    }

    /// Only hashes with no text: `###` - content after stripping is empty, passthrough.
    #[test]
    fn preprocess_only_hashes() {
        let result = preprocess_markdown("###");
        assert_eq!(result, "###");
    }

    /// Very long heading.
    #[test]
    fn preprocess_very_long_heading() {
        let long_text = "A".repeat(1000);
        let input = format!("# {long_text}");
        let result = preprocess_markdown(&input);
        assert!(result.starts_with("**"));
        assert!(result.contains(&long_text));
    }

    /// Unicode emoji in heading.
    #[test]
    fn preprocess_unicode_heading() {
        let result = preprocess_markdown("# \u{1F680} Launch \u{4F60}\u{597D}");
        assert!(result.contains("**\u{1F680} Launch \u{4F60}\u{597D}**"));
    }

    /// Quoted heading: `> # Heading` - starts with `>` not `#`, so passthrough.
    #[test]
    fn preprocess_blockquote_heading_passthrough() {
        let result = preprocess_markdown("> # Quoted heading");
        // Line starts with `>`, not `#`, so trimmed starts with `>` not `#`
        assert!(!result.contains("**"));
        assert!(result.contains("> # Quoted heading"));
    }

    /// All heading levels in sequence.
    #[test]
    fn preprocess_all_heading_levels() {
        let input = "# H1\n## H2\n### H3\n#### H4\n##### H5\n###### H6";
        let result = preprocess_markdown(input);
        for label in ["H1", "H2", "H3", "H4", "H5", "H6"] {
            assert!(result.contains(&format!("**{label}**")), "missing {label}");
        }
    }

    #[test]
    fn parse_timestamp_epoch_seconds_supports_utc_iso() {
        let parsed = parse_timestamp_epoch_seconds("1970-01-01T00:00:00Z");
        assert_eq!(parsed, Some(0));
    }

    #[test]
    fn parse_timestamp_epoch_seconds_supports_offset_iso() {
        let parsed = parse_timestamp_epoch_seconds("1970-01-01T01:00:00+01:00");
        assert_eq!(parsed, Some(0));
    }

    #[test]
    fn format_relative_age_uses_minutes_hours_and_days_steps() {
        assert_eq!(format_relative_age(1_000, 1_000 + 60), "<5m");
        assert_eq!(format_relative_age(1_000, 1_000 + 5 * 60), "5m");
        assert_eq!(format_relative_age(1_000, 1_000 + 3 * 60 * 60), "3h");
        assert_eq!(format_relative_age(1_000, 1_000 + 49 * 60 * 60), "2d 1h");
    }

    #[test]
    fn welcome_recent_lines_limits_to_five_and_uses_time_then_id_rows() {
        let block = WelcomeBlock {
            model_name: "m".to_owned(),
            cwd: "/cwd".to_owned(),
            recent_sessions: vec![
                RecentSessionInfo {
                    session_id: "11111111-1111-1111-1111-111111111111".to_owned(),
                    cwd: "/a".to_owned(),
                    title: Some("Fix bug".to_owned()),
                    updated_at: None,
                },
                RecentSessionInfo {
                    session_id: "22222222-2222-2222-2222-222222222222".to_owned(),
                    cwd: "/b".to_owned(),
                    title: None,
                    updated_at: None,
                },
                RecentSessionInfo {
                    session_id: "33333333-3333-3333-3333-333333333333".to_owned(),
                    cwd: "/c".to_owned(),
                    title: Some("Refactor".to_owned()),
                    updated_at: None,
                },
                RecentSessionInfo {
                    session_id: "44444444-4444-4444-4444-444444444444".to_owned(),
                    cwd: "/d".to_owned(),
                    title: Some("Fourth".to_owned()),
                    updated_at: None,
                },
                RecentSessionInfo {
                    session_id: "55555555-5555-5555-5555-555555555555".to_owned(),
                    cwd: "/e".to_owned(),
                    title: Some("Fifth".to_owned()),
                    updated_at: None,
                },
                RecentSessionInfo {
                    session_id: "66666666-6666-6666-6666-666666666666".to_owned(),
                    cwd: "/f".to_owned(),
                    title: Some("Should be hidden".to_owned()),
                    updated_at: None,
                },
            ],
            cache: BlockCache::default(),
        };

        let lines = welcome_recent_lines(&block);
        assert_eq!(lines[0], "Recent sessions:");
        assert_eq!(lines[1], "-- - Fix bug");
        assert_eq!(lines[2], "11111111-1111-1111-1111-111111111111");
        assert_eq!(lines[3], "-- - (no message)");
        assert_eq!(lines[4], "22222222-2222-2222-2222-222222222222");
        assert_eq!(lines[5], "-- - Refactor");
        assert_eq!(lines[6], "33333333-3333-3333-3333-333333333333");
        assert_eq!(lines[7], "-- - Fourth");
        assert_eq!(lines[8], "44444444-4444-4444-4444-444444444444");
        assert_eq!(lines[9], "-- - Fifth");
        assert_eq!(lines[10], "55555555-5555-5555-5555-555555555555");
        assert_eq!(lines.len(), 11);
    }

    #[test]
    fn welcome_recent_lines_truncates_long_title() {
        let long_title =
            "A very long title that should be truncated for readability in the overview panel";
        let block = WelcomeBlock {
            model_name: "m".to_owned(),
            cwd: "/cwd".to_owned(),
            recent_sessions: vec![RecentSessionInfo {
                session_id: "11111111-1111-1111-1111-111111111111".to_owned(),
                cwd: "/a".to_owned(),
                title: Some(long_title.to_owned()),
                updated_at: None,
            }],
            cache: BlockCache::default(),
        };

        let lines = welcome_recent_lines(&block);
        assert_eq!(lines[0], "Recent sessions:");
        assert!(lines[1].starts_with("-- - "));
        assert!(lines[1].ends_with("..."));
        assert_eq!(lines[2], "11111111-1111-1111-1111-111111111111");
    }

    // force_markdown_line_breaks

    #[test]
    fn force_breaks_adds_trailing_spaces() {
        let result = force_markdown_line_breaks("line1\nline2");
        assert!(result.contains("line1  \n"));
        assert!(result.contains("line2  "));
    }

    #[test]
    fn force_breaks_preserves_trailing_newline() {
        let result = force_markdown_line_breaks("hello\n");
        assert!(result.ends_with('\n'));
    }

    #[test]
    fn force_breaks_empty_lines_no_trailing_spaces() {
        let result = force_markdown_line_breaks("a\n\nb");
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].ends_with("  "));
        assert_eq!(lines[1], "");
        assert!(lines[2].ends_with("  "));
    }

    #[test]
    fn force_breaks_single_line_no_trailing_newline() {
        let result = force_markdown_line_breaks("hello");
        assert_eq!(result, "hello  ");
    }

    #[test]
    fn force_breaks_many_consecutive_empty_lines() {
        let result = force_markdown_line_breaks("a\n\n\nb");
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.len(), 4);
    }

    /// Empty input.
    #[test]
    fn force_breaks_empty_input() {
        let result = force_markdown_line_breaks("");
        assert_eq!(result, "");
    }

    /// Only empty lines.
    #[test]
    fn force_breaks_only_empty_lines() {
        let result = force_markdown_line_breaks("\n\n\n");
        let lines: Vec<&str> = result.lines().collect();
        // All lines are empty, so no trailing spaces added
        for line in &lines {
            assert!(line.is_empty(), "empty line got content: {line:?}");
        }
    }

    /// Line already ending with two spaces - gets two more.
    #[test]
    fn force_breaks_already_has_trailing_spaces() {
        let result = force_markdown_line_breaks("hello  \nworld");
        // "hello  " + "  " = "hello    "
        assert!(result.starts_with("hello    "));
    }

    /// Single newline (no content).
    #[test]
    fn force_breaks_single_newline() {
        let result = force_markdown_line_breaks("\n");
        // One empty line, should stay empty with trailing newline
        assert_eq!(result, "\n");
    }

    fn make_text_message(role: MessageRole, text: &str) -> ChatMessage {
        ChatMessage {
            role,
            blocks: vec![MessageBlock::Text(
                text.to_owned(),
                BlockCache::default(),
                IncrementalMarkdown::from_complete(text),
            )],
            usage: None,
        }
    }

    fn make_welcome_message(model_name: &str, cwd: &str) -> ChatMessage {
        ChatMessage::welcome(model_name, cwd)
    }

    fn ground_truth_height(msg: &mut ChatMessage, spinner: &SpinnerState, width: u16) -> usize {
        let mut lines = Vec::new();
        render_message(msg, spinner, width, &mut lines);
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }).line_count(width)
    }

    #[test]
    fn measure_height_matches_ground_truth_for_long_soft_wrap() {
        let text = "A".repeat(500);
        let spinner = SpinnerState {
            frame: 0,
            is_active: false,
            is_last_message: false,
            is_thinking_mid_turn: false,
            is_compacting: false,
        };

        let mut measured_msg = make_text_message(MessageRole::User, &text);
        let mut truth_msg = make_text_message(MessageRole::User, &text);

        let (h, _) = measure_message_height_cached(&mut measured_msg, &spinner, 32);
        let truth = ground_truth_height(&mut truth_msg, &spinner, 32);

        assert_eq!(h, truth);
    }

    #[test]
    fn measure_height_matches_ground_truth_after_resize() {
        let text =
            "This is a single very long line without explicit line breaks to stress soft wrapping."
                .repeat(20);
        let spinner = SpinnerState {
            frame: 0,
            is_active: false,
            is_last_message: false,
            is_thinking_mid_turn: false,
            is_compacting: false,
        };

        let mut measured_msg = make_text_message(MessageRole::Assistant, &text);
        let mut truth_wide = make_text_message(MessageRole::Assistant, &text);
        let mut truth_narrow = make_text_message(MessageRole::Assistant, &text);

        let (h_wide, _) = measure_message_height_cached(&mut measured_msg, &spinner, 100);
        let wide_truth = ground_truth_height(&mut truth_wide, &spinner, 100);
        assert_eq!(h_wide, wide_truth);

        // Reuse the same message to hit width-mismatch cache path.
        let (h_narrow, _) = measure_message_height_cached(&mut measured_msg, &spinner, 28);
        let narrow_truth = ground_truth_height(&mut truth_narrow, &spinner, 28);
        assert_eq!(h_narrow, narrow_truth);
    }

    #[test]
    fn render_from_offset_can_skip_entire_message() {
        let spinner = SpinnerState {
            frame: 0,
            is_active: false,
            is_last_message: false,
            is_thinking_mid_turn: false,
            is_compacting: false,
        };
        let mut msg = make_text_message(MessageRole::User, "hello\nworld");
        let mut truth_msg = make_text_message(MessageRole::User, "hello\nworld");
        let total = ground_truth_height(&mut truth_msg, &spinner, 120);

        let mut out = Vec::new();
        let rem = render_message_from_offset(&mut msg, &spinner, 120, total + 3, &mut out);

        assert!(out.is_empty());
        assert_eq!(rem, 3);
    }

    #[test]
    fn welcome_height_matches_ground_truth() {
        let spinner = SpinnerState {
            frame: 0,
            is_active: false,
            is_last_message: false,
            is_thinking_mid_turn: false,
            is_compacting: false,
        };
        let mut measured_msg = make_welcome_message("claude-sonnet-4-5", "~/project");
        let mut truth_msg = make_welcome_message("claude-sonnet-4-5", "~/project");

        let (h, _) = measure_message_height_cached(&mut measured_msg, &spinner, 52);
        let truth = ground_truth_height(&mut truth_msg, &spinner, 52);
        assert_eq!(h, truth);
    }
}
