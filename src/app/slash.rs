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
    App, AppStatus, BlockCache, ChatMessage, ChatViewport, FocusTarget, IncrementalMarkdown,
    MessageBlock, MessageRole, dialog::DialogState,
};
use crate::acp::client::ClientEvent;
use crate::app::{ModeInfo, ModeState};
use agent_client_protocol::{self as acp, Agent as _};
use std::rc::Rc;

pub const MAX_VISIBLE: usize = 8;
const MAX_CANDIDATES: usize = 50;

#[derive(Debug, Clone)]
pub struct SlashCandidate {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct SlashState {
    /// Character position where `/` token starts.
    pub trigger_row: usize,
    pub trigger_col: usize,
    /// Current typed query after `/`.
    pub query: String,
    /// Filtered list of supported commands.
    pub candidates: Vec<SlashCandidate>,
    /// Shared autocomplete dialog navigation state.
    pub dialog: DialogState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedSlash<'a> {
    name: &'a str,
    args: Vec<&'a str>,
}

fn parse(text: &str) -> Option<ParsedSlash<'_>> {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    let mut parts = trimmed.split_whitespace();
    let name = parts.next()?;
    Some(ParsedSlash { name, args: parts.collect() })
}

fn normalize_slash_name(name: &str) -> String {
    if name.starts_with('/') { name.to_owned() } else { format!("/{name}") }
}

fn detect_slash_at_cursor(
    lines: &[String],
    cursor_row: usize,
    cursor_col: usize,
) -> Option<(usize, usize, String)> {
    let line = lines.get(cursor_row)?;
    let first_non_ws = line.find(|c: char| !c.is_whitespace())?;
    let chars: Vec<char> = line.chars().collect();
    if chars.get(first_non_ws).copied() != Some('/') {
        return None;
    }

    let token_start = first_non_ws;
    let token_end =
        (token_start + 1..chars.len()).find(|&i| chars[i].is_whitespace()).unwrap_or(chars.len());

    if cursor_col <= token_start || cursor_col > token_end {
        return None;
    }

    let query: String = chars[token_start + 1..cursor_col].iter().collect();
    if query.chars().any(char::is_whitespace) {
        return None;
    }

    Some((cursor_row, token_start, query))
}

fn push_system_message(app: &mut App, text: impl Into<String>) {
    let text = text.into();
    app.messages.push(ChatMessage {
        role: MessageRole::System,
        blocks: vec![MessageBlock::Text(
            text.clone(),
            BlockCache::default(),
            IncrementalMarkdown::from_complete(&text),
        )],
    });
    app.viewport.engage_auto_scroll();
}

fn push_user_message(app: &mut App, text: impl Into<String>) {
    let text = text.into();
    app.messages.push(ChatMessage {
        role: MessageRole::User,
        blocks: vec![MessageBlock::Text(
            text.clone(),
            BlockCache::default(),
            IncrementalMarkdown::from_complete(&text),
        )],
    });
    app.viewport.engage_auto_scroll();
}

fn require_connection(
    app: &mut App,
    not_connected_msg: &'static str,
) -> Option<Rc<acp::ClientSideConnection>> {
    let Some(conn) = app.conn.as_ref() else {
        push_system_message(app, not_connected_msg);
        return None;
    };
    Some(Rc::clone(conn))
}

fn require_active_session(
    app: &mut App,
    not_connected_msg: &'static str,
    no_session_msg: &'static str,
) -> Option<(Rc<acp::ClientSideConnection>, acp::SessionId)> {
    let conn = require_connection(app, not_connected_msg)?;
    let Some(session_id) = app.session_id.clone() else {
        push_system_message(app, no_session_msg);
        return None;
    };
    Some((conn, session_id))
}

pub(crate) fn clear_conversation_history(app: &mut App) {
    let _ = app.finalize_in_progress_tool_calls(acp::ToolCallStatus::Failed);

    app.status = AppStatus::Ready;
    app.files_accessed = 0;
    app.cancelled_turn_pending_hint = false;

    app.messages.clear();
    app.messages.push(ChatMessage::welcome(&app.model_name, &app.cwd));
    app.viewport = ChatViewport::new();

    app.tool_call_index.clear();
    app.active_task_ids.clear();
    app.todos.clear();
    app.show_todo_panel = false;
    app.todo_scroll = 0;
    app.todo_selected = 0;
    app.cached_todo_compact = None;

    app.selection = None;
    app.rendered_chat_lines.clear();
    app.rendered_input_lines.clear();
    app.mention = None;
    app.slash = None;
    app.pending_submit = false;
    app.drain_key_count = 0;
    app.pending_paste_text.clear();
    app.normalize_focus_stack();
}

