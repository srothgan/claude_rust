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

use super::{
    App, AppStatus, CancelOrigin, FocusOwner, FocusTarget, HelpView, MessageBlock, ModeInfo,
    ModeState,
};
use crate::app::input::parse_paste_placeholder;
use crate::app::permissions::handle_permission_key;
use crate::app::selection::clear_selection;
use crate::app::{mention, slash};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::rc::Rc;

const HELP_TAB_PREV_KEY: KeyCode = KeyCode::Left;
const HELP_TAB_NEXT_KEY: KeyCode = KeyCode::Right;

fn is_ctrl_shortcut(modifiers: KeyModifiers) -> bool {
    modifiers.contains(KeyModifiers::CONTROL) && !modifiers.contains(KeyModifiers::ALT)
}

fn is_ctrl_char_shortcut(key: KeyEvent, expected: char) -> bool {
    is_ctrl_shortcut(key.modifiers)
        && matches!(key.code, KeyCode::Char(c) if c.eq_ignore_ascii_case(&expected))
}

fn is_permission_ctrl_shortcut(key: KeyEvent) -> bool {
    is_ctrl_char_shortcut(key, 'y')
        || is_ctrl_char_shortcut(key, 'a')
        || is_ctrl_char_shortcut(key, 'n')
}

fn handle_always_allowed_shortcuts(app: &mut App, key: KeyEvent) -> bool {
    if is_ctrl_char_shortcut(key, 'q') {
        app.should_quit = true;
        return true;
    }
    if is_ctrl_char_shortcut(key, 'c') {
        if copy_selection_to_clipboard(app) {
            clear_selection(app);
            return true;
        }
        app.should_quit = true;
        return true;
    }
    false
}

fn copy_selection_to_clipboard(app: &App) -> bool {
    let Some(selection) = app.selection else {
        return false;
    };
    let selected_text = selection_text_from_rendered_lines(app, selection);
    if selected_text.is_empty() {
        return false;
    }
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(selected_text);
    }
    true
}

fn selection_text_from_rendered_lines(app: &App, selection: super::SelectionState) -> String {
    let lines = match selection.kind {
        super::SelectionKind::Chat => &app.rendered_chat_lines,
        super::SelectionKind::Input => &app.rendered_input_lines,
    };
    if lines.is_empty() {
        return String::new();
    }

    let (start, end) = super::normalize_selection(selection.start, selection.end);
    if start.row >= lines.len() {
        return String::new();
    }
    let last_row = end.row.min(lines.len().saturating_sub(1));

    let mut out = String::new();
    for row in start.row..=last_row {
        let line = lines.get(row).map_or("", String::as_str);
        let start_col = if row == start.row { start.col } else { 0 };
        let end_col = if row == end.row { end.col } else { line.chars().count() };
        out.push_str(&slice_by_cols(line, start_col, end_col));
        if row < last_row {
            out.push('\n');
        }
    }
    out
}

fn slice_by_cols(text: &str, start_col: usize, end_col: usize) -> String {
    if start_col >= end_col {
        return String::new();
    }
    let mut out = String::new();
    for (i, ch) in text.chars().enumerate() {
        if i >= end_col {
            break;
        }
        if i >= start_col {
            out.push(ch);
        }
    }
    out
}

pub(super) fn dispatch_key_by_focus(app: &mut App, key: KeyEvent) {
    if handle_always_allowed_shortcuts(app, key) {
        return;
    }

    if matches!(app.status, AppStatus::Connecting | AppStatus::Resuming | AppStatus::Error) {
        handle_blocked_input_shortcuts(app, key);
        return;
    }

    sync_help_focus(app);

    if handle_global_shortcuts(app, key) {
        return;
    }

    match app.focus_owner() {
        FocusOwner::Mention => handle_autocomplete_key(app, key),
        FocusOwner::Help => handle_help_key(app, key),
        FocusOwner::Permission => {
            if !handle_permission_key(app, key) {
                handle_normal_key(app, key);
            }
        }
        FocusOwner::Input | FocusOwner::TodoList => handle_normal_key(app, key),
    }
}

