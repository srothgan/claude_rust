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

use crate::agent::events::ClientEvent;
use crate::agent::model;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::mpsc;

use super::focus::{FocusContext, FocusManager, FocusOwner, FocusTarget};
use super::input::InputState;
use super::mention;
use super::slash;

#[derive(Debug)]
pub struct ModeInfo {
    pub id: String,
    pub name: String,
}

#[derive(Debug)]
pub struct ModeState {
    pub current_mode_id: String,
    pub current_mode_name: String,
    pub available_modes: Vec<ModeInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HelpView {
    #[default]
    Keys,
    SlashCommands,
}

/// Login hint displayed when authentication is required during connection.
/// Rendered as a banner above the input field.
pub struct LoginHint {
    pub method_name: String,
    pub method_description: String,
}

/// A single todo item from Claude's `TodoWrite` tool call.
#[derive(Debug, Clone)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
    pub active_form: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentSessionInfo {
    pub session_id: String,
    pub cwd: String,
    pub title: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MessageUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub turn_cost_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SessionUsageState {
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_cache_write_tokens: u64,
    pub latest_input_tokens: Option<u64>,
    pub latest_output_tokens: Option<u64>,
    pub latest_cache_read_tokens: Option<u64>,
    pub latest_cache_write_tokens: Option<u64>,
    pub total_cost_usd: Option<f64>,
    /// True when cost started accumulating only after a resume because
    /// historical resume updates carried no cost baseline.
    pub cost_is_since_resume: bool,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub last_compaction_trigger: Option<model::CompactionTrigger>,
    pub last_compaction_pre_tokens: Option<u64>,
}

impl SessionUsageState {
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.total_input_tokens + self.total_output_tokens
    }

    #[must_use]
    pub fn context_used_tokens(&self) -> Option<u64> {
        let input = self.latest_input_tokens?;
        let output = self.latest_output_tokens?;
        let cache_read = self.latest_cache_read_tokens.unwrap_or(0);
        let cache_write = self.latest_cache_write_tokens.unwrap_or(0);
        Some(input.saturating_add(output).saturating_add(cache_read).saturating_add(cache_write))
    }
}

pub const DEFAULT_RENDER_CACHE_BUDGET_BYTES: usize = 24 * 1024 * 1024;
pub const DEFAULT_HISTORY_RETENTION_MAX_BYTES: usize = 64 * 1024 * 1024;

const HISTORY_HIDDEN_MARKER_PREFIX: &str = "Older messages hidden to keep memory bounded";
const HISTORY_ESTIMATE_MESSAGE_OVERHEAD_BYTES: usize = 64;
const HISTORY_ESTIMATE_BLOCK_OVERHEAD_BYTES: usize = 48;
const HISTORY_ESTIMATE_TOOLCALL_OVERHEAD_BYTES: usize = 256;
const HISTORY_ESTIMATE_WELCOME_OVERHEAD_BYTES: usize = 96;
static CACHE_ACCESS_TICK: AtomicU64 = AtomicU64::new(1);

fn next_cache_access_tick() -> u64 {
    CACHE_ACCESS_TICK.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderCacheBudget {
    pub max_bytes: usize,
    pub last_total_bytes: usize,
    pub last_evicted_bytes: usize,
    pub total_evictions: usize,
}

impl Default for RenderCacheBudget {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_RENDER_CACHE_BUDGET_BYTES,
            last_total_bytes: 0,
            last_evicted_bytes: 0,
            total_evictions: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryRetentionPolicy {
    pub max_bytes: usize,
}

impl Default for HistoryRetentionPolicy {
    fn default() -> Self {
        Self { max_bytes: DEFAULT_HISTORY_RETENTION_MAX_BYTES }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HistoryRetentionStats {
    pub total_before_bytes: usize,
    pub total_after_bytes: usize,
    pub dropped_messages: usize,
    pub dropped_bytes: usize,
    pub total_dropped_messages: usize,
    pub total_dropped_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheBudgetEnforceStats {
    pub total_before_bytes: usize,
    pub total_after_bytes: usize,
    pub evicted_bytes: usize,
    pub evicted_blocks: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CacheSlotCandidate {
    msg_idx: usize,
    block_idx: usize,
    bytes: usize,
    last_access_tick: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HistoryDropCandidate {
    msg_idx: usize,
    bytes: usize,
    approx_rows: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasteSessionState {
    pub id: u64,
    pub start: SelectionPoint,
    pub placeholder_index: Option<usize>,
}

#[allow(clippy::struct_excessive_bools)]
pub struct App {
    pub messages: Vec<ChatMessage>,
    /// Single owner of all chat layout state: scroll, per-message heights, prefix sums.
    pub viewport: ChatViewport,
    pub input: InputState,
    pub status: AppStatus,
    /// Session id currently being resumed via `/resume`.
    pub resuming_session_id: Option<String>,
    pub should_quit: bool,
    /// Optional fatal app error that should be surfaced at CLI boundary.
    pub exit_error: Option<crate::error::AppError>,
    pub session_id: Option<model::SessionId>,
    /// Agent connection handle. `None` while connecting (before bridge is ready).
    pub conn: Option<Rc<crate::agent::client::AgentConnection>>,
    pub model_name: String,
    pub cwd: String,
    pub cwd_raw: String,
    pub files_accessed: usize,
    pub mode: Option<ModeState>,
    /// Login hint shown when authentication is required. Rendered above the input field.
    pub login_hint: Option<LoginHint>,
    /// When true, the current/next turn completion should clear local conversation history.
    /// Set by `/compact` once the command is accepted for bridge forwarding.
    pub pending_compact_clear: bool,
    /// Active help overlay view when `?` help is open.
    pub help_view: HelpView,
    /// Tool call IDs with pending permission prompts, ordered by arrival.
    /// The first entry is the "focused" permission that receives keyboard input.
    /// Up / Down arrow keys cycle focus through the list.
    pub pending_permission_ids: Vec<String>,
    /// Set when a cancel notification succeeds; consumed on `TurnComplete`
    /// to render a red interruption hint in chat.
    pub cancelled_turn_pending_hint: bool,
    /// Queued submit text while a turn is still active.
    /// Latest submission wins and replaces older queued text.
    pub queued_submission: Option<String>,
    /// Origin of the in-flight cancellation request, if any.
    pub pending_cancel_origin: Option<CancelOrigin>,
    pub event_tx: mpsc::UnboundedSender<ClientEvent>,
    pub event_rx: mpsc::UnboundedReceiver<ClientEvent>,
    pub spinner_frame: usize,
    /// Session-level default for tool call collapsed state.
    /// Toggled by Ctrl+O - new tool calls inherit this value.
    pub tools_collapsed: bool,
    /// IDs of Task tool calls currently `InProgress` -- their children get hidden.
    /// Use `insert_active_task()`, `remove_active_task()`.
    pub active_task_ids: HashSet<String>,
    /// Shared terminal process map - used to snapshot output on completion.
    pub terminals: crate::agent::events::TerminalMap,
    /// Force a full terminal clear on next render frame.
    pub force_redraw: bool,
    /// O(1) lookup: `tool_call_id` -> `(message_index, block_index)`.
    /// Use `lookup_tool_call()`, `index_tool_call()`.
    pub tool_call_index: HashMap<String, (usize, usize)>,
    /// Current todo list from Claude's `TodoWrite` tool calls.
    pub todos: Vec<TodoItem>,
    /// Whether the header bar is visible.
    /// Toggled by Ctrl+H.
    pub show_header: bool,
    /// Whether the todo panel is expanded (true) or shows compact status line (false).
    /// Toggled by Ctrl+T.
    pub show_todo_panel: bool,
    /// Scroll offset for the expanded todo panel (capped at 5 visible lines).
    pub todo_scroll: usize,
    /// Selected todo index used for keyboard navigation in the open todo panel.
    pub todo_selected: usize,
    /// Focus manager for directional/navigation key ownership.
    pub focus: FocusManager,
    /// Commands advertised by the agent via `AvailableCommandsUpdate`.
    pub available_commands: Vec<model::AvailableCommand>,
    /// Recently persisted session IDs discovered at startup.
    pub recent_sessions: Vec<RecentSessionInfo>,
    /// Last known frame area (for mouse selection mapping).
    pub cached_frame_area: ratatui::layout::Rect,
    /// Current selection state for mouse-based selection.
    pub selection: Option<SelectionState>,
    /// Active scrollbar drag state while left mouse button is held on the rail.
    pub scrollbar_drag: Option<ScrollbarDragState>,
    /// Cached rendered chat lines for selection/copy.
    pub rendered_chat_lines: Vec<String>,
    /// Area where chat content was rendered (for selection mapping).
    pub rendered_chat_area: ratatui::layout::Rect,
    /// Cached rendered input lines for selection/copy.
    pub rendered_input_lines: Vec<String>,
    /// Area where input content was rendered (for selection mapping).
    pub rendered_input_area: ratatui::layout::Rect,
    /// Active `@` file mention autocomplete state.
    pub mention: Option<mention::MentionState>,
    /// Active slash-command autocomplete state.
    pub slash: Option<slash::SlashState>,
    /// Deferred submit: set `true` when Enter is pressed. If another key event
    /// arrives during the same drain cycle (paste), this is cleared and the Enter
    /// becomes a newline. After the drain, the main loop checks: if still `true`,
    /// strips the trailing newline and submits.
    pub pending_submit: bool,
    /// Count of key events processed in the current drain cycle. Used to detect
    /// paste: if >1 key events arrive in a single cycle, Enter is treated as a
    /// newline rather than submit.
    pub drain_key_count: usize,
    /// Timing-based paste burst detector. Tracks rapid key events to distinguish
    /// paste from typing when `Event::Paste` is not available (Windows).
    pub paste_burst: super::paste_burst::PasteBurstDetector,
    /// Buffered `Event::Paste` payload for this drain cycle.
    /// Some terminals split one clipboard paste into multiple chunks; we merge
    /// them and apply placeholder threshold to the merged content once per cycle.
    pub pending_paste_text: String,
    /// Pending paste session metadata for the currently queued `Event::Paste` payload.
    pub pending_paste_session: Option<PasteSessionState>,
    /// Most recent active placeholder paste session, used for safe chunk continuation.
    pub active_paste_session: Option<PasteSessionState>,
    /// Monotonic counter for paste session identifiers.
    pub next_paste_session_id: u64,
    /// Start cursor of the current rapid-key burst.
    pub paste_burst_start: Option<SelectionPoint>,
    /// Cached file list from cwd (scanned on first `@` trigger).
    pub file_cache: Option<Vec<mention::FileCandidate>>,
    /// Cached todo compact line (invalidated on `set_todos()`).
    pub cached_todo_compact: Option<ratatui::text::Line<'static>>,
    /// Current git branch (refreshed on focus gain + turn complete).
    pub git_branch: Option<String>,
    /// Cached header line (invalidated when git branch changes).
    pub cached_header_line: Option<ratatui::text::Line<'static>>,
    /// Cached footer line (invalidated on mode change).
    pub cached_footer_line: Option<ratatui::text::Line<'static>>,
    /// Optional startup update-check hint rendered at the footer's right edge.
    pub update_check_hint: Option<String>,
    /// Session-wide usage and cost telemetry from the bridge.
    pub session_usage: SessionUsageState,
    /// True while the SDK reports active compaction.
    pub is_compacting: bool,

    /// Indexed terminal tool calls: `(terminal_id, msg_idx, block_idx)`.
    /// Avoids O(n*m) scan of all messages/blocks every frame.
    pub terminal_tool_calls: Vec<(String, usize, usize)>,
    /// Dirty flag: skip `terminal.draw()` when nothing changed since last frame.
    pub needs_redraw: bool,
    /// Performance logger. Present only when built with `--features perf`.
    /// Taken out (`Option::take`) during render, used, then put back to avoid
    /// borrow conflicts with `&mut App`.
    pub perf: Option<crate::perf::PerfLogger>,
    /// Global in-memory budget for rendered block caches (message + tool + welcome).
    pub render_cache_budget: RenderCacheBudget,
    /// Byte budget for source conversation history retained in memory.
    pub history_retention: HistoryRetentionPolicy,
    /// Last history-retention enforcement statistics.
    pub history_retention_stats: HistoryRetentionStats,
    /// Smoothed frames-per-second (EMA of presented frame cadence).
    pub fps_ema: Option<f32>,
    /// Timestamp of the previous presented frame.
    pub last_frame_at: Option<Instant>,
}

impl App {
    /// Mark one presented frame at `now`, updating smoothed FPS.
    pub fn mark_frame_presented(&mut self, now: Instant) {
        let Some(prev) = self.last_frame_at.replace(now) else {
            return;
        };
        let dt = now.saturating_duration_since(prev).as_secs_f32();
        if dt <= f32::EPSILON {
            return;
        }
        let fps = (1.0 / dt).clamp(0.0, 240.0);
        self.fps_ema = Some(match self.fps_ema {
            Some(current) => current * 0.9 + fps * 0.1,
            None => fps,
        });
    }

    #[must_use]
    pub fn frame_fps(&self) -> Option<f32> {
        self.fps_ema
    }

    /// Ensure the synthetic welcome message exists at index 0.
    pub fn ensure_welcome_message(&mut self) {
        if self.messages.first().is_some_and(|m| matches!(m.role, MessageRole::Welcome)) {
            return;
        }
        self.messages.insert(
            0,
            ChatMessage::welcome_with_recent(&self.model_name, &self.cwd, &self.recent_sessions),
        );
        self.mark_all_message_layout_dirty();
    }

    /// Update the welcome message's model name, but only before chat starts.
    pub fn update_welcome_model_if_pristine(&mut self) {
        if self.messages.len() != 1 {
            return;
        }
        let Some(first) = self.messages.first_mut() else {
            return;
        };
        if !matches!(first.role, MessageRole::Welcome) {
            return;
        }
        let Some(MessageBlock::Welcome(welcome)) = first.blocks.first_mut() else {
            return;
        };
        welcome.model_name.clone_from(&self.model_name);
        welcome.cache.invalidate();
        self.mark_message_layout_dirty(0);
    }

    /// Update the welcome message with latest discovered recent sessions.
    pub fn sync_welcome_recent_sessions(&mut self) {
        let Some(first) = self.messages.first_mut() else {
            return;
        };
        if !matches!(first.role, MessageRole::Welcome) {
            return;
        }
        let Some(MessageBlock::Welcome(welcome)) = first.blocks.first_mut() else {
            return;
        };
        welcome.recent_sessions.clone_from(&self.recent_sessions);
        welcome.cache.invalidate();
        self.mark_message_layout_dirty(0);
    }

    /// Track a Task tool call as active (in-progress subagent).
    pub fn insert_active_task(&mut self, id: String) {
        self.active_task_ids.insert(id);
    }

    /// Remove a Task tool call from the active set (completed/failed).
    pub fn remove_active_task(&mut self, id: &str) {
        self.active_task_ids.remove(id);
    }

    /// Look up the (`message_index`, `block_index`) for a tool call ID.
    #[must_use]
    pub fn lookup_tool_call(&self, id: &str) -> Option<(usize, usize)> {
        self.tool_call_index.get(id).copied()
    }

    /// Register a tool call's position in the message/block arrays.
    pub fn index_tool_call(&mut self, id: String, msg_idx: usize, block_idx: usize) {
        self.tool_call_index.insert(id, (msg_idx, block_idx));
    }

    /// Mark message layout caches dirty from `msg_idx` onward.
    ///
    /// Non-tail changes invalidate prefix-sum fast path so a full rebuild happens once.
    pub fn mark_message_layout_dirty(&mut self, msg_idx: usize) {
        self.viewport.mark_message_dirty(msg_idx);
        if msg_idx + 1 < self.messages.len() {
            self.viewport.prefix_sums_width = 0;
        }
    }

    /// Mark all message layout caches dirty.
    pub fn mark_all_message_layout_dirty(&mut self) {
        if self.messages.is_empty() {
            return;
        }
        self.viewport.mark_message_dirty(0);
        self.viewport.prefix_sums_width = 0;
    }

    /// Force-finish any lingering in-progress tool calls.
    /// Returns the number of tool calls that were transitioned.
    pub fn finalize_in_progress_tool_calls(&mut self, new_status: model::ToolCallStatus) -> usize {
        let mut changed = 0usize;
        let mut cleared_permission = false;
        let mut first_changed_idx: Option<usize> = None;

        for (msg_idx, msg) in self.messages.iter_mut().enumerate() {
            for block in &mut msg.blocks {
                if let MessageBlock::ToolCall(tc) = block {
                    let tc = tc.as_mut();
                    if matches!(
                        tc.status,
                        model::ToolCallStatus::InProgress | model::ToolCallStatus::Pending
                    ) {
                        tc.status = new_status;
                        tc.mark_tool_call_layout_dirty();
                        if tc.pending_permission.take().is_some() {
                            cleared_permission = true;
                        }
                        first_changed_idx =
                            Some(first_changed_idx.map_or(msg_idx, |prev| prev.min(msg_idx)));
                        changed += 1;
                    }
                }
            }
        }

        if changed > 0 || cleared_permission {
            if let Some(msg_idx) = first_changed_idx {
                self.mark_message_layout_dirty(msg_idx);
            }
            self.pending_permission_ids.clear();
            self.release_focus_target(FocusTarget::Permission);
        }

        changed
    }

    #[must_use]
    fn is_history_hidden_marker_message(msg: &ChatMessage) -> bool {
        if !matches!(msg.role, MessageRole::System) {
            return false;
        }
        let Some(MessageBlock::Text(text, _, _)) = msg.blocks.first() else {
            return false;
        };
        text.starts_with(HISTORY_HIDDEN_MARKER_PREFIX)
    }

    #[must_use]
    fn is_history_protected_message(msg: &ChatMessage) -> bool {
        if matches!(msg.role, MessageRole::Welcome) {
            return true;
        }
        msg.blocks.iter().any(|block| {
            if let MessageBlock::ToolCall(tc) = block {
                tc.pending_permission.is_some()
                    || matches!(
                        tc.status,
                        model::ToolCallStatus::Pending | model::ToolCallStatus::InProgress
                    )
            } else {
                false
            }
        })
    }

    #[must_use]
    fn estimate_tool_content_bytes(content: &model::ToolCallContent) -> usize {
        match content {
            model::ToolCallContent::Content(inner) => match &inner.content {
                model::ContentBlock::Text(text) => text.text.len(),
                model::ContentBlock::Image(image) => {
                    image.data.len().saturating_add(image.mime_type.len())
                }
            },
            model::ToolCallContent::Diff(diff) => diff
                .path
                .to_string_lossy()
                .len()
                .saturating_add(diff.old_text.as_ref().map_or(0, String::len))
                .saturating_add(diff.new_text.len()),
            model::ToolCallContent::Terminal(term) => term.terminal_id.len(),
        }
    }

    #[must_use]
    fn estimate_json_value_bytes(value: &serde_json::Value) -> usize {
        serde_json::to_string(value).map_or(0, |json| json.len())
    }

    #[must_use]
    fn estimate_tool_call_bytes(tc: &ToolCallInfo) -> usize {
        let mut total = HISTORY_ESTIMATE_TOOLCALL_OVERHEAD_BYTES
            .saturating_add(tc.id.len())
            .saturating_add(tc.title.len())
            .saturating_add(tc.sdk_tool_name.len())
            .saturating_add(tc.terminal_id.as_ref().map_or(0, String::len))
            .saturating_add(tc.terminal_command.as_ref().map_or(0, String::len))
            .saturating_add(tc.terminal_output.as_ref().map_or(0, String::len));

        if let Some(raw_input) = &tc.raw_input {
            total = total.saturating_add(Self::estimate_json_value_bytes(raw_input));
        }
        for content in &tc.content {
            total = total.saturating_add(Self::estimate_tool_content_bytes(content));
        }
        if let Some(permission) = &tc.pending_permission {
            total = total.saturating_add(64);
            for option in &permission.options {
                total = total
                    .saturating_add(option.option_id.len())
                    .saturating_add(option.name.len())
                    .saturating_add(option.description.as_ref().map_or(0, String::len));
            }
        }

        total
    }

    #[must_use]
    pub fn estimate_message_bytes(msg: &ChatMessage) -> usize {
        let mut total = HISTORY_ESTIMATE_MESSAGE_OVERHEAD_BYTES;
        if msg.usage.is_some() {
            total = total.saturating_add(32);
        }

        for block in &msg.blocks {
            total = total.saturating_add(HISTORY_ESTIMATE_BLOCK_OVERHEAD_BYTES);
            match block {
                MessageBlock::Text(text, _, _) => {
                    // Text block source is currently held in both the plain String and
                    // IncrementalMarkdown source; estimate both copies.
                    total = total.saturating_add(text.len().saturating_mul(2));
                }
                MessageBlock::ToolCall(tc) => {
                    total = total.saturating_add(Self::estimate_tool_call_bytes(tc));
                }
                MessageBlock::Welcome(welcome) => {
                    total = total
                        .saturating_add(HISTORY_ESTIMATE_WELCOME_OVERHEAD_BYTES)
                        .saturating_add(welcome.model_name.len())
                        .saturating_add(welcome.cwd.len());
                    for session in &welcome.recent_sessions {
                        total = total
                            .saturating_add(session.session_id.len())
                            .saturating_add(session.cwd.len())
                            .saturating_add(session.title.as_ref().map_or(0, String::len))
                            .saturating_add(session.updated_at.as_ref().map_or(0, String::len));
                    }
                }
            }
        }
        total
    }

    #[must_use]
    pub fn estimate_history_bytes(&self) -> usize {
        self.messages.iter().map(Self::estimate_message_bytes).sum()
    }

    fn rebuild_tool_indices_and_terminal_refs(&mut self) {
        self.tool_call_index.clear();
        self.terminal_tool_calls.clear();

        let mut pending_permission_ids = Vec::new();
        for (msg_idx, msg) in self.messages.iter_mut().enumerate() {
            for (block_idx, block) in msg.blocks.iter_mut().enumerate() {
                if let MessageBlock::ToolCall(tc) = block {
                    let tc = tc.as_mut();
                    self.tool_call_index.insert(tc.id.clone(), (msg_idx, block_idx));
                    if let Some(terminal_id) = tc.terminal_id.clone() {
                        self.terminal_tool_calls.push((terminal_id, msg_idx, block_idx));
                    }
                    if let Some(permission) = tc.pending_permission.as_mut() {
                        permission.focused = false;
                        pending_permission_ids.push(tc.id.clone());
                    }
                }
            }
        }

        let permission_set: HashSet<&str> =
            pending_permission_ids.iter().map(String::as_str).collect();
        self.pending_permission_ids.retain(|id| permission_set.contains(id.as_str()));
        for id in pending_permission_ids {
            if !self.pending_permission_ids.iter().any(|existing| existing == &id) {
                self.pending_permission_ids.push(id);
            }
        }

        if let Some(first_id) = self.pending_permission_ids.first().cloned() {
            self.claim_focus_target(FocusTarget::Permission);
            if let Some((msg_idx, block_idx)) = self.lookup_tool_call(&first_id)
                && let Some(MessageBlock::ToolCall(tc)) =
                    self.messages.get_mut(msg_idx).and_then(|m| m.blocks.get_mut(block_idx))
                && let Some(permission) = tc.pending_permission.as_mut()
            {
                permission.focused = true;
            }
        } else {
            self.release_focus_target(FocusTarget::Permission);
        }
        self.normalize_focus_stack();
    }

    #[must_use]
    fn format_mib_tenths(bytes: usize) -> String {
        let tenths =
            (u128::try_from(bytes).unwrap_or(u128::MAX).saturating_mul(10) + 524_288) / 1_048_576;
        format!("{}.{}", tenths / 10, tenths % 10)
    }

    #[must_use]
    fn history_hidden_marker_text(
        total_dropped_messages: usize,
        total_dropped_bytes: usize,
    ) -> String {
        format!(
            "{HISTORY_HIDDEN_MARKER_PREFIX} (dropped {total_dropped_messages} messages, {} MiB).",
            Self::format_mib_tenths(total_dropped_bytes)
        )
    }

    fn upsert_history_hidden_marker(&mut self) {
        let marker_idx = self.messages.iter().position(Self::is_history_hidden_marker_message);
        if self.history_retention_stats.total_dropped_messages == 0 {
            if let Some(idx) = marker_idx {
                self.messages.remove(idx);
                self.mark_message_layout_dirty(idx);
                self.rebuild_tool_indices_and_terminal_refs();
            }
            return;
        }

        let marker_text = Self::history_hidden_marker_text(
            self.history_retention_stats.total_dropped_messages,
            self.history_retention_stats.total_dropped_bytes,
        );

        if let Some(idx) = marker_idx {
            if let Some(MessageBlock::Text(text, cache, incr)) =
                self.messages.get_mut(idx).and_then(|m| m.blocks.get_mut(0))
                && *text != marker_text
            {
                text.clone_from(&marker_text);
                *incr = IncrementalMarkdown::from_complete(&marker_text);
                cache.invalidate();
                self.mark_message_layout_dirty(idx);
            }
            return;
        }

        let insert_idx = usize::from(
            self.messages.first().is_some_and(|msg| matches!(msg.role, MessageRole::Welcome)),
        );
        self.messages.insert(
            insert_idx,
            ChatMessage {
                role: MessageRole::System,
                blocks: vec![MessageBlock::Text(
                    marker_text.clone(),
                    BlockCache::default(),
                    IncrementalMarkdown::from_complete(&marker_text),
                )],
                usage: None,
            },
        );
        self.mark_message_layout_dirty(insert_idx);
        self.rebuild_tool_indices_and_terminal_refs();
    }

    #[allow(clippy::cast_precision_loss)]
    pub fn enforce_history_retention(&mut self) -> HistoryRetentionStats {
        let mut stats = HistoryRetentionStats::default();
        let max_bytes = self.history_retention.max_bytes.max(1);
        stats.total_before_bytes = self.estimate_history_bytes();
        stats.total_after_bytes = stats.total_before_bytes;

        if stats.total_before_bytes > max_bytes {
            let mut candidates = Vec::new();
            for (msg_idx, msg) in self.messages.iter().enumerate() {
                if Self::is_history_hidden_marker_message(msg)
                    || Self::is_history_protected_message(msg)
                {
                    continue;
                }
                let bytes = Self::estimate_message_bytes(msg);
                if bytes == 0 {
                    continue;
                }
                candidates.push(HistoryDropCandidate {
                    msg_idx,
                    bytes,
                    approx_rows: self.viewport.message_height(msg_idx),
                });
            }

            let mut drop_candidates = Vec::new();
            for candidate in candidates {
                if stats.total_after_bytes <= max_bytes {
                    break;
                }
                stats.total_after_bytes = stats.total_after_bytes.saturating_sub(candidate.bytes);
                stats.dropped_bytes = stats.dropped_bytes.saturating_add(candidate.bytes);
                stats.dropped_messages = stats.dropped_messages.saturating_add(1);
                drop_candidates.push(candidate);
            }

            if !drop_candidates.is_empty() {
                let mut dropped_rows = 0usize;
                let drop_set: HashSet<usize> = drop_candidates
                    .iter()
                    .map(|candidate| {
                        dropped_rows = dropped_rows.saturating_add(candidate.approx_rows);
                        candidate.msg_idx
                    })
                    .collect();

                let mut retained =
                    Vec::with_capacity(self.messages.len().saturating_sub(drop_set.len()));
                for (msg_idx, msg) in self.messages.drain(..).enumerate() {
                    if !drop_set.contains(&msg_idx) {
                        retained.push(msg);
                    }
                }
                self.messages = retained;

                if !self.viewport.auto_scroll && dropped_rows > 0 {
                    self.viewport.scroll_target =
                        self.viewport.scroll_target.saturating_sub(dropped_rows);
                    self.viewport.scroll_offset =
                        self.viewport.scroll_offset.saturating_sub(dropped_rows);
                    let dropped_rows_f = dropped_rows as f32;
                    self.viewport.scroll_pos = if self.viewport.scroll_pos > dropped_rows_f {
                        self.viewport.scroll_pos - dropped_rows_f
                    } else {
                        0.0
                    };
                }
                self.rebuild_tool_indices_and_terminal_refs();
                self.mark_all_message_layout_dirty();
                self.needs_redraw = true;
            }
        }

        self.history_retention_stats.total_before_bytes = stats.total_before_bytes;
        self.history_retention_stats.total_dropped_messages = self
            .history_retention_stats
            .total_dropped_messages
            .saturating_add(stats.dropped_messages);
        self.history_retention_stats.total_dropped_bytes =
            self.history_retention_stats.total_dropped_bytes.saturating_add(stats.dropped_bytes);

        self.upsert_history_hidden_marker();

        stats.total_after_bytes = self.estimate_history_bytes();
        self.history_retention_stats.total_after_bytes = stats.total_after_bytes;
        self.history_retention_stats.dropped_messages = stats.dropped_messages;
        self.history_retention_stats.dropped_bytes = stats.dropped_bytes;

        stats.total_dropped_messages = self.history_retention_stats.total_dropped_messages;
        stats.total_dropped_bytes = self.history_retention_stats.total_dropped_bytes;
        stats
    }

    pub fn enforce_render_cache_budget(&mut self) -> CacheBudgetEnforceStats {
        let mut stats = CacheBudgetEnforceStats::default();
        let is_streaming = matches!(self.status, AppStatus::Thinking | AppStatus::Running);
        let msg_count = self.messages.len();
        let mut evictable = Vec::new();

        for (msg_idx, msg) in self.messages.iter().enumerate() {
            let protect_message_tail = is_streaming && (msg_idx + 1 == msg_count);
            for (block_idx, block) in msg.blocks.iter().enumerate() {
                let (cache, protect_block) = match block {
                    MessageBlock::Text(_, cache, _) => (cache, false),
                    MessageBlock::Welcome(welcome) => (&welcome.cache, false),
                    MessageBlock::ToolCall(tc) => (
                        &tc.cache,
                        matches!(
                            tc.status,
                            model::ToolCallStatus::Pending | model::ToolCallStatus::InProgress
                        ),
                    ),
                };

                let bytes = cache.cached_bytes();
                if bytes == 0 {
                    continue;
                }
                stats.total_before_bytes = stats.total_before_bytes.saturating_add(bytes);

                if !(protect_message_tail || protect_block) {
                    evictable.push(CacheSlotCandidate {
                        msg_idx,
                        block_idx,
                        bytes,
                        last_access_tick: cache.last_access_tick(),
                    });
                }
            }
        }

        if stats.total_before_bytes <= self.render_cache_budget.max_bytes {
            self.render_cache_budget.last_total_bytes = stats.total_before_bytes;
            self.render_cache_budget.last_evicted_bytes = 0;
            stats.total_after_bytes = stats.total_before_bytes;
            return stats;
        }

        evictable.sort_by_key(|slot| (slot.last_access_tick, std::cmp::Reverse(slot.bytes)));
        stats.total_after_bytes = stats.total_before_bytes;

        for slot in evictable {
            if stats.total_after_bytes <= self.render_cache_budget.max_bytes {
                break;
            }
            let removed = self.evict_cache_slot(slot.msg_idx, slot.block_idx);
            if removed == 0 {
                continue;
            }
            stats.total_after_bytes = stats.total_after_bytes.saturating_sub(removed);
            stats.evicted_bytes = stats.evicted_bytes.saturating_add(removed);
            stats.evicted_blocks = stats.evicted_blocks.saturating_add(1);
        }

        self.render_cache_budget.last_total_bytes = stats.total_after_bytes;
        self.render_cache_budget.last_evicted_bytes = stats.evicted_bytes;
        self.render_cache_budget.total_evictions =
            self.render_cache_budget.total_evictions.saturating_add(stats.evicted_blocks);

        stats
    }

    fn evict_cache_slot(&mut self, msg_idx: usize, block_idx: usize) -> usize {
        let Some(msg) = self.messages.get_mut(msg_idx) else {
            return 0;
        };
        let Some(block) = msg.blocks.get_mut(block_idx) else {
            return 0;
        };
        match block {
            MessageBlock::Text(_, cache, _) => cache.evict_cached_render(),
            MessageBlock::Welcome(welcome) => welcome.cache.evict_cached_render(),
            MessageBlock::ToolCall(tc) => tc.cache.evict_cached_render(),
        }
    }

    /// Build a minimal `App` for unit/integration tests.
    /// All fields get sensible defaults; the `mpsc` channel is wired up internally.
    #[doc(hidden)]
    #[must_use]
    pub fn test_default() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            messages: Vec::new(),
            viewport: ChatViewport::new(),
            input: InputState::new(),
            status: AppStatus::Ready,
            resuming_session_id: None,
            should_quit: false,
            exit_error: None,
            session_id: None,
            conn: None,
            model_name: "test-model".into(),
            cwd: "/test".into(),
            cwd_raw: "/test".into(),
            files_accessed: 0,
            mode: None,
            login_hint: None,
            pending_compact_clear: false,
            help_view: HelpView::Keys,
            pending_permission_ids: Vec::new(),
            cancelled_turn_pending_hint: false,
            queued_submission: None,
            pending_cancel_origin: None,
            event_tx: tx,
            event_rx: rx,
            spinner_frame: 0,
            tools_collapsed: false,
            active_task_ids: HashSet::default(),
            terminals: std::rc::Rc::default(),
            force_redraw: false,
            tool_call_index: HashMap::default(),
            todos: Vec::new(),
            show_header: true,
            show_todo_panel: false,
            todo_scroll: 0,
            todo_selected: 0,
            focus: FocusManager::default(),
            available_commands: Vec::new(),
            recent_sessions: Vec::new(),
            cached_frame_area: ratatui::layout::Rect::default(),
            selection: None,
            scrollbar_drag: None,
            rendered_chat_lines: Vec::new(),
            rendered_chat_area: ratatui::layout::Rect::default(),
            rendered_input_lines: Vec::new(),
            rendered_input_area: ratatui::layout::Rect::default(),
            mention: None,
            slash: None,
            pending_submit: false,
            drain_key_count: 0,
            paste_burst: super::paste_burst::PasteBurstDetector::new(),
            pending_paste_text: String::new(),
            pending_paste_session: None,
            active_paste_session: None,
            next_paste_session_id: 1,
            paste_burst_start: None,
            file_cache: None,
            cached_todo_compact: None,
            git_branch: None,
            cached_header_line: None,
            cached_footer_line: None,
            update_check_hint: None,
            session_usage: SessionUsageState::default(),
            is_compacting: false,
            terminal_tool_calls: Vec::new(),
            needs_redraw: true,
            perf: None,
            render_cache_budget: RenderCacheBudget::default(),
            history_retention: HistoryRetentionPolicy::default(),
            history_retention_stats: HistoryRetentionStats::default(),
            fps_ema: None,
            last_frame_at: None,
        }
    }

    /// Detect the current git branch and invalidate the header cache if it changed.
    pub fn refresh_git_branch(&mut self) {
        let new_branch = std::process::Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&self.cwd_raw)
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    let s = String::from_utf8_lossy(&o.stdout).trim().to_owned();
                    if s.is_empty() { None } else { Some(s) }
                } else {
                    None
                }
            });
        if new_branch != self.git_branch {
            self.git_branch = new_branch;
            self.cached_header_line = None;
        }
    }

    /// Resolve the effective focus owner for Up/Down and other directional keys.
    #[must_use]
    pub fn focus_owner(&self) -> FocusOwner {
        self.focus.owner(self.focus_context())
    }

    #[must_use]
    pub fn is_help_active(&self) -> bool {
        self.input.text().trim() == "?"
    }

    /// Claim key routing for a navigation target.
    /// The latest claimant wins.
    pub fn claim_focus_target(&mut self, target: FocusTarget) {
        let context = self.focus_context();
        self.focus.claim(target, context);
    }

    /// Release key routing claim for a navigation target.
    pub fn release_focus_target(&mut self, target: FocusTarget) {
        let context = self.focus_context();
        self.focus.release(target, context);
    }

    /// Drop claims that are no longer valid for current state.
    pub fn normalize_focus_stack(&mut self) {
        let context = self.focus_context();
        self.focus.normalize(context);
    }

    #[must_use]
    fn focus_context(&self) -> FocusContext {
        FocusContext::with_help(
            self.show_todo_panel && !self.todos.is_empty(),
            self.mention.is_some() || self.slash.is_some(),
            !self.pending_permission_ids.is_empty(),
            self.is_help_active(),
        )
    }
}

/// Single owner of all chat layout state: scroll, per-message heights, and prefix sums.
///
/// Consolidates state previously scattered across `App` (scroll fields, prefix sums),
/// `ChatMessage` (`cached_visual_height`/`cached_visual_width`), and `BlockCache` (`wrapped_height`/`wrapped_width`).
/// Per-block heights remain on `BlockCache` via `set_height()` / `height_at()`, but
/// the viewport owns the validity width that governs whether those caches are considered
/// current. On resize, `on_frame()` zeroes message heights and clears prefix sums,
/// causing the next `update_visual_heights()` pass to re-measure every message
/// using ground-truth `Paragraph::line_count()`.
pub struct ChatViewport {
    // --- Scroll ---
    /// Rendered scroll offset (rounded from `scroll_pos`).
    pub scroll_offset: usize,
    /// Target scroll offset requested by user input or auto-scroll.
    pub scroll_target: usize,
    /// Smooth scroll position (fractional) for animation.
    pub scroll_pos: f32,
    /// Smoothed scrollbar thumb top row (fractional) for animation.
    pub scrollbar_thumb_top: f32,
    /// Smoothed scrollbar thumb height (fractional) for animation.
    pub scrollbar_thumb_size: f32,
    /// Whether to auto-scroll to bottom on new content.
    pub auto_scroll: bool,

    // --- Layout ---
    /// Current terminal width. Set by `on_frame()` each render cycle.
    pub width: u16,
    /// Monotonic layout generation for width/global layout-affecting changes.
    /// Tool-call measurement cache keys include this to avoid stale heights.
    pub layout_generation: u64,

    // --- Per-message heights ---
    /// Visual height (in terminal rows) of each message, indexed by message position.
    /// Zeroed on resize; rebuilt by `measure_message_height()`.
    pub message_heights: Vec<usize>,
    /// Width at which `message_heights` was last computed.
    pub message_heights_width: u16,
    /// Oldest message index whose cached height may be stale.
    pub dirty_from: Option<usize>,

    // --- Prefix sums ---
    /// Cumulative heights: `height_prefix_sums[i]` = sum of heights `0..=i`.
    /// Enables O(log n) binary search for first visible message and O(1) total height.
    pub height_prefix_sums: Vec<usize>,
    /// Width at which prefix sums were last computed.
    pub prefix_sums_width: u16,
}

impl ChatViewport {
    /// Create a new viewport with default scroll state (auto-scroll enabled).
    #[must_use]
    pub fn new() -> Self {
        Self {
            scroll_offset: 0,
            scroll_target: 0,
            scroll_pos: 0.0,
            scrollbar_thumb_top: 0.0,
            scrollbar_thumb_size: 0.0,
            auto_scroll: true,
            width: 0,
            layout_generation: 1,
            message_heights: Vec::new(),
            message_heights_width: 0,
            dirty_from: None,
            height_prefix_sums: Vec::new(),
            prefix_sums_width: 0,
        }
    }

    /// Called at top of each render frame. Detects width change and invalidates
    /// all cached heights so they get re-measured at the new width.
    pub fn on_frame(&mut self, width: u16) {
        if self.width != 0 && self.width != width {
            tracing::debug!(
                "RESIZE: width {} -> {}, scroll_target={}, auto_scroll={}",
                self.width,
                width,
                self.scroll_target,
                self.auto_scroll
            );
            self.handle_resize();
        }
        self.width = width;
    }

    /// Invalidate height caches on terminal resize.
    ///
    /// Setting `message_heights_width = 0` forces `update_visual_heights()`
    /// to re-measure every message at the new width using ground-truth
    /// `line_count()`. Old message heights are kept as approximations so
    /// `content_height` stays reasonable on the resize frame.
    fn handle_resize(&mut self) {
        self.message_heights_width = 0;
        self.prefix_sums_width = 0;
        self.layout_generation = self.layout_generation.wrapping_add(1);
    }

    /// Bump layout generation for non-width global layout-affecting changes.
    pub fn bump_layout_generation(&mut self) {
        self.layout_generation = self.layout_generation.wrapping_add(1);
    }

    // --- Per-message height ---

    /// Get the cached visual height for message `idx`. Returns 0 if not yet computed.
    #[must_use]
    pub fn message_height(&self, idx: usize) -> usize {
        self.message_heights.get(idx).copied().unwrap_or(0)
    }

    /// Set the visual height for message `idx`, growing the vec if needed.
    ///
    /// Does NOT update `message_heights_width` - the caller must call
    /// `mark_heights_valid()` after the full re-measurement pass completes.
    pub fn set_message_height(&mut self, idx: usize, h: usize) {
        if idx >= self.message_heights.len() {
            self.message_heights.resize(idx + 1, 0);
        }
        self.message_heights[idx] = h;
    }

    /// Mark all message heights as valid at the current width.
    /// Call after `update_visual_heights()` finishes re-measuring.
    pub fn mark_heights_valid(&mut self) {
        self.message_heights_width = self.width;
        self.dirty_from = None;
    }

    /// Mark cached heights dirty from `idx` onward.
    pub fn mark_message_dirty(&mut self, idx: usize) {
        self.dirty_from = Some(self.dirty_from.map_or(idx, |oldest| oldest.min(idx)));
    }

    // --- Prefix sums ---

    /// Rebuild prefix sums from `message_heights`.
    /// O(1) fast path when width unchanged and only the last message changed (streaming).
    pub fn rebuild_prefix_sums(&mut self) {
        let n = self.message_heights.len();
        if self.prefix_sums_width == self.width && self.height_prefix_sums.len() == n && n > 0 {
            // Streaming fast path: only last message's height changed.
            let prev = if n >= 2 { self.height_prefix_sums[n - 2] } else { 0 };
            self.height_prefix_sums[n - 1] = prev + self.message_heights[n - 1];
            return;
        }
        // Full rebuild (resize or new messages added)
        self.height_prefix_sums.clear();
        self.height_prefix_sums.reserve(n);
        let mut acc = 0;
        for &h in &self.message_heights {
            acc += h;
            self.height_prefix_sums.push(acc);
        }
        self.prefix_sums_width = self.width;
    }

    /// Total height of all messages (O(1) via prefix sums).
    #[must_use]
    pub fn total_message_height(&self) -> usize {
        self.height_prefix_sums.last().copied().unwrap_or(0)
    }

    /// Cumulative height of messages `0..idx` (O(1) via prefix sums).
    #[must_use]
    pub fn cumulative_height_before(&self, idx: usize) -> usize {
        if idx == 0 { 0 } else { self.height_prefix_sums.get(idx - 1).copied().unwrap_or(0) }
    }

    /// Binary search for the first message whose cumulative range overlaps `scroll_offset`.
    #[must_use]
    pub fn find_first_visible(&self, scroll_offset: usize) -> usize {
        if self.height_prefix_sums.is_empty() {
            return 0;
        }
        self.height_prefix_sums
            .partition_point(|&h| h <= scroll_offset)
            .min(self.message_heights.len().saturating_sub(1))
    }

    // --- Scroll ---

    /// Scroll up by `lines`. Disables auto-scroll.
    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll_target = self.scroll_target.saturating_sub(lines);
        self.auto_scroll = false;
    }

    /// Scroll down by `lines`. Auto-scroll re-engagement handled by render.
    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll_target = self.scroll_target.saturating_add(lines);
    }

    /// Re-engage auto-scroll (stick to bottom).
    pub fn engage_auto_scroll(&mut self) {
        self.auto_scroll = true;
    }
}

impl Default for ChatViewport {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum AppStatus {
    /// Waiting for bridge adapter connection (TUI shown, input disabled).
    Connecting,
    /// Switching to another existing session via `/resume`.
    Resuming,
    Ready,
    Thinking,
    Running,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOrigin {
    Manual,
    AutoQueue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionKind {
    Chat,
    Input,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionPoint {
    pub row: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionState {
    pub kind: SelectionKind,
    pub start: SelectionPoint,
    pub end: SelectionPoint,
    pub dragging: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbarDragState {
    /// Row offset from thumb top where the initial click happened.
    pub thumb_grab_offset: usize,
}

pub struct ChatMessage {
    pub role: MessageRole,
    pub blocks: Vec<MessageBlock>,
    pub usage: Option<MessageUsage>,
}

impl ChatMessage {
    #[must_use]
    pub fn welcome(model_name: &str, cwd: &str) -> Self {
        Self::welcome_with_recent(model_name, cwd, &[])
    }

    #[must_use]
    pub fn welcome_with_recent(
        model_name: &str,
        cwd: &str,
        recent_sessions: &[RecentSessionInfo],
    ) -> Self {
        Self {
            role: MessageRole::Welcome,
            blocks: vec![MessageBlock::Welcome(WelcomeBlock {
                model_name: model_name.to_owned(),
                cwd: cwd.to_owned(),
                recent_sessions: recent_sessions.to_vec(),
                cache: BlockCache::default(),
            })],
            usage: None,
        }
    }
}

/// Cached rendered lines for a block. Stores a version counter so the cache
/// is only recomputed when the block content actually changes.
///
/// Fields are private - use `invalidate()` to mark stale, `is_stale()` to check,
/// `get()` to read cached lines, and `store()` to populate.
#[derive(Default)]
pub struct BlockCache {
    version: u64,
    lines: Option<Vec<ratatui::text::Line<'static>>>,
    /// Segmentation metadata for KB-sized cache chunks shared across message/tool caches.
    segments: Vec<CacheLineSegment>,
    /// Approximate UTF-8 byte size of cached rendered lines.
    cached_bytes: usize,
    /// Wrapped line count of the cached lines at `wrapped_width`.
    /// Computed via `Paragraph::line_count(width)` on the same lines stored in `lines`.
    wrapped_height: usize,
    /// The viewport width used to compute `wrapped_height`.
    wrapped_width: u16,
    wrapped_height_valid: bool,
    last_access_tick: Cell<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CacheLineSegment {
    start: usize,
    end: usize,
    wrapped_height: usize,
    wrapped_width: u16,
    wrapped_height_valid: bool,
}

impl CacheLineSegment {
    #[must_use]
    fn new(start: usize, end: usize) -> Self {
        Self { start, end, wrapped_height: 0, wrapped_width: 0, wrapped_height_valid: false }
    }
}

impl BlockCache {
    fn touch(&self) {
        self.last_access_tick.set(next_cache_access_tick());
    }

    /// Bump the version to invalidate cached lines and height.
    pub fn invalidate(&mut self) {
        self.version += 1;
        self.wrapped_height_valid = false;
    }

    /// Get a reference to the cached lines, if fresh.
    #[must_use]
    pub fn get(&self) -> Option<&Vec<ratatui::text::Line<'static>>> {
        if self.version == 0 {
            let lines = self.lines.as_ref();
            if lines.is_some() {
                self.touch();
            }
            lines
        } else {
            None
        }
    }

    /// Store freshly rendered lines, marking the cache as clean.
    /// Height is set separately via `set_height()` after measurement.
    pub fn store(&mut self, lines: Vec<ratatui::text::Line<'static>>) {
        self.store_with_policy(lines, *super::default_cache_split_policy());
    }

    /// Store freshly rendered lines using a shared KB split policy.
    pub fn store_with_policy(
        &mut self,
        lines: Vec<ratatui::text::Line<'static>>,
        policy: super::CacheSplitPolicy,
    ) {
        let segment_limit = policy.hard_limit_bytes.max(1);
        let (segments, cached_bytes) = build_line_segments(&lines, segment_limit);
        self.lines = Some(lines);
        self.segments = segments;
        self.cached_bytes = cached_bytes;
        self.version = 0;
        self.wrapped_height = 0;
        self.wrapped_width = 0;
        self.wrapped_height_valid = false;
        self.touch();
    }

    /// Set the wrapped height for the cached lines at the given width.
    /// Called by the viewport/chat layer after `Paragraph::line_count(width)`.
    /// Separate from `store()` so height measurement is the viewport's job.
    pub fn set_height(&mut self, height: usize, width: u16) {
        self.wrapped_height = height;
        self.wrapped_width = width;
        self.wrapped_height_valid = true;
        self.touch();
    }

    /// Store lines and set height in one call.
    /// Deprecated: prefer `store()` + `set_height()` to keep concerns separate.
    pub fn store_with_height(
        &mut self,
        lines: Vec<ratatui::text::Line<'static>>,
        height: usize,
        width: u16,
    ) {
        self.store(lines);
        self.set_height(height, width);
    }

    /// Get the cached wrapped height if cache is valid and was computed at the given width.
    #[must_use]
    pub fn height_at(&self, width: u16) -> Option<usize> {
        if self.version == 0 && self.wrapped_height_valid && self.wrapped_width == width {
            self.touch();
            Some(self.wrapped_height)
        } else {
            None
        }
    }

    /// Recompute wrapped height from cached segments and memoize it at `width`.
    /// Returns `None` when the render cache is stale.
    pub fn measure_and_set_height(&mut self, width: u16) -> Option<usize> {
        if self.version != 0 {
            return None;
        }
        if let Some(h) = self.height_at(width) {
            return Some(h);
        }

        let lines = self.lines.as_ref()?;

        if self.segments.is_empty() {
            self.set_height(0, width);
            return Some(0);
        }

        let mut total_height = 0usize;
        for segment in &mut self.segments {
            if segment.wrapped_height_valid && segment.wrapped_width == width {
                total_height = total_height.saturating_add(segment.wrapped_height);
                continue;
            }
            let segment_lines = lines[segment.start..segment.end].to_vec();
            let h = ratatui::widgets::Paragraph::new(ratatui::text::Text::from(segment_lines))
                .wrap(ratatui::widgets::Wrap { trim: false })
                .line_count(width);
            segment.wrapped_height = h;
            segment.wrapped_width = width;
            segment.wrapped_height_valid = true;
            total_height = total_height.saturating_add(h);
        }

        self.set_height(total_height, width);
        Some(total_height)
    }

    #[must_use]
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    #[must_use]
    pub fn cached_bytes(&self) -> usize {
        self.cached_bytes
    }

    #[must_use]
    pub fn last_access_tick(&self) -> u64 {
        self.last_access_tick.get()
    }

    pub fn evict_cached_render(&mut self) -> usize {
        let removed = self.cached_bytes;
        if removed == 0 {
            return 0;
        }
        self.lines = None;
        self.segments.clear();
        self.cached_bytes = 0;
        self.wrapped_height = 0;
        self.wrapped_width = 0;
        self.wrapped_height_valid = false;
        self.version = self.version.wrapping_add(1);
        removed
    }
}

fn build_line_segments(
    lines: &[ratatui::text::Line<'static>],
    segment_limit_bytes: usize,
) -> (Vec<CacheLineSegment>, usize) {
    if lines.is_empty() {
        return (Vec::new(), 0);
    }

    let limit = segment_limit_bytes.max(1);
    let mut segments = Vec::new();
    let mut total_bytes = 0usize;
    let mut start = 0usize;
    let mut acc = 0usize;

    for (idx, line) in lines.iter().enumerate() {
        let line_bytes = line_utf8_bytes(line).max(1);
        total_bytes = total_bytes.saturating_add(line_bytes);

        if idx > start && acc.saturating_add(line_bytes) > limit {
            segments.push(CacheLineSegment::new(start, idx));
            start = idx;
            acc = 0;
        }
        acc = acc.saturating_add(line_bytes);
    }

    segments.push(CacheLineSegment::new(start, lines.len()));
    (segments, total_bytes)
}

fn line_utf8_bytes(line: &ratatui::text::Line<'static>) -> usize {
    let span_bytes =
        line.spans.iter().fold(0usize, |acc, span| acc.saturating_add(span.content.len()));
    span_bytes.saturating_add(1)
}

/// Text holder for a single message block's markdown source.
///
/// Block splitting for streaming text is handled at the message construction
/// level. This type intentionally does no internal splitting.
#[derive(Default)]
pub struct IncrementalMarkdown {
    text: String,
}

impl IncrementalMarkdown {
    /// Create from existing full text (e.g. user messages, connection errors).
    /// Treats the entire text as one block source.
    #[must_use]
    pub fn from_complete(text: &str) -> Self {
        Self { text: text.to_owned() }
    }

    /// Append a streaming text chunk.
    pub fn append(&mut self, chunk: &str) {
        self.text.push_str(chunk);
    }

    /// Get the full source text.
    #[must_use]
    pub fn full_text(&self) -> String {
        self.text.clone()
    }

    /// Render this block source via the provided markdown renderer.
    /// `render_fn` converts a markdown source string into `Vec<Line>`.
    pub fn lines(
        &mut self,
        render_fn: &impl Fn(&str) -> Vec<ratatui::text::Line<'static>>,
    ) -> Vec<ratatui::text::Line<'static>> {
        render_fn(&self.text)
    }

    /// No-op: markdown render caching lives at `BlockCache` level.
    pub fn invalidate_renders(&mut self) {
        let _ = self.text.len();
    }

    /// No-op: markdown render caching lives at `BlockCache` level.
    pub fn ensure_rendered(
        &mut self,
        _render_fn: &impl Fn(&str) -> Vec<ratatui::text::Line<'static>>,
    ) {
        let _ = self.text.len();
    }
}

/// Ordered content block - text and tool calls interleaved as they arrive.
pub enum MessageBlock {
    Text(String, BlockCache, IncrementalMarkdown),
    ToolCall(Box<ToolCallInfo>),
    Welcome(WelcomeBlock),
}

#[derive(Debug)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Welcome,
}

pub struct WelcomeBlock {
    pub model_name: String,
    pub cwd: String,
    pub recent_sessions: Vec<RecentSessionInfo>,
    pub cache: BlockCache,
}

pub struct ToolCallInfo {
    pub id: String,
    pub title: String,
    /// The SDK tool name from `meta.claudeCode.toolName` when available.
    /// Falls back to a derived name when metadata is absent.
    pub sdk_tool_name: String,
    pub raw_input: Option<serde_json::Value>,
    pub status: model::ToolCallStatus,
    pub content: Vec<model::ToolCallContent>,
    pub collapsed: bool,
    /// Hidden tool calls are subagent children - not rendered directly.
    pub hidden: bool,
    /// Terminal ID if this is a Bash-like SDK tool call with a running/completed terminal.
    pub terminal_id: Option<String>,
    /// The shell command that was executed (e.g. "echo hello && ls -la").
    pub terminal_command: Option<String>,
    /// Snapshot of terminal output, updated each frame while `InProgress`.
    pub terminal_output: Option<String>,
    /// Length of terminal buffer at last snapshot - used to skip O(n) re-snapshots
    /// when the buffer hasn't grown.
    pub terminal_output_len: usize,
    /// Number of terminal output bytes consumed for incremental append updates.
    pub terminal_bytes_seen: usize,
    /// Current terminal snapshot ingestion mode.
    pub terminal_snapshot_mode: TerminalSnapshotMode,
    /// Monotonic generation for render-affecting changes.
    pub render_epoch: u64,
    /// Monotonic generation for layout-affecting changes.
    pub layout_epoch: u64,
    /// Last measured width used by tool-call height cache.
    pub last_measured_width: u16,
    /// Last measured visual height in wrapped rows.
    pub last_measured_height: usize,
    /// Layout epoch used for the last measured height.
    pub last_measured_layout_epoch: u64,
    /// Global layout generation used for the last measured height.
    pub last_measured_layout_generation: u64,
    /// Per-block render cache for this tool call.
    pub cache: BlockCache,
    /// Inline permission prompt - rendered inside this tool call block.
    pub pending_permission: Option<InlinePermission>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalSnapshotMode {
    AppendOnly,
    ReplaceSnapshot,
}

impl ToolCallInfo {
    #[must_use]
    pub fn is_execute_tool(&self) -> bool {
        is_execute_tool_name(&self.sdk_tool_name)
    }

    #[must_use]
    pub fn is_ask_question_tool(&self) -> bool {
        is_ask_question_tool_name(&self.sdk_tool_name)
    }

    /// Mark render cache for this tool call as stale.
    pub fn mark_tool_call_render_dirty(&mut self) {
        crate::perf::mark("tc_invalidations_requested");
        self.render_epoch = self.render_epoch.wrapping_add(1);
        self.cache.invalidate();
        crate::perf::mark("tc_invalidations_applied");
    }

    /// Mark layout cache for this tool call as stale.
    pub fn mark_tool_call_layout_dirty(&mut self) {
        self.layout_epoch = self.layout_epoch.wrapping_add(1);
        self.last_measured_width = 0;
        self.last_measured_height = 0;
        self.last_measured_layout_epoch = 0;
        self.last_measured_layout_generation = 0;
        self.mark_tool_call_render_dirty();
    }

    #[must_use]
    pub fn cache_measurement_key_matches(&self, width: u16, layout_generation: u64) -> bool {
        self.last_measured_width == width
            && self.last_measured_layout_epoch == self.layout_epoch
            && self.last_measured_layout_generation == layout_generation
    }

    pub fn record_measured_height(&mut self, width: u16, height: usize, layout_generation: u64) {
        self.last_measured_width = width;
        self.last_measured_height = height;
        self.last_measured_layout_epoch = self.layout_epoch;
        self.last_measured_layout_generation = layout_generation;
    }
}

#[must_use]
pub fn is_execute_tool_name(tool_name: &str) -> bool {
    tool_name.eq_ignore_ascii_case("bash")
}

#[must_use]
pub fn is_ask_question_tool_name(tool_name: &str) -> bool {
    tool_name.eq_ignore_ascii_case("askuserquestion")
}

/// Permission state stored inline on a `ToolCallInfo`, so the permission
/// controls render inside the tool call block (unified edit/permission UX).
pub struct InlinePermission {
    pub options: Vec<model::PermissionOption>,
    pub response_tx: tokio::sync::oneshot::Sender<model::RequestPermissionResponse>,
    pub selected_index: usize,
    /// Whether this permission currently has keyboard focus.
    /// When multiple permissions are pending, only the focused one
    /// shows the selection arrow and accepts Left/Right/Enter input.
    pub focused: bool,
}

#[cfg(test)]
mod tests {
    // =====
    // TESTS: 26
    // =====

    use super::*;
    use pretty_assertions::assert_eq;
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    // BlockCache

    #[test]
    fn cache_default_returns_none() {
        let cache = BlockCache::default();
        assert!(cache.get().is_none());
    }

    #[test]
    fn cache_store_then_get() {
        let mut cache = BlockCache::default();
        cache.store(vec![Line::from("hello")]);
        assert!(cache.get().is_some());
        assert_eq!(cache.get().unwrap().len(), 1);
    }

    #[test]
    fn cache_invalidate_then_get_returns_none() {
        let mut cache = BlockCache::default();
        cache.store(vec![Line::from("data")]);
        cache.invalidate();
        assert!(cache.get().is_none());
    }

    // BlockCache

    #[test]
    fn cache_store_after_invalidate() {
        let mut cache = BlockCache::default();
        cache.store(vec![Line::from("old")]);
        cache.invalidate();
        assert!(cache.get().is_none());
        cache.store(vec![Line::from("new")]);
        let lines = cache.get().unwrap();
        assert_eq!(lines.len(), 1);
        let span_content: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(span_content, "new");
    }

    #[test]
    fn cache_multiple_invalidations() {
        let mut cache = BlockCache::default();
        cache.store(vec![Line::from("data")]);
        cache.invalidate();
        cache.invalidate();
        cache.invalidate();
        assert!(cache.get().is_none());
        cache.store(vec![Line::from("fresh")]);
        assert!(cache.get().is_some());
    }

    #[test]
    fn cache_store_empty_lines() {
        let mut cache = BlockCache::default();
        cache.store(Vec::new());
        let lines = cache.get().unwrap();
        assert!(lines.is_empty());
    }

    /// Store twice without invalidating - second store overwrites first.
    #[test]
    fn cache_store_overwrite_without_invalidate() {
        let mut cache = BlockCache::default();
        cache.store(vec![Line::from("first")]);
        cache.store(vec![Line::from("second"), Line::from("line2")]);
        let lines = cache.get().unwrap();
        assert_eq!(lines.len(), 2);
        let content: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(content, "second");
    }

    /// `get()` called twice returns consistent data.
    #[test]
    fn cache_get_twice_consistent() {
        let mut cache = BlockCache::default();
        cache.store(vec![Line::from("stable")]);
        let first = cache.get().unwrap().len();
        let second = cache.get().unwrap().len();
        assert_eq!(first, second);
    }

    // BlockCache

    #[test]
    fn cache_store_many_lines() {
        let mut cache = BlockCache::default();
        let lines: Vec<Line<'static>> =
            (0..1000).map(|i| Line::from(Span::raw(format!("line {i}")))).collect();
        cache.store(lines);
        assert_eq!(cache.get().unwrap().len(), 1000);
    }

    #[test]
    fn cache_store_splits_into_kb_segments() {
        let mut cache = BlockCache::default();
        let long = "x".repeat(800);
        let lines: Vec<Line<'static>> = (0..12).map(|_| Line::from(long.clone())).collect();
        cache.store(lines);
        assert!(cache.segment_count() > 1);
        assert!(cache.cached_bytes() > 0);
    }

    #[test]
    fn cache_invalidate_without_store() {
        let mut cache = BlockCache::default();
        cache.invalidate();
        assert!(cache.get().is_none());
    }

    #[test]
    fn cache_rapid_store_invalidate_cycle() {
        let mut cache = BlockCache::default();
        for i in 0..50 {
            cache.store(vec![Line::from(format!("v{i}"))]);
            assert!(cache.get().is_some());
            cache.invalidate();
            assert!(cache.get().is_none());
        }
        cache.store(vec![Line::from("final")]);
        assert!(cache.get().is_some());
    }

    /// Store styled lines with multiple spans per line.
    #[test]
    fn cache_store_styled_lines() {
        let mut cache = BlockCache::default();
        let line = Line::from(vec![
            Span::styled("bold", Style::default().fg(Color::Red)),
            Span::raw(" normal "),
            Span::styled("blue", Style::default().fg(Color::Blue)),
        ]);
        cache.store(vec![line]);
        let lines = cache.get().unwrap();
        assert_eq!(lines[0].spans.len(), 3);
    }

    /// Version counter after many invalidations - verify it doesn't
    /// accidentally wrap to 0 (which would make stale data appear fresh).
    /// With u64, 10K invalidations is nowhere near overflow.
    #[test]
    fn cache_version_no_false_fresh_after_many_invalidations() {
        let mut cache = BlockCache::default();
        cache.store(vec![Line::from("data")]);
        for _ in 0..10_000 {
            cache.invalidate();
        }
        // Cache was invalidated 10K times without re-storing - must be stale
        assert!(cache.get().is_none());
    }

    /// Invalidate, store, invalidate, store - alternating pattern.
    #[test]
    fn cache_alternating_invalidate_store() {
        let mut cache = BlockCache::default();
        for i in 0..100 {
            cache.invalidate();
            assert!(cache.get().is_none(), "stale after invalidate at iter {i}");
            cache.store(vec![Line::from(format!("v{i}"))]);
            assert!(cache.get().is_some(), "fresh after store at iter {i}");
        }
    }

    // BlockCache height

    #[test]
    fn cache_height_default_returns_none() {
        let cache = BlockCache::default();
        assert!(cache.height_at(80).is_none());
    }

    #[test]
    fn cache_store_with_height_then_height_at() {
        let mut cache = BlockCache::default();
        cache.store_with_height(vec![Line::from("hello")], 1, 80);
        assert_eq!(cache.height_at(80), Some(1));
        assert!(cache.get().is_some());
    }

    #[test]
    fn cache_height_at_wrong_width_returns_none() {
        let mut cache = BlockCache::default();
        cache.store_with_height(vec![Line::from("hello")], 1, 80);
        assert!(cache.height_at(120).is_none());
    }

    #[test]
    fn cache_height_invalidated_returns_none() {
        let mut cache = BlockCache::default();
        cache.store_with_height(vec![Line::from("hello")], 1, 80);
        cache.invalidate();
        assert!(cache.height_at(80).is_none());
    }

    #[test]
    fn cache_store_without_height_has_no_height() {
        let mut cache = BlockCache::default();
        cache.store(vec![Line::from("hello")]);
        // store() without height leaves wrapped_width at 0
        assert!(cache.height_at(80).is_none());
    }

    #[test]
    fn cache_store_with_height_overwrite() {
        let mut cache = BlockCache::default();
        cache.store_with_height(vec![Line::from("old")], 1, 80);
        cache.invalidate();
        cache.store_with_height(vec![Line::from("new long line")], 3, 120);
        assert_eq!(cache.height_at(120), Some(3));
        assert!(cache.height_at(80).is_none());
    }

    // BlockCache set_height (separate from store)

    #[test]
    fn cache_set_height_after_store() {
        let mut cache = BlockCache::default();
        cache.store(vec![Line::from("hello")]);
        assert!(cache.height_at(80).is_none()); // no height yet
        cache.set_height(1, 80);
        assert_eq!(cache.height_at(80), Some(1));
        assert!(cache.get().is_some()); // lines still valid
    }

    #[test]
    fn cache_set_height_update_width() {
        let mut cache = BlockCache::default();
        cache.store(vec![Line::from("hello world")]);
        cache.set_height(1, 80);
        assert_eq!(cache.height_at(80), Some(1));
        // Re-measure at new width
        cache.set_height(2, 40);
        assert_eq!(cache.height_at(40), Some(2));
        assert!(cache.height_at(80).is_none()); // old width no longer valid
    }

    #[test]
    fn cache_set_height_invalidate_clears_height() {
        let mut cache = BlockCache::default();
        cache.store(vec![Line::from("data")]);
        cache.set_height(3, 80);
        cache.invalidate();
        assert!(cache.height_at(80).is_none()); // version mismatch
    }

    #[test]
    fn cache_set_height_on_invalidated_cache_returns_none() {
        let mut cache = BlockCache::default();
        cache.store(vec![Line::from("data")]);
        cache.invalidate(); // version != 0
        cache.set_height(5, 80);
        // height_at returns None because cache is stale (version != 0)
        assert!(cache.height_at(80).is_none());
    }

    #[test]
    fn cache_store_then_set_height_matches_store_with_height() {
        let mut cache_a = BlockCache::default();
        cache_a.store(vec![Line::from("test")]);
        cache_a.set_height(2, 100);

        let mut cache_b = BlockCache::default();
        cache_b.store_with_height(vec![Line::from("test")], 2, 100);

        assert_eq!(cache_a.height_at(100), cache_b.height_at(100));
        assert_eq!(cache_a.get().unwrap().len(), cache_b.get().unwrap().len());
    }

    #[test]
    fn cache_measure_and_set_height_from_segments() {
        let mut cache = BlockCache::default();
        let lines = vec![
            Line::from("alpha beta gamma delta epsilon"),
            Line::from("zeta eta theta iota kappa lambda"),
            Line::from("mu nu xi omicron pi rho sigma"),
        ];
        cache.store(lines.clone());
        let measured = cache.measure_and_set_height(16).expect("expected measured height");
        let expected = ratatui::widgets::Paragraph::new(ratatui::text::Text::from(lines))
            .wrap(ratatui::widgets::Wrap { trim: false })
            .line_count(16);
        assert_eq!(measured, expected);
        assert_eq!(cache.height_at(16), Some(expected));
    }

    #[test]
    fn cache_get_updates_last_access_tick() {
        let mut cache = BlockCache::default();
        cache.store(vec![Line::from("tick")]);
        let before = cache.last_access_tick();
        let _ = cache.get();
        let after = cache.last_access_tick();
        assert!(after > before);
    }

    // App tool_call_index

    fn make_test_app() -> App {
        App::test_default()
    }

    fn assistant_text_block(text: &str) -> MessageBlock {
        MessageBlock::Text(
            text.to_owned(),
            BlockCache::default(),
            IncrementalMarkdown::from_complete(text),
        )
    }

    fn user_text_message(text: &str) -> ChatMessage {
        ChatMessage {
            role: MessageRole::User,
            blocks: vec![assistant_text_block(text)],
            usage: None,
        }
    }

    fn assistant_tool_message(id: &str, status: model::ToolCallStatus) -> ChatMessage {
        ChatMessage {
            role: MessageRole::Assistant,
            blocks: vec![MessageBlock::ToolCall(Box::new(ToolCallInfo {
                id: id.to_owned(),
                title: format!("tool {id}"),
                sdk_tool_name: "Read".to_owned(),
                raw_input: None,
                status,
                content: Vec::new(),
                collapsed: false,
                hidden: false,
                terminal_id: None,
                terminal_command: None,
                terminal_output: Some("x".repeat(1024)),
                terminal_output_len: 1024,
                terminal_bytes_seen: 1024,
                terminal_snapshot_mode: TerminalSnapshotMode::AppendOnly,
                render_epoch: 0,
                layout_epoch: 0,
                last_measured_width: 0,
                last_measured_height: 0,
                last_measured_layout_epoch: 0,
                last_measured_layout_generation: 0,
                cache: BlockCache::default(),
                pending_permission: None,
            }))],
            usage: None,
        }
    }

    fn assistant_tool_message_with_pending_permission(id: &str) -> ChatMessage {
        let (tx, _rx) = tokio::sync::oneshot::channel();
        ChatMessage {
            role: MessageRole::Assistant,
            blocks: vec![MessageBlock::ToolCall(Box::new(ToolCallInfo {
                id: id.to_owned(),
                title: format!("tool {id}"),
                sdk_tool_name: "Read".to_owned(),
                raw_input: None,
                status: model::ToolCallStatus::Completed,
                content: Vec::new(),
                collapsed: false,
                hidden: false,
                terminal_id: None,
                terminal_command: None,
                terminal_output: Some("x".repeat(1024)),
                terminal_output_len: 1024,
                terminal_bytes_seen: 1024,
                terminal_snapshot_mode: TerminalSnapshotMode::AppendOnly,
                render_epoch: 0,
                layout_epoch: 0,
                last_measured_width: 0,
                last_measured_height: 0,
                last_measured_layout_epoch: 0,
                last_measured_layout_generation: 0,
                cache: BlockCache::default(),
                pending_permission: Some(InlinePermission {
                    options: vec![model::PermissionOption::new(
                        "allow-once",
                        "Allow once",
                        model::PermissionOptionKind::AllowOnce,
                    )],
                    response_tx: tx,
                    selected_index: 0,
                    focused: false,
                }),
            }))],
            usage: None,
        }
    }

    #[test]
    fn enforce_render_cache_budget_evicts_lru_block() {
        let mut app = make_test_app();
        app.messages = vec![
            ChatMessage {
                role: MessageRole::Assistant,
                blocks: vec![assistant_text_block("a")],
                usage: None,
            },
            ChatMessage {
                role: MessageRole::Assistant,
                blocks: vec![assistant_text_block("b")],
                usage: None,
            },
        ];

        let bytes_a = if let MessageBlock::Text(_, cache, _) = &mut app.messages[0].blocks[0] {
            cache.store(vec![Line::from("x".repeat(2200))]);
            cache.cached_bytes()
        } else {
            0
        };
        let bytes_b = if let MessageBlock::Text(_, cache, _) = &mut app.messages[1].blocks[0] {
            cache.store(vec![Line::from("y".repeat(2200))]);
            let _ = cache.get();
            cache.cached_bytes()
        } else {
            0
        };

        app.render_cache_budget.max_bytes = bytes_b;
        let stats = app.enforce_render_cache_budget();
        assert!(stats.evicted_blocks >= 1);
        assert!(stats.evicted_bytes >= bytes_a);
        assert!(stats.total_after_bytes <= app.render_cache_budget.max_bytes);

        if let MessageBlock::Text(_, cache, _) = &app.messages[0].blocks[0] {
            assert_eq!(cache.cached_bytes(), 0);
        } else {
            panic!("expected text block");
        }
        if let MessageBlock::Text(_, cache, _) = &app.messages[1].blocks[0] {
            assert_eq!(cache.cached_bytes(), bytes_b);
        } else {
            panic!("expected text block");
        }
    }

    #[test]
    fn enforce_render_cache_budget_protects_streaming_tail_message() {
        let mut app = make_test_app();
        app.status = AppStatus::Thinking;
        app.messages = vec![ChatMessage {
            role: MessageRole::Assistant,
            blocks: vec![assistant_text_block("streaming tail")],
            usage: None,
        }];

        let before = if let MessageBlock::Text(_, cache, _) = &mut app.messages[0].blocks[0] {
            cache.store(vec![Line::from("z".repeat(4096))]);
            cache.cached_bytes()
        } else {
            0
        };
        app.render_cache_budget.max_bytes = 64;
        let stats = app.enforce_render_cache_budget();
        assert_eq!(stats.evicted_blocks, 0);
        assert_eq!(stats.evicted_bytes, 0);

        if let MessageBlock::Text(_, cache, _) = &app.messages[0].blocks[0] {
            assert_eq!(cache.cached_bytes(), before);
        } else {
            panic!("expected text block");
        }
    }

    #[test]
    fn enforce_history_retention_noop_under_budget() {
        let mut app = make_test_app();
        app.messages = vec![
            ChatMessage::welcome("model", "/cwd"),
            user_text_message("small message"),
            user_text_message("another message"),
        ];
        app.history_retention.max_bytes = usize::MAX / 4;

        let stats = app.enforce_history_retention();
        assert_eq!(stats.dropped_messages, 0);
        assert_eq!(stats.total_dropped_messages, 0);
        assert!(!app.messages.iter().any(App::is_history_hidden_marker_message));
    }

    #[test]
    fn enforce_history_retention_drops_oldest_and_adds_marker() {
        let mut app = make_test_app();
        app.messages = vec![
            ChatMessage::welcome("model", "/cwd"),
            user_text_message("first old message"),
            user_text_message("second old message"),
            user_text_message("third old message"),
        ];
        app.history_retention.max_bytes = 1;

        let stats = app.enforce_history_retention();
        assert_eq!(stats.dropped_messages, 3);
        assert!(matches!(app.messages[0].role, MessageRole::Welcome));
        assert!(app.messages.iter().any(App::is_history_hidden_marker_message));
        assert_eq!(app.messages.len(), 2);
    }

    #[test]
    fn enforce_history_retention_preserves_in_progress_tool_message() {
        let mut app = make_test_app();
        app.messages = vec![
            ChatMessage::welcome("model", "/cwd"),
            user_text_message("droppable"),
            assistant_tool_message("tool-keep", model::ToolCallStatus::InProgress),
        ];
        app.history_retention.max_bytes = 1;

        let stats = app.enforce_history_retention();
        assert_eq!(stats.dropped_messages, 1);
        assert!(app.messages.iter().any(|msg| {
            msg.blocks.iter().any(|block| {
                matches!(
                    block,
                    MessageBlock::ToolCall(tc) if tc.id == "tool-keep"
                        && matches!(tc.status, model::ToolCallStatus::InProgress)
                )
            })
        }));
    }

    #[test]
    fn enforce_history_retention_preserves_pending_tool_message() {
        let mut app = make_test_app();
        app.messages = vec![
            ChatMessage::welcome("model", "/cwd"),
            user_text_message("droppable"),
            assistant_tool_message("tool-pending", model::ToolCallStatus::Pending),
        ];
        app.history_retention.max_bytes = 1;

        let stats = app.enforce_history_retention();
        assert_eq!(stats.dropped_messages, 1);
        assert!(app.messages.iter().any(|msg| {
            msg.blocks
                .iter()
                .any(|block| matches!(block, MessageBlock::ToolCall(tc) if tc.id == "tool-pending"))
        }));
    }

    #[test]
    fn enforce_history_retention_preserves_permission_tool_message() {
        let mut app = make_test_app();
        app.messages = vec![
            ChatMessage::welcome("model", "/cwd"),
            user_text_message("droppable"),
            assistant_tool_message_with_pending_permission("tool-perm"),
        ];
        app.history_retention.max_bytes = 1;

        let stats = app.enforce_history_retention();
        assert_eq!(stats.dropped_messages, 1);
        assert!(app.messages.iter().any(|msg| {
            msg.blocks
                .iter()
                .any(|block| matches!(block, MessageBlock::ToolCall(tc) if tc.id == "tool-perm"))
        }));
    }

    #[test]
    fn enforce_history_retention_rebuilds_tool_index_after_prune() {
        let mut app = make_test_app();
        app.messages = vec![
            ChatMessage::welcome("model", "/cwd"),
            user_text_message("drop this"),
            assistant_tool_message("tool-idx", model::ToolCallStatus::InProgress),
        ];
        app.index_tool_call("tool-idx".to_owned(), 99, 99);
        app.history_retention.max_bytes = 1;

        let _ = app.enforce_history_retention();
        assert_eq!(app.lookup_tool_call("tool-idx"), Some((2, 0)));
    }

    #[test]
    fn enforce_history_retention_keeps_single_marker_on_repeat() {
        let mut app = make_test_app();
        app.messages = vec![ChatMessage::welcome("model", "/cwd"), user_text_message("drop me")];
        app.history_retention.max_bytes = 1;

        let first = app.enforce_history_retention();
        let second = app.enforce_history_retention();
        let marker_count =
            app.messages.iter().filter(|msg| App::is_history_hidden_marker_message(msg)).count();

        assert_eq!(first.dropped_messages, 1);
        assert_eq!(second.dropped_messages, 0);
        assert_eq!(marker_count, 1);
    }

    #[test]
    fn lookup_missing_returns_none() {
        let app = make_test_app();
        assert!(app.lookup_tool_call("nonexistent").is_none());
    }

    #[test]
    fn index_and_lookup() {
        let mut app = make_test_app();
        app.index_tool_call("tc-123".into(), 2, 5);
        assert_eq!(app.lookup_tool_call("tc-123"), Some((2, 5)));
    }

    // App tool_call_index

    /// Index same ID twice - second write overwrites first.
    #[test]
    fn index_overwrite_existing() {
        let mut app = make_test_app();
        app.index_tool_call("tc-1".into(), 0, 0);
        app.index_tool_call("tc-1".into(), 5, 10);
        assert_eq!(app.lookup_tool_call("tc-1"), Some((5, 10)));
    }

    /// Empty string as tool call ID.
    #[test]
    fn index_empty_string_id() {
        let mut app = make_test_app();
        app.index_tool_call(String::new(), 1, 2);
        assert_eq!(app.lookup_tool_call(""), Some((1, 2)));
    }

    /// Stress: 1000 tool calls indexed and looked up.
    #[test]
    fn index_stress_1000_entries() {
        let mut app = make_test_app();
        for i in 0..1000 {
            app.index_tool_call(format!("tc-{i}"), i, i * 2);
        }
        // Spot check first, middle, last
        assert_eq!(app.lookup_tool_call("tc-0"), Some((0, 0)));
        assert_eq!(app.lookup_tool_call("tc-500"), Some((500, 1000)));
        assert_eq!(app.lookup_tool_call("tc-999"), Some((999, 1998)));
        // Non-existent still returns None
        assert!(app.lookup_tool_call("tc-1000").is_none());
    }

    /// Unicode in tool call ID.
    #[test]
    fn index_unicode_id() {
        let mut app = make_test_app();
        app.index_tool_call("\u{1F600}-tool".into(), 3, 7);
        assert_eq!(app.lookup_tool_call("\u{1F600}-tool"), Some((3, 7)));
    }

    // active_task_ids

    #[test]
    fn active_task_insert_remove() {
        let mut app = make_test_app();
        app.insert_active_task("task-1".into());
        assert!(app.active_task_ids.contains("task-1"));
        app.remove_active_task("task-1");
        assert!(!app.active_task_ids.contains("task-1"));
    }

    #[test]
    fn remove_nonexistent_task_is_noop() {
        let mut app = make_test_app();
        app.remove_active_task("does-not-exist");
        assert!(app.active_task_ids.is_empty());
    }

    // active_task_ids

    /// Insert same ID twice - set deduplicates; one remove clears it.
    #[test]
    fn active_task_insert_duplicate() {
        let mut app = make_test_app();
        app.insert_active_task("task-1".into());
        app.insert_active_task("task-1".into());
        assert_eq!(app.active_task_ids.len(), 1);
        app.remove_active_task("task-1");
        assert!(app.active_task_ids.is_empty());
    }

    /// Insert many tasks, remove in different order.
    #[test]
    fn active_task_insert_many_remove_out_of_order() {
        let mut app = make_test_app();
        for i in 0..100 {
            app.insert_active_task(format!("task-{i}"));
        }
        assert_eq!(app.active_task_ids.len(), 100);
        // Remove in reverse order
        for i in (0..100).rev() {
            app.remove_active_task(&format!("task-{i}"));
        }
        assert!(app.active_task_ids.is_empty());
    }

    /// Mixed insert/remove interleaving.
    #[test]
    fn active_task_interleaved_insert_remove() {
        let mut app = make_test_app();
        app.insert_active_task("a".into());
        app.insert_active_task("b".into());
        app.remove_active_task("a");
        app.insert_active_task("c".into());
        assert!(!app.active_task_ids.contains("a"));
        assert!(app.active_task_ids.contains("b"));
        assert!(app.active_task_ids.contains("c"));
        assert_eq!(app.active_task_ids.len(), 2);
    }

    /// Remove from empty set multiple times - no panic.
    #[test]
    fn active_task_remove_from_empty_repeatedly() {
        let mut app = make_test_app();
        for i in 0..100 {
            app.remove_active_task(&format!("ghost-{i}"));
        }
        assert!(app.active_task_ids.is_empty());
    }

    // IncrementalMarkdown

    /// Simple render function for tests: wraps each line in a `Line`.
    fn test_render(src: &str) -> Vec<Line<'static>> {
        src.lines().map(|l| Line::from(l.to_owned())).collect()
    }

    #[test]
    fn incr_default_empty() {
        let incr = IncrementalMarkdown::default();
        assert!(incr.full_text().is_empty());
    }

    #[test]
    fn incr_from_complete() {
        let incr = IncrementalMarkdown::from_complete("hello world");
        assert_eq!(incr.full_text(), "hello world");
    }

    #[test]
    fn incr_append_single_chunk() {
        let mut incr = IncrementalMarkdown::default();
        incr.append("hello");
        assert_eq!(incr.full_text(), "hello");
    }

    #[test]
    fn incr_append_accumulates_chunks() {
        let mut incr = IncrementalMarkdown::default();
        incr.append("line1");
        incr.append("\nline2");
        incr.append("\nline3");
        assert_eq!(incr.full_text(), "line1\nline2\nline3");
    }

    #[test]
    fn incr_append_preserves_paragraph_delimiters() {
        let mut incr = IncrementalMarkdown::default();
        incr.append("para1\n\npara2");
        assert_eq!(incr.full_text(), "para1\n\npara2");
    }

    #[test]
    fn incr_full_text_reconstruction() {
        let mut incr = IncrementalMarkdown::default();
        incr.append("p1\n\np2\n\np3");
        assert_eq!(incr.full_text(), "p1\n\np2\n\np3");
    }

    #[test]
    fn incr_lines_renders_all() {
        let mut incr = IncrementalMarkdown::default();
        incr.append("line1\n\nline2\n\nline3");
        let lines = incr.lines(&test_render);
        // test_render maps each source line to one output line
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn incr_ensure_rendered_noop_preserves_text() {
        let mut incr = IncrementalMarkdown::default();
        incr.append("p1\n\np2\n\ntail");
        incr.ensure_rendered(&test_render);
        assert_eq!(incr.full_text(), "p1\n\np2\n\ntail");
    }

    #[test]
    fn incr_invalidate_renders_noop_preserves_text() {
        let mut incr = IncrementalMarkdown::default();
        incr.append("p1\n\np2\n\ntail");
        incr.invalidate_renders();
        assert_eq!(incr.full_text(), "p1\n\np2\n\ntail");
    }

    #[test]
    fn incr_streaming_simulation() {
        // Simulate a realistic streaming scenario
        let mut incr = IncrementalMarkdown::default();
        let chunks = ["Here is ", "some text.\n", "\nNext para", "graph here.\n\n", "Final."];
        for chunk in chunks {
            incr.append(chunk);
        }
        assert_eq!(incr.full_text(), "Here is some text.\n\nNext paragraph here.\n\nFinal.");
    }

    // ChatViewport

    #[test]
    fn viewport_new_defaults() {
        let vp = ChatViewport::new();
        assert_eq!(vp.scroll_offset, 0);
        assert_eq!(vp.scroll_target, 0);
        assert!(vp.auto_scroll);
        assert_eq!(vp.width, 0);
        assert!(vp.message_heights.is_empty());
        assert!(vp.dirty_from.is_none());
        assert!(vp.height_prefix_sums.is_empty());
    }

    #[test]
    fn viewport_on_frame_sets_width() {
        let mut vp = ChatViewport::new();
        vp.on_frame(80);
        assert_eq!(vp.width, 80);
    }

    #[test]
    fn viewport_on_frame_resize_invalidates() {
        let mut vp = ChatViewport::new();
        vp.on_frame(80);
        vp.set_message_height(0, 10);
        vp.set_message_height(1, 20);
        vp.rebuild_prefix_sums();

        // Resize: old heights are kept as approximations,
        // but width markers are invalidated so re-measurement happens.
        vp.on_frame(120);
        assert_eq!(vp.message_height(0), 10); // kept, not zeroed
        assert_eq!(vp.message_height(1), 20); // kept, not zeroed
        assert_eq!(vp.message_heights_width, 0); // forces re-measure
        assert_eq!(vp.prefix_sums_width, 0); // forces rebuild
    }

    #[test]
    fn viewport_on_frame_same_width_no_invalidation() {
        let mut vp = ChatViewport::new();
        vp.on_frame(80);
        vp.set_message_height(0, 10);
        vp.on_frame(80); // same width
        assert_eq!(vp.message_height(0), 10); // not zeroed
    }

    #[test]
    fn viewport_message_height_set_and_get() {
        let mut vp = ChatViewport::new();
        vp.on_frame(80);
        vp.set_message_height(0, 5);
        vp.set_message_height(1, 10);
        assert_eq!(vp.message_height(0), 5);
        assert_eq!(vp.message_height(1), 10);
        assert_eq!(vp.message_height(2), 0); // out of bounds
    }

    #[test]
    fn viewport_message_height_grows_vec() {
        let mut vp = ChatViewport::new();
        vp.on_frame(80);
        vp.set_message_height(5, 42);
        assert_eq!(vp.message_heights.len(), 6);
        assert_eq!(vp.message_height(5), 42);
        assert_eq!(vp.message_height(3), 0); // gap filled with 0
    }

    #[test]
    fn viewport_mark_message_dirty_tracks_oldest_index() {
        let mut vp = ChatViewport::new();
        vp.mark_message_dirty(5);
        vp.mark_message_dirty(2);
        vp.mark_message_dirty(7);
        assert_eq!(vp.dirty_from, Some(2));
    }

    #[test]
    fn viewport_mark_heights_valid_clears_dirty_index() {
        let mut vp = ChatViewport::new();
        vp.on_frame(80);
        vp.mark_message_dirty(1);
        assert_eq!(vp.dirty_from, Some(1));
        vp.mark_heights_valid();
        assert!(vp.dirty_from.is_none());
    }

    #[test]
    fn viewport_prefix_sums_basic() {
        let mut vp = ChatViewport::new();
        vp.on_frame(80);
        vp.set_message_height(0, 5);
        vp.set_message_height(1, 10);
        vp.set_message_height(2, 3);
        vp.rebuild_prefix_sums();
        assert_eq!(vp.total_message_height(), 18);
        assert_eq!(vp.cumulative_height_before(0), 0);
        assert_eq!(vp.cumulative_height_before(1), 5);
        assert_eq!(vp.cumulative_height_before(2), 15);
    }

    #[test]
    fn viewport_prefix_sums_streaming_fast_path() {
        let mut vp = ChatViewport::new();
        vp.on_frame(80);
        vp.set_message_height(0, 5);
        vp.set_message_height(1, 10);
        vp.rebuild_prefix_sums();
        assert_eq!(vp.total_message_height(), 15);

        // Simulate streaming: last message grows
        vp.set_message_height(1, 20);
        vp.rebuild_prefix_sums(); // should hit fast path
        assert_eq!(vp.total_message_height(), 25);
        assert_eq!(vp.cumulative_height_before(1), 5);
    }

    #[test]
    fn viewport_find_first_visible() {
        let mut vp = ChatViewport::new();
        vp.on_frame(80);
        vp.set_message_height(0, 10);
        vp.set_message_height(1, 10);
        vp.set_message_height(2, 10);
        vp.rebuild_prefix_sums();

        assert_eq!(vp.find_first_visible(0), 0);
        assert_eq!(vp.find_first_visible(10), 1);
        assert_eq!(vp.find_first_visible(15), 1);
        assert_eq!(vp.find_first_visible(20), 2);
    }

    #[test]
    fn viewport_find_first_visible_handles_offsets_before_first_boundary() {
        let mut vp = ChatViewport::new();
        vp.on_frame(80);
        vp.set_message_height(0, 10);
        vp.set_message_height(1, 10);
        vp.rebuild_prefix_sums();

        assert_eq!(vp.find_first_visible(0), 0);
        assert_eq!(vp.find_first_visible(5), 0);
        assert_eq!(vp.find_first_visible(15), 1);
    }

    #[test]
    fn viewport_scroll_up_down() {
        let mut vp = ChatViewport::new();
        vp.scroll_target = 20;
        vp.auto_scroll = true;

        vp.scroll_up(5);
        assert_eq!(vp.scroll_target, 15);
        assert!(!vp.auto_scroll); // disabled on manual scroll

        vp.scroll_down(3);
        assert_eq!(vp.scroll_target, 18);
        assert!(!vp.auto_scroll); // not re-engaged by scroll_down
    }

    #[test]
    fn viewport_scroll_up_saturates() {
        let mut vp = ChatViewport::new();
        vp.scroll_target = 2;
        vp.scroll_up(10);
        assert_eq!(vp.scroll_target, 0);
    }

    #[test]
    fn viewport_engage_auto_scroll() {
        let mut vp = ChatViewport::new();
        vp.auto_scroll = false;
        vp.engage_auto_scroll();
        assert!(vp.auto_scroll);
    }

    #[test]
    fn viewport_default_eq_new() {
        let a = ChatViewport::new();
        let b = ChatViewport::default();
        assert_eq!(a.width, b.width);
        assert_eq!(a.auto_scroll, b.auto_scroll);
        assert_eq!(a.message_heights.len(), b.message_heights.len());
    }

    #[test]
    fn session_usage_total_tokens_excludes_cache_tokens() {
        let usage = SessionUsageState {
            total_input_tokens: 1_000,
            total_output_tokens: 2_000,
            total_cache_read_tokens: 50_000,
            total_cache_write_tokens: 10_000,
            ..SessionUsageState::default()
        };

        assert_eq!(usage.total_tokens(), 3_000);
    }

    #[test]
    fn session_usage_context_used_tokens_ignores_compaction_pre_tokens() {
        let usage = SessionUsageState {
            latest_input_tokens: Some(3_000),
            latest_output_tokens: Some(1_500),
            latest_cache_read_tokens: Some(50_000),
            latest_cache_write_tokens: Some(10_000),
            last_compaction_pre_tokens: Some(190_000),
            ..SessionUsageState::default()
        };

        assert_eq!(usage.context_used_tokens(), Some(64_500));
    }

    #[test]
    fn session_usage_context_used_tokens_requires_latest_turn_snapshot() {
        let usage = SessionUsageState {
            total_input_tokens: 99_000,
            total_output_tokens: 11_000,
            ..SessionUsageState::default()
        };

        assert_eq!(usage.context_used_tokens(), None);
    }

    #[test]
    fn focus_owner_defaults_to_input() {
        let app = make_test_app();
        assert_eq!(app.focus_owner(), FocusOwner::Input);
    }

    #[test]
    fn focus_owner_todo_when_panel_open_and_focused() {
        let mut app = make_test_app();
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
    fn focus_owner_permission_overrides_todo() {
        let mut app = make_test_app();
        app.todos.push(TodoItem {
            content: "Task".into(),
            status: TodoStatus::Pending,
            active_form: String::new(),
        });
        app.show_todo_panel = true;
        app.claim_focus_target(FocusTarget::TodoList);
        app.pending_permission_ids.push("perm-1".into());
        app.claim_focus_target(FocusTarget::Permission);
        assert_eq!(app.focus_owner(), FocusOwner::Permission);
    }

    #[test]
    fn focus_owner_mention_overrides_permission_and_todo() {
        let mut app = make_test_app();
        app.todos.push(TodoItem {
            content: "Task".into(),
            status: TodoStatus::Pending,
            active_form: String::new(),
        });
        app.show_todo_panel = true;
        app.claim_focus_target(FocusTarget::TodoList);
        app.pending_permission_ids.push("perm-1".into());
        app.claim_focus_target(FocusTarget::Permission);
        app.mention = Some(mention::MentionState {
            trigger_row: 0,
            trigger_col: 0,
            query: String::new(),
            candidates: Vec::new(),
            dialog: super::super::dialog::DialogState::default(),
        });
        app.claim_focus_target(FocusTarget::Mention);
        assert_eq!(app.focus_owner(), FocusOwner::Mention);
    }

    #[test]
    fn focus_owner_falls_back_to_input_when_claim_is_not_available() {
        let mut app = make_test_app();
        app.claim_focus_target(FocusTarget::TodoList);
        assert_eq!(app.focus_owner(), FocusOwner::Input);
    }

    #[test]
    fn claim_and_release_focus_target() {
        let mut app = make_test_app();
        app.todos.push(TodoItem {
            content: "Task".into(),
            status: TodoStatus::Pending,
            active_form: String::new(),
        });
        app.show_todo_panel = true;
        app.claim_focus_target(FocusTarget::TodoList);
        assert_eq!(app.focus_owner(), FocusOwner::TodoList);
        app.release_focus_target(FocusTarget::TodoList);
        assert_eq!(app.focus_owner(), FocusOwner::Input);
    }

    #[test]
    fn latest_claim_wins_across_equal_targets() {
        let mut app = make_test_app();
        app.todos.push(TodoItem {
            content: "Task".into(),
            status: TodoStatus::Pending,
            active_form: String::new(),
        });
        app.show_todo_panel = true;
        app.mention = Some(mention::MentionState {
            trigger_row: 0,
            trigger_col: 0,
            query: String::new(),
            candidates: Vec::new(),
            dialog: super::super::dialog::DialogState::default(),
        });
        app.pending_permission_ids.push("perm-1".into());

        app.claim_focus_target(FocusTarget::TodoList);
        assert_eq!(app.focus_owner(), FocusOwner::TodoList);

        app.claim_focus_target(FocusTarget::Permission);
        assert_eq!(app.focus_owner(), FocusOwner::Permission);

        app.claim_focus_target(FocusTarget::Mention);
        assert_eq!(app.focus_owner(), FocusOwner::Mention);

        app.release_focus_target(FocusTarget::Mention);
        assert_eq!(app.focus_owner(), FocusOwner::Permission);
    }
}