fn advertised_commands(app: &App) -> Vec<String> {
    app.available_commands.iter().map(|cmd| normalize_slash_name(&cmd.name)).collect()
}

fn is_hidden_offer_command(command_name: &str) -> bool {
    matches!(command_name, "/login" | "/logout")
}

fn supported_candidates(app: &App) -> Vec<SlashCandidate> {
    use std::collections::BTreeMap;

    let mut by_name: BTreeMap<String, String> = BTreeMap::new();
    by_name.insert("/cancel".into(), "Cancel active turn".into());
    by_name.insert("/compact".into(), "Clear conversation history".into());
    by_name.insert("/mode".into(), "Set session mode".into());
    by_name.insert("/model".into(), "Set session model".into());
    by_name.insert("/new-session".into(), "Start a fresh session".into());

    for cmd in &app.available_commands {
        let name = normalize_slash_name(&cmd.name);
        if is_hidden_offer_command(&name) {
            continue;
        }
        by_name.entry(name).or_insert_with(|| cmd.description.clone());
    }

    by_name.into_iter().map(|(name, description)| SlashCandidate { name, description }).collect()
}

fn filter_candidates(candidates: &[SlashCandidate], query: &str) -> Vec<SlashCandidate> {
    if query.is_empty() {
        return candidates.iter().take(MAX_CANDIDATES).cloned().collect();
    }
    let query_lower = query.to_lowercase();
    candidates
        .iter()
        .filter(|c| c.name[1..].to_lowercase().contains(&query_lower))
        .take(MAX_CANDIDATES)
        .cloned()
        .collect()
}

pub fn is_supported_command(app: &App, command_name: &str) -> bool {
    matches!(command_name, "/cancel" | "/compact" | "/mode" | "/model" | "/new-session")
        || advertised_commands(app).iter().any(|c| c == command_name)
}

pub fn activate(app: &mut App) {
    let detection =
        detect_slash_at_cursor(&app.input.lines, app.input.cursor_row, app.input.cursor_col);
    let Some((trigger_row, trigger_col, query)) = detection else {
        return;
    };

    let candidates = filter_candidates(&supported_candidates(app), &query);
    app.slash = Some(SlashState {
        trigger_row,
        trigger_col,
        query,
        candidates,
        dialog: DialogState::default(),
    });
    app.mention = None;
    app.claim_focus_target(FocusTarget::Mention);
}

pub fn update_query(app: &mut App) {
    let detection =
        detect_slash_at_cursor(&app.input.lines, app.input.cursor_row, app.input.cursor_col);
    let Some((trigger_row, trigger_col, query)) = detection else {
        deactivate(app);
        return;
    };

    let candidates = filter_candidates(&supported_candidates(app), &query);
    if let Some(ref mut slash) = app.slash {
        slash.trigger_row = trigger_row;
        slash.trigger_col = trigger_col;
        slash.query = query;
        slash.candidates = candidates;
        slash.dialog.clamp(slash.candidates.len(), MAX_VISIBLE);
    }
}

pub fn sync_with_cursor(app: &mut App) {
    let in_slash =
        detect_slash_at_cursor(&app.input.lines, app.input.cursor_row, app.input.cursor_col)
            .is_some();
    match (in_slash, app.slash.is_some()) {
        (true, true) => update_query(app),
        (true, false) => activate(app),
        (false, true) => deactivate(app),
        (false, false) => {}
    }
}

pub fn deactivate(app: &mut App) {
    app.slash = None;
    if app.mention.is_none() {
        app.release_focus_target(FocusTarget::Mention);
    }
}

pub fn move_up(app: &mut App) {
    if let Some(ref mut slash) = app.slash {
        slash.dialog.move_up(slash.candidates.len(), MAX_VISIBLE);
    }
}

pub fn move_down(app: &mut App) {
    if let Some(ref mut slash) = app.slash {
        slash.dialog.move_down(slash.candidates.len(), MAX_VISIBLE);
    }
}

