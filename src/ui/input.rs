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

use crate::app::input::parse_paste_placeholder_with_suffix;
use crate::app::mention;
use crate::app::{App, AppStatus, InputWrapCache};
use crate::ui::theme;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use tui_textarea::{CursorMove, TextArea, WrapMode};
use unicode_width::UnicodeWidthChar;

/// Horizontal padding to match header/footer inset.
const INPUT_PAD: u16 = 2;

/// Prompt column width: "❯ " = 2 columns (icon + space)
const PROMPT_WIDTH: u16 = 2;

/// Maximum input area height (lines) to prevent the input from consuming the entire screen.
const MAX_INPUT_HEIGHT: u16 = 12;
const HIGHLIGHT_SLASH_PRIORITY: u8 = 6;
const HIGHLIGHT_MENTION_PRIORITY: u8 = 7;
const HIGHLIGHT_PASTE_PRIORITY: u8 = 8;

/// Braille spinner frames (same as message.rs) for the connecting animation.
const SPINNER_FRAMES: &[char] = &[
    '\u{280B}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283C}', '\u{2834}', '\u{2826}', '\u{2827}',
    '\u{2807}', '\u{280F}',
];

/// Height of the login hint banner in lines (0 when no hint is active).
/// Used internally by `visual_line_count` and `render` so the layout
/// calculation and rendering stay in sync.
const LOGIN_HINT_LINES: u16 = 2;

/// Whether a login hint banner is active.
fn has_login_hint(app: &App) -> bool {
    app.login_hint.is_some()
}

#[allow(clippy::cast_possible_truncation)]
pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    // If there's a login hint, split off top rows for the hint banner
    let (hint_area, input_main_area) = if has_login_hint(app) {
        let [hint, main] =
            Layout::vertical([Constraint::Length(LOGIN_HINT_LINES), Constraint::Min(1)])
                .areas(area);
        (Some(hint), main)
    } else {
        (None, area)
    };

    // Render login hint banner if present
    if let (Some(hint_area), Some(hint)) = (hint_area, &app.login_hint) {
        let hint_pad = Rect {
            x: hint_area.x + INPUT_PAD,
            y: hint_area.y,
            width: hint_area.width.saturating_sub(INPUT_PAD * 2),
            height: hint_area.height,
        };
        let lines = vec![
            Line::from(Span::styled(
                format!(
                    "Authentication required: {} -- {}",
                    hint.method_name, hint.method_description
                ),
                Style::default().fg(Color::Yellow),
            )),
            Line::from(Span::styled(
                "Authentication command discoverability is not enabled in this build yet",
                Style::default().fg(theme::DIM),
            )),
        ];
        frame.render_widget(Paragraph::new(lines), hint_pad);
    }

    let padded = Rect {
        x: input_main_area.x + INPUT_PAD,
        y: input_main_area.y,
        width: input_main_area.width.saturating_sub(INPUT_PAD * 2),
        height: input_main_area.height,
    };

    // During Connecting state, show a spinner with static text
    if app.status == AppStatus::Connecting {
        let spinner_ch = SPINNER_FRAMES[app.spinner_frame % SPINNER_FRAMES.len()];
        let line = Line::from(vec![
            Span::styled(format!("{spinner_ch} "), Style::default().fg(theme::DIM)),
            Span::styled("Connecting to Claude Code...", Style::default().fg(theme::DIM)),
        ]);
        frame.render_widget(Paragraph::new(line), padded);
        return;
    }

    // Split into prompt icon column (fixed) and input column (remaining)
    let [prompt_area, input_area] =
        Layout::horizontal([Constraint::Length(PROMPT_WIDTH), Constraint::Min(1)]).areas(padded);

    // Render prompt icon
    let prompt = Line::from(Span::styled(
        format!("{} ", theme::PROMPT_CHAR),
        Style::default().fg(theme::RUST_ORANGE),
    ));
    frame.render_widget(Paragraph::new(prompt), prompt_area);

    if input_area.width == 0 {
        return;
    }

    let textarea = build_input_textarea(app);
    app.rendered_input_area = input_area;
    if app.selection.is_some() {
        app.rendered_input_lines = render_lines_from_textarea(&textarea, input_area);
    }
    frame.render_widget(&textarea, input_area);

    if let Some(sel) = app.selection
        && sel.kind == crate::app::SelectionKind::Input
    {
        frame.render_widget(SelectionOverlay { selection: sel }, input_area);
    }
}

fn build_input_textarea(app: &App) -> TextArea<'static> {
    let mut textarea = TextArea::from(app.input.lines.clone());
    textarea.set_wrap_mode(WrapMode::Glyph);
    textarea.set_placeholder_text("Type a message...");
    textarea.set_placeholder_style(Style::default().fg(theme::DIM));
    textarea.set_cursor_line_style(Style::default());
    textarea.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));

    textarea.move_cursor(CursorMove::Jump(
        u16::try_from(app.input.cursor_row).unwrap_or(u16::MAX),
        u16::try_from(app.input.cursor_col).unwrap_or(u16::MAX),
    ));

    apply_textarea_highlights(&mut textarea, &app.input.lines);
    textarea
}

fn apply_textarea_highlights(textarea: &mut TextArea<'_>, lines: &[String]) {
    let slash_style = Style::default().fg(theme::SLASH_COMMAND);
    let mention_style = Style::default().fg(Color::Cyan);
    let paste_style = Style::default().fg(Color::Green);

    for (row, line) in lines.iter().enumerate() {
        if let Some((start, end)) = slash_command_range(line) {
            textarea.custom_highlight(
                ((row, start), (row, end)),
                slash_style,
                HIGHLIGHT_SLASH_PRIORITY,
            );
        }

        for (start, end, _) in mention::find_mention_spans(line) {
            textarea.custom_highlight(
                ((row, start), (row, end)),
                mention_style,
                HIGHLIGHT_MENTION_PRIORITY,
            );
        }

        if let Some((_, suffix_end)) = parse_paste_placeholder_with_suffix(line) {
            textarea.custom_highlight(
                ((row, 0), (row, suffix_end)),
                paste_style,
                HIGHLIGHT_PASTE_PRIORITY,
            );
        }
    }
}

