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
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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

    // Footer: mode/help on the left, optional update hint on the right.
    if let Some(footer_area) = areas.footer {
        let _t = app.perf.as_ref().map(|p| p.start("ui::footer"));
        render_footer(frame, footer_area, app);
    }

    let fps_y = if areas.header.height > 0 { areas.header.y } else { frame_area.y };
    render_perf_fps_overlay(frame, frame_area, fps_y, app);
}

const FOOTER_PAD: u16 = 2;
const FOOTER_COLUMN_GAP: u16 = 1;
type FooterItem = Option<(String, Color)>;
const FOOTER_SPINNER_FRAMES: &[char] = &[
    '\u{280B}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283C}', '\u{2834}', '\u{2826}', '\u{2827}',
    '\u{2807}', '\u{280F}',
];

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
        let (telemetry, update_hint) = footer_right_items(app);
        match (telemetry, update_hint) {
            (Some((telem_text, telem_color)), Some((hint_text, hint_color))) => {
                let (left_area, mid_area, right_area) = split_footer_three_columns(padded);
                frame.render_widget(Paragraph::new(line.clone()), left_area);
                render_footer_right_info(frame, mid_area, &hint_text, hint_color);
                render_footer_right_info(frame, right_area, &telem_text, telem_color);
            }
            (Some((telem_text, telem_color)), None) => {
                let (left_area, right_area) = split_footer_columns(padded);
                frame.render_widget(Paragraph::new(line.clone()), left_area);
                render_footer_right_info(frame, right_area, &telem_text, telem_color);
            }
            (None, Some((hint_text, hint_color))) => {
                let (left_area, right_area) = split_footer_columns(padded);
                frame.render_widget(Paragraph::new(line.clone()), left_area);
                render_footer_right_info(frame, right_area, &hint_text, hint_color);
            }
            (None, None) => {
                frame.render_widget(Paragraph::new(line.clone()), padded);
            }
        }
    }
}

fn context_remaining_percent_rounded(used: u64, window: u64) -> Option<u64> {
    if window == 0 {
        return None;
    }
    let used_percent = (u128::from(used) * 100 + (u128::from(window) / 2)) / u128::from(window);
    Some(100_u64.saturating_sub(used_percent.min(100) as u64))
}

fn context_text(window: Option<u64>, used: Option<u64>, show_new_session_default: bool) -> String {
    if show_new_session_default {
        return "100%".to_owned();
    }

    window
        .zip(used)
        .and_then(|(w, u)| context_remaining_percent_rounded(u, w))
        .map_or_else(|| "-".to_owned(), |percent| format!("{percent}%"))
}

fn footer_telemetry_text(app: &App) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let totals = &app.session_usage;
    let is_new_session = app.session_id.is_some()
        && totals.total_tokens() == 0
        && totals.context_used_tokens().is_none();
    if app.session_id.is_some()
        || totals.context_window.is_some()
        || totals.context_used_tokens().is_some()
    {
        let context_text =
            context_text(totals.context_window, totals.context_used_tokens(), is_new_session);
        parts.push(format!("Context: {context_text}"));
    }

    if parts.is_empty() && !app.is_compacting {
        return None;
    }

    let mut text = parts.join(" | ");
    if app.is_compacting {
        let ch = FOOTER_SPINNER_FRAMES[app.spinner_frame % FOOTER_SPINNER_FRAMES.len()];
        text = if text.is_empty() {
            format!("{ch} Compacting...")
        } else {
            format!("{ch} Compacting...  {text}")
        };
    }
    Some(text)
}

/// Returns `(telemetry, update_hint)` -- either or both may be `None`.
fn footer_right_items(app: &App) -> (FooterItem, FooterItem) {
    let telemetry = footer_telemetry_text(app).map(|text| {
        let color = if app.is_compacting { theme::RUST_ORANGE } else { theme::DIM };
        (text, color)
    });
    let update_hint = app.update_check_hint.as_ref().map(|hint| (hint.clone(), theme::RUST_ORANGE));
    (telemetry, update_hint)
}

fn split_footer_columns(area: Rect) -> (Rect, Rect) {
    if area.width == 0 {
        return (area, Rect { width: 0, ..area });
    }

    let gap = if area.width > 2 { FOOTER_COLUMN_GAP } else { 0 };
    let usable_width = area.width.saturating_sub(gap);
    let left_width = usable_width.saturating_add(1) / 2;
    let right_width = usable_width.saturating_sub(left_width);

    let left = Rect { width: left_width, ..area };
    let right = Rect {
        x: area.x.saturating_add(left_width).saturating_add(gap),
        width: right_width,
        ..area
    };
    (left, right)
}

/// Three-column split: left (mode/shortcuts) | mid (update hint) | right (context/telemetry).
/// Mid and right are each ~quarter width, both intended for right-aligned content.
fn split_footer_three_columns(area: Rect) -> (Rect, Rect, Rect) {
    if area.width == 0 {
        let zero = Rect { width: 0, ..area };
        return (area, zero, zero);
    }

    let gap = if area.width > 4 { FOOTER_COLUMN_GAP } else { 0 };
    let usable = area.width.saturating_sub(gap.saturating_mul(2));
    let left_width = usable.saturating_add(1) / 2;
    let right_half = usable.saturating_sub(left_width);
    let mid_width = right_half.saturating_add(1) / 2;
    let right_width = right_half.saturating_sub(mid_width);

    let left = Rect { width: left_width, ..area };
    let mid =
        Rect { x: area.x.saturating_add(left_width).saturating_add(gap), width: mid_width, ..area };
    let right = Rect {
        x: area
            .x
            .saturating_add(left_width)
            .saturating_add(gap)
            .saturating_add(mid_width)
            .saturating_add(gap),
        width: right_width,
        ..area
    };
    (left, mid, right)
}

