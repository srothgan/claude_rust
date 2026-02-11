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

use super::{
    App, AppStatus, BlockCache, ChatMessage, InlinePermission, MessageBlock, MessageRole, ModeInfo,
    ModeState, SelectionKind, SelectionPoint, ToolCallInfo,
};
use crate::acp::client::ClientEvent;
use crate::app::input_submit::submit_input;
use crate::app::mention;
use crate::app::permissions::handle_permission_key;
use crate::app::selection::try_copy_selection;
use crate::app::todos::{apply_plan_todos, parse_todos, set_todos};
use agent_client_protocol::{self as acp, Agent as _};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use std::rc::Rc;

pub fn handle_terminal_event(app: &mut App, conn: &Rc<acp::ClientSideConnection>, event: Event) {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            if app.mention.is_some() {
                handle_mention_key(app, conn, key);
            } else if app.pending_permission_ids.is_empty() {
                handle_normal_key(app, conn, key);
            } else {
                handle_permission_key(app, key);
            }
        }
        Event::Mouse(mouse) => {
            handle_mouse_event(app, mouse);
        }
        Event::Paste(text) => {
            app.input.insert_str(&text);
        }
        Event::FocusGained => {
            app.refresh_git_branch();
        }
        // Resize is handled automatically by ratatui
        _ => {}
    }
}

const MOUSE_SCROLL_LINES: usize = 3;

struct MouseSelectionPoint {
    kind: SelectionKind,
    point: SelectionPoint,
}

fn handle_mouse_event(app: &mut App, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
            if let Some(pt) = mouse_point_to_selection(app, mouse) {
                app.selection = Some(super::SelectionState {
                    kind: pt.kind,
                    start: pt.point,
                    end: pt.point,
                    dragging: true,
                });
            }
        }
        MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
            let pt = mouse_point_to_selection(app, mouse);
            if let (Some(sel), Some(pt)) = (&mut app.selection, pt) {
                sel.end = pt.point;
            }
        }
        MouseEventKind::Up(crossterm::event::MouseButton::Left) => {
            if let Some(sel) = &mut app.selection {
                sel.dragging = false;
            }
        }
        _ => {}
    }
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            app.scroll_target = app.scroll_target.saturating_sub(MOUSE_SCROLL_LINES);
            app.auto_scroll = false;
        }
        MouseEventKind::ScrollDown => {
            app.scroll_target = app.scroll_target.saturating_add(MOUSE_SCROLL_LINES);
            // auto_scroll re-engagement handled by chat::render clamping
        }
        _ => {}
    }
}

fn mouse_point_to_selection(app: &App, mouse: MouseEvent) -> Option<MouseSelectionPoint> {
    let input_area = app.rendered_input_area;
    if mouse.column >= input_area.x
        && mouse.column < input_area.right()
        && mouse.row >= input_area.y
        && mouse.row < input_area.bottom()
    {
        let row = (mouse.row - input_area.y) as usize;
        let col = (mouse.column - input_area.x) as usize;
        return Some(MouseSelectionPoint {
            kind: SelectionKind::Input,
            point: SelectionPoint { row, col },
        });
    }

    let chat_area = app.rendered_chat_area;
    if mouse.column >= chat_area.x
        && mouse.column < chat_area.right()
        && mouse.row >= chat_area.y
        && mouse.row < chat_area.bottom()
    {
        let row = (mouse.row - chat_area.y) as usize;
        let col = (mouse.column - chat_area.x) as usize;
        return Some(MouseSelectionPoint {
            kind: SelectionKind::Chat,
            point: SelectionPoint { row, col },
        });
    }
    None
}