/// During blocked-input states (Connecting, Resuming, Error), keep input disabled and only allow
/// navigation/help shortcuts.
fn handle_blocked_input_shortcuts(app: &mut App, key: KeyEvent) {
    if is_ctrl_char_shortcut(key, 'u') && app.update_check_hint.is_some() {
        app.update_check_hint = None;
        sync_help_focus(app);
        return;
    }

    if is_ctrl_char_shortcut(key, 'h') {
        toggle_header(app);
        sync_help_focus(app);
        return;
    }

    if is_ctrl_char_shortcut(key, 'l') {
        app.force_redraw = true;
        sync_help_focus(app);
        return;
    }

    match (key.code, key.modifiers) {
        (KeyCode::Char('?'), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
            if app.is_help_active() {
                app.input.clear();
            } else {
                app.input.set_text("?");
            }
        }
        (HELP_TAB_PREV_KEY, m) if m == KeyModifiers::NONE && app.is_help_active() => {
            set_help_view(app, HelpView::Keys);
        }
        (HELP_TAB_NEXT_KEY, m) if m == KeyModifiers::NONE && app.is_help_active() => {
            set_help_view(app, HelpView::SlashCommands);
        }
        (KeyCode::Up, m) if m == KeyModifiers::NONE || m == KeyModifiers::CONTROL => {
            app.viewport.scroll_up(1);
        }
        (KeyCode::Down, m) if m == KeyModifiers::NONE || m == KeyModifiers::CONTROL => {
            app.viewport.scroll_down(1);
        }
        _ => {}
    }

    sync_help_focus(app);
}

/// Handle shortcuts that should work regardless of current focus owner.
fn handle_global_shortcuts(app: &mut App, key: KeyEvent) -> bool {
    // Session-only dismiss for update hint.
    if is_ctrl_char_shortcut(key, 'u') && app.update_check_hint.is_some() {
        app.update_check_hint = None;
        return true;
    }

    // Permission quick shortcuts are global when permissions are pending.
    if !app.pending_permission_ids.is_empty() && is_permission_ctrl_shortcut(key) {
        return handle_permission_key(app, key);
    }

    match (key.code, key.modifiers) {
        (KeyCode::Char('t'), m) if m == KeyModifiers::CONTROL => {
            toggle_todo_panel_focus(app);
            true
        }
        (KeyCode::Char('o'), m) if m == KeyModifiers::CONTROL => {
            toggle_all_tool_calls(app);
            true
        }
        (KeyCode::Char('l'), m) if m == KeyModifiers::CONTROL => {
            app.force_redraw = true;
            true
        }
        (KeyCode::Char('h'), m) if m == KeyModifiers::CONTROL => {
            toggle_header(app);
            true
        }
        (KeyCode::Up, m) if m == KeyModifiers::CONTROL => {
            app.viewport.scroll_up(1);
            true
        }
        (KeyCode::Down, m) if m == KeyModifiers::CONTROL => {
            app.viewport.scroll_down(1);
            true
        }
        _ => false,
    }
}

#[inline]
pub(super) fn is_printable_text_modifiers(modifiers: KeyModifiers) -> bool {
    let ctrl_alt =
        modifiers.contains(KeyModifiers::CONTROL) && modifiers.contains(KeyModifiers::ALT);
    !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) || ctrl_alt
}

