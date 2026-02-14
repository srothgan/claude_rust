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

use crate::app::App;
use crate::app::mention::MAX_VISIBLE;
use crate::ui::theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use unicode_width::UnicodeWidthChar;

/// Horizontal padding to match input inset.
const INPUT_PAD: u16 = 2;
/// Prompt column width: "❯ " = 2 columns
const PROMPT_WIDTH: u16 = 2;
/// Max dropdown width (characters).
const MAX_WIDTH: u16 = 60;
/// Min dropdown width so list entries stay readable.
const MIN_WIDTH: u16 = 20;
/// Vertical gap (in rows) between the `@` line and the dropdown.
const ANCHOR_VERTICAL_GAP: u16 = 1;
/// Keep in sync with `ui/input.rs`.
const LOGIN_HINT_LINES: u16 = 2;

pub fn is_active(app: &App) -> bool {
    app.mention.as_ref().is_some_and(|m| !m.candidates.is_empty())
}

#[allow(clippy::cast_possible_truncation)]
pub fn compute_height(app: &App) -> u16 {
    match &app.mention {
        Some(m) if !m.candidates.is_empty() => {
            let visible = m.candidates.len().min(MAX_VISIBLE) as u16;
            visible.saturating_add(2) // +2 for top/bottom border
        }
        _ => 0,
    }
}

/// Render the autocomplete dropdown as a floating overlay above the input area.
#[allow(clippy::cast_possible_truncation)]
pub fn render(frame: &mut Frame, input_area: Rect, app: &App) {
    let mention = match &app.mention {
        Some(m) if !m.candidates.is_empty() => m,
        _ => return,
    };

    let height = compute_height(app);
    if height == 0 {
        return;
    }

    let text_area = compute_text_area(input_area, app.login_hint.is_some());
    if text_area.width == 0 || text_area.height == 0 {
        return;
    }

    let (anchor_row, anchor_col) = wrapped_visual_pos(
        &app.input.lines,
        mention.trigger_row,
        mention.trigger_col,
        text_area.width,
    );

    // Anchor horizontally to the `@` position.
    let mut x = text_area.x.saturating_add(anchor_col).min(text_area.right().saturating_sub(1));
    let available_from_x = text_area.right().saturating_sub(x).max(1);
    let mut width = available_from_x.min(MAX_WIDTH);
    if width < MIN_WIDTH && text_area.width >= MIN_WIDTH {
        x = text_area.right().saturating_sub(MIN_WIDTH);
        width = MIN_WIDTH;
    }

    // Anchor vertically to the line containing `@`.
    let anchor_y = text_area.y.saturating_add(anchor_row).min(text_area.bottom().saturating_sub(1));
    let y = choose_dropdown_y(anchor_y, height, frame.area().y, frame.area().bottom());

    let dropdown_area = Rect { x, y, width, height };

    let visible_count = mention.candidates.len().min(MAX_VISIBLE);
    let start = mention.scroll_offset;
    let end = (start + visible_count).min(mention.candidates.len());

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(visible_count);
    for (i, candidate) in mention.candidates[start..end].iter().enumerate() {
        let global_idx = start + i;
        let is_selected = global_idx == mention.selected;

        let mut spans: Vec<Span<'static>> = Vec::new();

        // Selection indicator
        if is_selected {
            spans.push(Span::styled(
                " \u{25b8} ",
                Style::default().fg(theme::RUST_ORANGE).add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::raw("   "));
        }

        // Path with query highlight
        let path = &candidate.rel_path;
        let query = &mention.query;
        if query.is_empty() {
            spans.push(Span::raw(path.clone()));
        } else if let Some(match_start) = path.to_lowercase().find(&query.to_lowercase()) {
            let before = &path[..match_start];
            let matched = &path[match_start..match_start + query.len()];
            let after = &path[match_start + query.len()..];

            if !before.is_empty() {
                spans.push(Span::raw(before.to_owned()));
            }
            spans.push(Span::styled(
                matched.to_owned(),
                Style::default().fg(theme::RUST_ORANGE).add_modifier(Modifier::BOLD),
            ));
            if !after.is_empty() {
                spans.push(Span::raw(after.to_owned()));
            }
        } else {
            spans.push(Span::raw(path.clone()));
        }

        lines.push(Line::from(spans));
    }

    let title = format!(" Files & Folders ({}) ", mention.candidates.len());
    let block = Block::default()
        .title(Span::styled(title, Style::default().fg(theme::DIM)))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::DIM));

    let paragraph = Paragraph::new(lines).block(block);
    // Clear the area first so the overlay has a solid background
    frame.render_widget(ratatui::widgets::Clear, dropdown_area);
    frame.render_widget(paragraph, dropdown_area);
}

