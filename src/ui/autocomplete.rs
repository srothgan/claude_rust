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
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

/// Horizontal padding to match input inset.
const INPUT_PAD: u16 = 2;
/// Prompt column width: "❯ " = 2 columns
const PROMPT_WIDTH: u16 = 2;
/// Max dropdown width (characters).
const MAX_WIDTH: u16 = 60;

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

    // Position: above input, aligned with text start
    let x = input_area.x + INPUT_PAD + PROMPT_WIDTH;
    let width = input_area.width.saturating_sub(INPUT_PAD * 2 + PROMPT_WIDTH).min(MAX_WIDTH);
    let y = input_area.y.saturating_sub(height);

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