#[allow(clippy::too_many_lines)]
pub(super) fn handle_normal_key(app: &mut App, key: KeyEvent) {
    sync_help_focus(app);
    let input_version_before = app.input.version;

    // Timing-based paste detection: if key events arrive faster than the
    // burst interval, this is a paste (not typing). Cancel any pending submit.
    app.drain_key_count += 1;
    let was_paste = app.paste_burst.is_paste();
    let in_paste = app.paste_burst.on_key_event(app.input.lines.len());
    if in_paste && app.pending_submit {
        app.pending_submit = false;
    }
    let on_placeholder_line = app
        .input
        .lines
        .get(app.input.cursor_row)
        .and_then(|line| parse_paste_placeholder(line))
        .is_some();
    if in_paste && on_placeholder_line {
        // First transition into paste mode: remove the known leaked leading key
        // pattern (`<one char line>` + `<placeholder line>`).
        if !was_paste {
            cleanup_leaked_char_before_placeholder(app);
        }
        // While burst mode is active and a placeholder already represents the paste,
        // ignore burst key payload so no extra characters leak into the input.
        if matches!(
            key.code,
            KeyCode::Char(_) | KeyCode::Enter | KeyCode::Tab | KeyCode::Backspace | KeyCode::Delete
        ) {
            return;
        }
    }

    match (key.code, key.modifiers) {
        // Esc: cancel current turn if thinking/running
        (KeyCode::Esc, _) => {
            if app.focus_owner() == FocusOwner::TodoList {
                app.release_focus_target(FocusTarget::TodoList);
                return;
            }
            if matches!(app.status, AppStatus::Thinking | AppStatus::Running)
                && let Err(message) = super::input_submit::request_cancel(app, CancelOrigin::Manual)
            {
                tracing::error!("Failed to send cancel: {message}");
            }
        }
        // Enter (no modifiers): deferred submit for paste detection.
        // Insert a newline now; if no more keys arrive in this drain cycle
        // the main loop strips the trailing newline and calls submit_input().
        (KeyCode::Enter, m)
            if app.focus_owner() != FocusOwner::TodoList
                && !m.contains(KeyModifiers::SHIFT)
                && !m.contains(KeyModifiers::CONTROL) =>
        {
            let _ = app.input.textarea_insert_newline();
            app.pending_submit = true;
        }
        // Ctrl+Enter or Shift+Enter: explicit newline (never submits)
        (KeyCode::Enter, _) if app.focus_owner() != FocusOwner::TodoList => {
            app.pending_submit = false;
            let _ = app.input.textarea_insert_newline();
        }
        // TextArea-native history
        (KeyCode::Char('z'), m)
            if app.focus_owner() != FocusOwner::TodoList && m == KeyModifiers::CONTROL =>
        {
            let _ = app.input.textarea_undo();
        }
        (KeyCode::Char('y'), m)
            if app.focus_owner() != FocusOwner::TodoList && m == KeyModifiers::CONTROL =>
        {
            let _ = app.input.textarea_redo();
        }
        // Navigation
        (KeyCode::Left, m)
            if app.focus_owner() != FocusOwner::TodoList
                && m.contains(KeyModifiers::CONTROL)
                && !m.contains(KeyModifiers::ALT) =>
        {
            let _ = app.input.textarea_move_word_left();
        }
        (KeyCode::Right, m)
            if app.focus_owner() != FocusOwner::TodoList
                && m.contains(KeyModifiers::CONTROL)
                && !m.contains(KeyModifiers::ALT) =>
        {
            let _ = app.input.textarea_move_word_right();
        }
        (KeyCode::Left, _) if app.focus_owner() != FocusOwner::TodoList => {
            let _ = app.input.textarea_move_left();
        }
        (KeyCode::Right, _) if app.focus_owner() != FocusOwner::TodoList => {
            let _ = app.input.textarea_move_right();
        }
        (KeyCode::Up, _) if app.focus_owner() == FocusOwner::TodoList => {
            move_todo_selection_up(app);
        }
        (KeyCode::Down, _) if app.focus_owner() == FocusOwner::TodoList => {
            move_todo_selection_down(app);
        }
        (KeyCode::Up, _) => {
            if !try_move_input_cursor_up(app) {
                app.viewport.scroll_up(1);
            }
        }
        (KeyCode::Down, _) => {
            if !try_move_input_cursor_down(app) {
                app.viewport.scroll_down(1);
            }
        }
        (KeyCode::Home, _) if app.focus_owner() != FocusOwner::TodoList => {
            let _ = app.input.textarea_move_home();
        }
        (KeyCode::End, _) if app.focus_owner() != FocusOwner::TodoList => {
            let _ = app.input.textarea_move_end();
        }
        // Tab: toggle focus between input and open todo list
        (KeyCode::Tab, m)
            if !m.contains(KeyModifiers::SHIFT)
                && !m.contains(KeyModifiers::CONTROL)
                && !m.contains(KeyModifiers::ALT)
                && app.show_todo_panel
                && !app.todos.is_empty() =>
        {
            if app.focus_owner() == FocusOwner::TodoList {
                app.release_focus_target(FocusTarget::TodoList);
            } else {
                app.claim_focus_target(FocusTarget::TodoList);
            }
        }
        // Shift+Tab: cycle session mode
        (KeyCode::BackTab, _) => {
            if let Some(ref mode) = app.mode
                && mode.available_modes.len() > 1
            {
                let current_idx = mode
                    .available_modes
                    .iter()
                    .position(|m| m.id == mode.current_mode_id)
                    .unwrap_or(0);
                let next_idx = (current_idx + 1) % mode.available_modes.len();
                let next = &mode.available_modes[next_idx];

                // Fire-and-forget mode switch
                if let Some(ref conn) = app.conn
                    && let Some(sid) = app.session_id.clone()
                {
                    let mode_id = next.id.clone();
                    let conn = Rc::clone(conn);
                    tokio::task::spawn_local(async move {
                        if let Err(e) = conn.set_mode(sid.to_string(), mode_id) {
                            tracing::error!("Failed to set mode: {e}");
                        }
                    });
                }

                // Optimistic UI update (CurrentModeUpdate will confirm)
                let next_id = next.id.clone();
                let next_name = next.name.clone();
                let modes = mode
                    .available_modes
                    .iter()
                    .map(|m| ModeInfo { id: m.id.clone(), name: m.name.clone() })
                    .collect();
                app.mode = Some(ModeState {
                    current_mode_id: next_id,
                    current_mode_name: next_name,
                    available_modes: modes,
                });
                app.cached_footer_line = None;
            }
        }
        // Editing
        (KeyCode::Backspace, m)
            if app.focus_owner() != FocusOwner::TodoList
                && m.contains(KeyModifiers::CONTROL)
                && !m.contains(KeyModifiers::ALT) =>
        {
            let _ = app.input.textarea_delete_word_before();
        }
        (KeyCode::Delete, m)
            if app.focus_owner() != FocusOwner::TodoList
                && m.contains(KeyModifiers::CONTROL)
                && !m.contains(KeyModifiers::ALT) =>
        {
            let _ = app.input.textarea_delete_word_after();
        }
        (KeyCode::Backspace, _) if app.focus_owner() != FocusOwner::TodoList => {
            let _ = app.input.textarea_delete_char_before();
        }
        (KeyCode::Delete, _) if app.focus_owner() != FocusOwner::TodoList => {
            let _ = app.input.textarea_delete_char_after();
        }
        // Printable characters
        (KeyCode::Char(c), m) if is_printable_text_modifiers(m) => {
            if app.focus_owner() == FocusOwner::TodoList {
                app.release_focus_target(FocusTarget::TodoList);
            }
            let _ = app.input.textarea_insert_char(c);
            if c == '@' {
                mention::activate(app);
            } else if c == '/' {
                slash::activate(app);
            }
        }
        _ => {}
    }

    if app.input.version != input_version_before && should_sync_autocomplete_after_key(app, key) {
        mention::sync_with_cursor(app);
        slash::sync_with_cursor(app);
    }

    sync_help_focus(app);
}

