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
use crate::agent::model;
use crate::agent::model::PermissionOptionKind;
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
    let mut invalidated = false;
    if let Some(msg) = app.messages.get_mut(mi)
        && let Some(MessageBlock::ToolCall(tc)) = msg.blocks.get_mut(bi)
    {
        let tc = tc.as_mut();
        if let Some(ref mut perm) = tc.pending_permission {
            perm.focused = focused;
        }
        tc.cache.invalidate();
        invalidated = true;
    }
    if invalidated {
        app.mark_message_layout_dirty(mi);
    }
}

/// Find the option index for the currently focused permission by kind.
fn focused_option_index_by_kind(app: &App, kind: PermissionOptionKind) -> Option<usize> {
    focused_option_index_where(app, |opt| opt.kind == kind)
}

fn focused_option_index_where<F>(app: &App, mut predicate: F) -> Option<usize>
where
    F: FnMut(&model::PermissionOption) -> bool,
{
    let tool_id = app.pending_permission_ids.first()?;
    let (mi, bi) = app.tool_call_index.get(tool_id).copied()?;
    let MessageBlock::ToolCall(tc) = app.messages.get(mi)?.blocks.get(bi)? else {
        return None;
    };
    let pending = tc.pending_permission.as_ref()?;
    pending.options.iter().position(&mut predicate)
}

fn is_ctrl_shortcut(modifiers: KeyModifiers) -> bool {
    modifiers.contains(KeyModifiers::CONTROL) && !modifiers.contains(KeyModifiers::ALT)
}

fn is_ctrl_char_shortcut(key: KeyEvent, expected: char) -> bool {
    is_ctrl_shortcut(key.modifiers)
        && matches!(key.code, KeyCode::Char(c) if c.eq_ignore_ascii_case(&expected))
}

fn normalized_option_tokens(option: &model::PermissionOption) -> String {
    let mut out = String::new();
    for ch in option.name.chars().chain(option.option_id.chars()) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        }
    }
    out
}

fn option_tokens(option: &model::PermissionOption) -> (bool, bool, bool, bool) {
    let tokens = normalized_option_tokens(option);
    let allow_like =
        tokens.contains("allow") || tokens.contains("accept") || tokens.contains("approve");
    let reject_like =
        tokens.contains("reject") || tokens.contains("deny") || tokens.contains("disallow");
    let persistent_like = tokens.contains("always")
        || tokens.contains("dontask")
        || tokens.contains("remember")
        || tokens.contains("persist")
        || tokens.contains("bypasspermissions");
    let session_like = tokens.contains("session") || tokens.contains("onesession");
    (allow_like, reject_like, persistent_like, session_like)
}

fn option_is_allow_once_fallback(option: &model::PermissionOption) -> bool {
    let (allow_like, reject_like, persistent_like, session_like) = option_tokens(option);
    allow_like && !reject_like && !persistent_like && !session_like
}

fn option_is_allow_always_fallback(option: &model::PermissionOption) -> bool {
    let (allow_like, reject_like, persistent_like, _) = option_tokens(option);
    allow_like && !reject_like && persistent_like
}

fn option_is_allow_non_once_fallback(option: &model::PermissionOption) -> bool {
    let (allow_like, reject_like, persistent_like, session_like) = option_tokens(option);
    allow_like && !reject_like && (persistent_like || session_like)
}

fn option_is_reject_once_fallback(option: &model::PermissionOption) -> bool {
    let (allow_like, reject_like, persistent_like, _) = option_tokens(option);
    reject_like && !allow_like && !persistent_like
}