fn handle_normal_key(app: &mut App, conn: &Rc<acp::ClientSideConnection>, key: KeyEvent) {
    match (key.code, key.modifiers) {
        // Ctrl+C: quit (graceful shutdown handles cancel + cleanup)
        (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
            if try_copy_selection(app) {
                return;
            }
            app.should_quit = true;
        }
        // Esc: cancel current turn if thinking/running
        (KeyCode::Esc, _) => {
            if matches!(app.status, AppStatus::Thinking | AppStatus::Running)
                && let Some(sid) = app.session_id.clone()
            {
                let conn = Rc::clone(conn);
                tokio::task::spawn_local(async move {
                    if let Err(e) = conn.cancel(acp::CancelNotification::new(sid)).await {
                        tracing::error!("Failed to send cancel: {e}");
                    }
                });
                app.status = AppStatus::Ready;
            }
        }
        // Enter (no shift): submit input
        (KeyCode::Enter, m) if !m.contains(KeyModifiers::SHIFT) => {
            submit_input(app, conn);
        }
        // Shift+Enter: insert newline
        (KeyCode::Enter, _) => {
            app.input.insert_newline();
        }
        // Navigation
        (KeyCode::Left, _) => app.input.move_left(),
        (KeyCode::Right, _) => app.input.move_right(),
        (KeyCode::Up, m) if m.contains(KeyModifiers::CONTROL) => {
            // Ctrl+Up: scroll chat up
            app.scroll_target = app.scroll_target.saturating_sub(1);
            app.auto_scroll = false;
        }
        (KeyCode::Down, m) if m.contains(KeyModifiers::CONTROL) => {
            // Ctrl+Down: scroll chat down (clamped in chat::render)
            app.scroll_target = app.scroll_target.saturating_add(1);
        }
        (KeyCode::Up, _) => app.input.move_up(),
        (KeyCode::Down, _) => app.input.move_down(),
        (KeyCode::Home, _) => app.input.move_home(),
        (KeyCode::End, _) => app.input.move_end(),
        // Ctrl+T: toggle todo panel open/closed
        (KeyCode::Char('t'), m) if m.contains(KeyModifiers::CONTROL) => {
            app.show_todo_panel = !app.show_todo_panel;
        }
        // Ctrl+O: toggle expand/collapse on all tool calls
        (KeyCode::Char('o'), m) if m.contains(KeyModifiers::CONTROL) => {
            toggle_all_tool_calls(app);
        }
        // Ctrl+L: force full terminal redraw
        (KeyCode::Char('l'), m) if m.contains(KeyModifiers::CONTROL) => {
            app.force_redraw = true;
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
                if let Some(sid) = app.session_id.clone() {
                    let mode_id = acp::SessionModeId::new(next.id.as_str());
                    let conn = Rc::clone(conn);
                    tokio::task::spawn_local(async move {
                        if let Err(e) = conn
                            .set_session_mode(acp::SetSessionModeRequest::new(sid, mode_id))
                            .await
                        {
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
        (KeyCode::Backspace, _) => app.input.delete_char_before(),
        (KeyCode::Delete, _) => app.input.delete_char_after(),
        // Printable characters
        (KeyCode::Char(c), _) => {
            app.input.insert_char(c);
            if c == '@' {
                mention::activate(app);
            }
        }
        _ => {}
    }
}

/// Handle keystrokes while the `@` mention autocomplete dropdown is active.
fn handle_mention_key(app: &mut App, conn: &Rc<acp::ClientSideConnection>, key: KeyEvent) {
    match (key.code, key.modifiers) {
        (KeyCode::Up, _) => mention::move_up(app),
        (KeyCode::Down, _) => mention::move_down(app),
        (KeyCode::Enter | KeyCode::Tab, _) => mention::confirm_selection(app),
        (KeyCode::Esc, _) => mention::deactivate(app),
        (KeyCode::Backspace, _) => {
            app.input.delete_char_before();
            mention::update_query(app);
        }
        (KeyCode::Char(c), _) => {
            app.input.insert_char(c);
            if c.is_whitespace() {
                mention::deactivate(app);
            } else {
                mention::update_query(app);
            }
        }
        // Any other key: deactivate mention and forward to normal handling
        _ => {
            mention::deactivate(app);
            handle_normal_key(app, conn, key);
        }
    }
}

/// Toggle the session-level collapsed preference and apply to all tool calls.
fn toggle_all_tool_calls(app: &mut App) {
    app.tools_collapsed = !app.tools_collapsed;
    for msg in &mut app.messages {
        for block in &mut msg.blocks {
            if let MessageBlock::ToolCall(tc) = block {
                let tc = tc.as_mut();
                tc.collapsed = app.tools_collapsed;
                tc.cache.invalidate();
            }
        }
        // Invalidate visual height cache -- collapsed state changes rendered height
        msg.cached_visual_height = 0;
    }
}

pub fn handle_acp_event(app: &mut App, event: ClientEvent) {
    match event {
        ClientEvent::SessionUpdate(update) => handle_session_update(app, update),
        ClientEvent::PermissionRequest { request, response_tx } => {
            let tool_id = request.tool_call.tool_call_id.to_string();
            if let Some((mi, bi)) = app.lookup_tool_call(&tool_id) {
                if let Some(MessageBlock::ToolCall(tc)) =
                    app.messages.get_mut(mi).and_then(|m| m.blocks.get_mut(bi))
                {
                    let tc = tc.as_mut();
                    let is_first = app.pending_permission_ids.is_empty();
                    tc.pending_permission = Some(InlinePermission {
                        options: request.options,
                        response_tx,
                        selected_index: 0,
                        focused: is_first,
                    });
                    tc.cache.invalidate();
                    app.pending_permission_ids.push(tool_id);
                    app.auto_scroll = true;
                }
            } else {
                tracing::warn!(
                    "Permission request for unknown tool call: {tool_id}; auto-rejecting"
                );
                // Tool call not found -- reject by selecting last option
                if let Some(last_opt) = request.options.last() {
                    let _ = response_tx.send(acp::RequestPermissionResponse::new(
                        acp::RequestPermissionOutcome::Selected(
                            acp::SelectedPermissionOutcome::new(last_opt.option_id.clone()),
                        ),
                    ));
                }
            }
        }
        ClientEvent::TurnComplete => {
            app.status = AppStatus::Ready;
            app.files_accessed = 0;
            app.refresh_git_branch();
        }
        ClientEvent::TurnError(msg) => {
            tracing::error!("Turn error: {msg}");
            app.status = AppStatus::Error;
        }
    }
}

fn handle_tool_call(app: &mut App, tc: acp::ToolCall) {
    let title = tc.title.clone();
    let kind = tc.kind;
    let id_str = tc.tool_call_id.to_string();
    tracing::debug!(
        "ToolCall: id={id_str} title={title} kind={kind:?} status={:?} content_blocks={}",
        tc.status,
        tc.content.len()
    );

    // Extract claude_tool_name from meta.claudeCode.toolName
    let claude_tool_name = tc.meta.as_ref().and_then(|m| {
        m.get("claudeCode")
            .and_then(|v| v.get("toolName"))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    });

    let is_task = claude_tool_name.as_deref() == Some("Task");

    // Subagent children are never hidden -- they need to be visible so
    // permission prompts render and the user can interact with them.
    let hidden = false;

    // Extract todos from TodoWrite tool calls
    if claude_tool_name.as_deref() == Some("TodoWrite") {
        tracing::info!("TodoWrite ToolCall detected: id={id_str}, raw_input={:?}", tc.raw_input);
        if let Some(ref raw_input) = tc.raw_input {
            let todos = parse_todos(raw_input);
            tracing::info!("Parsed {} todos from ToolCall raw_input", todos.len());
            set_todos(app, todos);
        } else {
            tracing::warn!("TodoWrite ToolCall has no raw_input");
        }
    }

    // Track new Task tool calls as active subagents
    if is_task {
        app.insert_active_task(id_str.clone());
    }

    let tool_info = ToolCallInfo {
        id: id_str,
        title: shorten_tool_title(&tc.title, &app.cwd_raw),
        kind,
        status: tc.status,
        content: tc.content,
        collapsed: app.tools_collapsed,
        claude_tool_name,
        hidden,
        terminal_id: None,
        terminal_command: None,
        terminal_output: None,
        terminal_output_len: 0,
        cache: BlockCache::default(),
        pending_permission: None,
    };

    // Attach to current assistant message -- update existing or add new
    let msg_idx = app.messages.len().saturating_sub(1);
    let existing_pos = app.lookup_tool_call(&tool_info.id);
    let is_assistant =
        app.messages.last().is_some_and(|m| matches!(m.role, MessageRole::Assistant));

    if is_assistant {
        if let Some((mi, bi)) = existing_pos {
            if let Some(MessageBlock::ToolCall(existing)) =
                app.messages.get_mut(mi).and_then(|m| m.blocks.get_mut(bi))
            {
                let existing = existing.as_mut();
                existing.title.clone_from(&tool_info.title);
                existing.status = tool_info.status;
                existing.content.clone_from(&tool_info.content);
                existing.kind = tool_info.kind;
                existing.claude_tool_name.clone_from(&tool_info.claude_tool_name);
                existing.cache.invalidate();
            }
        } else if let Some(last) = app.messages.last_mut() {
            let block_idx = last.blocks.len();
            let tc_id = tool_info.id.clone();
            last.blocks.push(MessageBlock::ToolCall(Box::new(tool_info)));
            app.index_tool_call(tc_id, msg_idx, block_idx);
        }
    } else {
        // No assistant message yet — create one for this tool call
        let tc_id = tool_info.id.clone();
        let new_idx = app.messages.len();
        app.messages.push(ChatMessage {
            role: MessageRole::Assistant,
            blocks: vec![MessageBlock::ToolCall(Box::new(tool_info))],
            cached_visual_height: 0,
            cached_visual_width: 0,
        });
        app.index_tool_call(tc_id, new_idx, 0);
    }

    app.status = AppStatus::Running;
    if !hidden {
        app.files_accessed += 1;
    }
}

#[allow(clippy::too_many_lines)]
fn handle_session_update(app: &mut App, update: acp::SessionUpdate) {
    tracing::debug!("SessionUpdate variant: {}", session_update_name(&update));
    match update {
        acp::SessionUpdate::AgentMessageChunk(chunk) => {
            if let acp::ContentBlock::Text(text) = chunk.content {
                // Text is actively streaming — suppress the "Thinking..." spinner
                app.status = AppStatus::Running;

                // Append to last text block in current assistant message, or create new
                if let Some(last) = app.messages.last_mut()
                    && matches!(last.role, MessageRole::Assistant)
                {
                    // Append to last Text block if it exists, else push new one
                    if let Some(MessageBlock::Text(t, cache)) = last.blocks.last_mut() {
                        t.push_str(&text.text);
                        cache.invalidate();
                    } else {
                        last.blocks
                            .push(MessageBlock::Text(text.text.clone(), BlockCache::default()));
                    }
                    return;
                }
                app.messages.push(ChatMessage {
                    role: MessageRole::Assistant,
                    blocks: vec![MessageBlock::Text(text.text.clone(), BlockCache::default())],
                    cached_visual_height: 0,
                    cached_visual_width: 0,
                });
            }
        }
        acp::SessionUpdate::ToolCall(tc) => {
            handle_tool_call(app, tc);
        }
        acp::SessionUpdate::ToolCallUpdate(tcu) => {
            // Find and update the tool call by id (in-place)
            let id_str = tcu.tool_call_id.to_string();
            let has_content = tcu.fields.content.as_ref().map_or(0, Vec::len);
            tracing::debug!(
                "ToolCallUpdate: id={id_str} new_title={:?} new_status={:?} content_blocks={has_content}",
                tcu.fields.title,
                tcu.fields.status
            );

            // If this is a Task completing, remove from active list
            if matches!(
                tcu.fields.status,
                Some(acp::ToolCallStatus::Completed | acp::ToolCallStatus::Failed)
            ) {
                app.remove_active_task(&id_str);
            }

            let mut pending_todos: Option<Vec<super::TodoItem>> = None;
            if let Some((mi, bi)) = app.lookup_tool_call(&id_str) {
                if let Some(MessageBlock::ToolCall(tc)) =
                    app.messages.get_mut(mi).and_then(|m| m.blocks.get_mut(bi))
                {
                    let tc = tc.as_mut();
                    if let Some(status) = tcu.fields.status {
                        tc.status = status;
                    }
                    if let Some(title) = &tcu.fields.title {
                        tc.title = shorten_tool_title(title, &app.cwd_raw);
                    }
                    if let Some(content) = tcu.fields.content {
                        // Extract terminal_id and command from Terminal content blocks
                        for cb in &content {
                            if let acp::ToolCallContent::Terminal(t) = cb {
                                let tid = t.terminal_id.to_string();
                                if let Some(terminal) = app.terminals.borrow().get(&tid) {
                                    tc.terminal_command = Some(terminal.command.clone());
                                }
                                tc.terminal_id = Some(tid);
                            }
                        }
                        tc.content = content;
                    }
                    // Update claude_tool_name from update meta if present
                    if let Some(ref meta) = tcu.meta
                        && let Some(name) = meta
                            .get("claudeCode")
                            .and_then(|v| v.get("toolName"))
                            .and_then(|v| v.as_str())
                    {
                        tc.claude_tool_name = Some(name.to_owned());
                    }
                    // Update todos from TodoWrite raw_input updates
                    if tc.claude_tool_name.as_deref() == Some("TodoWrite") {
                        tracing::info!(
                            "TodoWrite ToolCallUpdate: id={id_str}, raw_input={:?}",
                            tcu.fields.raw_input
                        );
                        if let Some(ref raw_input) = tcu.fields.raw_input {
                            let todos = parse_todos(raw_input);
                            tracing::info!(
                                "Parsed {} todos from ToolCallUpdate raw_input",
                                todos.len()
                            );
                            pending_todos = Some(todos);
                        }
                    }
                    if matches!(
                        tc.status,
                        acp::ToolCallStatus::Completed | acp::ToolCallStatus::Failed
                    ) {
                        tc.collapsed = app.tools_collapsed;
                    }
                    tc.cache.invalidate();
                }
            } else {
                tracing::warn!("ToolCallUpdate: id={id_str} not found in index");
            }
            if let Some(todos) = pending_todos {
                set_todos(app, todos);
            }

            // If all tool calls have completed/failed, flip back to Thinking
            // (the turn is still active — TurnComplete hasn't arrived yet).
            if matches!(app.status, AppStatus::Running) && !has_in_progress_tool_calls(app) {
                app.status = AppStatus::Thinking;
            }
        }
        acp::SessionUpdate::UserMessageChunk(_) => {
            // Our own message echoed back -- we already display it
        }
        acp::SessionUpdate::AgentThoughtChunk(chunk) => {
            tracing::debug!("Agent thought: {:?}", chunk);
            app.status = AppStatus::Thinking;
        }
        acp::SessionUpdate::Plan(plan) => {
            tracing::debug!("Plan update: {:?}", plan);
            apply_plan_todos(app, &plan);
        }
        acp::SessionUpdate::AvailableCommandsUpdate(cmds) => {
            tracing::debug!("Available commands: {} commands", cmds.available_commands.len());
            app.available_commands = cmds.available_commands;
        }
        acp::SessionUpdate::CurrentModeUpdate(update) => {
            if let Some(ref mut mode) = app.mode {
                let mode_id = update.current_mode_id.to_string();
                if let Some(info) = mode.available_modes.iter().find(|m| m.id == mode_id) {
                    mode.current_mode_name.clone_from(&info.name);
                    mode.current_mode_id = mode_id;
                } else {
                    mode.current_mode_name.clone_from(&mode_id);
                    mode.current_mode_id = mode_id;
                }
                app.cached_footer_line = None;
            }
        }
        acp::SessionUpdate::ConfigOptionUpdate(config) => {
            tracing::debug!("Config update: {:?}", config);
        }
        acp::SessionUpdate::UsageUpdate(usage) => {
            tracing::debug!(
                "UsageUpdate: used={} size={} cost={:?}",
                usage.used,
                usage.size,
                usage.cost
            );
        }
        _ => {
            tracing::debug!("Unhandled session update");
        }
    }
}

/// Shorten absolute paths in tool titles to relative paths based on cwd.
/// e.g. "Read C:\\Users\\me\\project\\src\\main.rs" -> "Read src/main.rs"
/// Handles both `/` and `\\` separators on all platforms since the ACP adapter
/// may use either regardless of the host OS.
fn shorten_tool_title(title: &str, cwd_raw: &str) -> String {
    if cwd_raw.is_empty() {
        return title.to_owned();
    }

    // Quick check: if title doesn't contain any part of cwd, skip normalization
    // Use the first path component of cwd as a heuristic
    let cwd_start = cwd_raw.split(['/', '\\']).find(|s| !s.is_empty()).unwrap_or(cwd_raw);
    if !title.contains(cwd_start) {
        return title.to_owned();
    }

    // Normalize both to forward slashes for matching
    let cwd_norm = cwd_raw.replace('\\', "/");
    let title_norm = title.replace('\\', "/");

    // Ensure cwd ends with slash so we strip the separator too
    let with_sep = if cwd_norm.ends_with('/') { cwd_norm } else { format!("{cwd_norm}/") };

    if title_norm.contains(&with_sep) {
        return title_norm.replace(&with_sep, "");
    }
    title_norm
}

/// Check if any tool call in the current assistant message is still in-progress.
fn has_in_progress_tool_calls(app: &App) -> bool {
    if let Some(last) = app.messages.last()
        && matches!(last.role, MessageRole::Assistant)
    {
        return last.blocks.iter().any(|block| {
            matches!(
                block,
                MessageBlock::ToolCall(tc)
                    if matches!(tc.status, acp::ToolCallStatus::InProgress | acp::ToolCallStatus::Pending)
            )
        });
    }
    false
}

/// Return a human-readable name for a `SessionUpdate` variant (for debug logging).
fn session_update_name(update: &acp::SessionUpdate) -> &'static str {
    match update {
        acp::SessionUpdate::AgentMessageChunk(_) => "AgentMessageChunk",
        acp::SessionUpdate::ToolCall(_) => "ToolCall",
        acp::SessionUpdate::ToolCallUpdate(_) => "ToolCallUpdate",
        acp::SessionUpdate::UserMessageChunk(_) => "UserMessageChunk",
        acp::SessionUpdate::AgentThoughtChunk(_) => "AgentThoughtChunk",
        acp::SessionUpdate::Plan(_) => "Plan",
        acp::SessionUpdate::AvailableCommandsUpdate(_) => "AvailableCommandsUpdate",
        acp::SessionUpdate::CurrentModeUpdate(_) => "CurrentModeUpdate",
        acp::SessionUpdate::ConfigOptionUpdate(_) => "ConfigOptionUpdate",
        acp::SessionUpdate::UsageUpdate(_) => "UsageUpdate",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    // =====
    // TESTS: 36
    // =====

    use super::*;
    use pretty_assertions::assert_eq;

    // Helper: build a minimal ToolCallInfo with given id + status

    fn tool_call(id: &str, status: acp::ToolCallStatus) -> ToolCallInfo {
        ToolCallInfo {
            id: id.into(),
            title: id.into(),
            kind: acp::ToolKind::Read,
            status,
            content: vec![],
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

    fn assistant_msg(blocks: Vec<MessageBlock>) -> ChatMessage {
        ChatMessage {
            role: MessageRole::Assistant,
            blocks,
            cached_visual_height: 0,
            cached_visual_width: 0,
        }
    }

    fn user_msg(text: &str) -> ChatMessage {
        ChatMessage {
            role: MessageRole::User,
            blocks: vec![MessageBlock::Text(text.into(), BlockCache::default())],
            cached_visual_height: 0,
            cached_visual_width: 0,
        }
    }

    // shorten_tool_title

    #[test]
    fn shorten_unix_path() {
        let result =
            shorten_tool_title("Read /home/user/project/src/main.rs", "/home/user/project");
        assert_eq!(result, "Read src/main.rs");
    }

    #[test]
    fn shorten_windows_path() {
        let result = shorten_tool_title(
            "Read C:\\Users\\me\\project\\src\\main.rs",
            "C:\\Users\\me\\project",
        );
        assert_eq!(result, "Read src/main.rs");
    }

    #[test]
    fn shorten_no_match_returns_original() {
        let result = shorten_tool_title("Read /other/path/file.rs", "/home/user/project");
        assert_eq!(result, "Read /other/path/file.rs");
    }

    // shorten_tool_title

    #[test]
    fn shorten_empty_cwd() {
        let result = shorten_tool_title("Read /some/path/file.rs", "");
        assert_eq!(result, "Read /some/path/file.rs");
    }

    #[test]
    fn shorten_cwd_with_trailing_slash() {
        let result = shorten_tool_title("Read /home/user/project/file.rs", "/home/user/project/");
        assert_eq!(result, "Read file.rs");
    }

    #[test]
    fn shorten_title_is_just_path() {
        let result = shorten_tool_title("/home/user/project/file.rs", "/home/user/project");
        assert_eq!(result, "file.rs");
    }

    #[test]
    fn shorten_mixed_separators() {
        let result =
            shorten_tool_title("Read C:/Users/me/project/src/lib.rs", "C:\\Users\\me\\project");
        assert_eq!(result, "Read src/lib.rs");
    }

    #[test]
    fn shorten_empty_title() {
        assert_eq!(shorten_tool_title("", "/some/cwd"), "");
    }

    #[test]
    fn shorten_title_no_path_at_all() {
        assert_eq!(shorten_tool_title("Read", "/home/user"), "Read");
        assert_eq!(shorten_tool_title("Write something", "/proj"), "Write something");
    }

    #[test]
    fn shorten_title_equals_cwd_exactly() {
        // Title IS the cwd path — after stripping, nothing left
        let result = shorten_tool_title("/home/user/project", "/home/user/project");
        // The cwd+/ won't match because title doesn't have trailing content after cwd
        // cwd_norm = "/home/user/project/", title doesn't contain that
        assert_eq!(result, "/home/user/project");
    }

    // shorten_tool_title

    #[test]
    fn shorten_partial_match_no_false_positive() {
        let result = shorten_tool_title("Read /home/username/file.rs", "/home/user");
        assert_eq!(result, "Read /home/username/file.rs");
    }

    #[test]
    fn shorten_deeply_nested_path() {
        let cwd = "/a/b/c/d/e/f/g";
        let title = "Read /a/b/c/d/e/f/g/h/i/j.rs";
        let result = shorten_tool_title(title, cwd);
        assert_eq!(result, "Read h/i/j.rs");
    }

    #[test]
    fn shorten_cwd_appears_multiple_times() {
        let result = shorten_tool_title("Diff /proj/a.rs /proj/b.rs", "/proj");
        assert_eq!(result, "Diff a.rs b.rs");
    }

    /// Spaces in path (real Windows path with spaces).
    #[test]
    fn shorten_spaces_in_path() {
        let result = shorten_tool_title(
            "Read C:\\Users\\Simon Peter Rothgang\\Desktop\\project\\src\\main.rs",
            "C:\\Users\\Simon Peter Rothgang\\Desktop\\project",
        );
        assert_eq!(result, "Read src/main.rs");
    }

    /// Unicode characters in path components.
    #[test]
    fn shorten_unicode_in_path() {
        let result = shorten_tool_title(
            "Read /home/\u{00FC}ser/\u{30D7}\u{30ED}\u{30B8}\u{30A7}\u{30AF}\u{30C8}/src/lib.rs",
            "/home/\u{00FC}ser/\u{30D7}\u{30ED}\u{30B8}\u{30A7}\u{30AF}\u{30C8}",
        );
        assert_eq!(result, "Read src/lib.rs");
    }

    /// Root as cwd (Unix).
    #[test]
    fn shorten_cwd_is_root_unix() {
        // cwd = "/" => with_sep = "/", so "/foo/bar.rs".contains("/") => replaces
        let result = shorten_tool_title("Read /foo/bar.rs", "/");
        // "/" is first path component = "" (empty), heuristic check uses "" which is in everything
        // After normalization: cwd = "/", with_sep = "/", title contains "/" => replaces ALL "/"
        assert_eq!(result, "Read foobar.rs");
    }

    /// Root as cwd (Windows).
    #[test]
    fn shorten_cwd_is_drive_root_windows() {
        let result = shorten_tool_title("Read C:\\src\\main.rs", "C:\\");
        assert_eq!(result, "Read src/main.rs");
    }

    /// Very long path (stress test).
    #[test]
    fn shorten_very_long_path() {
        let segments: String = (0..50).fold(String::new(), |mut s, i| {
            use std::fmt::Write;
            write!(s, "/seg{i}").unwrap();
            s
        });
        let cwd = segments.clone();
        let title = format!("Read {segments}/deep/file.rs");
        let result = shorten_tool_title(&title, &cwd);
        assert_eq!(result, "Read deep/file.rs");
    }

    /// Case sensitivity: paths are case-sensitive.
    #[test]
    fn shorten_case_sensitive() {
        let result = shorten_tool_title("Read /Home/User/Project/file.rs", "/home/user/project");
        // Different case, so the first-component heuristic "home" matches "Home"?
        // No: cwd_start = "home", title doesn't contain "home" (has "Home") => early return
        assert_eq!(result, "Read /Home/User/Project/file.rs");
    }

    /// Cwd that is a prefix at directory boundary but not at cwd boundary.
    #[test]
    fn shorten_cwd_prefix_boundary() {
        // cwd="/pro" should NOT strip from "/project/file.rs"
        let result = shorten_tool_title("Read /project/file.rs", "/pro");
        // cwd_start = "pro", title contains "pro" (in "project") => proceeds to normalize
        // with_sep = "/pro/", title_norm = "Read /project/file.rs", doesn't contain "/pro/"
        assert_eq!(result, "Read /project/file.rs");
    }

    // has_in_progress_tool_calls

    fn make_test_app() -> App {
        App::test_default()
    }

    #[test]
    fn has_in_progress_empty_messages() {
        let app = make_test_app();
        assert!(!has_in_progress_tool_calls(&app));
    }

    #[test]
    fn has_in_progress_no_tool_calls() {
        let mut app = make_test_app();
        app.messages
            .push(assistant_msg(vec![MessageBlock::Text("hello".into(), BlockCache::default())]));
        assert!(!has_in_progress_tool_calls(&app));
    }

    #[test]
    fn has_in_progress_with_pending_tool() {
        let mut app = make_test_app();
        app.messages.push(assistant_msg(vec![MessageBlock::ToolCall(Box::new(tool_call(
            "tc1",
            acp::ToolCallStatus::Pending,
        )))]));
        assert!(has_in_progress_tool_calls(&app));
    }

    #[test]
    fn has_in_progress_with_in_progress_tool() {
        let mut app = make_test_app();
        app.messages.push(assistant_msg(vec![MessageBlock::ToolCall(Box::new(tool_call(
            "tc1",
            acp::ToolCallStatus::InProgress,
        )))]));
        assert!(has_in_progress_tool_calls(&app));
    }

    #[test]
    fn has_in_progress_all_completed() {
        let mut app = make_test_app();
        app.messages.push(assistant_msg(vec![MessageBlock::ToolCall(Box::new(tool_call(
            "tc1",
            acp::ToolCallStatus::Completed,
        )))]));
        assert!(!has_in_progress_tool_calls(&app));
    }

    #[test]
    fn has_in_progress_all_failed() {
        let mut app = make_test_app();
        app.messages.push(assistant_msg(vec![MessageBlock::ToolCall(Box::new(tool_call(
            "tc1",
            acp::ToolCallStatus::Failed,
        )))]));
        assert!(!has_in_progress_tool_calls(&app));
    }

    // has_in_progress_tool_calls

    #[test]
    fn has_in_progress_user_message_last() {
        let mut app = make_test_app();
        app.messages.push(user_msg("hi"));
        assert!(!has_in_progress_tool_calls(&app));
    }

    /// Only the LAST message matters — earlier assistant messages are ignored.
    #[test]
    fn has_in_progress_only_checks_last_message() {
        let mut app = make_test_app();
        // First assistant message has in-progress tool
        app.messages.push(assistant_msg(vec![MessageBlock::ToolCall(Box::new(tool_call(
            "tc1",
            acp::ToolCallStatus::InProgress,
        )))]));
        // Last message is user — should be false
        app.messages.push(user_msg("thanks"));
        assert!(!has_in_progress_tool_calls(&app));
    }

    /// Earlier assistant with in-progress, last assistant all completed.
    #[test]
    fn has_in_progress_ignores_earlier_assistant() {
        let mut app = make_test_app();
        app.messages.push(assistant_msg(vec![MessageBlock::ToolCall(Box::new(tool_call(
            "tc1",
            acp::ToolCallStatus::InProgress,
        )))]));
        app.messages.push(user_msg("ok"));
        app.messages.push(assistant_msg(vec![MessageBlock::ToolCall(Box::new(tool_call(
            "tc2",
            acp::ToolCallStatus::Completed,
        )))]));
        assert!(!has_in_progress_tool_calls(&app));
    }

    #[test]
    fn has_in_progress_mixed_completed_and_pending() {
        let mut app = make_test_app();
        app.messages.push(assistant_msg(vec![
            MessageBlock::ToolCall(Box::new(tool_call("tc1", acp::ToolCallStatus::Completed))),
            MessageBlock::ToolCall(Box::new(tool_call("tc2", acp::ToolCallStatus::InProgress))),
        ]));
        assert!(has_in_progress_tool_calls(&app));
    }

    /// Text blocks mixed with tool calls — text blocks are correctly skipped.
    #[test]
    fn has_in_progress_text_and_tools_mixed() {
        let mut app = make_test_app();
        app.messages.push(assistant_msg(vec![
            MessageBlock::Text("thinking...".into(), BlockCache::default()),
            MessageBlock::ToolCall(Box::new(tool_call("tc1", acp::ToolCallStatus::Completed))),
            MessageBlock::Text("done".into(), BlockCache::default()),
        ]));
        assert!(!has_in_progress_tool_calls(&app));
    }

    /// Stress: 100 completed tool calls + 1 pending at the end.
    #[test]
    fn has_in_progress_stress_100_tools_one_pending() {
        let mut app = make_test_app();
        let mut blocks: Vec<MessageBlock> = (0..100)
            .map(|i| {
                MessageBlock::ToolCall(Box::new(tool_call(
                    &format!("tc{i}"),
                    acp::ToolCallStatus::Completed,
                )))
            })
            .collect();
        blocks.push(MessageBlock::ToolCall(Box::new(tool_call(
            "tc_pending",
            acp::ToolCallStatus::Pending,
        ))));
        app.messages.push(assistant_msg(blocks));
        assert!(has_in_progress_tool_calls(&app));
    }

    /// Stress: 100 completed tool calls, none pending.
    #[test]
    fn has_in_progress_stress_100_tools_all_done() {
        let mut app = make_test_app();
        let blocks: Vec<MessageBlock> = (0..100)
            .map(|i| {
                MessageBlock::ToolCall(Box::new(tool_call(
                    &format!("tc{i}"),
                    acp::ToolCallStatus::Completed,
                )))
            })
            .collect();
        app.messages.push(assistant_msg(blocks));
        assert!(!has_in_progress_tool_calls(&app));
    }

    /// Mix of Failed and Completed — neither counts as in-progress.
    #[test]
    fn has_in_progress_failed_and_completed_mix() {
        let mut app = make_test_app();
        app.messages.push(assistant_msg(vec![
            MessageBlock::ToolCall(Box::new(tool_call("tc1", acp::ToolCallStatus::Completed))),
            MessageBlock::ToolCall(Box::new(tool_call("tc2", acp::ToolCallStatus::Failed))),
            MessageBlock::ToolCall(Box::new(tool_call("tc3", acp::ToolCallStatus::Completed))),
        ]));
        assert!(!has_in_progress_tool_calls(&app));
    }

    /// Empty assistant message (no blocks at all).
    #[test]
    fn has_in_progress_empty_assistant_blocks() {
        let mut app = make_test_app();
        app.messages.push(assistant_msg(vec![]));
        assert!(!has_in_progress_tool_calls(&app));
    }

    // make_test_app — verify defaults

    #[test]
    fn test_app_defaults() {
        let app = make_test_app();
        assert!(app.messages.is_empty());
        assert_eq!(app.scroll_offset, 0);
        assert_eq!(app.scroll_target, 0);
        assert!(app.auto_scroll);
        assert!(!app.should_quit);
        assert!(app.session_id.is_none());
        assert_eq!(app.files_accessed, 0);
        assert!(app.pending_permission_ids.is_empty());
        assert!(!app.tools_collapsed);
        assert!(!app.force_redraw);
        assert!(app.todos.is_empty());
        assert!(!app.show_todo_panel);
        assert!(app.selection.is_none());
        assert!(app.mention.is_none());
        assert!(app.rendered_chat_lines.is_empty());
        assert!(app.rendered_input_lines.is_empty());
        assert!(matches!(app.status, AppStatus::Ready));
    }
}