/// Confirm selected command in input, replacing the current slash token.
pub fn confirm_selection(app: &mut App) {
    let Some(slash) = app.slash.take() else {
        return;
    };

    let Some(candidate) = slash.candidates.get(slash.dialog.selected) else {
        if app.mention.is_none() {
            app.release_focus_target(FocusTarget::Mention);
        }
        return;
    };

    let Some(line) = app.input.lines.get_mut(slash.trigger_row) else {
        tracing::debug!(
            trigger_row = slash.trigger_row,
            line_count = app.input.lines.len(),
            "Slash confirm aborted: trigger row out of bounds"
        );
        if app.mention.is_none() {
            app.release_focus_target(FocusTarget::Mention);
        }
        return;
    };

    let chars: Vec<char> = line.chars().collect();
    if slash.trigger_col >= chars.len() {
        tracing::debug!(
            trigger_col = slash.trigger_col,
            line_len = chars.len(),
            "Slash confirm aborted: trigger column out of bounds"
        );
        if app.mention.is_none() {
            app.release_focus_target(FocusTarget::Mention);
        }
        return;
    }
    if chars[slash.trigger_col] != '/' {
        tracing::debug!(
            trigger_col = slash.trigger_col,
            found = ?chars[slash.trigger_col],
            "Slash confirm aborted: trigger column is not slash"
        );
        if app.mention.is_none() {
            app.release_focus_target(FocusTarget::Mention);
        }
        return;
    }

    let token_end = (slash.trigger_col + 1..chars.len())
        .find(|&i| chars[i].is_whitespace())
        .unwrap_or(chars.len());
    let before: String = chars[..slash.trigger_col].iter().collect();
    let after: String = chars[token_end..].iter().collect();
    let replacement =
        if after.is_empty() { format!("{} ", candidate.name) } else { candidate.name.clone() };
    let new_line = format!("{before}{replacement}{after}");
    let new_cursor_col = slash.trigger_col + replacement.chars().count();
    let new_line_len = new_line.chars().count();
    if new_cursor_col > new_line_len {
        tracing::warn!(
            cursor_col = new_cursor_col,
            line_len = new_line_len,
            "Slash confirm produced cursor beyond line length; clamping"
        );
    }
    *line = new_line;
    app.input.cursor_col = new_cursor_col.min(new_line_len);
    app.input.version += 1;
    app.input.sync_textarea_engine();

    if app.mention.is_none() {
        app.release_focus_target(FocusTarget::Mention);
    }
}

