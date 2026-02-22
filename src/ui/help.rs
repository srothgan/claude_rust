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

use crate::app::{App, AppStatus, FocusOwner, HelpView};
use crate::ui::theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Row, Table};
use unicode_width::UnicodeWidthStr;

const COLUMN_GAP: usize = 4;
const MAX_ROWS: usize = 8;
const HELP_VERTICAL_PADDING_LINES: usize = 1;

pub fn is_active(app: &App) -> bool {
    app.is_help_active()
}

#[allow(clippy::cast_possible_truncation)]
pub fn compute_height(app: &App, area_width: u16) -> u16 {
    if !is_active(app) {
        return 0;
    }
    let items = build_help_items(app);
    if items.is_empty() {
        return 0;
    }
    let inner_width = area_width.saturating_sub(2) as usize;
    let content_height = match app.help_view {
        HelpView::Keys => {
            let rows = items.len().div_ceil(2).min(MAX_ROWS);
            let col_width = (inner_width.saturating_sub(COLUMN_GAP)) / 2;
            let left_width = col_width;
            let right_width = col_width;

            let mut height = 0usize;
            for row in 0..rows {
                let left_idx = row;
                let right_idx = row + rows;
                let left = items.get(left_idx).cloned().unwrap_or_default();
                let right = items.get(right_idx).cloned().unwrap_or_default();

                let left_h = format_item_cell_lines(&left, left_width).len().max(1);
                let right_h = format_item_cell_lines(&right, right_width).len().max(1);
                height += left_h.max(right_h);
            }
            height
        }
        HelpView::SlashCommands => items
            .iter()
            .take(MAX_ROWS)
            .map(|item| format_item_cell_lines(item, inner_width).len().max(1))
            .sum(),
    };

    let inner_height = (content_height + HELP_VERTICAL_PADDING_LINES * 2) as u16;
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

    match app.help_view {
        HelpView::Keys => {
            let rows = items.len().div_ceil(2).min(MAX_ROWS);
            let max_items = rows * 2;
            let items = &items[..items.len().min(max_items)];
            let inner_width = area.width.saturating_sub(2) as usize;
            let col_width = (inner_width.saturating_sub(COLUMN_GAP)) / 2;
            let left_width = col_width;
            let right_width = col_width;

            let mut table_rows: Vec<Row<'static>> =
                Vec::with_capacity(rows + HELP_VERTICAL_PADDING_LINES * 2);

            for _ in 0..HELP_VERTICAL_PADDING_LINES {
                table_rows
                    .push(Row::new(vec![Cell::from(Line::default()), Cell::from(Line::default())]));
            }

            for row in 0..rows {
                let left_idx = row;
                let right_idx = row + rows;

                let left = items.get(left_idx).cloned().unwrap_or_default();
                let right = items.get(right_idx).cloned().unwrap_or_default();

                let left_lines = format_item_cell_lines(&left, left_width);
                let right_lines = format_item_cell_lines(&right, right_width);
                let row_height = left_lines.len().max(right_lines.len()).max(1);

                table_rows.push(
                    Row::new(vec![
                        Cell::from(Text::from(left_lines)),
                        Cell::from(Text::from(right_lines)),
                    ])
                    .height(row_height as u16),
                );
            }

            for _ in 0..HELP_VERTICAL_PADDING_LINES {
                table_rows
                    .push(Row::new(vec![Cell::from(Line::default()), Cell::from(Line::default())]));
            }

            let block = Block::default()
                .title(help_title(app.help_view))
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
        HelpView::SlashCommands => {
            let rows = items.len().min(MAX_ROWS);
            let items = &items[..rows];
            let inner_width = area.width.saturating_sub(2) as usize;
            let mut table_rows: Vec<Row<'static>> =
                Vec::with_capacity(rows + HELP_VERTICAL_PADDING_LINES * 2);

            for _ in 0..HELP_VERTICAL_PADDING_LINES {
                table_rows.push(Row::new(vec![Cell::from(Line::default())]));
            }

            for item in items {
                let lines = format_item_cell_lines(item, inner_width);
                let row_height = lines.len().max(1);
                table_rows
                    .push(Row::new(vec![Cell::from(Text::from(lines))]).height(row_height as u16));
            }

            for _ in 0..HELP_VERTICAL_PADDING_LINES {
                table_rows.push(Row::new(vec![Cell::from(Line::default())]));
            }

            let block = Block::default()
                .title(help_title(app.help_view))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded);

            let table =
                Table::new(table_rows, [ratatui::layout::Constraint::Length(inner_width as u16)])
                    .block(block);

            frame.render_widget(table, area);
        }
    }
}

