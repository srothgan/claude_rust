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

use super::{App, FocusTarget, MessageBlock, ToolCallInfo};
use agent_client_protocol as acp;
use agent_client_protocol::PermissionOptionKind;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;

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
    if let Some(msg) = app.messages.get_mut(mi)
        && let Some(MessageBlock::ToolCall(tc)) = msg.blocks.get_mut(bi)
    {
        let tc = tc.as_mut();
        if let Some(ref mut perm) = tc.pending_permission {
            perm.focused = focused;
        }
        tc.cache.invalidate();
    }
}

/// Find the option index for the currently focused permission by kind.
fn focused_option_index_by_kind(app: &App, kind: PermissionOptionKind) -> Option<usize> {
    let tool_id = app.pending_permission_ids.first()?;
    let (mi, bi) = app.tool_call_index.get(tool_id).copied()?;
    let MessageBlock::ToolCall(tc) = app.messages.get(mi)?.blocks.get(bi)? else {
        return None;
    };
    let pending = tc.pending_permission.as_ref()?;
    pending.options.iter().position(|opt| opt.kind == kind)
}

fn focused_permission_is_active(app: &App) -> bool {
    let Some(tool_id) = app.pending_permission_ids.first() else {
        return false;
    };
    let Some((mi, bi)) = app.tool_call_index.get(tool_id).copied() else {
        return false;
    };
    let Some(MessageBlock::ToolCall(tc)) = app.messages.get(mi).and_then(|m| m.blocks.get(bi))
    else {
        return false;
    };
    tc.pending_permission.as_ref().is_some_and(|p| p.focused)
}