/// Handle slash command submission.
///
/// Returns `true` if the slash input was fully handled and should not be sent as a prompt.
/// Returns `false` when the input should continue through the normal prompt path.
#[allow(clippy::too_many_lines)]
pub fn try_handle_submit(app: &mut App, text: &str) -> bool {
    let Some(parsed) = parse(text) else {
        return false;
    };

    match parsed.name {
        "/cancel" => {
            app.input.clear();
            let Some((conn, sid)) = require_active_session(
                app,
                "Cannot cancel: not connected yet.",
                "Cannot cancel: no active session.",
            ) else {
                return true;
            };

            let tx = app.event_tx.clone();
            tokio::task::spawn_local(async move {
                if let Err(e) = conn.cancel(acp::CancelNotification::new(sid)).await {
                    let _ = tx.send(ClientEvent::SlashCommandError(format!(
                        "Failed to run /cancel: {e}"
                    )));
                } else {
                    let _ = tx.send(ClientEvent::TurnCancelled);
                }
            });
            app.status = AppStatus::Ready;
            true
        }
        "/compact" => {
            app.input.clear();
            if !parsed.args.is_empty() {
                push_system_message(app, "Usage: /compact");
                return true;
            }

            if require_active_session(
                app,
                "Cannot compact: not connected yet.",
                "Cannot compact: no active session.",
            )
            .is_none()
            {
                return true;
            }

            // Forward `/compact` to ACP/Zed via the normal prompt path, then clear
            // local history once the turn completes.
            app.pending_compact_clear = true;
            false
        }
        "/mode" => {
            app.input.clear();
            let [requested_mode_arg] = parsed.args.as_slice() else {
                push_system_message(app, "Usage: /mode <id>");
                return true;
            };
            let requested_mode = *requested_mode_arg;

            let Some((conn, sid)) = require_active_session(
                app,
                "Cannot switch mode: not connected yet.",
                "Cannot switch mode: no active session.",
            ) else {
                return true;
            };

            if let Some(ref mode) = app.mode
                && !mode.available_modes.iter().any(|m| m.id == requested_mode)
            {
                push_system_message(app, format!("Unknown mode: {requested_mode}"));
                return true;
            }

            if let Some(ref mut mode_state) = app.mode
                && let Some(info) =
                    mode_state.available_modes.iter().find(|m| m.id == requested_mode)
            {
                mode_state.current_mode_id = info.id.clone();
                mode_state.current_mode_name = info.name.clone();
                app.cached_footer_line = None;
            }

            let tx = app.event_tx.clone();
            let mode_id = acp::SessionModeId::new(requested_mode);
            tokio::task::spawn_local(async move {
                if let Err(e) =
                    conn.set_session_mode(acp::SetSessionModeRequest::new(sid, mode_id)).await
                {
                    let _ = tx
                        .send(ClientEvent::SlashCommandError(format!("Failed to run /mode: {e}")));
                }
            });
            true
        }
        "/model" => {
            app.input.clear();
            let model_name = parsed.args.join(" ");
            if model_name.trim().is_empty() {
                push_system_message(app, "Usage: /model <name>");
                return true;
            }

            let Some((conn, sid)) = require_active_session(
                app,
                "Cannot switch model: not connected yet.",
                "Cannot switch model: no active session.",
            ) else {
                return true;
            };

            app.model_name.clone_from(&model_name);
            app.cached_header_line = None;

            let tx = app.event_tx.clone();
            tokio::task::spawn_local(async move {
                if let Err(e) = conn
                    .set_session_model(acp::SetSessionModelRequest::new(
                        sid,
                        acp::ModelId::new(model_name.as_str()),
                    ))
                    .await
                {
                    let _ = tx
                        .send(ClientEvent::SlashCommandError(format!("Failed to run /model: {e}")));
                }
            });
            true
        }
        "/new-session" => {
            app.input.clear();
            if !parsed.args.is_empty() {
                push_system_message(app, "Usage: /new-session");
                return true;
            }

            push_user_message(app, "/new-session");

            let Some(conn) =
                require_connection(app, "Cannot create new session: not connected yet.")
            else {
                return true;
            };
            let tx = app.event_tx.clone();
            let cwd = std::path::PathBuf::from(&app.cwd_raw);
            tokio::task::spawn_local(async move {
                match conn.new_session(acp::NewSessionRequest::new(&cwd)).await {
                    Ok(resp) => {
                        let model_name = resp
                            .models
                            .as_ref()
                            .and_then(|m| {
                                m.available_models
                                    .iter()
                                    .find(|info| info.model_id == m.current_model_id)
                                    .map(|info| info.name.clone())
                            })
                            .unwrap_or_else(|| "Unknown model".to_owned());
                        let mode = resp.modes.map(|ms| {
                            let current_id = ms.current_mode_id.to_string();
                            let available: Vec<ModeInfo> = ms
                                .available_modes
                                .iter()
                                .map(|m| ModeInfo { id: m.id.to_string(), name: m.name.clone() })
                                .collect();
                            let current_name = available
                                .iter()
                                .find(|m| m.id == current_id)
                                .map_or_else(|| current_id.clone(), |m| m.name.clone());
                            ModeState {
                                current_mode_id: current_id,
                                current_mode_name: current_name,
                                available_modes: available,
                            }
                        });
                        let _ = tx.send(ClientEvent::SessionReplaced {
                            session_id: resp.session_id,
                            model_name,
                            mode,
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(ClientEvent::SlashCommandError(format!(
                            "Failed to run /new-session: {e}"
                        )));
                    }
                }
            });
            true
        }
        _ => {
            if is_supported_command(app, parsed.name) {
                // Adapter-advertised slash command: let normal prompt path send it.
                false
            } else {
                app.input.clear();
                push_system_message(app, format!("{} is not yet supported", parsed.name));
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;

    #[test]
    fn parse_non_slash_returns_none() {
        assert!(parse("hello world").is_none());
    }

    #[test]
    fn parse_slash_name_and_args() {
        let parsed = parse("/mode plan").expect("slash command");
        assert_eq!(parsed.name, "/mode");
        assert_eq!(parsed.args, vec!["plan"]);
    }

    #[test]
    fn unsupported_command_is_handled_locally() {
        let mut app = App::test_default();
        let consumed = try_handle_submit(&mut app, "/definitely-unknown");
        assert!(consumed);
        let Some(last) = app.messages.last() else {
            panic!("expected system message");
        };
        assert!(matches!(last.role, MessageRole::System));
    }

    #[test]
    fn advertised_command_is_forwarded() {
        let mut app = App::test_default();
        app.available_commands = vec![acp::AvailableCommand::new("/help", "Help")];
        let consumed = try_handle_submit(&mut app, "/help");
        assert!(!consumed);
    }

    #[test]
    fn login_logout_are_hidden_from_candidates() {
        let mut app = App::test_default();
        app.available_commands = vec![
            acp::AvailableCommand::new("/login", "Login"),
            acp::AvailableCommand::new("/logout", "Logout"),
            acp::AvailableCommand::new("/help", "Help"),
        ];

        let names: Vec<String> = supported_candidates(&app).into_iter().map(|c| c.name).collect();
        assert!(names.iter().any(|n| n == "/help"));
        assert!(!names.iter().any(|n| n == "/login"));
        assert!(!names.iter().any(|n| n == "/logout"));
    }

    #[test]
    fn typed_login_is_still_forwarded_when_advertised() {
        let mut app = App::test_default();
        app.available_commands = vec![acp::AvailableCommand::new("/login", "Login")];

        let consumed = try_handle_submit(&mut app, "/login");
        assert!(!consumed);
    }

    #[test]
    fn new_session_command_is_rendered_as_user_message() {
        let mut app = App::test_default();

        let consumed = try_handle_submit(&mut app, "/new-session");
        assert!(consumed);
        assert!(app.messages.len() >= 2);

        let Some(first) = app.messages.first() else {
            panic!("expected first message");
        };
        assert!(matches!(first.role, MessageRole::User));
        let Some(MessageBlock::Text(text, _, _)) = first.blocks.first() else {
            panic!("expected user text block");
        };
        assert_eq!(text, "/new-session");
    }

    #[test]
    fn compact_without_connection_is_handled_locally() {
        let mut app = App::test_default();

        let consumed = try_handle_submit(&mut app, "/compact");
        assert!(consumed);
        assert!(!app.pending_compact_clear);
        let Some(last) = app.messages.last() else {
            panic!("expected system message");
        };
        assert!(matches!(last.role, MessageRole::System));
        let Some(MessageBlock::Text(text, _, _)) = last.blocks.first() else {
            panic!("expected text block");
        };
        assert_eq!(text, "Cannot compact: not connected yet.");
    }

    #[test]
    fn compact_with_args_returns_usage_message() {
        let mut app = App::test_default();
        app.messages.push(ChatMessage {
            role: MessageRole::User,
            blocks: vec![MessageBlock::Text(
                "keep".into(),
                BlockCache::default(),
                IncrementalMarkdown::from_complete("keep"),
            )],
        });

        let consumed = try_handle_submit(&mut app, "/compact now");
        assert!(consumed);
        assert!(app.messages.len() >= 2);
        let Some(last) = app.messages.last() else {
            panic!("expected system usage message");
        };
        assert!(matches!(last.role, MessageRole::System));
        let Some(MessageBlock::Text(text, _, _)) = last.blocks.first() else {
            panic!("expected text block");
        };
        assert_eq!(text, "Usage: /compact");
    }

    #[test]
    fn mode_with_extra_args_returns_usage_message() {
        let mut app = App::test_default();

        let consumed = try_handle_submit(&mut app, "/mode plan extra");
        assert!(consumed);
        let Some(last) = app.messages.last() else {
            panic!("expected system usage message");
        };
        assert!(matches!(last.role, MessageRole::System));
        let Some(MessageBlock::Text(text, _, _)) = last.blocks.first() else {
            panic!("expected text block");
        };
        assert_eq!(text, "Usage: /mode <id>");
    }

    #[test]
    fn confirm_selection_with_invalid_trigger_row_is_noop() {
        let mut app = App::test_default();
        app.input.set_text("/mode");
        app.slash = Some(SlashState {
            trigger_row: 99,
            trigger_col: 0,
            query: "m".into(),
            candidates: vec![SlashCandidate { name: "/mode".into(), description: String::new() }],
            dialog: DialogState::default(),
        });

        confirm_selection(&mut app);

        assert_eq!(app.input.text(), "/mode");
    }
}