fn build_help_items(app: &App) -> Vec<(String, String)> {
    match app.help_view {
        HelpView::Keys => build_key_help_items(app),
        HelpView::SlashCommands => build_slash_help_items(app),
    }
}

fn build_key_help_items(app: &App) -> Vec<(String, String)> {
    let mut items: Vec<(String, String)> = vec![
        ("Left/Right".to_owned(), "Switch help tab".to_owned()),
        // Global
        ("Ctrl+c".to_owned(), "Quit".to_owned()),
        ("Ctrl+h".to_owned(), "Toggle header".to_owned()),
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
    if focus_owner != FocusOwner::TodoList
        && focus_owner != FocusOwner::Mention
        && focus_owner != FocusOwner::Help
    {
        items.push(("Enter".to_owned(), "Send message".to_owned()));
        items.push(("Shift+Enter".to_owned(), "Insert newline".to_owned()));
        items.push(("Up/Down".to_owned(), "Move cursor / scroll chat".to_owned()));
        items.push(("Left/Right".to_owned(), "Move cursor".to_owned()));
        items.push(("Ctrl+Left/Right".to_owned(), "Word left/right".to_owned()));
        items.push(("Home/End".to_owned(), "Line start/end".to_owned()));
        items.push(("Backspace".to_owned(), "Delete before".to_owned()));
        items.push(("Delete".to_owned(), "Delete after".to_owned()));
        items.push(("Ctrl+Backspace/Delete".to_owned(), "Delete word".to_owned()));
        items.push(("Ctrl+z/y".to_owned(), "Undo/redo".to_owned()));
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

fn build_slash_help_items(app: &App) -> Vec<(String, String)> {
    let mut rows = vec![("Left/Right".to_owned(), "Switch help tab".to_owned())];
    if app.status == AppStatus::Connecting {
        rows.push(("Loading commands...".to_owned(), String::new()));
        return rows;
    }

    let mut commands: Vec<(String, String)> = app
        .available_commands
        .iter()
        .map(|cmd| {
            let name =
                if cmd.name.starts_with('/') { cmd.name.clone() } else { format!("/{}", cmd.name) };
            (name, cmd.description.clone())
        })
        .filter(|(name, _)| !matches!(name.as_str(), "/login" | "/logout"))
        .collect();

    commands.sort_by(|a, b| a.0.cmp(&b.0));
    commands.dedup_by(|a, b| a.0 == b.0);

    if commands.is_empty() {
        rows.push((
            "No ACP slash commands".to_owned(),
            "Not advertised in this session".to_owned(),
        ));
        return rows;
    }

    for (name, desc) in commands {
        let description =
            if desc.trim().is_empty() { "No description provided".to_owned() } else { desc };
        rows.push((name, description));
    }

    rows
}

fn help_title(view: HelpView) -> Line<'static> {
    let keys_style = if matches!(view, HelpView::Keys) {
        Style::default().fg(theme::RUST_ORANGE).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::DIM)
    };
    let slash_style = if matches!(view, HelpView::SlashCommands) {
        Style::default().fg(theme::RUST_ORANGE).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::DIM)
    };

    Line::from(vec![
        Span::styled(
            " Help ",
            Style::default().fg(theme::RUST_ORANGE).add_modifier(Modifier::BOLD),
        ),
        Span::styled("[", Style::default().fg(theme::DIM)),
        Span::styled("Keys", keys_style),
        Span::styled(" | ", Style::default().fg(theme::DIM)),
        Span::styled("Slash", slash_style),
        Span::styled("]", Style::default().fg(theme::DIM)),
    ])
}