/// Handle permission-only shortcuts.
/// Returns `true` when the key was consumed by permission UI.
pub(super) fn handle_permission_key(app: &mut App, key: KeyEvent) -> bool {
    let option_count = get_focused_permission_tc(app)
        .and_then(|tc| tc.pending_permission.as_ref())
        .map_or(0, |p| p.options.len());
    let permission_has_focus = focused_permission_is_active(app);

    match (key.code, key.modifiers) {
        // Up / Down: cycle focus between pending permissions
        (KeyCode::Up | KeyCode::Down, _)
            if permission_has_focus && app.pending_permission_ids.len() > 1 =>
        {
            // Unfocus the current (first) permission
            set_permission_focused(app, 0, false);

            if key.code == KeyCode::Down {
                // Move first to end (rotate forward)
                let first = app.pending_permission_ids.remove(0);
                app.pending_permission_ids.push(first);
            } else {
                // Move last to front (rotate backward)
                let Some(last) = app.pending_permission_ids.pop() else {
                    return false;
                };
                app.pending_permission_ids.insert(0, last);
            }

            // Focus the new first permission
            set_permission_focused(app, 0, true);
            // Scroll to the newly focused permission's tool call
            app.viewport.engage_auto_scroll();
            true
        }
        (KeyCode::Left, _) if permission_has_focus && option_count > 0 => {
            if let Some(tc) = get_focused_permission_tc(app)
                && let Some(ref mut p) = tc.pending_permission
            {
                p.selected_index = p.selected_index.saturating_sub(1);
                tc.cache.invalidate();
            }
            true
        }
        (KeyCode::Right, _) if permission_has_focus && option_count > 0 => {
            if let Some(tc) = get_focused_permission_tc(app)
                && let Some(ref mut p) = tc.pending_permission
                && p.selected_index + 1 < option_count
            {
                p.selected_index += 1;
                tc.cache.invalidate();
            }
            true
        }
        (KeyCode::Enter, _) if permission_has_focus && option_count > 0 => {
            respond_permission(app, None);
            true
        }
        // Quick shortcuts use Ctrl+lowercase so normal typing stays untouched.
        (KeyCode::Char('y'), m) if m == KeyModifiers::CONTROL => {
            if let Some(idx) = focused_option_index_by_kind(app, PermissionOptionKind::AllowOnce)
                .or_else(|| focused_option_index_by_kind(app, PermissionOptionKind::AllowAlways))
            {
                respond_permission(app, Some(idx));
                true
            } else {
                false
            }
        }
        (KeyCode::Char('a'), m) if m == KeyModifiers::CONTROL => {
            if let Some(idx) = focused_option_index_by_kind(app, PermissionOptionKind::AllowAlways)
            {
                respond_permission(app, Some(idx));
                true
            } else {
                false
            }
        }
        (KeyCode::Char('n'), m) if m == KeyModifiers::CONTROL => {
            if let Some(idx) = focused_option_index_by_kind(app, PermissionOptionKind::RejectOnce) {
                respond_permission(app, Some(idx));
                true
            } else {
                false
            }
        }
        (KeyCode::Esc, _) if permission_has_focus => {
            if let Some(idx) = focused_option_index_by_kind(app, PermissionOptionKind::RejectOnce)
                .or_else(|| focused_option_index_by_kind(app, PermissionOptionKind::RejectAlways))
            {
                respond_permission(app, Some(idx));
                true
            } else if option_count > 0 {
                // Fallback for unknown adapters: keep previous behavior if no kind metadata.
                respond_permission(app, Some(option_count - 1));
                true
            } else {
                false
            }
        }
        _ => false,
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
    if app.pending_permission_ids.is_empty() {
        app.release_focus_target(FocusTarget::Permission);
    } else {
        app.claim_focus_target(FocusTarget::Permission);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{
        App, AppStatus, BlockCache, ChatMessage, IncrementalMarkdown, InlinePermission,
        MessageBlock, MessageRole, ToolCallInfo,
    };
    use pretty_assertions::assert_eq;
    use tokio::sync::oneshot;

    fn test_tool_call(id: &str) -> ToolCallInfo {
        ToolCallInfo {
            id: id.to_owned(),
            title: format!("Tool {id}"),
            kind: acp::ToolKind::Read,
            status: acp::ToolCallStatus::InProgress,
            content: Vec::new(),
            collapsed: false,
            claude_tool_name: None,
            hidden: false,
            terminal_id: None,
            terminal_command: None,
            terminal_output: None,
            terminal_output_len: 0,
            cache: BlockCache::default(),
            pending_permission: None,
        }
    }

    fn assistant_tool_msg(tc: ToolCallInfo) -> ChatMessage {
        ChatMessage {
            role: MessageRole::Assistant,
            blocks: vec![MessageBlock::ToolCall(Box::new(tc))],
        }
    }

    fn allow_options() -> Vec<acp::PermissionOption> {
        vec![
            acp::PermissionOption::new("allow-once", "Allow once", PermissionOptionKind::AllowOnce),
            acp::PermissionOption::new(
                "allow-always",
                "Allow always",
                PermissionOptionKind::AllowAlways,
            ),
            acp::PermissionOption::new("reject-once", "Reject", PermissionOptionKind::RejectOnce),
        ]
    }

    fn add_permission(
        app: &mut App,
        tool_id: &str,
        options: Vec<acp::PermissionOption>,
        focused: bool,
    ) -> oneshot::Receiver<acp::RequestPermissionResponse> {
        let msg_idx = app.messages.len();
        app.messages.push(assistant_tool_msg(test_tool_call(tool_id)));
        app.index_tool_call(tool_id.to_owned(), msg_idx, 0);

        let (tx, rx) = oneshot::channel();
        if let Some(MessageBlock::ToolCall(tc)) =
            app.messages.get_mut(msg_idx).and_then(|m| m.blocks.get_mut(0))
        {
            tc.pending_permission =
                Some(InlinePermission { options, response_tx: tx, selected_index: 0, focused });
        }
        app.pending_permission_ids.push(tool_id.to_owned());
        rx
    }

    fn permission_focused(app: &App, tool_id: &str) -> bool {
        let Some((mi, bi)) = app.lookup_tool_call(tool_id) else {
            return false;
        };
        let Some(MessageBlock::ToolCall(tc)) = app.messages.get(mi).and_then(|m| m.blocks.get(bi))
        else {
            return false;
        };
        tc.pending_permission.as_ref().is_some_and(|p| p.focused)
    }

    #[test]
    fn step2_up_down_rotates_permission_focus_and_enter_targets_focused_prompt() {
        let mut app = App::test_default();
        app.status = AppStatus::Ready;
        let mut rx1 = add_permission(&mut app, "perm-1", allow_options(), true);
        let mut rx2 = add_permission(&mut app, "perm-2", allow_options(), false);

        assert_eq!(app.pending_permission_ids, vec!["perm-1", "perm-2"]);
        assert!(permission_focused(&app, "perm-1"));
        assert!(!permission_focused(&app, "perm-2"));

        let consumed = handle_permission_key(
            &mut app,
            KeyEvent::new(KeyCode::Down, crossterm::event::KeyModifiers::NONE),
        );
        assert!(consumed);
        assert_eq!(app.pending_permission_ids, vec!["perm-2", "perm-1"]);
        assert!(permission_focused(&app, "perm-2"));
        assert!(!permission_focused(&app, "perm-1"));

        let consumed = handle_permission_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, crossterm::event::KeyModifiers::NONE),
        );
        assert!(consumed);

        let resp2 = rx2.try_recv().expect("focused permission should receive response");
        let acp::RequestPermissionOutcome::Selected(sel2) = resp2.outcome else {
            panic!("expected selected permission response");
        };
        assert_eq!(sel2.option_id.to_string(), "allow-once");
        assert!(matches!(rx1.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
        assert_eq!(app.pending_permission_ids, vec!["perm-1"]);
    }

    #[test]
    fn step3_lowercase_a_is_not_consumed_by_permission_shortcuts() {
        let mut app = App::test_default();
        let mut rx = add_permission(&mut app, "perm-1", allow_options(), true);

        let consumed = handle_permission_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), crossterm::event::KeyModifiers::NONE),
        );

        assert!(!consumed, "lowercase 'a' should flow to normal typing");
        assert_eq!(app.pending_permission_ids, vec!["perm-1"]);
        assert!(matches!(rx.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
    }

    #[test]
    fn step4_ctrl_y_maps_to_allow_once_kind_and_only_resolves_one_permission() {
        let mut app = App::test_default();
        let mut rx1 = add_permission(
            &mut app,
            "perm-1",
            vec![
                acp::PermissionOption::new(
                    "allow-always",
                    "Allow always",
                    PermissionOptionKind::AllowAlways,
                ),
                acp::PermissionOption::new(
                    "allow-once",
                    "Allow once",
                    PermissionOptionKind::AllowOnce,
                ),
                acp::PermissionOption::new(
                    "reject-once",
                    "Reject",
                    PermissionOptionKind::RejectOnce,
                ),
            ],
            true,
        );
        let mut rx2 = add_permission(&mut app, "perm-2", allow_options(), false);

        let consumed = handle_permission_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('y'), crossterm::event::KeyModifiers::CONTROL),
        );
        assert!(consumed);

        let resp1 = rx1.try_recv().expect("first permission should be answered");
        let acp::RequestPermissionOutcome::Selected(sel1) = resp1.outcome else {
            panic!("expected selected permission response");
        };
        assert_eq!(sel1.option_id.to_string(), "allow-once");
        assert_eq!(app.pending_permission_ids, vec!["perm-2"]);
        assert!(matches!(rx2.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
    }

    #[test]
    fn plain_y_and_n_are_not_consumed() {
        let mut app = App::test_default();
        let mut rx = add_permission(&mut app, "perm-1", allow_options(), true);

        let consumed_y = handle_permission_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('y'), crossterm::event::KeyModifiers::NONE),
        );
        let consumed_n = handle_permission_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('n'), crossterm::event::KeyModifiers::NONE),
        );

        assert!(!consumed_y);
        assert!(!consumed_n);
        assert_eq!(app.pending_permission_ids, vec!["perm-1"]);
        assert!(matches!(rx.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
    }

    #[test]
    fn ctrl_n_rejects_focused_permission() {
        let mut app = App::test_default();
        let mut rx = add_permission(&mut app, "perm-1", allow_options(), true);

        let consumed = handle_permission_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('n'), crossterm::event::KeyModifiers::CONTROL),
        );
        assert!(consumed);
        assert!(app.pending_permission_ids.is_empty());

        let resp = rx.try_recv().expect("permission should be answered by ctrl+n");
        let acp::RequestPermissionOutcome::Selected(sel) = resp.outcome else {
            panic!("expected selected permission response");
        };
        assert_eq!(sel.option_id.to_string(), "reject-once");
    }

    #[test]
    fn ctrl_n_does_not_trigger_reject_always() {
        let mut app = App::test_default();
        let mut rx = add_permission(
            &mut app,
            "perm-1",
            vec![
                acp::PermissionOption::new(
                    "allow-once",
                    "Allow once",
                    PermissionOptionKind::AllowOnce,
                ),
                acp::PermissionOption::new(
                    "reject-always",
                    "Reject always",
                    PermissionOptionKind::RejectAlways,
                ),
            ],
            true,
        );

        let consumed = handle_permission_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('n'), crossterm::event::KeyModifiers::CONTROL),
        );
        assert!(!consumed);
        assert_eq!(app.pending_permission_ids, vec!["perm-1"]);
        assert!(matches!(rx.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
    }

    #[test]
    fn left_right_not_consumed_when_permission_not_focused() {
        let mut app = App::test_default();
        let mut rx = add_permission(&mut app, "perm-1", allow_options(), false);

        let consumed_left = handle_permission_key(
            &mut app,
            KeyEvent::new(KeyCode::Left, crossterm::event::KeyModifiers::NONE),
        );
        let consumed_right = handle_permission_key(
            &mut app,
            KeyEvent::new(KeyCode::Right, crossterm::event::KeyModifiers::NONE),
        );

        assert!(!consumed_left);
        assert!(!consumed_right);
        assert!(matches!(rx.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
    }

    #[test]
    fn enter_not_consumed_when_permission_not_focused() {
        let mut app = App::test_default();
        let mut rx = add_permission(&mut app, "perm-1", allow_options(), false);

        let consumed = handle_permission_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, crossterm::event::KeyModifiers::NONE),
        );

        assert!(!consumed);
        assert_eq!(app.pending_permission_ids, vec!["perm-1"]);
        assert!(matches!(rx.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
    }

    #[test]
    fn keeps_non_tool_blocks_untouched() {
        let app = App::test_default();
        let _ = IncrementalMarkdown::default();
        assert!(app.messages.is_empty());
    }
}
