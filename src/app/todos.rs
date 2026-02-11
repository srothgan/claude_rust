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

use super::{App, TodoItem, TodoStatus};
use agent_client_protocol as acp;

/// Parse a `TodoWrite` `raw_input` JSON value into a `Vec<TodoItem>`.
/// Expected shape: `{"todos": [{"content": "...", "status": "...", "activeForm": "..."}]}`
pub(super) fn parse_todos(raw_input: &serde_json::Value) -> Vec<TodoItem> {
    let Some(arr) = raw_input.get("todos").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| {
            let content = item.get("content")?.as_str()?.to_owned();
            let status_str = item.get("status")?.as_str()?;
            let active_form =
                item.get("activeForm").and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let status = match status_str {
                "in_progress" => TodoStatus::InProgress,
                "completed" => TodoStatus::Completed,
                _ => TodoStatus::Pending,
            };
            Some(TodoItem { content, status, active_form })
        })
        .collect()
}

pub(super) fn set_todos(app: &mut App, todos: Vec<TodoItem>) {
    app.cached_todo_compact = None;
    if todos.is_empty() {
        app.todos.clear();
        app.show_todo_panel = false;
        app.todo_scroll = 0;
        return;
    }

    let all_done = todos.iter().all(|t| t.status == TodoStatus::Completed);
    if all_done {
        app.todos.clear();
        app.show_todo_panel = false;
        app.todo_scroll = 0;
    } else {
        app.todos = todos;
    }
}

/// Convert ACP plan entries into the local todo list.
pub(super) fn apply_plan_todos(app: &mut App, plan: &acp::Plan) {
    app.cached_todo_compact = None;
    let mut todos = Vec::with_capacity(plan.entries.len());
    for entry in &plan.entries {
        let status_str = format!("{:?}", entry.status);
        let status = match status_str.as_str() {
            "InProgress" => TodoStatus::InProgress,
            "Completed" => TodoStatus::Completed,
            _ => TodoStatus::Pending,
        };
        todos.push(TodoItem { content: entry.content.clone(), status, active_form: String::new() });
    }
    set_todos(app, todos);
}
