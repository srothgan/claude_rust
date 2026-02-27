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
    App, AppStatus, BlockCache, CancelOrigin, ChatMessage, ChatViewport, FocusTarget,
    IncrementalMarkdown, MessageBlock, MessageRole, dialog::DialogState,
};
use crate::agent::events::ClientEvent;
use crate::agent::model;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_VISIBLE: usize = 8;
const MAX_CANDIDATES: usize = 50;

#[derive(Debug, Clone)]
pub struct SlashCandidate {
    pub insert_value: String,
    pub primary: String,
    pub secondary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashContext {
    CommandName,
    Argument { command: String, arg_index: usize, token_range: (usize, usize) },
}

#[derive(Debug, Clone)]
pub struct SlashState {
    /// Character position where `/` token starts.
    pub trigger_row: usize,
    pub trigger_col: usize,
    /// Current typed query for the active slash context.
    pub query: String,
    /// Command-name or argument context.
    pub context: SlashContext,
    /// Filtered list of supported candidates.
    pub candidates: Vec<SlashCandidate>,
    /// Shared autocomplete dialog navigation state.
    pub dialog: DialogState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SlashDetection {
    trigger_row: usize,
    trigger_col: usize,
    query: String,
    context: SlashContext,
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

pub fn is_cancel_command(text: &str) -> bool {
    parse(text).is_some_and(|parsed| parsed.name == "/cancel")
}

fn normalize_slash_name(name: &str) -> String {
    if name.starts_with('/') { name.to_owned() } else { format!("/{name}") }
}

fn detect_argument_at_cursor(
    chars: &[char],
    mut idx: usize,
    cursor_col: usize,
) -> Option<(usize, usize, usize)> {
    if cursor_col > chars.len() {
        return None;
    }

    let mut arg_index = 0usize;
    loop {
        while idx < chars.len() && chars[idx].is_whitespace() {
            if cursor_col == idx {
                return Some((arg_index, cursor_col, cursor_col));
            }
            idx += 1;
        }

        if idx >= chars.len() {
            if cursor_col >= idx {
                return Some((arg_index, cursor_col, cursor_col));
            }
            return None;
        }

        let token_start = idx;
        while idx < chars.len() && !chars[idx].is_whitespace() {
            idx += 1;
        }
        let token_end = idx;
        if (token_start..=token_end).contains(&cursor_col) {
            return Some((arg_index, token_start, token_end));
        }
        arg_index += 1;
    }
}

fn detect_slash_at_cursor(
    lines: &[String],
    cursor_row: usize,
    cursor_col: usize,
) -> Option<SlashDetection> {
    let line = lines.get(cursor_row)?;
    let first_non_ws = line.find(|c: char| !c.is_whitespace())?;
    let chars: Vec<char> = line.chars().collect();
    if chars.get(first_non_ws).copied() != Some('/') {
        return None;
    }

    let token_start = first_non_ws;
    let token_end =
        (token_start + 1..chars.len()).find(|&i| chars[i].is_whitespace()).unwrap_or(chars.len());

    if cursor_col <= token_start || cursor_col > chars.len() {
        return None;
    }

    if cursor_col <= token_end {
        let query: String = chars[token_start + 1..cursor_col].iter().collect();
        if query.chars().any(char::is_whitespace) {
            return None;
        }
        return Some(SlashDetection {
            trigger_row: cursor_row,
            trigger_col: token_start,
            query,
            context: SlashContext::CommandName,
        });
    }

    let command: String = chars[token_start..token_end].iter().collect();
    let (arg_index, token_start, token_end) =
        detect_argument_at_cursor(&chars, token_end, cursor_col)?;
    let query: String = chars[token_start..cursor_col.min(token_end)].iter().collect();

    Some(SlashDetection {
        trigger_row: cursor_row,
        trigger_col: token_start,
        query,
        context: SlashContext::Argument {
            command,
            arg_index,
            token_range: (token_start, token_end),
        },
    })
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
        usage: None,
    });
    app.enforce_history_retention();
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
        usage: None,
    });
    app.enforce_history_retention();
    app.viewport.engage_auto_scroll();
}