fn option_is_reject_fallback(option: &model::PermissionOption) -> bool {
    let (allow_like, reject_like, _, _) = option_tokens(option);
    reject_like && !allow_like
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

fn focused_permission_is_question_prompt(app: &App) -> bool {
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
    let Some(pending) = tc.pending_permission.as_ref() else {
        return false;
    };
    !pending.options.is_empty()
        && pending
            .options
            .iter()
            .all(|opt| matches!(opt.kind, PermissionOptionKind::QuestionChoice))
}

fn handle_permission_focus_cycle(
    app: &mut App,
    key: KeyEvent,
    permission_has_focus: bool,
) -> Option<bool> {
    if !permission_has_focus {
        return None;
    }
    if !matches!(key.code, KeyCode::Up | KeyCode::Down) {
        return None;
    }
    if app.pending_permission_ids.len() <= 1 {
        if focused_permission_is_question_prompt(app) {
            // For AskUserQuestion prompts, Up/Down are option navigation keys.
            return None;
        }
        // Single pending permission: consume navigation keys so they do not
        // leak into normal chat/input scrolling.
        return Some(true);
    }

    // Unfocus the current (first) permission.
    set_permission_focused(app, 0, false);

    if key.code == KeyCode::Down {
        // Move first to end (rotate forward).
        let first = app.pending_permission_ids.remove(0);
        app.pending_permission_ids.push(first);
    } else {
        // Move last to front (rotate backward).
        let Some(last) = app.pending_permission_ids.pop() else {
            return Some(false);
        };
        app.pending_permission_ids.insert(0, last);
    }

    // Focus the new first permission and scroll to it.
    set_permission_focused(app, 0, true);
    app.viewport.engage_auto_scroll();
    Some(true)
}

fn move_permission_option_left(app: &mut App) {
    let dirty_idx =
        app.pending_permission_ids.first().and_then(|tool_id| app.lookup_tool_call(tool_id));
    if let Some(tc) = get_focused_permission_tc(app)
        && let Some(ref mut p) = tc.pending_permission
    {
        p.selected_index = p.selected_index.saturating_sub(1);
        tc.cache.invalidate();
    }
    if let Some((mi, _)) = dirty_idx {
        app.mark_message_layout_dirty(mi);
    }
}

fn move_permission_option_right(app: &mut App, option_count: usize) {
    let dirty_idx =
        app.pending_permission_ids.first().and_then(|tool_id| app.lookup_tool_call(tool_id));
    if let Some(tc) = get_focused_permission_tc(app)
        && let Some(ref mut p) = tc.pending_permission
        && p.selected_index + 1 < option_count
    {
        p.selected_index += 1;
        tc.cache.invalidate();
    }
    if let Some((mi, _)) = dirty_idx {
        app.mark_message_layout_dirty(mi);
    }
}

fn handle_permission_option_keys(
    app: &mut App,
    key: KeyEvent,
    permission_has_focus: bool,
    option_count: usize,
    question_prompt: bool,
) -> Option<bool> {
    if !permission_has_focus {
        return None;
    }
    match key.code {
        KeyCode::Left if option_count > 0 => {
            move_permission_option_left(app);
            Some(true)
        }
        KeyCode::Right if option_count > 0 => {
            move_permission_option_right(app, option_count);
            Some(true)
        }
        KeyCode::Up if question_prompt && option_count > 0 => {
            move_permission_option_left(app);
            Some(true)
        }
        KeyCode::Down if question_prompt && option_count > 0 => {
            move_permission_option_right(app, option_count);
            Some(true)
        }
        KeyCode::Enter if option_count > 0 => {
            respond_permission(app, None);
            Some(true)
        }
        KeyCode::Esc => {
            if question_prompt {
                respond_permission_cancel(app);
                return Some(true);
            }
            if let Some(idx) = focused_option_index_by_kind(app, PermissionOptionKind::RejectOnce)
                .or_else(|| focused_option_index_by_kind(app, PermissionOptionKind::RejectAlways))
                .or_else(|| focused_option_index_where(app, option_is_reject_fallback))
            {
                respond_permission(app, Some(idx));
                Some(true)
            } else if option_count > 0 {
                // Fallback for unknown adapters: keep previous behavior if no kind metadata.
                respond_permission(app, Some(option_count - 1));
                Some(true)
            } else {
                Some(false)
            }
        }
        _ => None,
    }
}

fn handle_permission_quick_shortcuts(app: &mut App, key: KeyEvent) -> Option<bool> {
    if !matches!(key.code, KeyCode::Char(_)) {
        return None;
    }
    if focused_permission_is_question_prompt(app)
        && (is_ctrl_char_shortcut(key, 'y')
            || is_ctrl_char_shortcut(key, 'a')
            || is_ctrl_char_shortcut(key, 'n'))
    {
        return Some(true);
    }
    if is_ctrl_char_shortcut(key, 'y') {
        if let Some(idx) = focused_option_index_by_kind(app, PermissionOptionKind::AllowOnce)
            .or_else(|| focused_option_index_where(app, option_is_allow_once_fallback))
            .or_else(|| focused_option_index_by_kind(app, PermissionOptionKind::AllowSession))
            .or_else(|| focused_option_index_by_kind(app, PermissionOptionKind::AllowAlways))
            .or_else(|| focused_option_index_where(app, option_is_allow_always_fallback))
            .or_else(|| focused_option_index_where(app, option_is_allow_non_once_fallback))
        {
            respond_permission(app, Some(idx));
            return Some(true);
        }
        return Some(false);
    }
    if is_ctrl_char_shortcut(key, 'a') {
        if let Some(idx) = focused_option_index_by_kind(app, PermissionOptionKind::AllowSession)
            .or_else(|| focused_option_index_by_kind(app, PermissionOptionKind::AllowAlways))
            .or_else(|| focused_option_index_where(app, option_is_allow_non_once_fallback))
        {
            respond_permission(app, Some(idx));
            return Some(true);
        }
        return Some(false);
    }
    if is_ctrl_char_shortcut(key, 'n') {
        if let Some(idx) = focused_option_index_by_kind(app, PermissionOptionKind::RejectOnce)
            .or_else(|| focused_option_index_where(app, option_is_reject_once_fallback))
        {
            respond_permission(app, Some(idx));
            return Some(true);
        }
        return Some(false);
    }
    None
}

/// Handle permission-only shortcuts.
/// Returns `true` when the key was consumed by permission UI.
pub(super) fn handle_permission_key(app: &mut App, key: KeyEvent) -> bool {
    let option_count = get_focused_permission_tc(app)
        .and_then(|tc| tc.pending_permission.as_ref())
        .map_or(0, |p| p.options.len());
    let permission_has_focus = focused_permission_is_active(app);
    let question_prompt = focused_permission_is_question_prompt(app);

    if let Some(consumed) = handle_permission_focus_cycle(app, key, permission_has_focus) {
        return consumed;
    }
    if let Some(consumed) =
        handle_permission_option_keys(app, key, permission_has_focus, option_count, question_prompt)
    {
        return consumed;
    }
    if let Some(consumed) = handle_permission_quick_shortcuts(app, key) {
        return consumed;
    }
    false
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
    let mut invalidated = false;
    if let Some(pending) = tc.pending_permission.take() {
        let idx = override_index.unwrap_or(pending.selected_index);
        if let Some(opt) = pending.options.get(idx) {
            tracing::debug!(
                "permission selection: tool_call_id={} option_id={} option_name={} option_kind={:?}",
                tool_id,
                opt.option_id,
                opt.name,
                opt.kind
            );
            let _ = pending.response_tx.send(model::RequestPermissionResponse::new(
                model::RequestPermissionOutcome::Selected(model::SelectedPermissionOutcome::new(
                    opt.option_id.clone(),
                )),
            ));
        } else {
            tracing::warn!(
                "permission selection index out of bounds: tool_call_id={} selected_index={} options={}",
                tool_id,
                idx,
                pending.options.len()
            );
        }
        tc.cache.invalidate();
        invalidated = true;
    }
    if invalidated {
        app.mark_message_layout_dirty(mi);
    }

    // Focus the next permission in the queue (now at index 0), if any.
    set_permission_focused(app, 0, true);
    if app.pending_permission_ids.is_empty() {
        app.release_focus_target(FocusTarget::Permission);
    } else {
        app.claim_focus_target(FocusTarget::Permission);
    }
}

fn respond_permission_cancel(app: &mut App) {
    if app.pending_permission_ids.is_empty() {
        return;
    }
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
        let _ = pending.response_tx.send(model::RequestPermissionResponse::new(
            model::RequestPermissionOutcome::Cancelled,
        ));
        tc.cache.invalidate();
        app.mark_message_layout_dirty(mi);
    }

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
            sdk_tool_name: "Read".to_owned(),
            raw_input: None,
            status: model::ToolCallStatus::InProgress,
            content: Vec::new(),
            collapsed: false,
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
            usage: None,
        }
    }

    fn allow_options() -> Vec<model::PermissionOption> {
        vec![
            model::PermissionOption::new(
                "allow-once",
                "Allow once",
                PermissionOptionKind::AllowOnce,
            ),
            model::PermissionOption::new(
                "allow-always",
                "Allow always",
                PermissionOptionKind::AllowAlways,
            ),
            model::PermissionOption::new("reject-once", "Reject", PermissionOptionKind::RejectOnce),
        ]
    }

    fn add_permission(
        app: &mut App,
        tool_id: &str,
        options: Vec<model::PermissionOption>,
        focused: bool,
    ) -> oneshot::Receiver<model::RequestPermissionResponse> {
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
        let model::RequestPermissionOutcome::Selected(sel2) = resp2.outcome else {
            panic!("expected selected permission response");
        };
        assert_eq!(sel2.option_id.clone(), "allow-once");
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
                model::PermissionOption::new(
                    "allow-always",
                    "Allow always",
                    PermissionOptionKind::AllowAlways,
                ),
                model::PermissionOption::new(
                    "allow-once",
                    "Allow once",
                    PermissionOptionKind::AllowOnce,
                ),
                model::PermissionOption::new(
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
        let model::RequestPermissionOutcome::Selected(sel1) = resp1.outcome else {
            panic!("expected selected permission response");
        };
        assert_eq!(sel1.option_id.clone(), "allow-once");
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
        let model::RequestPermissionOutcome::Selected(sel) = resp.outcome else {
            panic!("expected selected permission response");
        };
        assert_eq!(sel.option_id.clone(), "reject-once");
    }

    #[test]
    fn ctrl_n_does_not_trigger_reject_always() {
        let mut app = App::test_default();
        let mut rx = add_permission(
            &mut app,
            "perm-1",
            vec![
                model::PermissionOption::new(
                    "allow-once",
                    "Allow once",
                    PermissionOptionKind::AllowOnce,
                ),
                model::PermissionOption::new(
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
    fn ctrl_a_matches_allow_always_by_label_when_kind_is_missing() {
        let mut app = App::test_default();
        let mut rx = add_permission(
            &mut app,
            "perm-1",
            vec![
                model::PermissionOption::new(
                    "allow-once",
                    "Allow once",
                    PermissionOptionKind::AllowOnce,
                ),
                model::PermissionOption::new(
                    "allow-always",
                    "Allow always",
                    PermissionOptionKind::AllowOnce,
                ),
                model::PermissionOption::new(
                    "reject-once",
                    "Reject",
                    PermissionOptionKind::RejectOnce,
                ),
            ],
            true,
        );

        let consumed = handle_permission_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), crossterm::event::KeyModifiers::CONTROL),
        );
        assert!(consumed);

        let resp = rx.try_recv().expect("permission should be answered by ctrl+a fallback");
        let model::RequestPermissionOutcome::Selected(sel) = resp.outcome else {
            panic!("expected selected permission response");
        };
        assert_eq!(sel.option_id.clone(), "allow-always");
    }

    #[test]
    fn ctrl_a_accepts_uppercase_with_shift_modifier() {
        let mut app = App::test_default();
        let mut rx = add_permission(&mut app, "perm-1", allow_options(), true);

        let consumed = handle_permission_key(
            &mut app,
            KeyEvent::new(
                KeyCode::Char('A'),
                crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::SHIFT,
            ),
        );
        assert!(consumed);

        let resp = rx.try_recv().expect("permission should be answered by uppercase ctrl+a");
        let model::RequestPermissionOutcome::Selected(sel) = resp.outcome else {
            panic!("expected selected permission response");
        };
        assert_eq!(sel.option_id.clone(), "allow-always");
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

    #[test]
    fn single_focused_permission_consumes_up_down_without_rotation() {
        let mut app = App::test_default();
        let mut rx = add_permission(&mut app, "perm-1", allow_options(), true);
        app.viewport.scroll_target = 7;

        let consumed_up = handle_permission_key(
            &mut app,
            KeyEvent::new(KeyCode::Up, crossterm::event::KeyModifiers::NONE),
        );
        let consumed_down = handle_permission_key(
            &mut app,
            KeyEvent::new(KeyCode::Down, crossterm::event::KeyModifiers::NONE),
        );

        assert!(consumed_up);
        assert!(consumed_down);
        assert_eq!(app.pending_permission_ids, vec!["perm-1"]);
        assert_eq!(app.viewport.scroll_target, 7);
        assert!(matches!(rx.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
    }
}
