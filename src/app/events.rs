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

use super::connect::take_connection_slot;
use super::selection::clear_selection;
use super::state::{RecentSessionInfo, ScrollbarDragState};
use super::{
    App, AppStatus, BlockCache, CancelOrigin, ChatMessage, FocusTarget, IncrementalMarkdown,
    InlinePermission, LoginHint, MessageBlock, MessageRole, MessageUsage, SelectionKind,
    SelectionPoint, ToolCallInfo,
};
use crate::agent::events::ClientEvent;
use crate::agent::model;
use crate::app::todos::{apply_plan_todos, parse_todos_if_present, set_todos};
#[cfg(test)]
use crossterm::event::KeyEvent;
use crossterm::event::{Event, KeyEventKind, MouseEvent, MouseEventKind};

const CONVERSATION_INTERRUPTED_HINT: &str =
    "Conversation interrupted. Tell the model how to proceed.";
const TURN_ERROR_INPUT_LOCK_HINT: &str =
    "Input disabled after an error. Press Ctrl+Q to quit and try again.";
const MSG_SPLIT_SOFT_LIMIT_BYTES: usize = 1536;
const MSG_SPLIT_HARD_LIMIT_BYTES: usize = 4096;

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
            if !matches!(app.status, AppStatus::Connecting | AppStatus::Resuming | AppStatus::Error)
            {
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
const SCROLLBAR_MIN_THUMB_HEIGHT: usize = 1;

struct MouseSelectionPoint {
    kind: SelectionKind,
    point: SelectionPoint,
}

fn handle_mouse_event(app: &mut App, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
            if start_scrollbar_drag(app, mouse) {
                return;
            }
            app.scrollbar_drag = None;
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
            if update_scrollbar_drag(app, mouse) {
                return;
            }
            let pt = mouse_point_to_selection(app, mouse);
            if let (Some(sel), Some(pt)) = (&mut app.selection, pt) {
                sel.end = pt.point;
            }
        }
        MouseEventKind::Up(crossterm::event::MouseButton::Left) => {
            app.scrollbar_drag = None;
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

#[derive(Clone, Copy)]
struct ScrollbarMetrics {
    viewport_height: usize,
    max_scroll: usize,
    thumb_size: usize,
    track_space: usize,
}

fn start_scrollbar_drag(app: &mut App, mouse: MouseEvent) -> bool {
    if !mouse_on_scrollbar_rail(app, mouse) {
        return false;
    }
    let Some(metrics) = scrollbar_metrics(app) else {
        return false;
    };
    let Some(local_row) = mouse_row_on_chat_track(app, mouse) else {
        return false;
    };

    let (thumb_top, thumb_size) = current_thumb_geometry(app, metrics);
    let thumb_end = thumb_top.saturating_add(thumb_size);
    let grab_offset = if (thumb_top..thumb_end).contains(&local_row) {
        local_row.saturating_sub(thumb_top)
    } else {
        thumb_size / 2
    };

    set_scroll_from_thumb_top(app, local_row.saturating_sub(grab_offset), metrics);
    app.scrollbar_drag = Some(ScrollbarDragState { thumb_grab_offset: grab_offset });
    clear_selection(app);
    true
}

fn update_scrollbar_drag(app: &mut App, mouse: MouseEvent) -> bool {
    let Some(drag) = app.scrollbar_drag else {
        return false;
    };
    let Some(metrics) = scrollbar_metrics(app) else {
        app.scrollbar_drag = None;
        return false;
    };
    let Some(local_row) = mouse_row_on_chat_track(app, mouse) else {
        return false;
    };

    set_scroll_from_thumb_top(app, local_row.saturating_sub(drag.thumb_grab_offset), metrics);
    true
}

fn scrollbar_metrics(app: &App) -> Option<ScrollbarMetrics> {
    let area = app.rendered_chat_area;
    if area.width == 0 || area.height == 0 {
        return None;
    }

    let viewport_height = area.height as usize;
    let content_height = app.viewport.total_message_height();
    if content_height <= viewport_height {
        return None;
    }

    let max_scroll = content_height.saturating_sub(viewport_height);
    let thumb_size = viewport_height
        .saturating_mul(viewport_height)
        .checked_div(content_height)
        .unwrap_or(0)
        .max(SCROLLBAR_MIN_THUMB_HEIGHT)
        .min(viewport_height);
    let track_space = viewport_height.saturating_sub(thumb_size);

    Some(ScrollbarMetrics { viewport_height, max_scroll, thumb_size, track_space })
}

#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::cast_sign_loss)]
fn current_thumb_geometry(app: &App, metrics: ScrollbarMetrics) -> (usize, usize) {
    let mut thumb_size = app.viewport.scrollbar_thumb_size.round() as usize;
    if thumb_size == 0 {
        thumb_size = metrics.thumb_size;
    }
    thumb_size = thumb_size.max(SCROLLBAR_MIN_THUMB_HEIGHT).min(metrics.viewport_height);
    let max_top = metrics.viewport_height.saturating_sub(thumb_size);
    let thumb_top = app.viewport.scrollbar_thumb_top.round().clamp(0.0, max_top as f32) as usize;
    (thumb_top, thumb_size)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::cast_sign_loss)]
fn set_scroll_from_thumb_top(app: &mut App, thumb_top: usize, metrics: ScrollbarMetrics) {
    let thumb_top = thumb_top.min(metrics.track_space);
    let target = if metrics.track_space == 0 {
        0
    } else {
        ((thumb_top as f32 / metrics.track_space as f32) * metrics.max_scroll as f32).round()
            as usize
    }
    .min(metrics.max_scroll);

    app.viewport.auto_scroll = false;
    app.viewport.scroll_target = target;
    // Keep content movement responsive while dragging the thumb.
    app.viewport.scroll_pos = target as f32;
    app.viewport.scroll_offset = target;
}

fn mouse_on_scrollbar_rail(app: &App, mouse: MouseEvent) -> bool {
    let area = app.rendered_chat_area;
    if area.width == 0 || area.height == 0 {
        return false;
    }
    let rail_x = area.right().saturating_sub(1);
    mouse.column == rail_x && mouse.row >= area.y && mouse.row < area.bottom()
}

fn mouse_row_on_chat_track(app: &App, mouse: MouseEvent) -> Option<usize> {
    let area = app.rendered_chat_area;
    if area.height == 0 {
        return None;
    }
    let max_row = area.height.saturating_sub(1) as usize;
    if mouse.row < area.y {
        return Some(0);
    }
    if mouse.row >= area.bottom() {
        return Some(max_row);
    }
    Some((mouse.row - area.y) as usize)
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
pub fn handle_client_event(app: &mut App, event: ClientEvent) {
    app.needs_redraw = true;
    match event {
        ClientEvent::SessionUpdate(update) => handle_session_update(app, update),
        ClientEvent::PermissionRequest { request, response_tx } => {
            let tool_id = request.tool_call.tool_call_id.clone();
            if let Some((mi, bi)) = app.lookup_tool_call(&tool_id) {
                if app.pending_permission_ids.iter().any(|id| id == &tool_id) {
                    tracing::warn!(
                        "Duplicate permission request for tool call: {tool_id}; auto-rejecting duplicate"
                    );
                    // Keep the original pending prompt and reject duplicate request.
                    if let Some(last_opt) = request.options.last() {
                        let _ = response_tx.send(model::RequestPermissionResponse::new(
                            model::RequestPermissionOutcome::Selected(
                                model::SelectedPermissionOutcome::new(last_opt.option_id.clone()),
                            ),
                        ));
                    }
                    return;
                }

                let mut layout_dirty = false;
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
                    tc.mark_tool_call_layout_dirty();
                    layout_dirty = true;
                    app.pending_permission_ids.push(tool_id);
                    app.claim_focus_target(FocusTarget::Permission);
                    app.viewport.engage_auto_scroll();
                } else {
                    tracing::warn!(
                        "Permission request for non-tool block index: {tool_id}; auto-rejecting"
                    );
                    if let Some(last_opt) = request.options.last() {
                        let _ = response_tx.send(model::RequestPermissionResponse::new(
                            model::RequestPermissionOutcome::Selected(
                                model::SelectedPermissionOutcome::new(last_opt.option_id.clone()),
                            ),
                        ));
                    }
                }
                if layout_dirty {
                    app.mark_message_layout_dirty(mi);
                }
            } else {
                tracing::warn!(
                    "Permission request for unknown tool call: {tool_id}; auto-rejecting"
                );
                // Tool call not found -- reject by selecting last option
                if let Some(last_opt) = request.options.last() {
                    let _ = response_tx.send(model::RequestPermissionResponse::new(
                        model::RequestPermissionOutcome::Selected(
                            model::SelectedPermissionOutcome::new(last_opt.option_id.clone()),
                        ),
                    ));
                }
            }
        }
        ClientEvent::TurnCancelled => {
            app.pending_compact_clear = false;
            app.is_compacting = false;
            if app.pending_cancel_origin.is_none() {
                app.pending_cancel_origin = Some(CancelOrigin::Manual);
            }
            app.cancelled_turn_pending_hint =
                matches!(app.pending_cancel_origin, Some(CancelOrigin::Manual));
            let _ = app.finalize_in_progress_tool_calls(model::ToolCallStatus::Failed);
        }
        ClientEvent::TurnComplete => {
            let tail_assistant_idx =
                app.messages.iter().rposition(|m| matches!(m.role, MessageRole::Assistant));
            let turn_was_active = matches!(app.status, AppStatus::Thinking | AppStatus::Running);
            let should_compact_clear = app.pending_compact_clear;
            app.pending_compact_clear = false;
            app.is_compacting = false;
            let cancelled_requested = app.pending_cancel_origin.is_some();
            let show_interrupted_hint =
                matches!(app.pending_cancel_origin, Some(CancelOrigin::Manual));
            app.pending_cancel_origin = None;
            app.cancelled_turn_pending_hint = false;
            if cancelled_requested {
                let _ = app.finalize_in_progress_tool_calls(model::ToolCallStatus::Failed);
            } else {
                let _ = app.finalize_in_progress_tool_calls(model::ToolCallStatus::Completed);
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
            } else if turn_was_active || cancelled_requested {
                mark_turn_exit_assistant_layout_dirty(app, tail_assistant_idx);
            }
            super::input_submit::drain_queued_submission(app);
        }
        ClientEvent::TurnError(msg) => {
            let tail_assistant_idx =
                app.messages.iter().rposition(|m| matches!(m.role, MessageRole::Assistant));
            let turn_was_active = matches!(app.status, AppStatus::Thinking | AppStatus::Running);
            let should_compact_clear = app.pending_compact_clear;
            app.pending_compact_clear = false;
            app.is_compacting = false;
            let cancelled_requested = app.pending_cancel_origin;
            let show_interrupted_hint = matches!(cancelled_requested, Some(CancelOrigin::Manual));
            app.pending_cancel_origin = None;
            app.cancelled_turn_pending_hint = false;

            if cancelled_requested.is_some() {
                let summary = summarize_internal_error(&msg);
                tracing::warn!(
                    error_preview = %summary,
                    "Turn error suppressed after cancellation request"
                );
                if should_compact_clear {
                    super::slash::clear_conversation_history(app);
                } else {
                    mark_turn_exit_assistant_layout_dirty(app, tail_assistant_idx);
                }
                let _ = app.finalize_in_progress_tool_calls(model::ToolCallStatus::Failed);
                app.input.clear();
                app.pending_submit = false;
                app.status = AppStatus::Ready;
                app.files_accessed = 0;
                app.active_task_ids.clear();
                app.refresh_git_branch();
                if show_interrupted_hint {
                    push_interrupted_hint(app);
                }
                super::input_submit::drain_queued_submission(app);
                return;
            }

            tracing::error!("Turn error: {msg}");
            if looks_like_internal_error(&msg) {
                tracing::debug!(
                    error_preview = %summarize_internal_error(&msg),
                    "Internal bridge/adapter turn error payload"
                );
            }
            if should_compact_clear {
                super::slash::clear_conversation_history(app);
            }
            let _ = app.finalize_in_progress_tool_calls(model::ToolCallStatus::Failed);
            app.input.clear();
            app.pending_submit = false;
            app.status = AppStatus::Error;
            push_turn_error_message(app, &msg);
            if !should_compact_clear && turn_was_active {
                mark_turn_exit_assistant_layout_dirty(app, tail_assistant_idx);
            }
        }
        ClientEvent::Connected { session_id, cwd, model_name, mode, history_updates } => {
            // Grab connection from the shared slot
            if let Some(slot) = take_connection_slot() {
                app.conn = Some(slot.conn);
            }
            apply_session_cwd(app, cwd);
            app.session_id = Some(session_id);
            app.model_name = model_name;
            app.mode = mode;
            app.login_hint = None;
            app.pending_compact_clear = false;
            app.is_compacting = false;
            app.session_usage = super::SessionUsageState::default();
            app.cancelled_turn_pending_hint = false;
            app.pending_cancel_origin = None;
            app.queued_submission = None;
            app.cached_header_line = None;
            app.cached_footer_line = None;
            app.update_welcome_model_if_pristine();
            app.sync_welcome_recent_sessions();
            if !history_updates.is_empty() {
                load_resume_history(app, &history_updates);
            }
            app.status = AppStatus::Ready;
            app.resuming_session_id = None;
        }
        ClientEvent::SessionsListed { sessions, .. } => {
            app.recent_sessions = sessions
                .into_iter()
                .map(|entry| RecentSessionInfo {
                    session_id: entry.session_id,
                    cwd: entry.cwd,
                    title: entry.title,
                    updated_at: entry.updated_at,
                })
                .collect();
            app.sync_welcome_recent_sessions();
        }
        ClientEvent::AuthRequired { method_name, method_description } => {
            // Show auth context without pre-filling /login. Slash login/logout
            // discoverability is intentionally deferred for now.
            app.status = AppStatus::Ready;
            app.resuming_session_id = None;
            app.login_hint = Some(LoginHint { method_name, method_description });
            app.pending_compact_clear = false;
            app.is_compacting = false;
            app.cancelled_turn_pending_hint = false;
            app.pending_cancel_origin = None;
            app.queued_submission = None;
        }
        ClientEvent::ConnectionFailed(msg) => {
            app.pending_compact_clear = false;
            app.is_compacting = false;
            app.cancelled_turn_pending_hint = false;
            app.pending_cancel_origin = None;
            app.queued_submission = None;
            app.resuming_session_id = None;
            app.input.clear();
            app.pending_submit = false;
            app.status = AppStatus::Error;
            push_connection_error_message(app, &msg);
        }
        ClientEvent::SlashCommandError(msg) => {
            app.messages.push(ChatMessage {
                role: MessageRole::System,
                blocks: vec![MessageBlock::Text(
                    msg.clone(),
                    BlockCache::default(),
                    IncrementalMarkdown::from_complete(&msg),
                )],
                usage: None,
            });
            app.viewport.engage_auto_scroll();
            app.status = AppStatus::Ready;
            app.resuming_session_id = None;
        }
        ClientEvent::SessionReplaced { session_id, cwd, model_name, mode, history_updates } => {
            app.pending_compact_clear = false;
            app.is_compacting = false;
            app.pending_cancel_origin = None;
            app.queued_submission = None;
            apply_session_cwd(app, cwd);
            reset_for_new_session(app, session_id, model_name, mode);
            if !history_updates.is_empty() {
                load_resume_history(app, &history_updates);
            }
            app.status = AppStatus::Ready;
            app.resuming_session_id = None;
        }
        ClientEvent::UpdateAvailable { latest_version, current_version } => {
            app.update_check_hint = Some(format!(
                "Update available: v{latest_version} (current v{current_version})  Ctrl+U to hide"
            ));
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
        usage: None,
    });
    app.viewport.engage_auto_scroll();
}

fn mark_turn_exit_assistant_layout_dirty(app: &mut App, idx: Option<usize>) {
    let Some(idx) = idx else {
        return;
    };
    if app.messages.get(idx).is_some_and(|msg| matches!(msg.role, MessageRole::Assistant)) {
        app.mark_message_layout_dirty(idx);
    }
}

fn push_turn_error_message(app: &mut App, error: &str) {
    let message = format!("Turn failed: {error}\n\n{TURN_ERROR_INPUT_LOCK_HINT}");
    app.messages.push(ChatMessage {
        role: MessageRole::System,
        blocks: vec![MessageBlock::Text(
            message.clone(),
            BlockCache::default(),
            IncrementalMarkdown::from_complete(&message),
        )],
        usage: None,
    });
    app.viewport.engage_auto_scroll();
}

fn push_connection_error_message(app: &mut App, error: &str) {
    let message = format!("Connection failed: {error}\n\n{TURN_ERROR_INPUT_LOCK_HINT}");
    app.messages.push(ChatMessage {
        role: MessageRole::System,
        blocks: vec![MessageBlock::Text(
            message.clone(),
            BlockCache::default(),
            IncrementalMarkdown::from_complete(&message),
        )],
        usage: None,
    });
    app.viewport.engage_auto_scroll();
}

fn shorten_cwd_display(cwd_raw: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if cwd_raw.starts_with(home_str.as_ref()) {
            return format!("~{}", &cwd_raw[home_str.len()..]);
        }
    }
    cwd_raw.to_owned()
}