fn require_connection(
    app: &mut App,
    not_connected_msg: &'static str,
) -> Option<Rc<crate::agent::client::AgentConnection>> {
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
) -> Option<(Rc<crate::agent::client::AgentConnection>, model::SessionId)> {
    let conn = require_connection(app, not_connected_msg)?;
    let Some(session_id) = app.session_id.clone() else {
        push_system_message(app, no_session_msg);
        return None;
    };
    Some((conn, session_id))
}

pub(crate) fn clear_conversation_history(app: &mut App) {
    let _ = app.finalize_in_progress_tool_calls(model::ToolCallStatus::Failed);

    app.status = AppStatus::Ready;
    app.resuming_session_id = None;
    app.files_accessed = 0;
    app.is_compacting = false;
    app.cancelled_turn_pending_hint = false;
    app.pending_cancel_origin = None;

    app.messages.clear();
    app.messages.push(ChatMessage::welcome_with_recent(
        &app.model_name,
        &app.cwd,
        &app.recent_sessions,
    ));
    app.history_retention_stats = super::state::HistoryRetentionStats::default();
    app.enforce_history_retention();
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

fn find_advertised_command<'a>(
    app: &'a App,
    command_name: &str,
) -> Option<&'a model::AvailableCommand> {
    app.available_commands.iter().find(|cmd| normalize_slash_name(&cmd.name) == command_name)
}

fn is_hidden_offer_command(command_name: &str) -> bool {
    matches!(command_name, "/login" | "/logout")
}

fn is_builtin_variable_input_command(command_name: &str) -> bool {
    matches!(command_name, "/mode" | "/model" | "/resume")
}

fn is_variable_input_command(app: &App, command_name: &str) -> bool {
    if is_builtin_variable_input_command(command_name) {
        return true;
    }

    find_advertised_command(app, command_name)
        .and_then(|cmd| cmd.input_hint.as_ref())
        .is_some_and(|hint| !hint.trim().is_empty())
}

fn supported_command_candidates(app: &App) -> Vec<SlashCandidate> {
    use std::collections::BTreeMap;

    let mut by_name: BTreeMap<String, String> = BTreeMap::new();
    by_name.insert("/cancel".into(), "Cancel active turn".into());
    by_name.insert("/compact".into(), "Clear conversation history".into());
    by_name.insert("/mode".into(), "Set session mode".into());
    by_name.insert("/model".into(), "Set session model".into());
    by_name.insert("/new-session".into(), "Start a fresh session".into());
    by_name.insert("/resume".into(), "Resume a session by ID".into());

    for cmd in &app.available_commands {
        let name = normalize_slash_name(&cmd.name);
        if is_hidden_offer_command(&name) {
            continue;
        }
        by_name.entry(name).or_insert_with(|| cmd.description.clone());
    }

    by_name
        .into_iter()
        .map(|(name, description)| SlashCandidate {
            insert_value: name.clone(),
            primary: name,
            secondary: if description.trim().is_empty() { None } else { Some(description) },
        })
        .collect()
}

fn filter_command_candidates(candidates: &[SlashCandidate], query: &str) -> Vec<SlashCandidate> {
    if query.is_empty() {
        return candidates.iter().take(MAX_CANDIDATES).cloned().collect();
    }

    let query_lower = query.to_lowercase();
    candidates
        .iter()
        .filter(|candidate| {
            let body = candidate.primary.strip_prefix('/').unwrap_or(&candidate.primary);
            body.to_lowercase().contains(&query_lower)
        })
        .take(MAX_CANDIDATES)
        .cloned()
        .collect()
}

fn candidate_matches(candidate: &SlashCandidate, query_lower: &str) -> bool {
    candidate.primary.to_lowercase().contains(query_lower)
        || candidate.insert_value.to_lowercase().contains(query_lower)
        || candidate
            .secondary
            .as_ref()
            .is_some_and(|secondary| secondary.to_lowercase().contains(query_lower))
}