fn try_move_input_cursor_up(app: &mut App) -> bool {
    let before = (app.input.cursor_row, app.input.cursor_col);
    let _ = app.input.textarea_move_up();
    (app.input.cursor_row, app.input.cursor_col) != before
}

fn try_move_input_cursor_down(app: &mut App) -> bool {
    let before = (app.input.cursor_row, app.input.cursor_col);
    let _ = app.input.textarea_move_down();
    (app.input.cursor_row, app.input.cursor_col) != before
}

fn should_sync_autocomplete_after_key(app: &App, key: KeyEvent) -> bool {
    if app.focus_owner() == FocusOwner::TodoList {
        return false;
    }

    match (key.code, key.modifiers) {
        (
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::Backspace
            | KeyCode::Delete
            | KeyCode::Enter,
            _,
        ) => true,
        (KeyCode::Char('z' | 'y'), m) if m == KeyModifiers::CONTROL => true,
        (KeyCode::Char(_), m) if is_printable_text_modifiers(m) => true,
        _ => false,
    }
}

/// Remove a leaked pre-placeholder character caused by key-burst + paste-event
/// overlap. We only touch the narrow shape:
/// line 0 = exactly one char, line 1 = placeholder (cursor on placeholder).
pub(super) fn cleanup_leaked_char_before_placeholder(app: &mut App) {
    if app.input.lines.len() != 2 || app.input.cursor_row != 1 {
        return;
    }
    if app.input.lines[0].chars().count() != 1 {
        return;
    }
    app.input.lines.remove(0);
    app.input.cursor_row = 0;
    app.input.cursor_col = app.input.lines[0].chars().count();
    app.input.version += 1;
    app.input.sync_textarea_engine();
}

pub(super) fn toggle_todo_panel_focus(app: &mut App) {
    if app.todos.is_empty() {
        app.show_todo_panel = false;
        app.release_focus_target(FocusTarget::TodoList);
        app.todo_scroll = 0;
        app.todo_selected = 0;
        return;
    }

    app.show_todo_panel = !app.show_todo_panel;
    if app.show_todo_panel {
        app.claim_focus_target(FocusTarget::TodoList);
        // Start at in-progress todo when available; fallback to first item.
        app.todo_selected =
            app.todos.iter().position(|t| t.status == super::TodoStatus::InProgress).unwrap_or(0);
    } else {
        app.release_focus_target(FocusTarget::TodoList);
    }
}

