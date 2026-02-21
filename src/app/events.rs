// claude_rust - A native Rust terminal interface for Claude Code
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

use super::connect::take_connection_slot;
use super::selection::clear_selection;
use super::{
    App, AppStatus, BlockCache, ChatMessage, FocusTarget, IncrementalMarkdown, InlinePermission,
    LoginHint, MessageBlock, MessageRole, SelectionKind, SelectionPoint, ToolCallInfo,
};
use crate::acp::client::ClientEvent;
use crate::app::todos::{apply_plan_todos, parse_todos, set_todos};
use agent_client_protocol::{self as acp};
#[cfg(test)]
use crossterm::event::KeyEvent;
use crossterm::event::{Event, KeyEventKind, MouseEvent, MouseEventKind};

const CONVERSATION_INTERRUPTED_HINT: &str =
    "Conversation interrupted. Tell the model how to proceed.";

pub fn handle_terminal_event(app: &mut App, event: Event) {
    app.needs_redraw = true;
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            super::keys::dispatch_key_by_focus(app, key);
        }
        Event::Mouse(mouse) => {
            handle_mouse_event(app, mouse);
        }
        Event::Paste(text) => {
            if app.status != AppStatus::Connecting {
                // Queue paste chunks for this drain cycle. Some terminals split a
                // single clipboard paste into multiple `Event::Paste` payloads.
                app.pending_paste_text.push_str(&text);
            }
        }
        Event::FocusGained => {
            app.refresh_git_branch();
        }
        Event::Resize(_, _) => {
            // Force a full terminal clear on resize. Without this, terminal
            // emulators (especially on Windows) corrupt their scrollback buffer
            // when the alternate screen is resized, causing the visible area to
            // shift even though ratatui paints the correct content. The clear
            // resets the terminal's internal state.
            app.force_redraw = true;
        }
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
            } else {
                clear_selection(app);
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
            if app.selection.is_some() {
                clear_selection(app);
            }
            app.viewport.scroll_up(MOUSE_SCROLL_LINES);
        }
        MouseEventKind::ScrollDown => {
            if app.selection.is_some() {
                clear_selection(app);
            }
            app.viewport.scroll_down(MOUSE_SCROLL_LINES);
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

#[cfg(test)]
fn handle_normal_key(app: &mut App, key: KeyEvent) {
    super::keys::handle_normal_key(app, key);
}

#[cfg(test)]
fn cleanup_leaked_char_before_placeholder(app: &mut App) {
    super::keys::cleanup_leaked_char_before_placeholder(app);
}

#[cfg(test)]
fn handle_mention_key(app: &mut App, key: KeyEvent) {
    super::keys::handle_mention_key(app, key);
}

#[cfg(test)]
fn dispatch_key_by_focus(app: &mut App, key: KeyEvent) {
    super::keys::dispatch_key_by_focus(app, key);
}

#[allow(clippy::too_many_lines)]
pub fn handle_acp_event(app: &mut App, event: ClientEvent) {
    app.needs_redraw = true;
    match event {
        ClientEvent::SessionUpdate(update) => handle_session_update(app, update),
        ClientEvent::PermissionRequest { request, response_tx } => {
            let tool_id = request.tool_call.tool_call_id.to_string();
            if let Some((mi, bi)) = app.lookup_tool_call(&tool_id) {
                if app.pending_permission_ids.iter().any(|id| id == &tool_id) {
                    tracing::warn!(
                        "Duplicate permission request for tool call: {tool_id}; auto-rejecting duplicate"
                    );
                    // Keep the original pending prompt and reject duplicate request.
                    if let Some(last_opt) = request.options.last() {
                        let _ = response_tx.send(acp::RequestPermissionResponse::new(
                            acp::RequestPermissionOutcome::Selected(
                                acp::SelectedPermissionOutcome::new(last_opt.option_id.clone()),
                            ),
                        ));
                    }
                    return;
                }

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
                    app.claim_focus_target(FocusTarget::Permission);
                    app.viewport.engage_auto_scroll();
                } else {
                    tracing::warn!(
                        "Permission request for non-tool block index: {tool_id}; auto-rejecting"
                    );
                    if let Some(last_opt) = request.options.last() {
                        let _ = response_tx.send(acp::RequestPermissionResponse::new(
                            acp::RequestPermissionOutcome::Selected(
                                acp::SelectedPermissionOutcome::new(last_opt.option_id.clone()),
                            ),
                        ));
                    }
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
        ClientEvent::TurnCancelled => {
            app.pending_compact_clear = false;
            app.cancelled_turn_pending_hint = true;
            let _ = app.finalize_in_progress_tool_calls(acp::ToolCallStatus::Failed);
        }
        ClientEvent::TurnComplete => {
            let should_compact_clear = app.pending_compact_clear;
            app.pending_compact_clear = false;
            let show_interrupted_hint = app.cancelled_turn_pending_hint;
            app.cancelled_turn_pending_hint = false;
            if show_interrupted_hint {
                let _ = app.finalize_in_progress_tool_calls(acp::ToolCallStatus::Failed);
            } else {
                let _ = app.finalize_in_progress_tool_calls(acp::ToolCallStatus::Completed);
            }
            app.status = AppStatus::Ready;
            app.files_accessed = 0;
            app.active_task_ids.clear();
            app.refresh_git_branch();
            if show_interrupted_hint {
                push_interrupted_hint(app);
            }
            if should_compact_clear {
                super::slash::clear_conversation_history(app);
            }
        }
        ClientEvent::TurnError(msg) => {
            let should_compact_clear = app.pending_compact_clear;
            app.pending_compact_clear = false;
            tracing::error!("Turn error: {msg}");
            if looks_like_internal_error(&msg) {
                tracing::debug!(
                    error_preview = %summarize_internal_error(&msg),
                    "Internal ACP/adapter turn error payload"
                );
            }
            app.cancelled_turn_pending_hint = false;
            let _ = app.finalize_in_progress_tool_calls(acp::ToolCallStatus::Failed);
            app.status = AppStatus::Error;
            if should_compact_clear {
                super::slash::clear_conversation_history(app);
            }
        }
        ClientEvent::Connected { session_id, model_name, mode } => {
            // Grab connection + child from the shared slot
            if let Some(slot) = take_connection_slot() {
                app.conn = Some(slot.conn);
                app.adapter_child = Some(slot.child);
            }
            app.session_id = Some(session_id);
            app.model_name = model_name;
            app.mode = mode;
            app.status = AppStatus::Ready;
            app.login_hint = None;
            app.pending_compact_clear = false;
            app.cancelled_turn_pending_hint = false;
            app.cached_header_line = None;
            app.cached_footer_line = None;
            app.update_welcome_model_if_pristine();
        }
        ClientEvent::AuthRequired { method_name, method_description } => {
            // Show auth context without pre-filling /login. Slash login/logout
            // discoverability is intentionally deferred for now.
            app.status = AppStatus::Ready;
            app.login_hint = Some(LoginHint { method_name, method_description });
            app.pending_compact_clear = false;
            app.cancelled_turn_pending_hint = false;
        }
        ClientEvent::ConnectionFailed(msg) => {
            app.pending_compact_clear = false;
            app.cancelled_turn_pending_hint = false;
            app.status = AppStatus::Error;
            app.messages.push(ChatMessage {
                role: MessageRole::Assistant,
                blocks: vec![MessageBlock::Text(
                    format!("Connection failed: {msg}"),
                    BlockCache::default(),
                    IncrementalMarkdown::default(),
                )],
            });
        }
        ClientEvent::SlashCommandError(msg) => {
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                blocks: vec![MessageBlock::Text(
                    msg.clone(),
                    BlockCache::default(),
                    IncrementalMarkdown::from_complete(&msg),
                )],
            });
            app.viewport.engage_auto_scroll();
            app.status = AppStatus::Ready;
        }
        ClientEvent::SessionReplaced { session_id, model_name, mode } => {
            app.pending_compact_clear = false;
            reset_for_new_session(app, session_id, model_name, mode);
        }
    }
}

fn push_interrupted_hint(app: &mut App) {
    app.messages.push(ChatMessage {
        role: MessageRole::System,
        blocks: vec![MessageBlock::Text(
            CONVERSATION_INTERRUPTED_HINT.to_owned(),
            BlockCache::default(),
            IncrementalMarkdown::from_complete(CONVERSATION_INTERRUPTED_HINT),
        )],
    });
    app.viewport.engage_auto_scroll();
}

#[allow(clippy::too_many_lines)]
fn reset_for_new_session(
    app: &mut App,
    session_id: acp::SessionId,
    model_name: String,
    mode: Option<super::ModeState>,
) {
    crate::acp::client::kill_all_terminals(&app.terminals);

    app.session_id = Some(session_id);
    app.model_name = model_name;
    app.mode = mode;
    app.status = AppStatus::Ready;
    app.login_hint = None;
    app.pending_compact_clear = false;
    app.should_quit = false;
    app.files_accessed = 0;
    app.cancelled_turn_pending_hint = false;

    app.messages.clear();
    app.messages.push(ChatMessage::welcome(&app.model_name, &app.cwd));
    app.viewport = super::ChatViewport::new();

    app.input.clear();
    app.pending_submit = false;
    app.drain_key_count = 0;
    app.paste_burst.reset();
    app.pending_paste_text.clear();
    app.input_wrap_cache = None;

    app.pending_permission_ids.clear();
    app.active_task_ids.clear();
    app.tool_call_index.clear();
    app.todos.clear();
    app.show_todo_panel = false;
    app.todo_scroll = 0;
    app.todo_selected = 0;
    app.focus = super::FocusManager::default();
    app.available_commands.clear();

    app.selection = None;
    app.rendered_chat_lines.clear();
    app.rendered_chat_area = ratatui::layout::Rect::default();
    app.rendered_input_lines.clear();
    app.rendered_input_area = ratatui::layout::Rect::default();
    app.mention = None;
    app.slash = None;
    app.file_cache = None;

    app.cached_todo_compact = None;
    app.cached_header_line = None;
    app.cached_footer_line = None;
    app.terminal_tool_calls.clear();
    app.force_redraw = true;
    app.needs_redraw = true;
    app.refresh_git_branch();
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
        // No assistant message yet - create one for this tool call
        let tc_id = tool_info.id.clone();
        let new_idx = app.messages.len();
        app.messages.push(ChatMessage {
            role: MessageRole::Assistant,
            blocks: vec![MessageBlock::ToolCall(Box::new(tool_info))],
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
                // Text is actively streaming - suppress the "Thinking..." spinner
                app.status = AppStatus::Running;

                // Append to last text block in current assistant message, or create new
                if let Some(last) = app.messages.last_mut()
                    && matches!(last.role, MessageRole::Assistant)
                {
                    // Append to last Text block if it exists, else push new one
                    if let Some(MessageBlock::Text(t, cache, incr)) = last.blocks.last_mut() {
                        t.push_str(&text.text);
                        incr.append(&text.text);
                        cache.invalidate();
                    } else {
                        let mut incr = IncrementalMarkdown::default();
                        incr.append(&text.text);
                        last.blocks.push(MessageBlock::Text(
                            text.text.clone(),
                            BlockCache::default(),
                            incr,
                        ));
                    }
                    return;
                }
                let mut incr = IncrementalMarkdown::default();
                incr.append(&text.text);
                app.messages.push(ChatMessage {
                    role: MessageRole::Assistant,
                    blocks: vec![MessageBlock::Text(
                        text.text.clone(),
                        BlockCache::default(),
                        incr,
                    )],
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
            if matches!(tcu.fields.status, Some(acp::ToolCallStatus::Failed))
                && let Some(content_preview) =
                    internal_failed_tool_content_preview(tcu.fields.content.as_deref())
            {
                let claude_tool_name = tcu.meta.as_ref().and_then(|m| {
                    m.get("claudeCode").and_then(|v| v.get("toolName")).and_then(|v| v.as_str())
                });
                tracing::debug!(
                    tool_call_id = %id_str,
                    title = ?tcu.fields.title,
                    claude_tool_name = ?claude_tool_name,
                    content_preview = %content_preview,
                    "Internal failed ToolCallUpdate payload"
                );
            }

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
                                tc.terminal_id = Some(tid.clone());
                                app.terminal_tool_calls.push((tid, mi, bi));
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
            // (the turn is still active - TurnComplete hasn't arrived yet).
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
            if app.slash.is_some() {
                super::slash::update_query(app);
            }
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

fn internal_failed_tool_content_preview(
    content: Option<&[acp::ToolCallContent]>,
) -> Option<String> {
    let text = content?.iter().find_map(|c| match c {
        acp::ToolCallContent::Content(inner) => match &inner.content {
            acp::ContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        },
        _ => None,
    })?;
    if !looks_like_internal_error(text) {
        return None;
    }
    Some(summarize_internal_error(text))
}

fn preview_for_log(input: &str) -> String {
    const LIMIT: usize = 240;
    let mut out = String::new();
    for (i, ch) in input.chars().enumerate() {
        if i >= LIMIT {
            out.push_str("...");
            break;
        }
        out.push(ch);
    }
    out.replace('\n', "\\n")
}

fn looks_like_internal_error(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    has_internal_error_keywords(&lower)
        || looks_like_json_rpc_error_shape(&lower)
        || looks_like_xml_error_shape(&lower)
}

fn has_internal_error_keywords(lower: &str) -> bool {
    [
        "internal error",
        "adapter",
        "acp",
        "json-rpc",
        "rpc",
        "protocol error",
        "transport",
        "handshake failed",
        "session creation failed",
        "connection closed",
        "event channel closed",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn looks_like_json_rpc_error_shape(lower: &str) -> bool {
    (lower.contains("\"jsonrpc\"") && lower.contains("\"error\""))
        || lower.contains("\"code\":-32603")
        || lower.contains("\"code\": -32603")
}

fn looks_like_xml_error_shape(lower: &str) -> bool {
    let has_error_node = lower.contains("<error") || lower.contains("<fault");
    let has_detail_node = lower.contains("<message>") || lower.contains("<code>");
    has_error_node && has_detail_node
}

fn summarize_internal_error(input: &str) -> String {
    if let Some(msg) = extract_xml_tag_value(input, "message") {
        return preview_for_log(msg);
    }
    if let Some(msg) = extract_json_string_field(input, "message") {
        return preview_for_log(&msg);
    }
    let fallback = input.lines().find(|line| !line.trim().is_empty()).unwrap_or(input);
    preview_for_log(fallback.trim())
}

fn extract_xml_tag_value<'a>(input: &'a str, tag: &str) -> Option<&'a str> {
    let lower = input.to_ascii_lowercase();
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = lower.find(&open)? + open.len();
    let end = start + lower[start..].find(&close)?;
    let value = input[start..end].trim();
    (!value.is_empty()).then_some(value)
}

fn extract_json_string_field(input: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\"");
    let start = input.find(&needle)? + needle.len();
    let rest = input[start..].trim_start();
    let colon_idx = rest.find(':')?;
    let mut chars = rest[colon_idx + 1..].trim_start().chars();
    if chars.next()? != '"' {
        return None;
    }

    let mut escaped = false;
    let mut out = String::new();
    for ch in chars {
        if escaped {
            let mapped = match ch {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                '"' => '"',
                '\\' => '\\',
                _ => ch,
            };
            out.push(mapped);
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some(out),
            _ => out.push(ch),
        }
    }
    None
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
    use crate::app::{FocusOwner, FocusTarget, HelpView, TodoItem, TodoStatus, mention};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use pretty_assertions::assert_eq;
    use tokio::sync::oneshot;

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
        ChatMessage { role: MessageRole::Assistant, blocks }
    }

    fn user_msg(text: &str) -> ChatMessage {
        ChatMessage {
            role: MessageRole::User,
            blocks: vec![MessageBlock::Text(
                text.into(),
                BlockCache::default(),
                IncrementalMarkdown::default(),
            )],
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
        // Title IS the cwd path - after stripping, nothing left
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

    fn connected_event(model_name: &str) -> ClientEvent {
        ClientEvent::Connected {
            session_id: acp::SessionId::new("test-session"),
            model_name: model_name.to_owned(),
            mode: None,
        }
    }

    #[test]
    fn has_in_progress_empty_messages() {
        let app = make_test_app();
        assert!(!has_in_progress_tool_calls(&app));
    }

    #[test]
    fn has_in_progress_no_tool_calls() {
        let mut app = make_test_app();
        app.messages.push(assistant_msg(vec![MessageBlock::Text(
            "hello".into(),
            BlockCache::default(),
            IncrementalMarkdown::default(),
        )]));
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

    /// Only the LAST message matters - earlier assistant messages are ignored.
    #[test]
    fn has_in_progress_only_checks_last_message() {
        let mut app = make_test_app();
        // First assistant message has in-progress tool
        app.messages.push(assistant_msg(vec![MessageBlock::ToolCall(Box::new(tool_call(
            "tc1",
            acp::ToolCallStatus::InProgress,
        )))]));
        // Last message is user - should be false
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

    /// Text blocks mixed with tool calls - text blocks are correctly skipped.
    #[test]
    fn has_in_progress_text_and_tools_mixed() {
        let mut app = make_test_app();
        app.messages.push(assistant_msg(vec![
            MessageBlock::Text(
                "thinking...".into(),
                BlockCache::default(),
                IncrementalMarkdown::default(),
            ),
            MessageBlock::ToolCall(Box::new(tool_call("tc1", acp::ToolCallStatus::Completed))),
            MessageBlock::Text(
                "done".into(),
                BlockCache::default(),
                IncrementalMarkdown::default(),
            ),
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

    /// Mix of Failed and Completed - neither counts as in-progress.
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

    // make_test_app - verify defaults

    #[test]
    fn test_app_defaults() {
        let app = make_test_app();
        assert!(app.messages.is_empty());
        assert_eq!(app.viewport.scroll_offset, 0);
        assert_eq!(app.viewport.scroll_target, 0);
        assert!(app.viewport.auto_scroll);
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
        assert!(!app.cancelled_turn_pending_hint);
        assert!(app.rendered_chat_lines.is_empty());
        assert!(app.rendered_input_lines.is_empty());
        assert!(matches!(app.status, AppStatus::Ready));
    }

    #[test]
    fn turn_complete_after_cancel_renders_interrupted_hint() {
        let mut app = make_test_app();

        handle_acp_event(&mut app, ClientEvent::TurnCancelled);
        assert!(app.cancelled_turn_pending_hint);

        handle_acp_event(&mut app, ClientEvent::TurnComplete);

        assert!(!app.cancelled_turn_pending_hint);
        let last = app.messages.last().expect("expected interruption hint message");
        assert!(matches!(last.role, MessageRole::System));
        let Some(MessageBlock::Text(text, _, _)) = last.blocks.first() else {
            panic!("expected text block");
        };
        assert_eq!(text, CONVERSATION_INTERRUPTED_HINT);
    }

    #[test]
    fn connected_updates_welcome_model_while_pristine() {
        let mut app = make_test_app();
        app.messages.push(ChatMessage::welcome("Connecting...", "/test"));

        handle_acp_event(&mut app, connected_event("claude-updated"));

        let Some(first) = app.messages.first() else {
            panic!("missing welcome message");
        };
        let Some(MessageBlock::Welcome(welcome)) = first.blocks.first() else {
            panic!("expected welcome block");
        };
        assert_eq!(welcome.model_name, "claude-updated");
    }

    #[test]
    fn connected_does_not_update_welcome_after_chat_started() {
        let mut app = make_test_app();
        app.messages.push(ChatMessage::welcome("Connecting...", "/test"));
        app.messages.push(user_msg("hello"));

        handle_acp_event(&mut app, connected_event("claude-updated"));

        let Some(first) = app.messages.first() else {
            panic!("missing first message");
        };
        let Some(MessageBlock::Welcome(welcome)) = first.blocks.first() else {
            panic!("expected welcome block");
        };
        assert_eq!(welcome.model_name, "Connecting...");
    }

    #[test]
    fn auth_required_sets_hint_without_prefilling_login_command() {
        let mut app = make_test_app();
        app.input.set_text("keep me");

        handle_acp_event(
            &mut app,
            ClientEvent::AuthRequired {
                method_name: "oauth".into(),
                method_description: "Open browser".into(),
            },
        );

        assert!(matches!(app.status, AppStatus::Ready));
        assert_eq!(app.input.text(), "keep me");
        let Some(hint) = &app.login_hint else {
            panic!("expected login hint");
        };
        assert_eq!(hint.method_name, "oauth");
        assert_eq!(hint.method_description, "Open browser");
    }

    #[test]
    fn session_replaced_resets_chat_and_transient_state() {
        let mut app = make_test_app();
        app.messages.push(user_msg("hello"));
        app.messages.push(assistant_msg(vec![MessageBlock::Text(
            "world".into(),
            BlockCache::default(),
            IncrementalMarkdown::from_complete("world"),
        )]));
        app.status = AppStatus::Running;
        app.files_accessed = 9;
        app.pending_permission_ids.push("perm-1".into());
        app.todo_selected = 2;
        app.show_todo_panel = true;
        app.todos.push(TodoItem {
            content: "Task".into(),
            status: TodoStatus::InProgress,
            active_form: String::new(),
        });
        app.mention = Some(mention::MentionState {
            trigger_row: 0,
            trigger_col: 0,
            query: String::new(),
            candidates: Vec::new(),
            dialog: super::super::dialog::DialogState::default(),
        });

        handle_acp_event(
            &mut app,
            ClientEvent::SessionReplaced {
                session_id: acp::SessionId::new("replacement"),
                model_name: "new-model".into(),
                mode: None,
            },
        );

        assert!(matches!(app.status, AppStatus::Ready));
        assert_eq!(
            app.session_id.as_ref().map(ToString::to_string).as_deref(),
            Some("replacement")
        );
        assert_eq!(app.model_name, "new-model");
        assert_eq!(app.messages.len(), 1);
        assert!(matches!(app.messages[0].role, MessageRole::Welcome));
        assert_eq!(app.files_accessed, 0);
        assert!(app.pending_permission_ids.is_empty());
        assert!(app.todos.is_empty());
        assert!(!app.show_todo_panel);
        assert!(app.mention.is_none());
    }

    #[test]
    fn turn_complete_without_cancel_does_not_render_interrupted_hint() {
        let mut app = make_test_app();
        handle_acp_event(&mut app, ClientEvent::TurnComplete);
        assert!(app.messages.is_empty());
    }

    #[test]
    fn turn_complete_clears_history_when_compact_pending() {
        let mut app = make_test_app();
        app.session_id = Some(acp::SessionId::new("session-x"));
        app.pending_compact_clear = true;
        app.messages.push(user_msg("/compact"));
        app.messages.push(assistant_msg(vec![MessageBlock::Text(
            "compacted".into(),
            BlockCache::default(),
            IncrementalMarkdown::from_complete("compacted"),
        )]));

        handle_acp_event(&mut app, ClientEvent::TurnComplete);

        assert!(!app.pending_compact_clear);
        assert_eq!(app.messages.len(), 1);
        assert!(matches!(app.messages[0].role, MessageRole::Welcome));
        assert_eq!(app.session_id.as_ref().map(ToString::to_string).as_deref(), Some("session-x"));
    }

    #[test]
    fn turn_error_also_clears_history_when_compact_pending() {
        let mut app = make_test_app();
        app.pending_compact_clear = true;
        app.messages.push(user_msg("/compact"));

        handle_acp_event(&mut app, ClientEvent::TurnError("adapter failed".into()));

        assert!(!app.pending_compact_clear);
        assert!(matches!(app.status, AppStatus::Ready));
        assert_eq!(app.messages.len(), 1);
        assert!(matches!(app.messages[0].role, MessageRole::Welcome));
    }

    #[test]
    fn turn_cancel_marks_active_tools_failed() {
        let mut app = make_test_app();
        app.messages.push(assistant_msg(vec![
            MessageBlock::ToolCall(Box::new(tool_call("tc1", acp::ToolCallStatus::InProgress))),
            MessageBlock::ToolCall(Box::new(tool_call("tc2", acp::ToolCallStatus::Pending))),
            MessageBlock::ToolCall(Box::new(tool_call("tc3", acp::ToolCallStatus::Completed))),
        ]));

        handle_acp_event(&mut app, ClientEvent::TurnCancelled);

        let Some(last) = app.messages.last() else {
            panic!("missing assistant message");
        };
        let statuses: Vec<acp::ToolCallStatus> = last
            .blocks
            .iter()
            .filter_map(|b| match b {
                MessageBlock::ToolCall(tc) => Some(tc.status),
                _ => None,
            })
            .collect();
        assert_eq!(
            statuses,
            vec![
                acp::ToolCallStatus::Failed,
                acp::ToolCallStatus::Failed,
                acp::ToolCallStatus::Completed
            ]
        );
    }

    #[test]
    fn turn_complete_marks_lingering_tools_completed() {
        let mut app = make_test_app();
        app.messages.push(assistant_msg(vec![
            MessageBlock::ToolCall(Box::new(tool_call("tc1", acp::ToolCallStatus::InProgress))),
            MessageBlock::ToolCall(Box::new(tool_call("tc2", acp::ToolCallStatus::Pending))),
        ]));

        handle_acp_event(&mut app, ClientEvent::TurnComplete);

        let Some(last) = app.messages.last() else {
            panic!("missing assistant message");
        };
        let statuses: Vec<acp::ToolCallStatus> = last
            .blocks
            .iter()
            .filter_map(|b| match b {
                MessageBlock::ToolCall(tc) => Some(tc.status),
                _ => None,
            })
            .collect();
        assert_eq!(statuses, vec![acp::ToolCallStatus::Completed, acp::ToolCallStatus::Completed]);
    }

    #[test]
    fn ctrl_v_not_inserted_as_text() {
        let mut app = make_test_app();
        handle_normal_key(&mut app, KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
        assert_eq!(app.input.text(), "");
    }

    #[test]
    fn ctrl_v_not_inserted_when_mention_key_handler_is_active() {
        let mut app = make_test_app();
        handle_mention_key(&mut app, KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
        assert_eq!(app.input.text(), "");
    }

    #[test]
    fn cleanup_leaked_char_before_placeholder_removes_prefix_line() {
        let mut app = make_test_app();
        app.input.lines = vec!["C".into(), "[Pasted Text 1 - 11 lines]".into()];
        app.input.cursor_row = 1;
        app.input.cursor_col = app.input.lines[1].chars().count();

        cleanup_leaked_char_before_placeholder(&mut app);

        assert_eq!(app.input.lines, vec!["[Pasted Text 1 - 11 lines]"]);
        assert_eq!(app.input.cursor_row, 0);
        assert_eq!(app.input.cursor_col, "[Pasted Text 1 - 11 lines]".chars().count());
    }

    #[test]
    fn altgr_at_inserts_char_and_activates_mention() {
        let mut app = make_test_app();
        handle_normal_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('@'), KeyModifiers::CONTROL | KeyModifiers::ALT),
        );

        assert_eq!(app.input.text(), "@");
        assert!(app.mention.is_some());
    }

    #[test]
    fn ctrl_backspace_and_delete_use_word_operations() {
        let mut app = make_test_app();
        app.input.set_text("hello world");

        handle_normal_key(&mut app, KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL));
        assert_eq!(app.input.text(), "hello ");

        app.input.move_home();
        handle_normal_key(&mut app, KeyEvent::new(KeyCode::Delete, KeyModifiers::CONTROL));
        assert_eq!(app.input.text(), " ");
    }

    #[test]
    fn ctrl_z_and_y_undo_and_redo_textarea_history() {
        let mut app = make_test_app();
        app.input.set_text("hello world");

        handle_normal_key(&mut app, KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL));
        assert_eq!(app.input.text(), "hello ");

        handle_normal_key(&mut app, KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(app.input.text(), "hello world");

        handle_normal_key(&mut app, KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert_eq!(app.input.text(), "hello ");
    }

    #[test]
    fn ctrl_left_right_move_by_word() {
        let mut app = make_test_app();
        app.input.set_text("hello world");
        app.input.move_home();

        handle_normal_key(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL));
        assert!(app.input.cursor_col > 0);

        handle_normal_key(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(app.input.cursor_col, 0);
    }

    #[test]
    fn help_overlay_left_right_switches_help_view_tab() {
        let mut app = make_test_app();
        app.input.set_text("?");
        app.help_view = HelpView::Keys;

        dispatch_key_by_focus(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.help_view, HelpView::SlashCommands);

        dispatch_key_by_focus(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.help_view, HelpView::Keys);
    }

    #[test]
    fn tab_toggles_todo_focus_target_for_open_todos() {
        let mut app = make_test_app();
        app.todos.push(TodoItem {
            content: "Task".into(),
            status: TodoStatus::Pending,
            active_form: String::new(),
        });
        app.show_todo_panel = true;

        handle_normal_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.focus_owner(), FocusOwner::TodoList);

        handle_normal_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.focus_owner(), FocusOwner::Input);
    }

    #[test]
    fn up_down_in_todo_focus_changes_todo_selection() {
        let mut app = make_test_app();
        app.todos = vec![
            TodoItem {
                content: "Task 1".into(),
                status: TodoStatus::Pending,
                active_form: String::new(),
            },
            TodoItem {
                content: "Task 2".into(),
                status: TodoStatus::InProgress,
                active_form: String::new(),
            },
            TodoItem {
                content: "Task 3".into(),
                status: TodoStatus::Pending,
                active_form: String::new(),
            },
        ];
        app.show_todo_panel = true;
        app.claim_focus_target(FocusTarget::TodoList);
        app.todo_selected = 1;

        let before_cursor_row = app.input.cursor_row;
        let before_cursor_col = app.input.cursor_col;
        handle_normal_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.todo_selected, 2);
        assert_eq!(app.input.cursor_row, before_cursor_row);
        assert_eq!(app.input.cursor_col, before_cursor_col);

        handle_normal_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.todo_selected, 1);
    }

    #[test]
    fn permission_owner_overrides_todo_focus_for_up_down() {
        let mut app = make_test_app();
        app.todos.push(TodoItem {
            content: "Task".into(),
            status: TodoStatus::Pending,
            active_form: String::new(),
        });
        app.show_todo_panel = true;
        app.claim_focus_target(FocusTarget::TodoList);
        app.todo_selected = 0;
        let _rx_a = attach_pending_permission(
            &mut app,
            "perm-a",
            vec![
                acp::PermissionOption::new("allow", "Allow", acp::PermissionOptionKind::AllowOnce),
                acp::PermissionOption::new("deny", "Deny", acp::PermissionOptionKind::RejectOnce),
            ],
            true,
        );
        let _rx_b = attach_pending_permission(
            &mut app,
            "perm-b",
            vec![
                acp::PermissionOption::new("allow", "Allow", acp::PermissionOptionKind::AllowOnce),
                acp::PermissionOption::new("deny", "Deny", acp::PermissionOptionKind::RejectOnce),
            ],
            false,
        );
        app.claim_focus_target(FocusTarget::Permission);

        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        );

        assert_eq!(app.pending_permission_ids, vec!["perm-b", "perm-a"]);
        assert_eq!(app.todo_selected, 0);
    }

    #[test]
    fn permission_focus_allows_typing_for_non_permission_keys() {
        let mut app = make_test_app();
        app.pending_permission_ids.push("perm-1".into());
        app.claim_focus_target(FocusTarget::Permission);

        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE)),
        );

        assert_eq!(app.input.text(), "h");
    }

    #[test]
    fn permission_focus_allows_ctrl_t_toggle_todos() {
        let mut app = make_test_app();
        app.pending_permission_ids.push("perm-1".into());
        app.claim_focus_target(FocusTarget::Permission);
        app.todos.push(TodoItem {
            content: "Task".into(),
            status: TodoStatus::Pending,
            active_form: String::new(),
        });

        assert!(!app.show_todo_panel);
        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)),
        );
        assert!(app.show_todo_panel);
    }

    fn attach_pending_permission(
        app: &mut App,
        tool_id: &str,
        options: Vec<acp::PermissionOption>,
        focused: bool,
    ) -> oneshot::Receiver<acp::RequestPermissionResponse> {
        let (response_tx, response_rx) = oneshot::channel();
        let mut tc = tool_call(tool_id, acp::ToolCallStatus::InProgress);
        tc.pending_permission =
            Some(InlinePermission { options, response_tx, selected_index: 0, focused });
        app.messages.push(assistant_msg(vec![MessageBlock::ToolCall(Box::new(tc))]));
        let msg_idx = app.messages.len().saturating_sub(1);
        app.index_tool_call(tool_id.into(), msg_idx, 0);
        app.pending_permission_ids.push(tool_id.into());
        app.claim_focus_target(FocusTarget::Permission);
        response_rx
    }

    fn push_todo_and_focus(app: &mut App) {
        app.todos.push(TodoItem {
            content: "Task".into(),
            status: TodoStatus::Pending,
            active_form: String::new(),
        });
        app.show_todo_panel = true;
        app.claim_focus_target(FocusTarget::TodoList);
        assert_eq!(app.focus_owner(), FocusOwner::TodoList);
    }

    #[test]
    fn permission_ctrl_y_works_even_when_todo_focus_owns_navigation() {
        let mut app = make_test_app();
        let mut response_rx = attach_pending_permission(
            &mut app,
            "perm-1",
            vec![
                acp::PermissionOption::new("allow", "Allow", acp::PermissionOptionKind::AllowOnce),
                acp::PermissionOption::new("deny", "Deny", acp::PermissionOptionKind::RejectOnce),
            ],
            true,
        );

        // Override focus owner to todo to prove the quick shortcut is global.
        push_todo_and_focus(&mut app);

        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL)),
        );

        let resp = response_rx.try_recv().expect("ctrl+y should resolve pending permission");
        let acp::RequestPermissionOutcome::Selected(selected) = resp.outcome else {
            panic!("expected selected permission response");
        };
        assert_eq!(selected.option_id.to_string(), "allow");
        assert!(app.pending_permission_ids.is_empty());
    }

    #[test]
    fn permission_ctrl_a_works_even_when_todo_focus_owns_navigation() {
        let mut app = make_test_app();
        let mut response_rx = attach_pending_permission(
            &mut app,
            "perm-1",
            vec![
                acp::PermissionOption::new(
                    "allow-once",
                    "Allow once",
                    acp::PermissionOptionKind::AllowOnce,
                ),
                acp::PermissionOption::new(
                    "allow-always",
                    "Allow always",
                    acp::PermissionOptionKind::AllowAlways,
                ),
                acp::PermissionOption::new("deny", "Deny", acp::PermissionOptionKind::RejectOnce),
            ],
            true,
        );
        push_todo_and_focus(&mut app);

        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
        );

        let resp = response_rx.try_recv().expect("ctrl+a should resolve pending permission");
        let acp::RequestPermissionOutcome::Selected(selected) = resp.outcome else {
            panic!("expected selected permission response");
        };
        assert_eq!(selected.option_id.to_string(), "allow-always");
        assert!(app.pending_permission_ids.is_empty());
    }

    #[test]
    fn permission_ctrl_n_works_even_when_mention_focus_owns_navigation() {
        let mut app = make_test_app();
        let mut response_rx = attach_pending_permission(
            &mut app,
            "perm-1",
            vec![
                acp::PermissionOption::new("allow", "Allow", acp::PermissionOptionKind::AllowOnce),
                acp::PermissionOption::new("deny", "Deny", acp::PermissionOptionKind::RejectOnce),
            ],
            true,
        );

        app.mention = Some(mention::MentionState {
            trigger_row: 0,
            trigger_col: 0,
            query: String::new(),
            candidates: Vec::new(),
            dialog: super::super::dialog::DialogState::default(),
        });
        app.claim_focus_target(FocusTarget::Mention);
        assert_eq!(app.focus_owner(), FocusOwner::Mention);

        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
        );

        let resp = response_rx.try_recv().expect("ctrl+n should resolve pending permission");
        let acp::RequestPermissionOutcome::Selected(selected) = resp.outcome else {
            panic!("expected selected permission response");
        };
        assert_eq!(selected.option_id.to_string(), "deny");
        assert!(app.pending_permission_ids.is_empty());
    }

    #[test]
    fn connecting_state_ctrl_c_quits_without_selection_copy_path() {
        let mut app = make_test_app();
        app.status = AppStatus::Connecting;
        app.selection = Some(crate::app::SelectionState {
            kind: crate::app::SelectionKind::Input,
            start: crate::app::SelectionPoint { row: 0, col: 0 },
            end: crate::app::SelectionPoint { row: 0, col: 0 },
            dragging: false,
        });

        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );

        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_c_with_selection_never_falls_through_to_quit() {
        let mut app = make_test_app();
        app.selection = Some(crate::app::SelectionState {
            kind: crate::app::SelectionKind::Input,
            start: crate::app::SelectionPoint { row: 0, col: 0 },
            end: crate::app::SelectionPoint { row: 0, col: 0 },
            dragging: false,
        });

        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );

        assert!(!app.should_quit);
        assert!(app.selection.is_none());
    }

    #[test]
    fn mouse_scroll_clears_selection_before_scrolling() {
        let mut app = make_test_app();
        app.viewport.scroll_target = 2;
        app.selection = Some(crate::app::SelectionState {
            kind: crate::app::SelectionKind::Chat,
            start: crate::app::SelectionPoint { row: 0, col: 0 },
            end: crate::app::SelectionPoint { row: 0, col: 1 },
            dragging: false,
        });

        handle_terminal_event(
            &mut app,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
        );

        assert!(app.selection.is_none());
        assert_eq!(app.viewport.scroll_target, 5);
    }

    #[test]
    fn mention_owner_overrides_todo_focus_then_releases_back() {
        let mut app = make_test_app();
        app.todos.push(TodoItem {
            content: "Task".into(),
            status: TodoStatus::Pending,
            active_form: String::new(),
        });
        app.show_todo_panel = true;
        app.claim_focus_target(FocusTarget::TodoList);
        app.mention = Some(mention::MentionState {
            trigger_row: 0,
            trigger_col: 0,
            query: String::new(),
            candidates: Vec::new(),
            dialog: super::super::dialog::DialogState::default(),
        });
        app.claim_focus_target(FocusTarget::Mention);

        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );

        assert!(app.mention.is_none());
        assert_eq!(app.focus_owner(), FocusOwner::TodoList);
    }

    #[test]
    fn up_down_without_focus_scrolls_chat() {
        let mut app = make_test_app();
        app.viewport.scroll_target = 5;
        app.viewport.auto_scroll = true;

        handle_normal_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.viewport.scroll_target, 4);
        assert!(!app.viewport.auto_scroll);

        handle_normal_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.viewport.scroll_target, 5);
    }

    #[test]
    fn up_down_moves_input_cursor_when_multiline() {
        let mut app = make_test_app();
        app.input.set_text("line1\nline2\nline3");
        app.input.cursor_row = 1;
        app.input.cursor_col = 3;
        app.viewport.scroll_target = 7;

        handle_normal_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input.cursor_row, 0);
        assert_eq!(app.viewport.scroll_target, 7);

        handle_normal_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.input.cursor_row, 1);
        assert_eq!(app.viewport.scroll_target, 7);
    }

    #[test]
    fn down_at_input_bottom_falls_back_to_chat_scroll() {
        let mut app = make_test_app();
        app.input.set_text("line1\nline2");
        app.input.cursor_row = 1;
        app.input.cursor_col = 0;
        app.viewport.scroll_target = 2;

        handle_normal_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

        assert_eq!(app.input.cursor_row, 1);
        assert_eq!(app.viewport.scroll_target, 3);
    }

    #[test]
    fn internal_error_detection_accepts_xml_payload() {
        let payload =
            "<error><code>-32603</code><message>Adapter process crashed</message></error>";
        assert!(looks_like_internal_error(payload));
    }

    #[test]
    fn internal_error_detection_rejects_plain_bash_failure() {
        let payload = "bash: unknown_command: command not found";
        assert!(!looks_like_internal_error(payload));
    }

    #[test]
    fn summarize_internal_error_prefers_xml_message() {
        let payload =
            "<error><code>-32603</code><message>Adapter process crashed</message></error>";
        assert_eq!(summarize_internal_error(payload), "Adapter process crashed");
    }

    #[test]
    fn summarize_internal_error_reads_json_rpc_message() {
        let payload = r#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"internal rpc fault"}}"#;
        assert_eq!(summarize_internal_error(payload), "internal rpc fault");
    }
}
