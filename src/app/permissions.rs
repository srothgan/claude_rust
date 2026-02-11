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

use super::{App, MessageBlock, ToolCallInfo};
use agent_client_protocol as acp;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;

/// Look up the tool call that currently has keyboard focus for its permission.
/// This is the first entry in `pending_permission_ids`.
/// Returns mutable reference to its `ToolCallInfo`.
fn get_focused_permission_tc(app: &mut App) -> Option<&mut ToolCallInfo> {
    let tool_id = app.pending_permission_ids.first()?;
    let (mi, bi) = app.tool_call_index.get(tool_id).copied()?;
    match app.messages.get_mut(mi)?.blocks.get_mut(bi)? {
        MessageBlock::ToolCall(tc) if tc.pending_permission.is_some() => Some(tc.as_mut()),
        _ => None,
    }
}

/// Set the `focused` flag on a permission at the given index in `pending_permission_ids`.
/// Also invalidates the tool call's render cache.
fn set_permission_focused(app: &mut App, queue_index: usize, focused: bool) {
    let Some(tool_id) = app.pending_permission_ids.get(queue_index) else {
        return;
    };
    let Some((mi, bi)) = app.tool_call_index.get(tool_id).copied() else {
        return;
    };
    if let Some(msg) = app.messages.get_mut(mi) {
        if let Some(MessageBlock::ToolCall(tc)) = msg.blocks.get_mut(bi) {
            let tc = tc.as_mut();
            if let Some(ref mut perm) = tc.pending_permission {
                perm.focused = focused;
            }
            tc.cache.invalidate();
        }
        // Invalidate visual height -- focused/unfocused permission lines differ
        msg.cached_visual_height = 0;
    }
}

pub(super) fn handle_permission_key(app: &mut App, key: KeyEvent) {
    let option_count = get_focused_permission_tc(app)
        .and_then(|tc| tc.pending_permission.as_ref())
        .map_or(0, |p| p.options.len());

    match key.code {
        // Up / Down: cycle focus between pending permissions
        KeyCode::Up | KeyCode::Down if app.pending_permission_ids.len() > 1 => {
            // Unfocus the current (first) permission
            set_permission_focused(app, 0, false);

            if key.code == KeyCode::Down {
                // Move first to end (rotate forward)
                let first = app.pending_permission_ids.remove(0);
                app.pending_permission_ids.push(first);
            } else {
                // Move last to front (rotate backward)
                let Some(last) = app.pending_permission_ids.pop() else {
                    return;
                };
                app.pending_permission_ids.insert(0, last);
            }

            // Focus the new first permission
            set_permission_focused(app, 0, true);
            // Scroll to the newly focused permission's tool call
            app.auto_scroll = true;
        }
        KeyCode::Left => {
            if let Some(tc) = get_focused_permission_tc(app)
                && let Some(ref mut p) = tc.pending_permission
            {
                p.selected_index = p.selected_index.saturating_sub(1);
                tc.cache.invalidate();
            }
        }
        KeyCode::Right => {
            if let Some(tc) = get_focused_permission_tc(app)
                && let Some(ref mut p) = tc.pending_permission
                && p.selected_index + 1 < option_count
            {
                p.selected_index += 1;
                tc.cache.invalidate();
            }
        }
        KeyCode::Enter => {
            respond_permission(app, None);
        }
        KeyCode::Char('y' | 'Y') => {
            respond_permission(app, Some(0));
        }
        KeyCode::Char('a' | 'A') => {
            if option_count > 1 {
                respond_permission(app, Some(1));
            }
        }
        KeyCode::Char('n' | 'N') | KeyCode::Esc => {
            if option_count > 0 {
                respond_permission(app, Some(option_count - 1));
            }
        }
        _ => {}
    }
}

fn respond_permission(app: &mut App, override_index: Option<usize>) {
    if app.pending_permission_ids.is_empty() {
        return;
    }
    // Remove the focused (first) permission from the queue.
    let tool_id = app.pending_permission_ids.remove(0);

    let Some((mi, bi)) = app.tool_call_index.get(&tool_id).copied() else {
        return;
    };
    let Some(MessageBlock::ToolCall(tc)) =
        app.messages.get_mut(mi).and_then(|m| m.blocks.get_mut(bi))
    else {
        return;
    };
    let tc = tc.as_mut();
    if let Some(pending) = tc.pending_permission.take() {
        let idx = override_index.unwrap_or(pending.selected_index);
        if let Some(opt) = pending.options.get(idx) {
            let _ = pending.response_tx.send(acp::RequestPermissionResponse::new(
                acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(
                    opt.option_id.clone(),
                )),
            ));
        }
        tc.cache.invalidate();
    }

    // Focus the next permission in the queue (now at index 0), if any.
    set_permission_focused(app, 0, true);
}