fn compute_text_area(input_area: Rect, has_login_hint: bool) -> Rect {
    let input_main_area = if has_login_hint {
        let [_hint, main] =
            Layout::vertical([Constraint::Length(LOGIN_HINT_LINES), Constraint::Min(1)])
                .areas(input_area);
        main
    } else {
        input_area
    };

    let padded = Rect {
        x: input_main_area.x + INPUT_PAD,
        y: input_main_area.y,
        width: input_main_area.width.saturating_sub(INPUT_PAD * 2),
        height: input_main_area.height,
    };
    let [_prompt_area, text_area] =
        Layout::horizontal([Constraint::Length(PROMPT_WIDTH), Constraint::Min(1)]).areas(padded);
    text_area
}

#[allow(clippy::cast_possible_truncation)]
fn wrapped_visual_pos(
    lines: &[String],
    target_row: usize,
    target_col: usize,
    width: u16,
) -> (u16, u16) {
    let width = width as usize;
    if width == 0 {
        return (0, 0);
    }

    let mut visual_row: u16 = 0;
    for (row, line) in lines.iter().enumerate() {
        let mut col_width: usize = 0;
        let mut char_idx: usize = 0;

        if row == target_row && target_col == 0 {
            return (visual_row, 0);
        }

        for ch in line.chars() {
            if row == target_row && char_idx == target_col {
                return (visual_row, col_width as u16);
            }

            let w = UnicodeWidthChar::width(ch).unwrap_or(0);
            if w > 0 && col_width + w > width && col_width > 0 {
                visual_row = visual_row.saturating_add(1);
                col_width = 0;
            }

            if w > width && col_width == 0 {
                visual_row = visual_row.saturating_add(1);
                char_idx += 1;
                continue;
            }

            if w > 0 {
                col_width += w;
            }
            char_idx += 1;
        }

        if row == target_row && char_idx == target_col {
            if col_width >= width {
                return (visual_row.saturating_add(1), 0);
            }
            return (visual_row, col_width as u16);
        }

        visual_row = visual_row.saturating_add(1);
    }

    (visual_row, 0)
}

fn choose_dropdown_y(anchor_y: u16, height: u16, frame_top: u16, frame_bottom: u16) -> u16 {
    if height == 0 || frame_bottom <= frame_top {
        return frame_top;
    }

    // Candidate with required gap below the `@` line.
    let below_y = anchor_y.saturating_add(1).saturating_add(ANCHOR_VERTICAL_GAP);
    let rows_below_with_gap = frame_bottom.saturating_sub(below_y);
    let fits_below_with_gap = height <= rows_below_with_gap;

    // Candidate with required gap above the `@` line.
    let above_y = anchor_y.saturating_sub(height.saturating_add(ANCHOR_VERTICAL_GAP));
    let rows_above_with_gap =
        anchor_y.saturating_sub(frame_top.saturating_add(ANCHOR_VERTICAL_GAP));
    let fits_above_with_gap = height <= rows_above_with_gap;

    let mut y = if fits_below_with_gap {
        below_y
    } else if fits_above_with_gap {
        above_y
    } else if rows_below_with_gap >= rows_above_with_gap {
        // Not enough room with a full gap; prefer below side and relax the gap.
        anchor_y.saturating_add(1)
    } else {
        // Not enough room with a full gap; prefer above side and relax the gap.
        anchor_y.saturating_sub(height)
    };

    // Clamp into frame.
    let max_y = frame_bottom.saturating_sub(height);
    y = y.clamp(frame_top, max_y);

    // Final guard: avoid covering the `@` row when either side has enough space without gap.
    let overlaps_anchor = y <= anchor_y && anchor_y < y.saturating_add(height);
    if overlaps_anchor {
        let can_place_below = anchor_y.saturating_add(1).saturating_add(height) <= frame_bottom;
        let can_place_above = frame_top.saturating_add(height) <= anchor_y;
        if can_place_below {
            y = anchor_y.saturating_add(1);
        } else if can_place_above {
            y = anchor_y.saturating_sub(height);
        }
    }

    y.clamp(frame_top, max_y)
}

#[cfg(test)]
mod tests {
    use super::choose_dropdown_y;

    #[test]
    fn dropdown_prefers_below_with_gap_when_space_available() {
        let y = choose_dropdown_y(10, 4, 0, 30);
        assert_eq!(y, 12);
    }

    #[test]
    fn dropdown_uses_above_with_gap_when_below_too_small() {
        let y = choose_dropdown_y(9, 6, 0, 12);
        assert_eq!(y, 2);
    }

    #[test]
    fn dropdown_does_not_cover_anchor_row_when_possible() {
        let anchor = 5;
        let height = 5;
        let y = choose_dropdown_y(anchor, height, 0, 11);
        assert!(!(y <= anchor && anchor < y + height));
    }
}