fn filter_argument_candidates(candidates: &[SlashCandidate], query: &str) -> Vec<SlashCandidate> {
    if query.is_empty() {
        return candidates.iter().take(MAX_CANDIDATES).cloned().collect();
    }

    let query_lower = query.to_lowercase();
    candidates
        .iter()
        .filter(|candidate| candidate_matches(candidate, &query_lower))
        .take(MAX_CANDIDATES)
        .cloned()
        .collect()
}

fn parse_date_ymd(raw: &str) -> Option<(i32, u32, u32)> {
    if raw.len() != 10 {
        return None;
    }
    let bytes = raw.as_bytes();
    if bytes.get(4).copied() != Some(b'-') || bytes.get(7).copied() != Some(b'-') {
        return None;
    }
    let year: i32 = raw[0..4].parse().ok()?;
    let month: u32 = raw[5..7].parse().ok()?;
    let day: u32 = raw[8..10].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some((year, month, day))
}

fn parse_time_hms(raw: &str) -> Option<(u32, u32, u32)> {
    let time_raw = raw.split('.').next()?;
    if time_raw.len() != 8 {
        return None;
    }
    let bytes = time_raw.as_bytes();
    if bytes.get(2).copied() != Some(b':') || bytes.get(5).copied() != Some(b':') {
        return None;
    }
    let hour: u32 = time_raw[0..2].parse().ok()?;
    let minute: u32 = time_raw[3..5].parse().ok()?;
    let second: u32 = time_raw[6..8].parse().ok()?;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some((hour, minute, second))
}

fn parse_timezone_offset_seconds(raw: &str) -> Option<i64> {
    if raw.eq_ignore_ascii_case("z") {
        return Some(0);
    }
    if raw.len() != 6 || (!raw.starts_with('+') && !raw.starts_with('-')) {
        return None;
    }
    let bytes = raw.as_bytes();
    if bytes.get(3).copied() != Some(b':') {
        return None;
    }
    let hours: i64 = raw[1..3].parse().ok()?;
    let minutes: i64 = raw[4..6].parse().ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    let sign = if raw.starts_with('-') { -1 } else { 1 };
    Some(sign * (hours * 3600 + minutes * 60))
}

fn days_since_unix_epoch(year: i32, month: u32, day: u32) -> Option<i64> {
    let month_i32 = i32::try_from(month).ok()?;
    let day_i32 = i32::try_from(day).ok()?;
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = month_i32 + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day_i32 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(i64::from(era) * 146_097 + i64::from(doe) - 719_468)
}

fn parse_timestamp_epoch_seconds(raw: &str) -> Option<i64> {
    let trimmed = raw.trim();
    let (date_raw, time_and_zone_raw) =
        trimmed.split_once('T').or_else(|| trimmed.split_once(' '))?;
    let (year, month, day) = parse_date_ymd(date_raw)?;

    let tz_split = time_and_zone_raw
        .char_indices()
        .find(|(idx, ch)| *idx >= 5 && matches!(ch, 'Z' | 'z' | '+' | '-'))
        .map(|(idx, _)| idx);
    let (time_raw, tz_raw) = match tz_split {
        Some(idx) => (&time_and_zone_raw[..idx], Some(&time_and_zone_raw[idx..])),
        None => (time_and_zone_raw, None),
    };
    let (hour, minute, second) = parse_time_hms(time_raw)?;
    let tz_offset = tz_raw.map_or(Some(0), parse_timezone_offset_seconds)?;

    let days = days_since_unix_epoch(year, month, day)?;
    let seconds_in_day = i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second);
    days.checked_mul(86_400)?.checked_add(seconds_in_day)?.checked_sub(tz_offset)
}

fn now_epoch_seconds() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}

