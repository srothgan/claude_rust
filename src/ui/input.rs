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

// TODO: Replace custom InputState with tui-textarea when it supports ratatui 0.30
// Track: https://github.com/rhysd/tui-textarea/pull/118

use crate::app::App;
use crate::ui::theme;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthChar;

/// Horizontal padding to match header/footer inset.
const INPUT_PAD: u16 = 2;

/// Prompt column width: "❯ " = 2 columns (icon + space)
const PROMPT_WIDTH: u16 = 2;

/// Maximum input area height (lines) to prevent the input from consuming the entire screen.
const MAX_INPUT_HEIGHT: u16 = 12;

pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let padded = Rect {
        x: area.x + INPUT_PAD,
        y: area.y,
        width: area.width.saturating_sub(INPUT_PAD * 2),
        height: area.height,
    };

    // Split into prompt icon column (fixed) and input column (remaining)
    let [prompt_area, input_area] =
        Layout::horizontal([Constraint::Length(PROMPT_WIDTH), Constraint::Min(1)]).areas(padded);

    // Render prompt icon
    let prompt = Line::from(Span::styled(
        format!("{} ", theme::PROMPT_CHAR),
        Style::default().fg(theme::RUST_ORANGE),
    ));
    frame.render_widget(Paragraph::new(prompt), prompt_area);

    if app.input.is_empty() {
        // Placeholder
        let placeholder = Line::from(Span::styled(
            "Type a message...",
            Style::default().fg(theme::DIM),
        ));
        frame.render_widget(Paragraph::new(placeholder), input_area);

        // Cursor at start of input area
        frame.set_cursor_position((input_area.x, input_area.y));
        return;
    }

    // Build wrapped input lines using character-based wrapping for stability.
    let content_width = input_area.width;
    if content_width == 0 {
        return;
    }

    let (lines, cursor_pos) = wrap_lines_and_cursor(
        &app.input.lines,
        app.input.cursor_row,
        app.input.cursor_col,
        content_width,
    );

    let paragraph = Paragraph::new(lines);
    app.rendered_input_area = input_area;
    app.rendered_input_lines = render_lines_from_paragraph(&paragraph, input_area);
    frame.render_widget(paragraph, input_area);

    if let Some(sel) = app.selection
        && sel.kind == crate::app::SelectionKind::Input
    {
        frame.render_widget(SelectionOverlay { selection: sel }, input_area);
    }

    if let Some((row, col)) = cursor_pos {
        let cursor_x = input_area.x + col;
        let cursor_y = input_area.y + row;
        if cursor_x < input_area.right() && cursor_y < input_area.bottom() {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }
}

struct SelectionOverlay {
    selection: crate::app::SelectionState,
}

impl Widget for SelectionOverlay {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (start, end) =
            crate::app::normalize_selection(self.selection.start, self.selection.end);
        for row in start.row..=end.row {
            let y = area.y.saturating_add(row as u16);
            if y >= area.bottom() {
                break;
            }
            let row_start = if row == start.row { start.col } else { 0 };
            let row_end = if row == end.row {
                end.col
            } else {
                area.width as usize
            };
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

fn render_lines_from_paragraph(paragraph: &Paragraph, area: Rect) -> Vec<String> {
    let mut buf = Buffer::empty(area);
    paragraph.clone().render(area, &mut buf);
    let mut lines = Vec::with_capacity(area.height as usize);
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            if let Some(cell) = buf.cell((area.x + x, area.y + y)) {
                line.push_str(cell.symbol());
            }
        }
        lines.push(line.trim_end().to_string());
    }
    lines
}

/// Compute the number of visual lines the input occupies, accounting for wrapping.
/// Used by the layout to allocate the correct input area height.
pub fn visual_line_count(app: &App, area_width: u16) -> u16 {
    if app.input.is_empty() {
        return 1;
    }
    // Input content width = total area minus horizontal padding minus prompt column
    let content_width = area_width
        .saturating_sub(INPUT_PAD * 2)
        .saturating_sub(PROMPT_WIDTH) as usize;
    if content_width == 0 {
        return app.input.line_count();
    }

    let (lines, _) = wrap_lines_and_cursor(
        &app.input.lines,
        app.input.cursor_row,
        app.input.cursor_col,
        content_width as u16,
    );
    (lines.len() as u16).min(MAX_INPUT_HEIGHT)
}

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