fn sync_welcome_cwd(app: &mut App) {
    let Some(first) = app.messages.first_mut() else {
        return;
    };
    if !matches!(first.role, MessageRole::Welcome) {
        return;
    }
    let Some(MessageBlock::Welcome(welcome)) = first.blocks.first_mut() else {
        return;
    };
    welcome.cwd.clone_from(&app.cwd);
    welcome.cache.invalidate();
    app.mark_message_layout_dirty(0);
}

fn apply_session_cwd(app: &mut App, cwd_raw: String) {
    app.cwd_raw = cwd_raw;
    app.cwd = shorten_cwd_display(&app.cwd_raw);
    app.file_cache = None;
    app.cached_header_line = None;
    app.cached_footer_line = None;
    app.refresh_git_branch();
    sync_welcome_cwd(app);
}

fn update_session_usage(app: &mut App, usage: &model::UsageUpdate) -> MessageUsage {
    let has_turn_usage_snapshot = usage.input_tokens.is_some()
        || usage.output_tokens.is_some()
        || usage.cache_read_tokens.is_some()
        || usage.cache_write_tokens.is_some();
    if has_turn_usage_snapshot {
        app.session_usage.latest_input_tokens = usage.input_tokens;
        app.session_usage.latest_output_tokens = usage.output_tokens;
        app.session_usage.latest_cache_read_tokens = usage.cache_read_tokens;
        app.session_usage.latest_cache_write_tokens = usage.cache_write_tokens;
    }

    if let Some(v) = usage.input_tokens {
        app.session_usage.total_input_tokens =
            app.session_usage.total_input_tokens.saturating_add(v);
    }
    if let Some(v) = usage.output_tokens {
        app.session_usage.total_output_tokens =
            app.session_usage.total_output_tokens.saturating_add(v);
    }
    if let Some(v) = usage.cache_read_tokens {
        app.session_usage.total_cache_read_tokens =
            app.session_usage.total_cache_read_tokens.saturating_add(v);
    }
    if let Some(v) = usage.cache_write_tokens {
        app.session_usage.total_cache_write_tokens =
            app.session_usage.total_cache_write_tokens.saturating_add(v);
    }

    if let Some(v) = usage.total_cost_usd {
        // Prefer adapter-reported cumulative total when available.
        app.session_usage.total_cost_usd = Some(v);
        if app.session_usage.cost_is_since_resume {
            let includes_historical_baseline = usage.turn_cost_usd.is_none_or(|turn| v > turn);
            if includes_historical_baseline {
                app.session_usage.cost_is_since_resume = false;
            }
        }
    } else if let Some(v) = usage.turn_cost_usd {
        app.session_usage.total_cost_usd =
            Some(app.session_usage.total_cost_usd.unwrap_or(0.0) + v);
    }

    if let Some(v) = usage.context_window {
        app.session_usage.context_window = Some(v);
    }
    if let Some(v) = usage.max_output_tokens {
        app.session_usage.max_output_tokens = Some(v);
    }

    MessageUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        turn_cost_usd: usage.turn_cost_usd,
    }
}

fn attach_usage_to_latest_assistant_message(app: &mut App, usage: MessageUsage) {
    for (idx, msg) in app.messages.iter_mut().enumerate().rev() {
        if matches!(msg.role, MessageRole::Assistant) {
            msg.usage = Some(usage);
            app.mark_message_layout_dirty(idx);
            break;
        }
    }
}

fn append_resume_user_message_chunk(app: &mut App, chunk: &model::ContentChunk) {
    let model::ContentBlock::Text(text) = &chunk.content else {
        return;
    };
    if text.text.is_empty() {
        return;
    }

    if let Some(last) = app.messages.last_mut()
        && matches!(last.role, MessageRole::User)
    {
        if let Some(MessageBlock::Text(existing, cache, incr)) = last.blocks.last_mut() {
            existing.push_str(&text.text);
            incr.append(&text.text);
            cache.invalidate();
        } else {
            let mut incr = IncrementalMarkdown::default();
            incr.append(&text.text);
            last.blocks.push(MessageBlock::Text(text.text.clone(), BlockCache::default(), incr));
        }
        return;
    }

    let mut incr = IncrementalMarkdown::default();
    incr.append(&text.text);
    app.messages.push(ChatMessage {
        role: MessageRole::User,
        blocks: vec![MessageBlock::Text(text.text.clone(), BlockCache::default(), incr)],
        usage: None,
    });
}