fn format_relative_age(epoch_seconds: i64) -> String {
    let now_seconds = now_epoch_seconds();
    let delta_seconds = if now_seconds >= epoch_seconds {
        now_seconds - epoch_seconds
    } else {
        epoch_seconds - now_seconds
    };

    if delta_seconds < 5 * 60 {
        return "<5m".to_owned();
    }
    if delta_seconds < 60 * 60 {
        return format!("{}m", delta_seconds / 60);
    }
    if delta_seconds < 24 * 60 * 60 {
        return format!("{}h", delta_seconds / (60 * 60));
    }

    let total_hours = delta_seconds / (60 * 60);
    let days = total_hours / 24;
    let hours = total_hours % 24;
    format!("{days}d {hours}h")
}

fn session_age_label(updated_at: Option<&str>) -> String {
    let Some(raw) = updated_at else {
        return "--".to_owned();
    };
    let Some(epoch) = parse_timestamp_epoch_seconds(raw) else {
        return "--".to_owned();
    };
    format_relative_age(epoch)
}

/// Source: <https://code.claude.com/docs/en/model-config>
/// Section: Model aliases
const CLAUDE_CODE_MODEL_CANDIDATES: &[(&str, &str)] = &[
    ("default", "Recommended model setting based on account"),
    ("sonnet", "Latest Sonnet model for daily coding tasks"),
    ("opus", "Latest Opus model for complex reasoning tasks"),
    ("haiku", "Fast and efficient model for simple tasks"),
    ("sonnet[1m]", "Sonnet with 1 million token context window"),
    ("opusplan", "Opus during plan mode, Sonnet during execution"),
];