fn format_item_cell_lines(item: &(String, String), width: usize) -> Vec<Line<'static>> {
    let (label, desc) = item;
    if width == 0 {
        return vec![Line::default()];
    }
    if label.is_empty() && desc.is_empty() {
        return vec![Line::default()];
    }

    let label = truncate_to_width(label, width);
    let label_width = UnicodeWidthStr::width(label.as_str());
    let sep = " : ";
    let sep_width = UnicodeWidthStr::width(sep);

    if desc.is_empty() {
        return vec![Line::from(Span::styled(
            label,
            Style::default().add_modifier(Modifier::BOLD),
        ))];
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut rest = desc.to_owned();

    if label_width + sep_width < width {
        let first_desc_width = width - label_width - sep_width;
        let (first_chunk, remaining) = take_prefix_by_width(&rest, first_desc_width);
        lines.push(Line::from(vec![
            Span::styled(label, Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(sep.to_owned(), Style::default().fg(theme::DIM)),
            Span::raw(first_chunk),
        ]));
        rest = remaining;
    } else {
        lines.push(Line::from(Span::styled(label, Style::default().add_modifier(Modifier::BOLD))));
    }

    while !rest.is_empty() {
        let (chunk, remaining) = take_prefix_by_width(&rest, width);
        if chunk.is_empty() {
            break;
        }
        lines.push(Line::raw(chunk));
        rest = remaining;
    }

    if lines.is_empty() { vec![Line::default()] } else { lines }
}

fn take_prefix_by_width(text: &str, width: usize) -> (String, String) {
    if width == 0 || text.is_empty() {
        return (String::new(), text.to_owned());
    }

    let mut used = 0usize;
    let mut split_at = 0usize;
    for (idx, ch) in text.char_indices() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > width {
            break;
        }
        used += w;
        split_at = idx + ch.len_utf8();
    }

    if split_at == 0 {
        return (String::new(), text.to_owned());
    }

    (text[..split_at].to_owned(), text[split_at..].to_owned())
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
    use crate::app::{App, AppStatus, FocusTarget, HelpView, TodoItem, TodoStatus};

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
    fn key_tab_shows_ctrl_h_toggle_header_shortcut() {
        let app = App::test_default();
        let items = build_help_items(&app);
        assert!(has_item(&items, "Ctrl+h", "Toggle header"));
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

    #[test]
    fn slash_tab_shows_advertised_commands_with_description() {
        let mut app = App::test_default();
        app.help_view = HelpView::SlashCommands;
        app.available_commands = vec![
            agent_client_protocol::AvailableCommand::new("/help", "Open help"),
            agent_client_protocol::AvailableCommand::new("memory", ""),
        ];

        let items = build_help_items(&app);
        assert!(has_item(&items, "/help", "Open help"));
        assert!(has_item(&items, "/memory", "No description provided"));
    }

    #[test]
    fn slash_tab_hides_login_logout_commands() {
        let mut app = App::test_default();
        app.help_view = HelpView::SlashCommands;
        app.available_commands = vec![
            agent_client_protocol::AvailableCommand::new("/login", "Login"),
            agent_client_protocol::AvailableCommand::new("/logout", "Logout"),
            agent_client_protocol::AvailableCommand::new("/mode", "Switch mode"),
        ];

        let items = build_help_items(&app);
        assert!(!has_item(&items, "/login", "Login"));
        assert!(!has_item(&items, "/logout", "Logout"));
        assert!(has_item(&items, "/mode", "Switch mode"));
    }

    #[test]
    fn slash_tab_shows_loading_commands_while_connecting() {
        let mut app = App::test_default();
        app.help_view = HelpView::SlashCommands;
        app.status = AppStatus::Connecting;

        let items = build_help_items(&app);
        assert!(has_item(&items, "Loading commands...", ""));
        assert!(!has_item(&items, "No ACP slash commands", "Not advertised in this session"));
    }
}
