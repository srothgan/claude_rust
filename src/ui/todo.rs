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

use crate::app::{App, TodoStatus};
use crate::ui::theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// Maximum visible lines in the expanded todo panel.
const MAX_VISIBLE: usize = 5;

/// Compute the height the todo panel needs in the layout.
/// Returns 0 when there are no todos, 1 for the closed compact line,
/// or min(todo_count, MAX_VISIBLE) for the open panel.
pub fn compute_height(app: &App) -> u16 {
    if app.todos.is_empty() {
        return 0;
    }
    if !app.show_todo_panel {
        // Closed: compact one-line status
        return 1;
    }
    // Open: capped at MAX_VISIBLE
    app.todos.len().min(MAX_VISIBLE) as u16
}

/// Render the todo panel into the given area.
pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    tracing::debug!(
        "todo::render: {} todos, show_panel={}, area={}x{}",
        app.todos.len(),
        app.show_todo_panel,
        area.width,
        area.height
    );
    if app.todos.is_empty() {
        return;
    }

    if !app.show_todo_panel {
        render_closed(frame, area, app);
    } else {
        render_open(frame, area, app);
    }
}

/// Closed state: single compact line showing progress and current task.
/// Format: `  [3/7] Running tests`
fn render_closed(frame: &mut Frame, area: Rect, app: &App) {
    let completed = app
        .todos
        .iter()
        .filter(|t| t.status == TodoStatus::Completed)
        .count();
    let total = app.todos.len();

    // Find the current in-progress task's activeForm, or fall back to content
    let current = app
        .todos
        .iter()
        .find(|t| t.status == TodoStatus::InProgress);

    let task_text = match current {
        Some(t) if !t.active_form.is_empty() => t.active_form.clone(),
        Some(t) => t.content.clone(),
        None => {
            // No in-progress task — show next pending or "All done"
            if completed == total {
                "All tasks completed".to_string()
            } else {
                app.todos
                    .iter()
                    .find(|t| t.status == TodoStatus::Pending)
                    .map(|t| t.content.clone())
                    .unwrap_or_default()
            }
        }
    };

    let line = Line::from(vec![
        Span::styled("  [", Style::default().fg(theme::DIM)),
        Span::styled(
            format!("{completed}/{total}"),
            Style::default().fg(theme::RUST_ORANGE),
        ),
        Span::styled("] ", Style::default().fg(theme::DIM)),
        Span::styled(task_text, Style::default().fg(Color::White)),
    ]);

    frame.render_widget(Paragraph::new(line), area);
}

/// Open state: full list with status icons, scrollable when > MAX_VISIBLE.
fn render_open(frame: &mut Frame, area: Rect, app: &mut App) {
    let total = app.todos.len();
    let visible = (area.height as usize).min(total);

    // Clamp scroll offset
    let max_scroll = total.saturating_sub(visible);
    if app.todo_scroll > max_scroll {
        app.todo_scroll = max_scroll;
    }

    // Auto-scroll to keep the in-progress item visible
    if let Some(ip_idx) = app.todos.iter().position(|t| t.status == TodoStatus::InProgress) {
        if ip_idx < app.todo_scroll {
            app.todo_scroll = ip_idx;
        } else if ip_idx >= app.todo_scroll + visible {
            app.todo_scroll = ip_idx.saturating_sub(visible.saturating_sub(1));
        }
    }

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(visible);

    for todo in app.todos.iter().skip(app.todo_scroll).take(visible) {
        let (icon, icon_color) = match todo.status {
            TodoStatus::Completed => ("\u{2713}", Color::Green),     // ✓
            TodoStatus::InProgress => ("\u{25b8}", theme::RUST_ORANGE), // ▸
            TodoStatus::Pending => ("\u{25cb}", theme::DIM),         // ○
        };

        let text_style = match todo.status {
            TodoStatus::Completed => Style::default()
                .fg(theme::DIM)
                .add_modifier(Modifier::CROSSED_OUT),
            TodoStatus::InProgress => Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
            TodoStatus::Pending => Style::default().fg(Color::Gray),
        };

        let display_text = if todo.status == TodoStatus::InProgress && !todo.active_form.is_empty()
        {
            &todo.active_form
        } else {
            &todo.content
        };

        lines.push(Line::from(vec![
            Span::styled(format!("  {icon} "), Style::default().fg(icon_color)),
            Span::styled(display_text.clone(), text_style),
        ]));
    }

    frame.render_widget(Paragraph::new(lines), area);
}