fn argument_candidates(app: &App, command_name: &str, arg_index: usize) -> Vec<SlashCandidate> {
    if arg_index > 0 {
        return Vec::new();
    }

    match command_name {
        "/resume" => app
            .recent_sessions
            .iter()
            .map(|session| {
                let title = session.title.as_deref().map_or("", str::trim);
                let title = if title.is_empty() { "(no message)" } else { title };
                let age = session_age_label(session.updated_at.as_deref());
                SlashCandidate {
                    insert_value: session.session_id.clone(),
                    primary: format!("{age} - {title}"),
                    secondary: Some(session.session_id.clone()),
                }
            })
            .collect(),
        "/mode" => app
            .mode
            .as_ref()
            .map(|mode| {
                mode.available_modes
                    .iter()
                    .map(|entry| SlashCandidate {
                        insert_value: entry.id.clone(),
                        primary: entry.name.clone(),
                        secondary: Some(entry.id.clone()),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        "/model" => CLAUDE_CODE_MODEL_CANDIDATES
            .iter()
            .map(|(name, label)| SlashCandidate {
                insert_value: (*name).to_owned(),
                primary: (*name).to_owned(),
                secondary: Some((*label).to_owned()),
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn build_slash_state(app: &App) -> Option<SlashState> {
    let detection =
        detect_slash_at_cursor(&app.input.lines, app.input.cursor_row, app.input.cursor_col)?;

    let candidates = match &detection.context {
        SlashContext::CommandName => {
            filter_command_candidates(&supported_command_candidates(app), &detection.query)
        }
        SlashContext::Argument { command, arg_index, .. } => {
            if !is_variable_input_command(app, command) {
                return None;
            }
            filter_argument_candidates(
                &argument_candidates(app, command, *arg_index),
                &detection.query,
            )
        }
    };
    if candidates.is_empty() {
        return None;
    }

    Some(SlashState {
        trigger_row: detection.trigger_row,
        trigger_col: detection.trigger_col,
        query: detection.query,
        context: detection.context,
        candidates,
        dialog: DialogState::default(),
    })
}

pub fn is_supported_command(app: &App, command_name: &str) -> bool {
    matches!(command_name, "/cancel" | "/compact" | "/mode" | "/model" | "/new-session" | "/resume")
        || advertised_commands(app).iter().any(|c| c == command_name)
}

pub fn activate(app: &mut App) {
    let Some(state) = build_slash_state(app) else {
        return;
    };

    app.slash = Some(state);
    app.mention = None;
    app.claim_focus_target(FocusTarget::Mention);
}

pub fn update_query(app: &mut App) {
    let Some(next_state) = build_slash_state(app) else {
        deactivate(app);
        return;
    };

    if let Some(ref mut slash) = app.slash {
        let keep_selection = slash.context == next_state.context;
        let dialog = if keep_selection { slash.dialog } else { DialogState::default() };
        slash.trigger_row = next_state.trigger_row;
        slash.trigger_col = next_state.trigger_col;
        slash.query = next_state.query;
        slash.context = next_state.context;
        slash.candidates = next_state.candidates;
        slash.dialog = dialog;
        slash.dialog.clamp(slash.candidates.len(), MAX_VISIBLE);
    } else {
        app.slash = Some(next_state);
        app.claim_focus_target(FocusTarget::Mention);
    }
}

pub fn sync_with_cursor(app: &mut App) {
    match (build_slash_state(app), app.slash.is_some()) {
        (Some(_), true) => update_query(app),
        (Some(_), false) => activate(app),
        (None, true) => deactivate(app),
        (None, false) => {}
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

/// Confirm selected candidate in input.
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
    let (replace_start, replace_end) = match slash.context {
        SlashContext::CommandName => {
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
            (slash.trigger_col, token_end)
        }
        SlashContext::Argument { token_range, .. } => {
            let (start, end) = token_range;
            if start > end || end > chars.len() {
                tracing::debug!(
                    start,
                    end,
                    line_len = chars.len(),
                    "Slash confirm aborted: invalid argument token range"
                );
                if app.mention.is_none() {
                    app.release_focus_target(FocusTarget::Mention);
                }
                return;
            }
            (start, end)
        }
    };

    let before: String = chars[..replace_start].iter().collect();
    let after: String = chars[replace_end..].iter().collect();
    let replacement = if after.is_empty() {
        format!("{} ", candidate.insert_value)
    } else {
        candidate.insert_value.clone()
    };
    let new_line = format!("{before}{replacement}{after}");
    let new_cursor_col = replace_start + replacement.chars().count();
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

    sync_with_cursor(app);
    if app.slash.is_none() && app.mention.is_none() {
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
            if !matches!(app.status, AppStatus::Thinking | AppStatus::Running) {
                push_system_message(app, "Cannot cancel: no active turn.");
                return true;
            }
            if let Err(message) = super::input_submit::request_cancel(app, CancelOrigin::Manual) {
                push_system_message(app, format!("Failed to run /cancel: {message}"));
            }
            true
        }
        "/compact" => {
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

            // Forward `/compact` through the bridge via the normal prompt path, then clear
            // local history once the turn completes.
            app.pending_compact_clear = true;
            app.is_compacting = true;
            false
        }
        "/mode" => {
            let [requested_mode_arg] = parsed.args.as_slice() else {
                push_system_message(app, "Usage: /mode <id>");
                return true;
            };
            let requested_mode = *requested_mode_arg;
            let requested_mode_owned = requested_mode.to_owned();

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
            tokio::task::spawn_local(async move {
                if let Err(e) = conn.set_mode(sid.to_string(), requested_mode_owned) {
                    let _ = tx
                        .send(ClientEvent::SlashCommandError(format!("Failed to run /mode: {e}")));
                }
            });
            true
        }
        "/model" => {
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
                if let Err(e) = conn.set_model(sid.to_string(), model_name) {
                    let _ = tx
                        .send(ClientEvent::SlashCommandError(format!("Failed to run /model: {e}")));
                }
            });
            true
        }
        "/new-session" => {
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
            let cwd = app.cwd_raw.clone();
            let model_override = Some(app.model_name.clone());
            let yolo = false;
            tokio::task::spawn_local(async move {
                if let Err(e) = conn.new_session(cwd, yolo, model_override) {
                    let _ = tx.send(ClientEvent::SlashCommandError(format!(
                        "Failed to run /new-session: {e}"
                    )));
                }
            });
            true
        }
        "/resume" => {
            let [session_id_arg] = parsed.args.as_slice() else {
                push_system_message(app, "Usage: /resume <session_id>");
                return true;
            };
            let session_id = (*session_id_arg).trim();
            if session_id.is_empty() {
                push_system_message(app, "Usage: /resume <session_id>");
                return true;
            }

            push_user_message(app, format!("/resume {session_id}"));

            let Some(conn) = require_connection(app, "Cannot resume session: not connected yet.")
            else {
                return true;
            };
            app.status = AppStatus::Resuming;
            app.resuming_session_id = Some(session_id.to_owned());
            let tx = app.event_tx.clone();
            let session_id = session_id.to_owned();
            tokio::task::spawn_local(async move {
                if let Err(e) = conn.load_session(session_id) {
                    let _ = tx.send(ClientEvent::SlashCommandError(format!(
                        "Failed to run /resume: {e}"
                    )));
                }
            });
            true
        }
        _ => {
            if is_supported_command(app, parsed.name) {
                // Adapter-advertised slash command: let normal prompt path send it.
                false
            } else {
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
        app.available_commands = vec![model::AvailableCommand::new("/help", "Help")];
        let consumed = try_handle_submit(&mut app, "/help");
        assert!(!consumed);
    }

    #[test]
    fn login_logout_are_hidden_from_candidates() {
        let mut app = App::test_default();
        app.available_commands = vec![
            model::AvailableCommand::new("/login", "Login"),
            model::AvailableCommand::new("/logout", "Logout"),
            model::AvailableCommand::new("/help", "Help"),
        ];

        let names: Vec<String> =
            supported_command_candidates(&app).into_iter().map(|c| c.primary).collect();
        assert!(names.iter().any(|n| n == "/help"));
        assert!(!names.iter().any(|n| n == "/login"));
        assert!(!names.iter().any(|n| n == "/logout"));
    }

    #[test]
    fn detect_slash_argument_context_after_first_space() {
        let lines = vec!["/mode pla".to_owned()];
        let detection = detect_slash_at_cursor(&lines, 0, "/mode pla".chars().count())
            .expect("slash detection");

        match detection.context {
            SlashContext::Argument { command, arg_index, token_range } => {
                assert_eq!(command, "/mode");
                assert_eq!(arg_index, 0);
                assert_eq!(token_range, (6, 9));
            }
            SlashContext::CommandName => panic!("expected argument context"),
        }
        assert_eq!(detection.query, "pla");
    }

    #[test]
    fn mode_argument_candidates_are_dynamic() {
        let mut app = App::test_default();
        app.mode = Some(super::super::ModeState {
            current_mode_id: "plan".to_owned(),
            current_mode_name: "Plan".to_owned(),
            available_modes: vec![
                super::super::ModeInfo { id: "plan".to_owned(), name: "Plan".to_owned() },
                super::super::ModeInfo { id: "code".to_owned(), name: "Code".to_owned() },
            ],
        });

        let candidates = argument_candidates(&app, "/mode", 0);
        assert!(candidates.iter().any(|c| c.insert_value == "plan"));
        assert!(candidates.iter().any(|c| c.insert_value == "code"));
        assert!(candidates.iter().any(|c| c.primary == "Plan"));
        assert!(candidates.iter().any(|c| c.secondary.as_deref() == Some("plan")));
    }

    #[test]
    fn model_argument_candidates_include_aliases() {
        let app = App::test_default();
        let candidates = argument_candidates(&app, "/model", 0);
        for alias in ["default", "sonnet", "opus", "haiku", "sonnet[1m]", "opusplan"] {
            assert!(candidates.iter().any(|c| c.insert_value == alias), "missing alias {alias}");
        }
    }

    #[test]
    fn model_argument_candidates_do_not_include_retired_37_alias() {
        let app = App::test_default();
        let candidates = argument_candidates(&app, "/model", 0);
        assert!(!candidates.iter().any(|c| c.insert_value.contains("3-7")));
    }

    #[test]
    fn non_variable_command_argument_mode_is_disabled() {
        let mut app = App::test_default();
        app.input.set_text("/cancel now");
        app.input.cursor_row = 0;
        app.input.cursor_col = "/cancel now".chars().count();
        sync_with_cursor(&mut app);
        assert!(app.slash.is_none());
    }

    #[test]
    fn variable_command_argument_mode_deactivates_when_no_match() {
        let mut app = App::test_default();
        app.mode = Some(super::super::ModeState {
            current_mode_id: "plan".to_owned(),
            current_mode_name: "Plan".to_owned(),
            available_modes: vec![super::super::ModeInfo {
                id: "plan".to_owned(),
                name: "Plan".to_owned(),
            }],
        });
        app.input.set_text("/mode xyz");
        app.input.cursor_row = 0;
        app.input.cursor_col = "/mode xyz".chars().count();
        sync_with_cursor(&mut app);
        assert!(app.slash.is_none());
    }

    #[test]
    fn confirm_selection_replaces_only_active_argument_token() {
        let mut app = App::test_default();
        app.input.set_text("/resume old-id trailing");
        app.input.cursor_row = 0;
        app.input.cursor_col = "/resume old-id".chars().count();
        app.slash = Some(SlashState {
            trigger_row: 0,
            trigger_col: 8,
            query: "old-id".to_owned(),
            context: SlashContext::Argument {
                command: "/resume".to_owned(),
                arg_index: 0,
                token_range: (8, 14),
            },
            candidates: vec![SlashCandidate {
                insert_value: "new-id".to_owned(),
                primary: "New".to_owned(),
                secondary: None,
            }],
            dialog: DialogState::default(),
        });

        confirm_selection(&mut app);

        assert_eq!(app.input.text(), "/resume new-id trailing");
    }

    #[test]
    fn typed_login_is_still_forwarded_when_advertised() {
        let mut app = App::test_default();
        app.available_commands = vec![model::AvailableCommand::new("/login", "Login")];

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
    fn resume_with_missing_id_returns_usage() {
        let mut app = App::test_default();
        let consumed = try_handle_submit(&mut app, "/resume");
        assert!(consumed);
        let Some(last) = app.messages.last() else {
            panic!("expected usage message");
        };
        let Some(MessageBlock::Text(text, _, _)) = last.blocks.first() else {
            panic!("expected text block");
        };
        assert_eq!(text, "Usage: /resume <session_id>");
    }

    #[test]
    fn resume_command_is_rendered_as_user_message() {
        let mut app = App::test_default();

        let consumed = try_handle_submit(&mut app, "/resume abc-123");
        assert!(consumed);
        assert!(app.messages.len() >= 2);

        let Some(first) = app.messages.first() else {
            panic!("expected user message");
        };
        assert!(matches!(first.role, MessageRole::User));
        let Some(MessageBlock::Text(text, _, _)) = first.blocks.first() else {
            panic!("expected text block");
        };
        assert_eq!(text, "/resume abc-123");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resume_sets_resuming_state_when_connected() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut app = App::test_default();
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                app.conn = Some(std::rc::Rc::new(crate::agent::client::AgentConnection::new(tx)));

                let consumed = try_handle_submit(&mut app, "/resume abc-123");
                assert!(consumed);
                assert!(matches!(app.status, AppStatus::Resuming));
                assert_eq!(app.resuming_session_id.as_deref(), Some("abc-123"));

                tokio::task::yield_now().await;
                assert!(rx.try_recv().is_ok());
            })
            .await;
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
    fn compact_with_active_session_sets_pending_and_compacting() {
        let mut app = App::test_default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        app.conn = Some(std::rc::Rc::new(crate::agent::client::AgentConnection::new(tx)));
        app.session_id = Some(model::SessionId::new("session-1"));

        let consumed = try_handle_submit(&mut app, "/compact");
        assert!(!consumed);
        assert!(app.pending_compact_clear);
        assert!(app.is_compacting);
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
            usage: None,
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
            context: SlashContext::CommandName,
            candidates: vec![SlashCandidate {
                insert_value: "/mode".into(),
                primary: "/mode".into(),
                secondary: None,
            }],
            dialog: DialogState::default(),
        });

        confirm_selection(&mut app);

        assert_eq!(app.input.text(), "/mode");
    }
}