fn load_resume_history(app: &mut App, history_updates: &[model::SessionUpdate]) {
    app.messages.clear();
    app.messages.push(ChatMessage::welcome_with_recent(
        &app.model_name,
        &app.cwd,
        &app.recent_sessions,
    ));
    for update in history_updates {
        match update {
            model::SessionUpdate::UserMessageChunk(chunk) => {
                append_resume_user_message_chunk(app, chunk);
            }
            _ => handle_session_update(app, update.clone()),
        }
    }
    let resumed_with_tokens = app.session_usage.total_tokens() > 0;
    if resumed_with_tokens && app.session_usage.total_cost_usd.is_none() {
        app.session_usage.cost_is_since_resume = true;
    }
    let _ = app.finalize_in_progress_tool_calls(model::ToolCallStatus::Failed);
    app.viewport = super::ChatViewport::new();
    app.viewport.engage_auto_scroll();
}

#[allow(clippy::too_many_lines)]
fn reset_for_new_session(
    app: &mut App,
    session_id: model::SessionId,
    model_name: String,
    mode: Option<super::ModeState>,
) {
    crate::agent::events::kill_all_terminals(&app.terminals);

    app.session_id = Some(session_id);
    app.model_name = model_name;
    app.mode = mode;
    app.login_hint = None;
    app.pending_compact_clear = false;
    app.is_compacting = false;
    app.session_usage = super::SessionUsageState::default();
    app.should_quit = false;
    app.files_accessed = 0;
    app.cancelled_turn_pending_hint = false;
    app.pending_cancel_origin = None;
    app.queued_submission = None;

    app.messages.clear();
    app.messages.push(ChatMessage::welcome_with_recent(
        &app.model_name,
        &app.cwd,
        &app.recent_sessions,
    ));
    app.viewport = super::ChatViewport::new();

    app.input.clear();
    app.pending_submit = false;
    app.drain_key_count = 0;
    app.paste_burst.reset();
    app.pending_paste_text.clear();

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
    app.scrollbar_drag = None;
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

fn sdk_tool_name_from_meta(meta: Option<&serde_json::Value>) -> Option<&str> {
    meta.and_then(|m| m.get("claudeCode")).and_then(|v| v.get("toolName")).and_then(|v| v.as_str())
}

fn fallback_sdk_tool_name(kind: model::ToolKind) -> &'static str {
    match kind {
        model::ToolKind::Read => "Read",
        model::ToolKind::Edit => "Edit",
        model::ToolKind::Delete => "Delete",
        model::ToolKind::Move => "Move",
        model::ToolKind::Search => "Search",
        model::ToolKind::Execute => "Bash",
        model::ToolKind::Think => "Think",
        model::ToolKind::Fetch => "Fetch",
        model::ToolKind::SwitchMode => "ExitPlanMode",
        model::ToolKind::Other => "Tool",
    }
}

fn resolve_sdk_tool_name(kind: model::ToolKind, meta: Option<&serde_json::Value>) -> String {
    sdk_tool_name_from_meta(meta)
        .filter(|name| !name.trim().is_empty())
        .map_or_else(|| fallback_sdk_tool_name(kind).to_owned(), str::to_owned)
}

#[allow(clippy::too_many_lines)]
fn handle_tool_call(app: &mut App, tc: model::ToolCall) {
    let title = tc.title.clone();
    let kind = tc.kind;
    let id_str = tc.tool_call_id.clone();
    tracing::debug!(
        "ToolCall: id={id_str} title={title} kind={kind:?} status={:?} content_blocks={} has_raw_output={}",
        tc.status,
        tc.content.len(),
        tc.raw_output.is_some()
    );

    let sdk_tool_name = resolve_sdk_tool_name(kind, tc.meta.as_ref());
    let is_task = sdk_tool_name == "Task";

    // Subagent children are never hidden -- they need to be visible so
    // permission prompts render and the user can interact with them.
    let hidden = false;

    // Extract todos from TodoWrite tool calls
    if sdk_tool_name == "TodoWrite" {
        tracing::info!("TodoWrite ToolCall detected: id={id_str}, raw_input={:?}", tc.raw_input);
        if let Some(ref raw_input) = tc.raw_input {
            if let Some(todos) = parse_todos_if_present(raw_input) {
                tracing::info!("Parsed {} todos from ToolCall raw_input", todos.len());
                set_todos(app, todos);
            } else {
                tracing::debug!(
                    "TodoWrite ToolCall raw_input has no todos array yet; preserving existing todos"
                );
            }
        } else {
            tracing::warn!("TodoWrite ToolCall has no raw_input");
        }
    }

    // Track new Task tool calls as active subagents
    if is_task {
        app.insert_active_task(id_str.clone());
    }

    let initial_execute_output = if super::is_execute_tool_name(&sdk_tool_name) {
        tc.raw_output.as_ref().and_then(raw_output_to_terminal_text)
    } else {
        None
    };

    let mut tool_info = ToolCallInfo {
        id: id_str,
        title: shorten_tool_title(&tc.title, &app.cwd_raw),
        sdk_tool_name,
        raw_input: tc.raw_input,
        status: tc.status,
        content: tc.content,
        collapsed: app.tools_collapsed,
        hidden,
        terminal_id: None,
        terminal_command: None,
        terminal_output: None,
        terminal_output_len: 0,
        terminal_bytes_seen: 0,
        terminal_snapshot_mode: crate::app::TerminalSnapshotMode::AppendOnly,
        render_epoch: 0,
        layout_epoch: 0,
        last_measured_width: 0,
        last_measured_height: 0,
        last_measured_layout_epoch: 0,
        last_measured_layout_generation: 0,
        cache: BlockCache::default(),
        pending_permission: None,
    };
    if let Some(output) = initial_execute_output {
        tool_info.terminal_output_len = output.len();
        tool_info.terminal_bytes_seen = output.len();
        tool_info.terminal_output = Some(output);
        tool_info.terminal_snapshot_mode = crate::app::TerminalSnapshotMode::ReplaceSnapshot;
    }

    // Attach to current assistant message -- update existing or add new
    let msg_idx = app.messages.len().saturating_sub(1);
    let existing_pos = app.lookup_tool_call(&tool_info.id);
    let is_assistant =
        app.messages.last().is_some_and(|m| matches!(m.role, MessageRole::Assistant));

    if is_assistant {
        if let Some((mi, bi)) = existing_pos {
            let mut layout_dirty = false;
            if let Some(MessageBlock::ToolCall(existing)) =
                app.messages.get_mut(mi).and_then(|m| m.blocks.get_mut(bi))
            {
                let existing = existing.as_mut();
                let mut changed = false;
                if existing.title != tool_info.title {
                    existing.title.clone_from(&tool_info.title);
                    changed = true;
                }
                if existing.status != tool_info.status {
                    existing.status = tool_info.status;
                    changed = true;
                }
                if existing.content != tool_info.content {
                    existing.content.clone_from(&tool_info.content);
                    changed = true;
                }
                if existing.sdk_tool_name != tool_info.sdk_tool_name {
                    existing.sdk_tool_name.clone_from(&tool_info.sdk_tool_name);
                    changed = true;
                }
                if existing.raw_input != tool_info.raw_input {
                    existing.raw_input.clone_from(&tool_info.raw_input);
                    changed = true;
                }
                if changed {
                    existing.mark_tool_call_layout_dirty();
                    layout_dirty = true;
                } else {
                    crate::perf::mark("tool_update_noop_skips");
                }
            }
            if layout_dirty {
                app.mark_message_layout_dirty(mi);
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
            usage: None,
        });
        app.index_tool_call(tc_id, new_idx, 0);
    }

    app.status = AppStatus::Running;
    if !hidden {
        app.files_accessed += 1;
    }
}

