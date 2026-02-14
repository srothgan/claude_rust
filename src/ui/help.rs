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

use crate::app::{App, FocusOwner};
use crate::ui::theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Row, Table};
use unicode_width::UnicodeWidthStr;

const COLUMN_GAP: usize = 4;
const MAX_ROWS: usize = 8;

pub fn is_active(app: &App) -> bool {
    app.input.text().trim() == "?"
}

#[allow(clippy::cast_possible_truncation)]
pub fn compute_height(app: &App, _area_width: u16) -> u16 {
    if !is_active(app) {
        return 0;
    }
    let items = build_help_items(app);
    if items.is_empty() {
        return 0;
    }
    let rows = items.len().div_ceil(2).min(MAX_ROWS);
    let inner_height = rows as u16;
    // Border top + bottom.
    inner_height.saturating_add(2)
}

#[allow(clippy::cast_possible_truncation)]
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 || area.width == 0 || !is_active(app) {
        return;
    }

    let items = build_help_items(app);
    if items.is_empty() {
        return;
    }

    let rows = items.len().div_ceil(2).min(MAX_ROWS);
    let max_items = rows * 2;
    let items = &items[..items.len().min(max_items)];
    let inner_width = area.width.saturating_sub(2) as usize;
    let col_width = (inner_width.saturating_sub(COLUMN_GAP)) / 2;
    let left_width = col_width;
    let right_width = col_width;

    let mut table_rows: Vec<Row<'static>> = Vec::with_capacity(rows);
    for row in 0..rows {
        let left_idx = row;
        let right_idx = row + rows;

        let left = items.get(left_idx).cloned().unwrap_or_default();
        let right = items.get(right_idx).cloned().unwrap_or_default();

        let left_cell = format_item_cell(&left, left_width);
        let right_cell = format_item_cell(&right, right_width);

        table_rows.push(Row::new(vec![Cell::from(left_cell), Cell::from(right_cell)]));
    }

    let block = Block::default()
        .title(Span::styled(
            " Help ",
            Style::default().fg(theme::RUST_ORANGE).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);

    let table = Table::new(
        table_rows,
        [
            ratatui::layout::Constraint::Length(left_width as u16),
            ratatui::layout::Constraint::Length(right_width as u16),
        ],
    )
    .column_spacing(COLUMN_GAP as u16)
    .block(block);

    frame.render_widget(table, area);
}

fn build_help_items(app: &App) -> Vec<(String, String)> {
    let mut items: Vec<(String, String)> = vec![
        // Global
        ("Ctrl+c".to_owned(), "Quit".to_owned()),
        ("Ctrl+l".to_owned(), "Redraw screen".to_owned()),
        ("Shift+Tab".to_owned(), "Cycle mode".to_owned()),
        ("Ctrl+o".to_owned(), "Toggle tool collapse".to_owned()),
        ("Ctrl+t".to_owned(), "Toggle todos (when available)".to_owned()),
        // Chat scrolling
        ("Ctrl+Up/Down".to_owned(), "Scroll chat".to_owned()),
        ("Mouse wheel".to_owned(), "Scroll chat".to_owned()),
    ];
    let focus_owner = app.focus_owner();

    if app.show_todo_panel && !app.todos.is_empty() {
        items.push(("Tab".to_owned(), "Toggle todo focus".to_owned()));
    }

    // Input + navigation (active outside todo-list and mention focus)
    if focus_owner != FocusOwner::TodoList && focus_owner != FocusOwner::Mention {
        items.push(("Enter".to_owned(), "Send message".to_owned()));
        items.push(("Shift+Enter".to_owned(), "Insert newline".to_owned()));
        items.push(("Up/Down".to_owned(), "Move cursor / scroll chat".to_owned()));
        items.push(("Left/Right".to_owned(), "Move cursor".to_owned()));
        items.push(("Home/End".to_owned(), "Line start/end".to_owned()));
        items.push(("Backspace".to_owned(), "Delete before".to_owned()));
        items.push(("Delete".to_owned(), "Delete after".to_owned()));
        items.push(("Paste".to_owned(), "Insert text".to_owned()));
    }

    // Turn control
    if matches!(app.status, crate::app::AppStatus::Thinking | crate::app::AppStatus::Running) {
        items.push(("Esc".to_owned(), "Cancel current turn".to_owned()));
    } else if focus_owner == FocusOwner::TodoList {
        items.push(("Esc".to_owned(), "Exit todo focus".to_owned()));
    } else {
        items.push(("Esc".to_owned(), "No-op (idle)".to_owned()));
    }

    // Permissions (when prompts are active)
    if !app.pending_permission_ids.is_empty() && focus_owner == FocusOwner::Permission {
        if app.pending_permission_ids.len() > 1 {
            items.push(("Up/Down".to_owned(), "Switch prompt focus".to_owned()));
        }
        items.push(("Left/Right".to_owned(), "Select option".to_owned()));
        items.push(("Enter".to_owned(), "Confirm option".to_owned()));
        items.push(("Ctrl+y/a/n".to_owned(), "Quick select".to_owned()));
        items.push(("Esc".to_owned(), "Reject".to_owned()));
    }
    if focus_owner == FocusOwner::TodoList {
        items.push(("Up/Down".to_owned(), "Select todo (todo focus)".to_owned()));
    }

    items
}

fn format_item_cell(item: &(String, String), width: usize) -> Line<'static> {
    let (label, desc) = item;
    if label.is_empty() && desc.is_empty() {
        return Line::default();
    }
    let label_style_width = UnicodeWidthStr::width(label.as_str());
    let sep = " : ";
    let sep_width = UnicodeWidthStr::width(sep);
    let desc_width = width.saturating_sub(label_style_width + sep_width);

    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::styled(
        truncate_to_width(label, label_style_width),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(sep.to_owned(), Style::default().fg(theme::DIM)));
    if !desc.is_empty() && desc_width > 0 {
        spans.push(Span::raw(truncate_to_width(desc, desc_width)));
    }
    Line::from(spans)
}

fn truncate_to_width(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(text) <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > width {
            break;
        }
        out.push(ch);
        used += w;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::build_help_items;
    use crate::app::{App, FocusTarget, TodoItem, TodoStatus};

    fn has_item(items: &[(String, String)], key: &str, desc: &str) -> bool {
        items.iter().any(|(k, d)| k == key && d == desc)
    }

    #[test]
    fn tab_toggle_only_shown_when_todos_available() {
        let mut app = App::test_default();
        let items = build_help_items(&app);
        assert!(!has_item(&items, "Tab", "Toggle todo focus"));

        app.show_todo_panel = true;
        app.todos.push(TodoItem {
            content: "Task".into(),
            status: TodoStatus::Pending,
            active_form: String::new(),
        });
        let items = build_help_items(&app);
        assert!(has_item(&items, "Tab", "Toggle todo focus"));
    }

    #[test]
    fn permission_navigation_only_shown_when_permission_has_focus() {
        let mut app = App::test_default();
        app.pending_permission_ids = vec!["perm-1".into(), "perm-2".into()];

        // Without permission focus claim, do not show permission-only arrows.
        let items = build_help_items(&app);
        assert!(!has_item(&items, "Left/Right", "Select option"));
        assert!(!has_item(&items, "Up/Down", "Switch prompt focus"));

        app.claim_focus_target(FocusTarget::Permission);
        let items = build_help_items(&app);
        assert!(has_item(&items, "Left/Right", "Select option"));
        assert!(has_item(&items, "Up/Down", "Switch prompt focus"));
    }
}