fn slash_command_range(line: &str) -> Option<(usize, usize)> {
    let start = line.find(|c: char| !c.is_whitespace())?;
    if line.as_bytes().get(start).copied() != Some(b'/') {
        return None;
    }
    let rel_end =
        line[start..].find(char::is_whitespace).unwrap_or_else(|| line.len().saturating_sub(start));
    let end = start + rel_end;
    if end <= start + 1 {
        return None;
    }
    Some((start, end))
}

struct SelectionOverlay {
    selection: crate::app::SelectionState,
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

fn render_lines_from_textarea(textarea: &TextArea<'_>, area: Rect) -> Vec<String> {
    let mut buf = Buffer::empty(area);
    textarea.render(area, &mut buf);
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

/// Compute (or return cached) wrap result for the input field.
/// The cache is keyed by `(input.version, content_width)` and is stored on `App`.
#[allow(clippy::cast_possible_truncation)]
fn get_or_compute_wrap(app: &mut App, content_width: u16) {
    let fresh = app
        .input_wrap_cache
        .as_ref()
        .is_none_or(|c| c.version != app.input.version || c.content_width != content_width);
    if fresh {
        let (wrapped_lines, cursor_pos) = wrap_lines_and_cursor(
            &app.input.lines,
            app.input.cursor_row,
            app.input.cursor_col,
            content_width,
        );
        app.input_wrap_cache = Some(InputWrapCache {
            version: app.input.version,
            content_width,
            wrapped_lines,
            cursor_pos,
        });
    }
}

/// Total visual height for the input area: input lines + login hint banner.
/// Called by the layout to allocate the correct input area height.
#[allow(clippy::cast_possible_truncation)]
pub fn visual_line_count(app: &mut App, area_width: u16) -> u16 {
    let hint = if has_login_hint(app) { LOGIN_HINT_LINES } else { 0 };
    let input_lines = if app.input.is_empty() {
        1
    } else {
        let content_width = area_width.saturating_sub(INPUT_PAD * 2).saturating_sub(PROMPT_WIDTH);
        if content_width == 0 {
            app.input.line_count()
        } else {
            get_or_compute_wrap(app, content_width);
            app.input_wrap_cache
                .as_ref()
                .map_or(1, |c| (c.wrapped_lines.len() as u16).min(MAX_INPUT_HEIGHT))
        }
    };
    hint + input_lines
}

#[allow(clippy::cast_possible_truncation)]
fn wrap_lines_and_cursor(
    lines: &[String],
    cursor_row: usize,
    cursor_col: usize,
    content_width: u16,
) -> (Vec<Line<'static>>, Option<(u16, u16)>) {
    let width = content_width as usize;
    let mut wrapped: Vec<Line<'static>> = Vec::new();
    let mut cursor_pos: Option<(u16, u16)> = None;
    let mut visual_row: u16 = 0;

    if width == 0 {
        return (wrapped, None);
    }

    for (row, line) in lines.iter().enumerate() {
        let mut col: usize = 0;
        let mut current = String::new();
        let mut char_idx: usize = 0;

        if row == cursor_row && cursor_col == 0 {
            cursor_pos = Some((visual_row, 0));
        }

        for ch in line.chars() {
            if row == cursor_row && char_idx == cursor_col {
                cursor_pos = Some((visual_row, col as u16));
            }

            let w = UnicodeWidthChar::width(ch).unwrap_or(0);
            if w > 0 && col + w > width && col > 0 {
                wrapped.push(Line::from(Span::raw(std::mem::take(&mut current))));
                visual_row = visual_row.saturating_add(1);
                col = 0;
            }

            if w > width && col == 0 {
                current.push(ch);
                wrapped.push(Line::from(Span::raw(std::mem::take(&mut current))));
                visual_row = visual_row.saturating_add(1);
                col = 0;
                char_idx += 1;
                continue;
            }

            current.push(ch);
            if w > 0 {
                col += w;
            }
            char_idx += 1;
        }

        if row == cursor_row && char_idx == cursor_col {
            if col >= width {
                cursor_pos = Some((visual_row.saturating_add(1), 0));
            } else {
                cursor_pos = Some((visual_row, col as u16));
            }
        }

        if line.is_empty() {
            wrapped.push(Line::from(Span::raw(String::new())));
            visual_row = visual_row.saturating_add(1);
        } else if !current.is_empty() {
            wrapped.push(Line::from(Span::raw(current)));
            visual_row = visual_row.saturating_add(1);
        }
    }

    if lines.is_empty() {
        wrapped.push(Line::from(Span::raw(String::new())));
    }

    (wrapped, cursor_pos)
}

#[cfg(test)]
mod tests {
    use super::slash_command_range;

    #[test]
    fn slash_range_matches_leading_command_token() {
        assert_eq!(slash_command_range("/mode plan"), Some((0, 5)));
        assert_eq!(slash_command_range("  /mode plan"), Some((2, 7)));
    }

    #[test]
    fn slash_range_ignores_non_command_lines() {
        assert_eq!(slash_command_range("hello /mode"), None);
        assert_eq!(slash_command_range("/"), None);
        assert_eq!(slash_command_range("   "), None);
    }
}