#[allow(clippy::too_many_lines)]
fn handle_session_update(app: &mut App, update: model::SessionUpdate) {
    tracing::debug!("SessionUpdate variant: {}", session_update_name(&update));
    match update {
        model::SessionUpdate::AgentMessageChunk(chunk) => {
            if let model::ContentBlock::Text(text) = chunk.content {
                // Text is actively streaming - suppress the "Thinking..." spinner
                app.status = AppStatus::Running;
                if text.text.is_empty() {
                    return;
                }

                // Append to last text block in current assistant message, splitting
                // the block into frozen chunks at prioritized boundaries.
                if let Some(last) = app.messages.last_mut()
                    && matches!(last.role, MessageRole::Assistant)
                {
                    append_agent_stream_text(&mut last.blocks, &text.text);
                    return;
                }

                let mut blocks = Vec::new();
                append_agent_stream_text(&mut blocks, &text.text);
                app.messages.push(ChatMessage {
                    role: MessageRole::Assistant,
                    blocks,
                    usage: None,
                });
            }
        }
        model::SessionUpdate::ToolCall(tc) => {
            handle_tool_call(app, tc);
        }
        model::SessionUpdate::ToolCallUpdate(tcu) => {
            // Find and update the tool call by id (in-place)
            let id_str = tcu.tool_call_id.clone();
            let has_content = tcu.fields.content.as_ref().map_or(0, Vec::len);
            let has_raw_output = tcu.fields.raw_output.is_some();
            tracing::debug!(
                "ToolCallUpdate: id={id_str} new_title={:?} new_status={:?} content_blocks={has_content} has_raw_output={has_raw_output}",
                tcu.fields.title,
                tcu.fields.status
            );
            if has_raw_output {
                tracing::debug!(
                    "ToolCallUpdate raw_output: id={id_str} {:?}",
                    tcu.fields.raw_output
                );
            }
            if matches!(tcu.fields.status, Some(model::ToolCallStatus::Failed))
                && let Some(content_preview) = internal_failed_tool_content_preview(
                    tcu.fields.content.as_deref(),
                    tcu.fields.raw_output.as_ref(),
                )
            {
                let sdk_tool_name = sdk_tool_name_from_meta(tcu.meta.as_ref());
                tracing::debug!(
                    tool_call_id = %id_str,
                    title = ?tcu.fields.title,
                    sdk_tool_name = ?sdk_tool_name,
                    content_preview = %content_preview,
                    "Internal failed ToolCallUpdate payload"
                );
            }

            // If this is a Task completing, remove from active list
            if matches!(
                tcu.fields.status,
                Some(model::ToolCallStatus::Completed | model::ToolCallStatus::Failed)
            ) {
                app.remove_active_task(&id_str);
            }

            let mut pending_todos: Option<Vec<super::TodoItem>> = None;
            let mut layout_dirty_idx: Option<usize> = None;
            if let Some((mi, bi)) = app.lookup_tool_call(&id_str) {
                if let Some(MessageBlock::ToolCall(tc)) =
                    app.messages.get_mut(mi).and_then(|m| m.blocks.get_mut(bi))
                {
                    let tc = tc.as_mut();
                    let mut changed = false;
                    if let Some(status) = tcu.fields.status
                        && tc.status != status
                    {
                        tc.status = status;
                        changed = true;
                    }
                    if let Some(title) = &tcu.fields.title {
                        let shortened = shorten_tool_title(title, &app.cwd_raw);
                        if tc.title != shortened {
                            tc.title = shortened;
                            changed = true;
                        }
                    }
                    if let Some(content) = tcu.fields.content {
                        // Extract terminal_id and command from Terminal content blocks
                        for cb in &content {
                            if let model::ToolCallContent::Terminal(t) = cb {
                                let tid = t.terminal_id.clone();
                                if let Some(terminal) = app.terminals.borrow().get(&tid)
                                    && tc.terminal_command.as_deref()
                                        != Some(terminal.command.as_str())
                                {
                                    tc.terminal_command = Some(terminal.command.clone());
                                    changed = true;
                                }
                                if tc.terminal_id.as_deref() != Some(tid.as_str()) {
                                    tc.terminal_id = Some(tid.clone());
                                    changed = true;
                                }
                                if !app
                                    .terminal_tool_calls
                                    .iter()
                                    .any(|(id, m, b)| id == &tid && *m == mi && *b == bi)
                                {
                                    app.terminal_tool_calls.push((tid, mi, bi));
                                }
                            }
                        }
                        if tc.content != content {
                            tc.content = content;
                            changed = true;
                        }
                    }
                    if let Some(raw_input) = tcu.fields.raw_input.as_ref()
                        && tc.raw_input.as_ref() != Some(raw_input)
                    {
                        tc.raw_input = Some(raw_input.clone());
                        changed = true;
                    }
                    // Keep updating Execute output from raw_output so long-running commands
                    // can stream visible terminal text before completion.
                    if tc.is_execute_tool()
                        && let Some(raw_output) = tcu.fields.raw_output.as_ref()
                        && let Some(output) = raw_output_to_terminal_text(raw_output)
                        && tc.terminal_output.as_deref() != Some(output.as_str())
                    {
                        tc.terminal_output_len = output.len();
                        tc.terminal_bytes_seen = output.len();
                        tc.terminal_output = Some(output);
                        tc.terminal_snapshot_mode =
                            crate::app::TerminalSnapshotMode::ReplaceSnapshot;
                        changed = true;
                    }
                    // Update sdk_tool_name from update meta when provided.
                    if let Some(name) = sdk_tool_name_from_meta(tcu.meta.as_ref())
                        && !name.trim().is_empty()
                        && tc.sdk_tool_name != name
                    {
                        tc.sdk_tool_name = name.to_owned();
                        changed = true;
                    }
                    // Update todos from TodoWrite raw_input updates
                    if tc.sdk_tool_name == "TodoWrite" {
                        tracing::info!(
                            "TodoWrite ToolCallUpdate: id={id_str}, raw_input={:?}",
                            tcu.fields.raw_input
                        );
                        if let Some(ref raw_input) = tcu.fields.raw_input {
                            if let Some(todos) = parse_todos_if_present(raw_input) {
                                tracing::info!(
                                    "Parsed {} todos from ToolCallUpdate raw_input",
                                    todos.len()
                                );
                                pending_todos = Some(todos);
                            } else {
                                tracing::debug!(
                                    "TodoWrite ToolCallUpdate raw_input has no todos array yet; preserving existing todos"
                                );
                            }
                        }
                    }
                    if matches!(
                        tc.status,
                        model::ToolCallStatus::Completed | model::ToolCallStatus::Failed
                    ) && tc.collapsed != app.tools_collapsed
                    {
                        tc.collapsed = app.tools_collapsed;
                        changed = true;
                    }
                    if changed {
                        tc.mark_tool_call_layout_dirty();
                        layout_dirty_idx = Some(mi);
                    } else {
                        crate::perf::mark("tool_update_noop_skips");
                    }
                }
            } else {
                tracing::warn!("ToolCallUpdate: id={id_str} not found in index");
            }
            if let Some(mi) = layout_dirty_idx {
                app.mark_message_layout_dirty(mi);
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
        model::SessionUpdate::UserMessageChunk(_) => {
            // Our own message echoed back -- we already display it
        }
        model::SessionUpdate::AgentThoughtChunk(chunk) => {
            tracing::debug!("Agent thought: {:?}", chunk);
            app.status = AppStatus::Thinking;
        }
        model::SessionUpdate::Plan(plan) => {
            tracing::debug!("Plan update: {:?}", plan);
            apply_plan_todos(app, &plan);
        }
        model::SessionUpdate::AvailableCommandsUpdate(cmds) => {
            tracing::debug!("Available commands: {} commands", cmds.available_commands.len());
            app.available_commands = cmds.available_commands;
            if app.slash.is_some() {
                super::slash::update_query(app);
            }
        }
        model::SessionUpdate::CurrentModeUpdate(update) => {
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
        model::SessionUpdate::ConfigOptionUpdate(config) => {
            tracing::debug!("Config update: {:?}", config);
        }
        model::SessionUpdate::UsageUpdate(usage) => {
            let message_usage = update_session_usage(app, &usage);
            attach_usage_to_latest_assistant_message(app, message_usage);
            app.cached_footer_line = None;
            tracing::debug!(
                "UsageUpdate: in={:?} out={:?} cache_read={:?} cache_write={:?} total_cost={:?} turn_cost={:?} ctx_window={:?}",
                usage.input_tokens,
                usage.output_tokens,
                usage.cache_read_tokens,
                usage.cache_write_tokens,
                usage.total_cost_usd,
                usage.turn_cost_usd,
                usage.context_window
            );
        }
        model::SessionUpdate::SessionStatusUpdate(status) => {
            // TODO(runtime-verification): confirm in real SDK sessions that compaction
            // status updates are emitted consistently; if not, add a fallback indicator.
            app.is_compacting = matches!(status, model::SessionStatus::Compacting);
            app.cached_footer_line = None;
            tracing::debug!("SessionStatusUpdate: compacting={}", app.is_compacting);
        }
        model::SessionUpdate::CompactionBoundary(boundary) => {
            app.is_compacting = true;
            app.session_usage.last_compaction_trigger = Some(boundary.trigger);
            app.session_usage.last_compaction_pre_tokens = Some(boundary.pre_tokens);
            app.cached_footer_line = None;
            tracing::debug!(
                "CompactionBoundary: trigger={:?} pre_tokens={}",
                boundary.trigger,
                boundary.pre_tokens
            );
            if matches!(boundary.trigger, model::CompactionTrigger::Auto) {
                let text = "Auto-compacting context...";
                app.messages.push(ChatMessage {
                    role: MessageRole::System,
                    blocks: vec![MessageBlock::Text(
                        text.to_owned(),
                        BlockCache::default(),
                        IncrementalMarkdown::from_complete(text),
                    )],
                    usage: None,
                });
                app.viewport.engage_auto_scroll();
            }
        }
    }
}

fn internal_failed_tool_content_preview(
    content: Option<&[model::ToolCallContent]>,
    raw_output: Option<&serde_json::Value>,
) -> Option<String> {
    let text = content
        .and_then(|items| {
            items.iter().find_map(|c| match c {
                model::ToolCallContent::Content(inner) => match &inner.content {
                    model::ContentBlock::Text(t) => Some(t.text.clone()),
                    model::ContentBlock::Image(_) => None,
                },
                _ => None,
            })
        })
        .or_else(|| raw_output.and_then(raw_output_to_terminal_text))?;
    if !looks_like_internal_error(&text) {
        return None;
    }
    Some(summarize_internal_error(&text))
}

fn raw_output_to_terminal_text(raw_output: &serde_json::Value) -> Option<String> {
    match raw_output {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => (!s.is_empty()).then(|| s.clone()),
        serde_json::Value::Array(items) => {
            let chunks: Vec<&str> = items.iter().filter_map(extract_text_field).collect();
            if chunks.is_empty() {
                serde_json::to_string_pretty(raw_output).ok().filter(|s| !s.is_empty())
            } else {
                Some(chunks.join("\n"))
            }
        }
        value => extract_text_field(value)
            .map(str::to_owned)
            .or_else(|| serde_json::to_string_pretty(value).ok().filter(|s| !s.is_empty())),
    }
}

fn extract_text_field(value: &serde_json::Value) -> Option<&str> {
    value.get("text").and_then(serde_json::Value::as_str)
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
        "bridge",
        "json-rpc",
        "rpc",
        "protocol error",
        "transport",
        "handshake failed",
        "session creation failed",
        "connection closed",
        "event channel closed",
        "tool permission request failed",
        "zoderror",
        "invalid_union",
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
    if let Some(summary) = summarize_permission_schema_error(input) {
        return preview_for_log(&summary);
    }
    if let Some(msg) = extract_xml_tag_value(input, "message") {
        return preview_for_log(msg);
    }
    if let Some(msg) = extract_json_string_field(input, "message") {
        return preview_for_log(&msg);
    }
    let fallback = input.lines().find(|line| !line.trim().is_empty()).unwrap_or(input);
    preview_for_log(fallback.trim())
}

fn summarize_permission_schema_error(input: &str) -> Option<String> {
    let lower = input.to_ascii_lowercase();
    if !lower.contains("tool permission request failed") {
        return None;
    }

    let detail = if let Some(msg) = extract_json_string_field(input, "message") {
        msg
    } else {
        input.lines().find(|line| !line.trim().is_empty()).unwrap_or(input).trim().to_owned()
    };

    Some(format!("Tool permission request failed: {detail}"))
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
/// Handles both `/` and `\\` separators on all platforms since the bridge adapter
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
                    if matches!(tc.status, model::ToolCallStatus::InProgress | model::ToolCallStatus::Pending)
            )
        });
    }
    false
}

fn new_text_block(text: String) -> MessageBlock {
    let incr = IncrementalMarkdown::from_complete(&text);
    MessageBlock::Text(text, BlockCache::default(), incr)
}

fn append_agent_stream_text(blocks: &mut Vec<MessageBlock>, chunk: &str) {
    if chunk.is_empty() {
        return;
    }
    if let Some(MessageBlock::Text(text, cache, incr)) = blocks.last_mut() {
        text.push_str(chunk);
        incr.append(chunk);
        cache.invalidate();
    } else {
        blocks.push(new_text_block(chunk.to_owned()));
    }

    let split_count = split_tail_text_block(blocks);
    if split_count > 0 {
        crate::perf::mark_with("text_block_split_count", "count", split_count);
    }

    if let Some(MessageBlock::Text(text, _, _)) = blocks.last() {
        crate::perf::mark_with("text_block_active_tail_bytes", "bytes", text.len());
    }
    let text_block_count = blocks.iter().filter(|b| matches!(b, MessageBlock::Text(..))).count();
    crate::perf::mark_with("text_block_frozen_count", "count", text_block_count.saturating_sub(1));
}

fn split_tail_text_block(blocks: &mut Vec<MessageBlock>) -> usize {
    let mut split_count = 0usize;
    loop {
        let Some(tail_idx) = blocks.len().checked_sub(1) else {
            break;
        };
        let Some(split_at) = blocks.get(tail_idx).and_then(|block| {
            if let MessageBlock::Text(text, _, _) = block {
                find_text_block_split_index(text)
            } else {
                None
            }
        }) else {
            break;
        };

        let (completed, remainder) = match blocks.get(tail_idx) {
            Some(MessageBlock::Text(text, _, _)) => {
                (text[..split_at].to_owned(), text[split_at..].to_owned())
            }
            _ => break,
        };

        if completed.is_empty() || remainder.is_empty() {
            break;
        }

        blocks[tail_idx] = new_text_block(remainder);
        blocks.insert(tail_idx, new_text_block(completed));
        split_count += 1;
    }
    split_count
}