pub(super) fn move_todo_selection_up(app: &mut App) {
    if app.todos.is_empty() || !app.show_todo_panel {
        app.release_focus_target(FocusTarget::TodoList);
        return;
    }
    app.todo_selected = app.todo_selected.saturating_sub(1);
}

pub(super) fn move_todo_selection_down(app: &mut App) {
    if app.todos.is_empty() || !app.show_todo_panel {
        app.release_focus_target(FocusTarget::TodoList);
        return;
    }
    let max = app.todos.len().saturating_sub(1);
    if app.todo_selected < max {
        app.todo_selected += 1;
    }
}

/// Handle keystrokes while mention/slash autocomplete dropdown is active.
pub(super) fn handle_autocomplete_key(app: &mut App, key: KeyEvent) {
    if app.mention.is_some() {
        handle_mention_key(app, key);
        return;
    }
    if app.slash.is_some() {
        handle_slash_key(app, key);
        return;
    }
    dispatch_key_by_focus(app, key);
}

fn handle_help_key(app: &mut App, key: KeyEvent) {
    match (key.code, key.modifiers) {
        (HELP_TAB_PREV_KEY, m) if m == KeyModifiers::NONE => set_help_view(app, HelpView::Keys),
        (HELP_TAB_NEXT_KEY, m) if m == KeyModifiers::NONE => {
            set_help_view(app, HelpView::SlashCommands);
        }
        _ => handle_normal_key(app, key),
    }
}

fn set_help_view(app: &mut App, next: HelpView) {
    if app.help_view != next {
        tracing::debug!(from = ?app.help_view, to = ?next, "Help view changed via keyboard");
        app.help_view = next;
    }
}

fn sync_help_focus(app: &mut App) {
    if app.is_help_active()
        && app.pending_permission_ids.is_empty()
        && app.mention.is_none()
        && app.slash.is_none()
    {
        app.claim_focus_target(FocusTarget::Help);
    } else {
        app.release_focus_target(FocusTarget::Help);
    }
}

/// Handle keystrokes while the `@` mention autocomplete dropdown is active.
pub(super) fn handle_mention_key(app: &mut App, key: KeyEvent) {
    match (key.code, key.modifiers) {
        (KeyCode::Up, _) => mention::move_up(app),
        (KeyCode::Down, _) => mention::move_down(app),
        (KeyCode::Enter | KeyCode::Tab, _) => mention::confirm_selection(app),
        (KeyCode::Esc, _) => mention::deactivate(app),
        (KeyCode::Backspace, _) => {
            let _ = app.input.textarea_delete_char_before();
            mention::update_query(app);
        }
        (KeyCode::Char(c), m) if is_printable_text_modifiers(m) => {
            let _ = app.input.textarea_insert_char(c);
            if c.is_whitespace() {
                mention::deactivate(app);
            } else {
                mention::update_query(app);
            }
        }
        // Any other key: deactivate mention and forward to normal handling
        _ => {
            mention::deactivate(app);
            dispatch_key_by_focus(app, key);
        }
    }
}

/// Handle keystrokes while slash autocomplete dropdown is active.
fn handle_slash_key(app: &mut App, key: KeyEvent) {
    match (key.code, key.modifiers) {
        (KeyCode::Up, _) => slash::move_up(app),
        (KeyCode::Down, _) => slash::move_down(app),
        (KeyCode::Enter | KeyCode::Tab, _) => slash::confirm_selection(app),
        (KeyCode::Esc, _) => slash::deactivate(app),
        (KeyCode::Backspace, _) => {
            let _ = app.input.textarea_delete_char_before();
            slash::update_query(app);
        }
        (KeyCode::Char(c), m) if is_printable_text_modifiers(m) => {
            let _ = app.input.textarea_insert_char(c);
            slash::update_query(app);
        }
        _ => {
            slash::deactivate(app);
            dispatch_key_by_focus(app, key);
        }
    }
}

/// Toggle the session-level collapsed preference and apply to all tool calls.
pub(super) fn toggle_all_tool_calls(app: &mut App) {
    app.tools_collapsed = !app.tools_collapsed;
    for msg in &mut app.messages {
        for block in &mut msg.blocks {
            if let MessageBlock::ToolCall(tc) = block {
                let tc = tc.as_mut();
                tc.collapsed = app.tools_collapsed;
                tc.cache.invalidate();
            }
        }
    }
    app.mark_all_message_layout_dirty();
}

/// Toggle the header visibility.
pub(super) fn toggle_header(app: &mut App) {
    app.show_header = !app.show_header;
}