fn fit_footer_right_text(text: &str, max_width: usize) -> Option<String> {
    if max_width == 0 || text.trim().is_empty() {
        return None;
    }

    if UnicodeWidthStr::width(text) <= max_width {
        return Some(text.to_owned());
    }

    if max_width <= 3 {
        return Some(".".repeat(max_width));
    }

    let mut fitted = String::new();
    let mut width: usize = 0;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width.saturating_add(ch_width).saturating_add(3) > max_width {
            break;
        }
        fitted.push(ch);
        width = width.saturating_add(ch_width);
    }

    if fitted.is_empty() {
        return Some("...".to_owned());
    }
    fitted.push_str("...");
    Some(fitted)
}

fn render_footer_right_info(frame: &mut Frame, area: Rect, right_text: &str, right_color: Color) {
    if area.width == 0 {
        return;
    }
    let Some(fitted) = fit_footer_right_text(right_text, usize::from(area.width)) else {
        return;
    };

    let line = Line::from(Span::styled(fitted, Style::default().fg(right_color)));
    frame.render_widget(Paragraph::new(line).alignment(Alignment::Right), area);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::model;
    use crate::app::{
        App, BlockCache, ChatMessage, IncrementalMarkdown, MessageBlock, MessageRole,
    };

    #[test]
    fn split_footer_columns_preserves_total_width() {
        let area = Rect::new(0, 0, 80, 1);
        let (left, right) = split_footer_columns(area);
        assert_eq!(left.width.saturating_add(right.width).saturating_add(FOOTER_COLUMN_GAP), 80);
        assert_eq!(left.width, 40);
        assert_eq!(right.width, 39);
    }

    #[test]
    fn split_footer_three_columns_preserves_total_width() {
        let area = Rect::new(0, 0, 80, 1);
        let (left, mid, right) = split_footer_three_columns(area);
        // left + gap + mid + gap + right == 80
        assert_eq!(
            left.width
                .saturating_add(FOOTER_COLUMN_GAP)
                .saturating_add(mid.width)
                .saturating_add(FOOTER_COLUMN_GAP)
                .saturating_add(right.width),
            80
        );
        // left gets ~half, mid and right each get ~quarter
        assert_eq!(left.width, 39);
        assert_eq!(mid.width, 20);
        assert_eq!(right.width, 19);
    }

    #[test]
    fn split_footer_three_columns_zero_width() {
        let area = Rect::new(0, 0, 0, 1);
        let (left, mid, right) = split_footer_three_columns(area);
        assert_eq!(left.width, 0);
        assert_eq!(mid.width, 0);
        assert_eq!(right.width, 0);
    }

    #[test]
    fn fit_footer_right_text_truncates_when_needed() {
        let text = "Context: 37%";
        let fitted = fit_footer_right_text(text, 8).expect("fitted text");
        assert!(fitted.ends_with("..."));
        assert!(UnicodeWidthStr::width(fitted.as_str()) <= 8);
    }

    #[test]
    fn fit_footer_right_text_keeps_compacting_prefix() {
        let text = "\u{280B} Compacting...  Context: 37%";
        let fitted = fit_footer_right_text(text, 20).expect("fitted text");
        assert!(fitted.starts_with('\u{280B}'));
        assert!(UnicodeWidthStr::width(fitted.as_str()) <= 20);
    }

    #[test]
    fn context_text_new_session_defaults_to_full() {
        assert_eq!(context_text(None, None, true), "100%");
        assert_eq!(context_text(Some(200_000), None, true), "100%");
    }

    #[test]
    fn context_text_unknown_when_not_new_session() {
        assert_eq!(context_text(None, None, false), "-");
        assert_eq!(context_text(Some(200_000), None, false), "-");
    }

    #[test]
    fn context_text_computes_percent_when_defined() {
        assert_eq!(context_text(Some(200_000), Some(100_000), false), "50%");
    }

    #[test]
    fn footer_telemetry_new_session_uses_unknown_defaults() {
        let mut app = App::test_default();
        app.session_id = Some(model::SessionId::new("session-new"));

        let text = footer_telemetry_text(&app).expect("footer telemetry");
        assert_eq!(text, "Context: 100%");
    }

    #[test]
    fn footer_telemetry_still_defaults_to_full_after_first_user_message() {
        let mut app = App::test_default();
        app.session_id = Some(model::SessionId::new("session-new"));
        app.messages.push(ChatMessage {
            role: MessageRole::User,
            blocks: vec![MessageBlock::Text(
                "hello".to_owned(),
                BlockCache::default(),
                IncrementalMarkdown::from_complete("hello"),
            )],
            usage: None,
        });

        let text = footer_telemetry_text(&app).expect("footer telemetry");
        assert_eq!(text, "Context: 100%");
    }

    #[test]
    fn footer_telemetry_resume_ignores_cost_and_tokens() {
        let mut app = App::test_default();
        app.session_id = Some(model::SessionId::new("session-resume"));
        app.session_usage.total_input_tokens = 400;
        app.session_usage.total_cost_usd = Some(0.35);
        app.session_usage.cost_is_since_resume = true;

        let text = footer_telemetry_text(&app).expect("footer telemetry");
        assert_eq!(text, "Context: -");
    }
}