fn find_text_block_split_index(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut in_fence = false;
    let mut i = 0usize;

    let mut soft_newline = None;
    let mut soft_sentence = None;
    let mut hard_newline = None;
    let mut hard_sentence = None;
    let mut post_hard_newline = None;
    let mut post_hard_sentence = None;

    while i < bytes.len() {
        if (i == 0 || bytes[i - 1] == b'\n') && bytes[i..].starts_with(b"```") {
            in_fence = !in_fence;
        }

        if !in_fence {
            if i + 1 < bytes.len() && bytes[i] == b'\n' && bytes[i + 1] == b'\n' {
                let split_at = i + 2;
                if split_at < bytes.len() {
                    return Some(split_at);
                }
                return None;
            }

            if bytes[i] == b'\n' {
                track_text_split_candidate(
                    i + 1,
                    &mut soft_newline,
                    &mut hard_newline,
                    &mut post_hard_newline,
                );
            }

            if is_sentence_boundary(bytes, i) {
                track_text_split_candidate(
                    i + 1,
                    &mut soft_sentence,
                    &mut hard_sentence,
                    &mut post_hard_sentence,
                );
            }
        }
        i += 1;
    }

    if bytes.len() >= MSG_SPLIT_SOFT_LIMIT_BYTES
        && let Some(split_at) = pick_text_split_candidate(soft_newline, soft_sentence)
        && split_at < bytes.len()
    {
        return Some(split_at);
    }

    if bytes.len() >= MSG_SPLIT_HARD_LIMIT_BYTES
        && let Some(split_at) =
            hard_newline.or(post_hard_newline).or(hard_sentence).or(post_hard_sentence)
        && split_at < bytes.len()
    {
        return Some(split_at);
    }

    None
}

fn track_text_split_candidate(
    split_at: usize,
    soft_slot: &mut Option<usize>,
    hard_slot: &mut Option<usize>,
    post_hard_slot: &mut Option<usize>,
) {
    if split_at <= MSG_SPLIT_SOFT_LIMIT_BYTES {
        *soft_slot = Some(split_at);
    }
    if split_at <= MSG_SPLIT_HARD_LIMIT_BYTES {
        *hard_slot = Some(split_at);
    } else if post_hard_slot.is_none() {
        *post_hard_slot = Some(split_at);
    }
}

fn pick_text_split_candidate(newline: Option<usize>, sentence: Option<usize>) -> Option<usize> {
    newline.or(sentence)
}

fn is_sentence_boundary(bytes: &[u8], i: usize) -> bool {
    matches!(bytes[i], b'.' | b'!' | b'?')
        && (i + 1 == bytes.len() || matches!(bytes[i + 1], b' ' | b'\t' | b'\r' | b'\n'))
}

/// Return a human-readable name for a `SessionUpdate` variant (for debug logging).
fn session_update_name(update: &model::SessionUpdate) -> &'static str {
    match update {
        model::SessionUpdate::AgentMessageChunk(_) => "AgentMessageChunk",
        model::SessionUpdate::ToolCall(_) => "ToolCall",
        model::SessionUpdate::ToolCallUpdate(_) => "ToolCallUpdate",
        model::SessionUpdate::UserMessageChunk(_) => "UserMessageChunk",
        model::SessionUpdate::AgentThoughtChunk(_) => "AgentThoughtChunk",
        model::SessionUpdate::Plan(_) => "Plan",
        model::SessionUpdate::AvailableCommandsUpdate(_) => "AvailableCommandsUpdate",
        model::SessionUpdate::CurrentModeUpdate(_) => "CurrentModeUpdate",
        model::SessionUpdate::ConfigOptionUpdate(_) => "ConfigOptionUpdate",
        model::SessionUpdate::UsageUpdate(_) => "UsageUpdate",
        model::SessionUpdate::SessionStatusUpdate(_) => "SessionStatusUpdate",
        model::SessionUpdate::CompactionBoundary(_) => "CompactionBoundary",
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
    use ratatui::layout::Rect;
    use tokio::sync::oneshot;

    // Helper: build a minimal ToolCallInfo with given id + status

    fn tool_call(id: &str, status: model::ToolCallStatus) -> ToolCallInfo {
        ToolCallInfo {
            id: id.into(),
            title: id.into(),
            sdk_tool_name: "Read".into(),
            raw_input: None,
            status,
            content: vec![],
            collapsed: false,
            hidden: false,
            terminal_id: None,
            terminal_command: None,
            terminal_output: None,
            terminal_output_len: 0,
            terminal_bytes_seen: 0,
            terminal_snapshot_mode: crate::app::TerminalSnapshotMode::AppendOnly,
            render_epoch: 0,
            layout_epoch: 0,
            last_measured_width: 0,
            last_measured_height: 0,
            last_measured_layout_epoch: 0,
            last_measured_layout_generation: 0,
            cache: BlockCache::default(),
            pending_permission: None,
        }
    }

    fn assistant_msg(blocks: Vec<MessageBlock>) -> ChatMessage {
        ChatMessage { role: MessageRole::Assistant, blocks, usage: None }
    }

    fn user_msg(text: &str) -> ChatMessage {
        ChatMessage {
            role: MessageRole::User,
            blocks: vec![MessageBlock::Text(
                text.into(),
                BlockCache::default(),
                IncrementalMarkdown::default(),
            )],
            usage: None,
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

    #[test]
    fn split_index_prefers_double_newline() {
        let text = "first\n\nsecond";
        let split_at = find_text_block_split_index(text);
        assert_eq!(split_at, Some("first\n\n".len()));
    }

    #[test]
    fn split_index_soft_limit_prefers_newline() {
        let prefix = "a".repeat(MSG_SPLIT_SOFT_LIMIT_BYTES - 1);
        let text = format!("{prefix}\n{}", "b".repeat(32));
        let split_at = find_text_block_split_index(&text).expect("expected split index");
        assert_eq!(&text[..split_at], format!("{prefix}\n"));
    }

    #[test]
    fn split_index_hard_limit_uses_sentence_when_needed() {
        let prefix = "a".repeat(MSG_SPLIT_HARD_LIMIT_BYTES + 32);
        let text = format!("{prefix}. tail");
        let split_at = find_text_block_split_index(&text).expect("expected split index");
        assert_eq!(&text[..split_at], format!("{prefix}."));
    }

    #[test]
    fn split_index_ignores_double_newline_inside_code_fence() {
        let text = "```\nline1\n\nline2\n```";
        assert!(find_text_block_split_index(text).is_none());
    }

    #[test]
    fn agent_message_chunk_splits_into_frozen_text_blocks() {
        let mut app = make_test_app();
        handle_client_event(
            &mut app,
            ClientEvent::SessionUpdate(model::SessionUpdate::AgentMessageChunk(
                model::ContentChunk::new(model::ContentBlock::Text(model::TextContent::new(
                    "p1\n\np2\n\np3",
                ))),
            )),
        );

        assert_eq!(app.messages.len(), 1);
        let Some(last) = app.messages.last() else {
            panic!("missing assistant message");
        };
        assert!(matches!(last.role, MessageRole::Assistant));
        assert_eq!(last.blocks.len(), 3);
        let Some(MessageBlock::Text(b1, _, _)) = last.blocks.first() else {
            panic!("expected first text block");
        };
        let Some(MessageBlock::Text(b2, _, _)) = last.blocks.get(1) else {
            panic!("expected second text block");
        };
        let Some(MessageBlock::Text(b3, _, _)) = last.blocks.get(2) else {
            panic!("expected third text block");
        };
        assert_eq!(b1, "p1\n\n");
        assert_eq!(b2, "p2\n\n");
        assert_eq!(b3, "p3");
    }

    // has_in_progress_tool_calls

    fn make_test_app() -> App {
        App::test_default()
    }

    fn connected_event(model_name: &str) -> ClientEvent {
        ClientEvent::Connected {
            session_id: model::SessionId::new("test-session"),
            cwd: "/test".into(),
            model_name: model_name.to_owned(),
            mode: None,
            history_updates: Vec::new(),
        }
    }

    #[test]
    fn raw_output_string_maps_to_terminal_text() {
        let raw = serde_json::json!("hello\nworld");
        assert_eq!(raw_output_to_terminal_text(&raw).as_deref(), Some("hello\nworld"));
    }

    #[test]
    fn raw_output_text_array_maps_to_terminal_text() {
        let raw = serde_json::json!([
            {"type": "text", "text": "first"},
            {"type": "text", "text": "second"}
        ]);
        assert_eq!(raw_output_to_terminal_text(&raw).as_deref(), Some("first\nsecond"));
    }

    #[test]
    fn execute_tool_update_uses_raw_output_fallback() {
        let mut app = make_test_app();
        let tc = model::ToolCall::new("tc-exec", "Terminal")
            .kind(model::ToolKind::Execute)
            .status(model::ToolCallStatus::InProgress);
        handle_client_event(
            &mut app,
            ClientEvent::SessionUpdate(model::SessionUpdate::ToolCall(tc)),
        );

        let fields = model::ToolCallUpdateFields::new()
            .status(model::ToolCallStatus::Completed)
            .raw_output(serde_json::json!("line 1\nline 2"));
        let update = model::ToolCallUpdate::new("tc-exec", fields);
        handle_client_event(
            &mut app,
            ClientEvent::SessionUpdate(model::SessionUpdate::ToolCallUpdate(update)),
        );

        let Some((mi, bi)) = app.lookup_tool_call("tc-exec") else {
            panic!("tool call not indexed");
        };
        let Some(MessageBlock::ToolCall(tc)) = app.messages.get(mi).and_then(|m| m.blocks.get(bi))
        else {
            panic!("tool call block missing");
        };
        assert_eq!(tc.terminal_output.as_deref(), Some("line 1\nline 2"));
    }

    #[test]
    fn tool_call_update_noop_does_not_bump_epochs() {
        let mut app = make_test_app();
        let tc = model::ToolCall::new("tc-noop", "Read file")
            .kind(model::ToolKind::Read)
            .status(model::ToolCallStatus::InProgress);
        handle_client_event(
            &mut app,
            ClientEvent::SessionUpdate(model::SessionUpdate::ToolCall(tc)),
        );

        let (mi, bi) = app.lookup_tool_call("tc-noop").expect("tool call not indexed");
        let (before_render, before_layout, before_dirty_from) = {
            let MessageBlock::ToolCall(tc) = &app.messages[mi].blocks[bi] else {
                panic!("tool call block missing");
            };
            (tc.render_epoch, tc.layout_epoch, app.viewport.dirty_from)
        };

        let update = model::ToolCallUpdate::new(
            "tc-noop",
            model::ToolCallUpdateFields::new().status(model::ToolCallStatus::InProgress),
        );
        handle_client_event(
            &mut app,
            ClientEvent::SessionUpdate(model::SessionUpdate::ToolCallUpdate(update)),
        );

        let MessageBlock::ToolCall(tc) = &app.messages[mi].blocks[bi] else {
            panic!("tool call block missing");
        };
        assert_eq!(tc.render_epoch, before_render);
        assert_eq!(tc.layout_epoch, before_layout);
        assert_eq!(app.viewport.dirty_from, before_dirty_from);
    }

    #[test]
    fn todowrite_tool_call_without_todos_array_preserves_existing_todos() {
        let mut app = make_test_app();
        app.todos.push(TodoItem {
            content: "Existing todo".into(),
            status: TodoStatus::InProgress,
            active_form: String::new(),
        });
        app.show_todo_panel = true;

        let todo_call = model::ToolCall::new("tc-todo-empty", "TodoWrite")
            .kind(model::ToolKind::Other)
            .raw_input(serde_json::json!({}))
            .meta(serde_json::json!({"claudeCode": {"toolName": "TodoWrite"}}));
        handle_client_event(
            &mut app,
            ClientEvent::SessionUpdate(model::SessionUpdate::ToolCall(todo_call)),
        );

        assert_eq!(app.todos.len(), 1);
        assert_eq!(app.todos[0].content, "Existing todo");
        assert_eq!(app.todos[0].status, TodoStatus::InProgress);
        assert!(app.show_todo_panel);
    }

    #[test]
    fn todowrite_tool_call_update_without_todos_array_preserves_existing_todos() {
        let mut app = make_test_app();
        let todo_call = model::ToolCall::new("tc-todo-update", "TodoWrite")
            .kind(model::ToolKind::Other)
            .raw_input(serde_json::json!({
                "todos": [{"content": "Task A", "status": "in_progress"}]
            }))
            .meta(serde_json::json!({"claudeCode": {"toolName": "TodoWrite"}}));
        handle_client_event(
            &mut app,
            ClientEvent::SessionUpdate(model::SessionUpdate::ToolCall(todo_call)),
        );
        assert_eq!(app.todos.len(), 1);
        assert_eq!(app.todos[0].content, "Task A");

        let update = model::ToolCallUpdate::new(
            "tc-todo-update",
            model::ToolCallUpdateFields::new().raw_input(serde_json::json!({})),
        );
        handle_client_event(
            &mut app,
            ClientEvent::SessionUpdate(model::SessionUpdate::ToolCallUpdate(update)),
        );

        assert_eq!(app.todos.len(), 1);
        assert_eq!(app.todos[0].content, "Task A");
        assert_eq!(app.todos[0].status, TodoStatus::InProgress);
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
            model::ToolCallStatus::Pending,
        )))]));
        assert!(has_in_progress_tool_calls(&app));
    }

    #[test]
    fn has_in_progress_with_in_progress_tool() {
        let mut app = make_test_app();
        app.messages.push(assistant_msg(vec![MessageBlock::ToolCall(Box::new(tool_call(
            "tc1",
            model::ToolCallStatus::InProgress,
        )))]));
        assert!(has_in_progress_tool_calls(&app));
    }

    #[test]
    fn has_in_progress_all_completed() {
        let mut app = make_test_app();
        app.messages.push(assistant_msg(vec![MessageBlock::ToolCall(Box::new(tool_call(
            "tc1",
            model::ToolCallStatus::Completed,
        )))]));
        assert!(!has_in_progress_tool_calls(&app));
    }

    #[test]
    fn has_in_progress_all_failed() {
        let mut app = make_test_app();
        app.messages.push(assistant_msg(vec![MessageBlock::ToolCall(Box::new(tool_call(
            "tc1",
            model::ToolCallStatus::Failed,
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
            model::ToolCallStatus::InProgress,
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
            model::ToolCallStatus::InProgress,
        )))]));
        app.messages.push(user_msg("ok"));
        app.messages.push(assistant_msg(vec![MessageBlock::ToolCall(Box::new(tool_call(
            "tc2",
            model::ToolCallStatus::Completed,
        )))]));
        assert!(!has_in_progress_tool_calls(&app));
    }

    #[test]
    fn has_in_progress_mixed_completed_and_pending() {
        let mut app = make_test_app();
        app.messages.push(assistant_msg(vec![
            MessageBlock::ToolCall(Box::new(tool_call("tc1", model::ToolCallStatus::Completed))),
            MessageBlock::ToolCall(Box::new(tool_call("tc2", model::ToolCallStatus::InProgress))),
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
            MessageBlock::ToolCall(Box::new(tool_call("tc1", model::ToolCallStatus::Completed))),
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
                    model::ToolCallStatus::Completed,
                )))
            })
            .collect();
        blocks.push(MessageBlock::ToolCall(Box::new(tool_call(
            "tc_pending",
            model::ToolCallStatus::Pending,
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
                    model::ToolCallStatus::Completed,
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
            MessageBlock::ToolCall(Box::new(tool_call("tc1", model::ToolCallStatus::Completed))),
            MessageBlock::ToolCall(Box::new(tool_call("tc2", model::ToolCallStatus::Failed))),
            MessageBlock::ToolCall(Box::new(tool_call("tc3", model::ToolCallStatus::Completed))),
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

        handle_client_event(&mut app, ClientEvent::TurnCancelled);
        assert!(app.cancelled_turn_pending_hint);

        handle_client_event(&mut app, ClientEvent::TurnComplete);

        assert!(!app.cancelled_turn_pending_hint);
        let last = app.messages.last().expect("expected interruption hint message");
        assert!(matches!(last.role, MessageRole::System));
        let Some(MessageBlock::Text(text, _, _)) = last.blocks.first() else {
            panic!("expected text block");
        };
        assert_eq!(text, CONVERSATION_INTERRUPTED_HINT);
    }

    #[test]
    fn turn_complete_after_manual_cancel_marks_tail_assistant_layout_dirty() {
        let mut app = make_test_app();
        app.status = AppStatus::Thinking;
        app.messages.push(user_msg("build app"));
        app.messages.push(assistant_msg(vec![MessageBlock::Text(
            "partial output".into(),
            BlockCache::default(),
            IncrementalMarkdown::from_complete("partial output"),
        )]));
        app.pending_cancel_origin = Some(CancelOrigin::Manual);

        handle_client_event(&mut app, ClientEvent::TurnComplete);

        assert!(matches!(app.status, AppStatus::Ready));
        assert_eq!(app.viewport.dirty_from, Some(1));
        let Some(last) = app.messages.last() else {
            panic!("expected interruption hint message");
        };
        assert!(matches!(last.role, MessageRole::System));
    }

    #[test]
    fn turn_complete_after_auto_cancel_marks_tail_assistant_layout_dirty() {
        let mut app = make_test_app();
        app.status = AppStatus::Running;
        app.messages.push(user_msg("build app"));
        app.messages.push(assistant_msg(vec![MessageBlock::Text(
            "partial output".into(),
            BlockCache::default(),
            IncrementalMarkdown::from_complete("partial output"),
        )]));
        app.pending_cancel_origin = Some(CancelOrigin::AutoQueue);

        handle_client_event(&mut app, ClientEvent::TurnComplete);

        assert!(matches!(app.status, AppStatus::Ready));
        assert_eq!(app.viewport.dirty_from, Some(1));
        let Some(last) = app.messages.last() else {
            panic!("expected assistant message");
        };
        assert!(matches!(last.role, MessageRole::Assistant));
    }

    #[test]
    fn connected_updates_welcome_model_while_pristine() {
        let mut app = make_test_app();
        app.messages.push(ChatMessage::welcome("Connecting...", "/test"));

        handle_client_event(&mut app, connected_event("claude-updated"));

        let Some(first) = app.messages.first() else {
            panic!("missing welcome message");
        };
        let Some(MessageBlock::Welcome(welcome)) = first.blocks.first() else {
            panic!("expected welcome block");
        };
        assert_eq!(welcome.model_name, "claude-updated");
    }

    #[test]
    fn connected_updates_cwd_and_clears_resuming_marker() {
        let mut app = make_test_app();
        app.messages.push(ChatMessage::welcome("Connecting...", "/test"));
        app.file_cache = Some(Vec::new());
        app.resuming_session_id = Some("resume-123".into());

        handle_client_event(
            &mut app,
            ClientEvent::Connected {
                session_id: model::SessionId::new("session-cwd"),
                cwd: "/changed".into(),
                model_name: "claude-updated".into(),
                mode: None,
                history_updates: Vec::new(),
            },
        );

        assert_eq!(app.cwd_raw, "/changed");
        assert_eq!(app.cwd, "/changed");
        assert!(app.file_cache.is_none());
        assert!(app.resuming_session_id.is_none());
        let Some(first) = app.messages.first() else {
            panic!("missing welcome message");
        };
        let Some(MessageBlock::Welcome(welcome)) = first.blocks.first() else {
            panic!("expected welcome block");
        };
        assert_eq!(welcome.cwd, "/changed");
    }

    #[test]
    fn connected_does_not_update_welcome_after_chat_started() {
        let mut app = make_test_app();
        app.messages.push(ChatMessage::welcome("Connecting...", "/test"));
        app.messages.push(user_msg("hello"));

        handle_client_event(&mut app, connected_event("claude-updated"));

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

        handle_client_event(
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
    fn update_available_sets_footer_hint() {
        let mut app = make_test_app();
        assert!(app.update_check_hint.is_none());

        handle_client_event(
            &mut app,
            ClientEvent::UpdateAvailable {
                latest_version: "0.3.0".into(),
                current_version: "0.2.0".into(),
            },
        );

        assert_eq!(
            app.update_check_hint.as_deref(),
            Some("Update available: v0.3.0 (current v0.2.0)  Ctrl+U to hide")
        );
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

        handle_client_event(
            &mut app,
            ClientEvent::SessionReplaced {
                session_id: model::SessionId::new("replacement"),
                cwd: "/replacement".into(),
                model_name: "new-model".into(),
                mode: None,
                history_updates: Vec::new(),
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
        assert_eq!(app.cwd_raw, "/replacement");
        assert_eq!(app.cwd, "/replacement");
        let Some(MessageBlock::Welcome(welcome)) = app.messages[0].blocks.first() else {
            panic!("expected welcome block");
        };
        assert_eq!(welcome.cwd, "/replacement");
    }

    #[test]
    fn slash_command_error_while_resuming_returns_ready_and_clears_marker() {
        let mut app = make_test_app();
        app.status = AppStatus::Resuming;
        app.resuming_session_id = Some("resume-123".into());

        handle_client_event(&mut app, ClientEvent::SlashCommandError("resume failed".into()));

        assert!(matches!(app.status, AppStatus::Ready));
        assert!(app.resuming_session_id.is_none());
    }

    #[test]
    fn resume_does_not_add_confirmation_system_message() {
        let mut app = make_test_app();
        app.resuming_session_id = Some("requested-123".into());

        handle_client_event(
            &mut app,
            ClientEvent::SessionReplaced {
                session_id: model::SessionId::new("active-456"),
                cwd: "/replacement".into(),
                model_name: "new-model".into(),
                mode: None,
                history_updates: Vec::new(),
            },
        );

        assert_eq!(app.messages.len(), 1);
        assert!(matches!(app.messages[0].role, MessageRole::Welcome));
        assert!(app.resuming_session_id.is_none());
        assert!(matches!(app.status, AppStatus::Ready));
    }

    #[test]
    fn resume_history_renders_user_message_chunks() {
        let mut app = make_test_app();
        let history_updates = vec![
            model::SessionUpdate::UserMessageChunk(model::ContentChunk::new(
                model::ContentBlock::Text(model::TextContent::new("first user line")),
            )),
            model::SessionUpdate::AgentMessageChunk(model::ContentChunk::new(
                model::ContentBlock::Text(model::TextContent::new("assistant reply")),
            )),
        ];

        handle_client_event(
            &mut app,
            ClientEvent::SessionReplaced {
                session_id: model::SessionId::new("active-456"),
                cwd: "/replacement".into(),
                model_name: "new-model".into(),
                mode: None,
                history_updates,
            },
        );

        assert_eq!(app.messages.len(), 3);
        assert!(matches!(app.messages[0].role, MessageRole::Welcome));
        assert!(matches!(app.messages[1].role, MessageRole::User));
        assert!(matches!(app.messages[2].role, MessageRole::Assistant));

        let Some(MessageBlock::Text(user_text, _, _)) = app.messages[1].blocks.first() else {
            panic!("expected user text block");
        };
        assert_eq!(user_text, "first user line");
    }

    #[test]
    fn resume_history_forces_open_tool_calls_to_failed() {
        let mut app = make_test_app();
        let open_tool = model::ToolCall::new("resume-open", "Execute command")
            .kind(model::ToolKind::Execute)
            .status(model::ToolCallStatus::InProgress);

        handle_client_event(
            &mut app,
            ClientEvent::SessionReplaced {
                session_id: model::SessionId::new("active-789"),
                cwd: "/replacement".into(),
                model_name: "new-model".into(),
                mode: None,
                history_updates: vec![model::SessionUpdate::ToolCall(open_tool)],
            },
        );

        let Some((mi, bi)) = app.lookup_tool_call("resume-open") else {
            panic!("missing tool call index");
        };
        let Some(MessageBlock::ToolCall(tc)) = app.messages.get(mi).and_then(|m| m.blocks.get(bi))
        else {
            panic!("expected tool call block");
        };
        assert_eq!(tc.status, model::ToolCallStatus::Failed);
    }

    #[test]
    fn resume_history_marks_cost_as_since_resume_when_missing() {
        let mut app = make_test_app();
        let history_updates = vec![model::SessionUpdate::UsageUpdate(model::UsageUpdate {
            input_tokens: Some(410),
            output_tokens: Some(19),
            cache_read_tokens: Some(52_000),
            cache_write_tokens: Some(1_250),
            total_cost_usd: None,
            turn_cost_usd: None,
            context_window: None,
            max_output_tokens: None,
        })];

        handle_client_event(
            &mut app,
            ClientEvent::SessionReplaced {
                session_id: model::SessionId::new("active-901"),
                cwd: "/replacement".into(),
                model_name: "new-model".into(),
                mode: None,
                history_updates,
            },
        );

        assert!(app.session_usage.cost_is_since_resume);
        assert!(app.session_usage.total_cost_usd.is_none());
    }

    #[test]
    fn total_cost_update_clears_since_resume_cost_marker() {
        let mut app = make_test_app();
        app.session_id = Some(model::SessionId::new("active-902"));
        app.session_usage.cost_is_since_resume = true;
        app.session_usage.total_input_tokens = 500;

        handle_client_event(
            &mut app,
            ClientEvent::SessionUpdate(model::SessionUpdate::UsageUpdate(model::UsageUpdate {
                input_tokens: Some(2),
                output_tokens: Some(6),
                cache_read_tokens: Some(20_000),
                cache_write_tokens: Some(800),
                total_cost_usd: Some(4.20),
                turn_cost_usd: Some(0.20),
                context_window: Some(200_000),
                max_output_tokens: None,
            })),
        );

        let total_cost = app.session_usage.total_cost_usd.expect("total cost");
        assert!((total_cost - 4.20).abs() < f64::EPSILON);
        assert!(!app.session_usage.cost_is_since_resume);
    }

    #[test]
    fn turn_complete_without_cancel_does_not_render_interrupted_hint() {
        let mut app = make_test_app();
        handle_client_event(&mut app, ClientEvent::TurnComplete);
        assert!(app.messages.is_empty());
    }

    #[test]
    fn turn_complete_clears_history_when_compact_pending() {
        let mut app = make_test_app();
        app.session_id = Some(model::SessionId::new("session-x"));
        app.pending_compact_clear = true;
        app.messages.push(user_msg("/compact"));
        app.messages.push(assistant_msg(vec![MessageBlock::Text(
            "compacted".into(),
            BlockCache::default(),
            IncrementalMarkdown::from_complete("compacted"),
        )]));

        handle_client_event(&mut app, ClientEvent::TurnComplete);

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

        handle_client_event(&mut app, ClientEvent::TurnError("adapter failed".into()));

        assert!(!app.pending_compact_clear);
        assert!(matches!(app.status, AppStatus::Error));
        assert_eq!(app.messages.len(), 2);
        assert!(matches!(app.messages[0].role, MessageRole::Welcome));
        let Some(ChatMessage { role: MessageRole::System, blocks, .. }) = app.messages.get(1)
        else {
            panic!("expected system error message");
        };
        let Some(MessageBlock::Text(text, _, _)) = blocks.first() else {
            panic!("expected text block");
        };
        assert!(text.contains("Turn failed: adapter failed"));
        assert!(text.contains("Press Ctrl+Q to quit and try again"));
    }

    #[test]
    fn compaction_boundary_enables_compacting_and_records_boundary() {
        let mut app = make_test_app();
        assert!(!app.is_compacting);

        handle_client_event(
            &mut app,
            ClientEvent::SessionUpdate(model::SessionUpdate::CompactionBoundary(
                model::CompactionBoundary {
                    trigger: model::CompactionTrigger::Manual,
                    pre_tokens: 123_456,
                },
            )),
        );

        assert!(app.is_compacting);
        assert_eq!(
            app.session_usage.last_compaction_trigger,
            Some(model::CompactionTrigger::Manual)
        );
        assert_eq!(app.session_usage.last_compaction_pre_tokens, Some(123_456));
    }

    #[test]
    fn turn_error_after_cancel_shows_interrupted_hint_instead_of_error_block() {
        let mut app = make_test_app();
        app.messages.push(user_msg("build app"));

        handle_client_event(&mut app, ClientEvent::TurnCancelled);
        assert!(app.cancelled_turn_pending_hint);

        handle_client_event(
            &mut app,
            ClientEvent::TurnError("Error: Request was aborted.\n    at stack line".into()),
        );

        assert!(!app.cancelled_turn_pending_hint);
        assert!(matches!(app.status, AppStatus::Ready));

        let Some(last) = app.messages.last() else {
            panic!("expected interruption hint message");
        };
        assert!(matches!(last.role, MessageRole::System));
        let Some(MessageBlock::Text(text, _, _)) = last.blocks.first() else {
            panic!("expected text block");
        };
        assert_eq!(text, CONVERSATION_INTERRUPTED_HINT);
    }

    #[test]
    fn turn_error_after_auto_cancel_marks_tail_assistant_layout_dirty() {
        let mut app = make_test_app();
        app.status = AppStatus::Running;
        app.messages.push(user_msg("build app"));
        app.messages.push(assistant_msg(vec![MessageBlock::Text(
            "partial output".into(),
            BlockCache::default(),
            IncrementalMarkdown::from_complete("partial output"),
        )]));
        app.pending_cancel_origin = Some(CancelOrigin::AutoQueue);

        handle_client_event(
            &mut app,
            ClientEvent::TurnError("Error: Request was aborted.\n    at stack line".into()),
        );

        assert!(matches!(app.status, AppStatus::Ready));
        assert_eq!(app.viewport.dirty_from, Some(1));
        assert_eq!(app.messages.len(), 2);
        let Some(last) = app.messages.last() else {
            panic!("expected assistant message");
        };
        assert!(matches!(last.role, MessageRole::Assistant));
    }

    #[test]
    fn turn_cancel_marks_active_tools_failed() {
        let mut app = make_test_app();
        app.messages.push(assistant_msg(vec![
            MessageBlock::ToolCall(Box::new(tool_call("tc1", model::ToolCallStatus::InProgress))),
            MessageBlock::ToolCall(Box::new(tool_call("tc2", model::ToolCallStatus::Pending))),
            MessageBlock::ToolCall(Box::new(tool_call("tc3", model::ToolCallStatus::Completed))),
        ]));

        handle_client_event(&mut app, ClientEvent::TurnCancelled);

        let Some(last) = app.messages.last() else {
            panic!("missing assistant message");
        };
        let statuses: Vec<model::ToolCallStatus> = last
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
                model::ToolCallStatus::Failed,
                model::ToolCallStatus::Failed,
                model::ToolCallStatus::Completed
            ]
        );
    }

    #[test]
    fn turn_complete_marks_lingering_tools_completed() {
        let mut app = make_test_app();
        app.messages.push(assistant_msg(vec![
            MessageBlock::ToolCall(Box::new(tool_call("tc1", model::ToolCallStatus::InProgress))),
            MessageBlock::ToolCall(Box::new(tool_call("tc2", model::ToolCallStatus::Pending))),
        ]));

        handle_client_event(&mut app, ClientEvent::TurnComplete);

        let Some(last) = app.messages.last() else {
            panic!("missing assistant message");
        };
        let statuses: Vec<model::ToolCallStatus> = last
            .blocks
            .iter()
            .filter_map(|b| match b {
                MessageBlock::ToolCall(tc) => Some(tc.status),
                _ => None,
            })
            .collect();
        assert_eq!(
            statuses,
            vec![model::ToolCallStatus::Completed, model::ToolCallStatus::Completed]
        );
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
                model::PermissionOption::new(
                    "allow",
                    "Allow",
                    model::PermissionOptionKind::AllowOnce,
                ),
                model::PermissionOption::new(
                    "deny",
                    "Deny",
                    model::PermissionOptionKind::RejectOnce,
                ),
            ],
            true,
        );
        let _rx_b = attach_pending_permission(
            &mut app,
            "perm-b",
            vec![
                model::PermissionOption::new(
                    "allow",
                    "Allow",
                    model::PermissionOptionKind::AllowOnce,
                ),
                model::PermissionOption::new(
                    "deny",
                    "Deny",
                    model::PermissionOptionKind::RejectOnce,
                ),
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

    #[test]
    fn ctrl_h_toggles_header_visibility() {
        let mut app = make_test_app();
        assert!(app.show_header);

        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL)),
        );
        assert!(!app.show_header);

        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL)),
        );
        assert!(app.show_header);
    }

    #[test]
    fn ctrl_u_hides_update_hint_globally() {
        let mut app = make_test_app();
        app.update_check_hint = Some("Update available: v9.9.9 (current v0.2.0)".into());
        app.todos.push(TodoItem {
            content: "Task".into(),
            status: TodoStatus::Pending,
            active_form: String::new(),
        });
        app.show_todo_panel = true;
        app.claim_focus_target(FocusTarget::TodoList);
        assert_eq!(app.focus_owner(), FocusOwner::TodoList);

        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
        );

        assert!(app.update_check_hint.is_none());
    }

    fn attach_pending_permission(
        app: &mut App,
        tool_id: &str,
        options: Vec<model::PermissionOption>,
        focused: bool,
    ) -> oneshot::Receiver<model::RequestPermissionResponse> {
        let (response_tx, response_rx) = oneshot::channel();
        let mut tc = tool_call(tool_id, model::ToolCallStatus::InProgress);
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
                model::PermissionOption::new(
                    "allow",
                    "Allow",
                    model::PermissionOptionKind::AllowOnce,
                ),
                model::PermissionOption::new(
                    "deny",
                    "Deny",
                    model::PermissionOptionKind::RejectOnce,
                ),
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
        let model::RequestPermissionOutcome::Selected(selected) = resp.outcome else {
            panic!("expected selected permission response");
        };
        assert_eq!(selected.option_id.clone(), "allow");
        assert!(app.pending_permission_ids.is_empty());
    }

    #[test]
    fn permission_ctrl_a_works_even_when_todo_focus_owns_navigation() {
        let mut app = make_test_app();
        let mut response_rx = attach_pending_permission(
            &mut app,
            "perm-1",
            vec![
                model::PermissionOption::new(
                    "allow-once",
                    "Allow once",
                    model::PermissionOptionKind::AllowOnce,
                ),
                model::PermissionOption::new(
                    "allow-always",
                    "Allow always",
                    model::PermissionOptionKind::AllowAlways,
                ),
                model::PermissionOption::new(
                    "deny",
                    "Deny",
                    model::PermissionOptionKind::RejectOnce,
                ),
            ],
            true,
        );
        push_todo_and_focus(&mut app);

        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
        );

        let resp = response_rx.try_recv().expect("ctrl+a should resolve pending permission");
        let model::RequestPermissionOutcome::Selected(selected) = resp.outcome else {
            panic!("expected selected permission response");
        };
        assert_eq!(selected.option_id.clone(), "allow-always");
        assert!(app.pending_permission_ids.is_empty());
    }

    #[test]
    fn permission_ctrl_n_works_even_when_mention_focus_owns_navigation() {
        let mut app = make_test_app();
        let mut response_rx = attach_pending_permission(
            &mut app,
            "perm-1",
            vec![
                model::PermissionOption::new(
                    "allow",
                    "Allow",
                    model::PermissionOptionKind::AllowOnce,
                ),
                model::PermissionOption::new(
                    "deny",
                    "Deny",
                    model::PermissionOptionKind::RejectOnce,
                ),
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
        let model::RequestPermissionOutcome::Selected(selected) = resp.outcome else {
            panic!("expected selected permission response");
        };
        assert_eq!(selected.option_id.clone(), "deny");
        assert!(app.pending_permission_ids.is_empty());
    }

    #[test]
    fn connecting_state_ctrl_c_with_non_empty_selection_does_not_quit() {
        let mut app = make_test_app();
        app.status = AppStatus::Connecting;
        app.rendered_input_lines = vec!["copy".to_owned()];
        app.selection = Some(crate::app::SelectionState {
            kind: crate::app::SelectionKind::Input,
            start: crate::app::SelectionPoint { row: 0, col: 0 },
            end: crate::app::SelectionPoint { row: 0, col: 4 },
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
    fn connecting_state_allows_navigation_and_help_shortcuts() {
        let mut app = make_test_app();
        app.status = AppStatus::Connecting;
        app.help_view = HelpView::Keys;
        app.viewport.scroll_target = 2;
        assert!(app.show_header);

        // Chat navigation remains available during startup.
        handle_terminal_event(&mut app, Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)));
        assert_eq!(app.viewport.scroll_target, 1);
        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        );
        assert_eq!(app.viewport.scroll_target, 2);

        // Help toggle via "?" remains available.
        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)),
        );
        assert!(app.is_help_active());

        // Help tab navigation still works.
        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)),
        );
        assert_eq!(app.help_view, HelpView::SlashCommands);

        // Global UI navigation shortcuts still work.
        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL)),
        );
        assert!(!app.show_header);
    }

    #[test]
    fn connecting_state_blocks_input_shortcuts_and_tab() {
        let mut app = make_test_app();
        app.status = AppStatus::Connecting;
        app.input.set_text("seed");
        app.pending_submit = false;
        app.help_view = HelpView::Keys;

        for key in [
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('@'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        ] {
            handle_terminal_event(&mut app, Event::Key(key));
        }

        assert_eq!(app.input.text(), "seed");
        assert!(!app.pending_submit);
        assert_eq!(app.help_view, HelpView::Keys);
    }

    #[test]
    fn ctrl_c_with_non_empty_selection_does_not_quit_and_clears_selection() {
        let mut app = make_test_app();
        app.rendered_input_lines = vec!["copy".to_owned()];
        app.selection = Some(crate::app::SelectionState {
            kind: crate::app::SelectionKind::Input,
            start: crate::app::SelectionPoint { row: 0, col: 0 },
            end: crate::app::SelectionPoint { row: 0, col: 4 },
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
    fn ctrl_c_without_selection_quits() {
        let mut app = make_test_app();
        app.selection = None;

        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );

        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_c_second_press_after_copy_quits() {
        let mut app = make_test_app();
        app.rendered_input_lines = vec!["copy".to_owned()];
        app.selection = Some(crate::app::SelectionState {
            kind: crate::app::SelectionKind::Input,
            start: crate::app::SelectionPoint { row: 0, col: 0 },
            end: crate::app::SelectionPoint { row: 0, col: 4 },
            dragging: false,
        });

        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );
        assert!(!app.should_quit);
        assert!(app.selection.is_none());

        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );
        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_c_with_zero_length_selection_quits() {
        let mut app = make_test_app();
        app.rendered_input_lines = vec!["copy".to_owned()];
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
    fn ctrl_c_with_whitespace_selection_copies_and_clears_selection() {
        let mut app = make_test_app();
        app.rendered_input_lines = vec!["   ".to_owned()];
        app.selection = Some(crate::app::SelectionState {
            kind: crate::app::SelectionKind::Input,
            start: crate::app::SelectionPoint { row: 0, col: 0 },
            end: crate::app::SelectionPoint { row: 0, col: 1 },
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
    fn ctrl_q_quits_even_with_selection() {
        let mut app = make_test_app();
        app.selection = Some(crate::app::SelectionState {
            kind: crate::app::SelectionKind::Input,
            start: crate::app::SelectionPoint { row: 0, col: 0 },
            end: crate::app::SelectionPoint { row: 0, col: 0 },
            dragging: false,
        });

        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)),
        );

        assert!(app.should_quit);
    }

    #[test]
    fn connecting_state_ctrl_q_quits() {
        let mut app = make_test_app();
        app.status = AppStatus::Connecting;

        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)),
        );

        assert!(app.should_quit);
    }

    #[test]
    fn error_state_blocks_input_shortcuts() {
        let mut app = make_test_app();
        app.status = AppStatus::Error;
        app.input.set_text("seed");
        app.pending_submit = false;

        for key in [
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('@'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        ] {
            handle_terminal_event(&mut app, Event::Key(key));
        }

        assert_eq!(app.input.text(), "seed");
        assert!(!app.pending_submit);
    }

    #[test]
    fn error_state_ctrl_q_quits() {
        let mut app = make_test_app();
        app.status = AppStatus::Error;

        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)),
        );

        assert!(app.should_quit);
    }

    #[test]
    fn error_state_ctrl_c_quits() {
        let mut app = make_test_app();
        app.status = AppStatus::Error;

        handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );

        assert!(app.should_quit);
    }

    #[test]
    fn error_state_blocks_paste_events() {
        let mut app = make_test_app();
        app.status = AppStatus::Error;

        handle_terminal_event(&mut app, Event::Paste("blocked".into()));

        assert!(app.pending_paste_text.is_empty());
        assert!(app.input.is_empty());
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
    fn mouse_down_on_scrollbar_rail_starts_drag_and_scrolls() {
        let mut app = make_test_app();
        app.rendered_chat_area = Rect::new(0, 0, 20, 10);
        app.viewport.height_prefix_sums = vec![30];
        app.viewport.scrollbar_thumb_top = 0.0;
        app.viewport.scrollbar_thumb_size = 3.0;
        app.selection = Some(crate::app::SelectionState {
            kind: crate::app::SelectionKind::Chat,
            start: crate::app::SelectionPoint { row: 0, col: 0 },
            end: crate::app::SelectionPoint { row: 0, col: 1 },
            dragging: false,
        });

        handle_terminal_event(
            &mut app,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: 19,
                row: 9,
                modifiers: KeyModifiers::NONE,
            }),
        );

        assert!(app.scrollbar_drag.is_some());
        assert!(app.selection.is_none());
        assert!(!app.viewport.auto_scroll);
        assert!(app.viewport.scroll_target > 0);
    }

    #[test]
    fn dragging_scrollbar_thumb_can_reach_bottom_and_top() {
        let mut app = make_test_app();
        app.rendered_chat_area = Rect::new(0, 0, 20, 10);
        app.viewport.height_prefix_sums = vec![30];
        app.viewport.scrollbar_thumb_top = 0.0;
        app.viewport.scrollbar_thumb_size = 3.0;

        handle_terminal_event(
            &mut app,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: 19,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
        );
        handle_terminal_event(
            &mut app,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                column: 19,
                row: 9,
                modifiers: KeyModifiers::NONE,
            }),
        );
        assert_eq!(app.viewport.scroll_target, 20);

        handle_terminal_event(
            &mut app,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                column: 19,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
        );
        assert_eq!(app.viewport.scroll_target, 0);

        handle_terminal_event(
            &mut app,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
                column: 19,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
        );
        assert!(app.scrollbar_drag.is_none());
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

    #[test]
    fn internal_error_detection_accepts_permission_zod_payload() {
        let payload = "Tool permission request failed: ZodError: [{\"message\":\"Invalid input\"}]";
        assert!(looks_like_internal_error(payload));
    }

    #[test]
    fn summarize_internal_error_prefers_permission_failure_summary() {
        let payload = "Tool permission request failed: ZodError: [{\"message\":\"Invalid input: expected record, received undefined\"}]";
        assert_eq!(
            summarize_internal_error(payload),
            "Tool permission request failed: Invalid input: expected record, received undefined"
        );
    }
}
