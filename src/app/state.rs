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
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
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
                        tc.cache.invalidate();
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
    /// Wrapped line count of the cached lines at `wrapped_width`.
    /// Computed via `Paragraph::line_count(width)` on the same lines stored in `lines`.
    wrapped_height: usize,
    /// The viewport width used to compute `wrapped_height`.
    wrapped_width: u16,
}

impl BlockCache {
    /// Bump the version to invalidate cached lines and height.
    pub fn invalidate(&mut self) {
        self.version += 1;
    }

    /// Get a reference to the cached lines, if fresh.
    #[must_use]
    pub fn get(&self) -> Option<&Vec<ratatui::text::Line<'static>>> {
        if self.version == 0 { self.lines.as_ref() } else { None }
    }

    /// Store freshly rendered lines, marking the cache as clean.
    /// Height is set separately via `set_height()` after measurement.
    pub fn store(&mut self, lines: Vec<ratatui::text::Line<'static>>) {
        self.lines = Some(lines);
        self.version = 0;
    }

    /// Set the wrapped height for the cached lines at the given width.
    /// Called by the viewport/chat layer after `Paragraph::line_count(width)`.
    /// Separate from `store()` so height measurement is the viewport's job.
    pub fn set_height(&mut self, height: usize, width: u16) {
        self.wrapped_height = height;
        self.wrapped_width = width;
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
        if self.version == 0 && self.wrapped_width == width {
            Some(self.wrapped_height)
        } else {
            None
        }
    }
}

/// Paragraph-level incremental markdown cache.
///
/// During streaming, text arrives in small chunks appended to a growing block.
/// Instead of re-parsing the entire block every frame, we split on paragraph
/// boundaries (`\n\n` outside code fences) and cache rendered lines for each
/// completed paragraph. Only the in-progress tail paragraph gets re-rendered.
#[derive(Default)]
pub struct IncrementalMarkdown {
    /// Completed paragraphs: `(source_text, rendered_lines)`.
    paragraphs: Vec<(String, Vec<ratatui::text::Line<'static>>)>,
    /// The in-progress tail paragraph being streamed into.
    tail: String,
    /// Whether we are currently inside a code fence.
    in_code_fence: bool,
    /// Byte offset into `tail` where the next scan should start.
    /// Avoids re-scanning already-processed bytes (which would re-toggle fence state).
    scan_offset: usize,
}

impl IncrementalMarkdown {
    /// Create from existing full text (e.g. user messages, connection errors).
    /// Treats the entire text as a single completed paragraph.
    #[must_use]
    pub fn from_complete(text: &str) -> Self {
        Self { paragraphs: Vec::new(), tail: text.to_owned(), in_code_fence: false, scan_offset: 0 }
    }

    /// Append a streaming text chunk. Splits completed paragraphs off the tail.
    pub fn append(&mut self, chunk: &str) {
        // Back up scan_offset by 1 to catch \n\n spanning old/new boundary
        self.scan_offset = self.scan_offset.min(self.tail.len().saturating_sub(1));
        self.tail.push_str(chunk);
        self.split_completed_paragraphs();
    }

    /// Get the full source text (all paragraphs + tail).
    #[must_use]
    pub fn full_text(&self) -> String {
        let mut out = String::new();
        for (src, _) in &self.paragraphs {
            out.push_str(src);
            out.push_str("\n\n");
        }
        out.push_str(&self.tail);
        out
    }

    /// Render all lines: cached paragraphs + fresh tail.
    /// `render_fn` converts a markdown source string into `Vec<Line>`.
    /// Lazily renders any paragraph whose cache is still empty.
    pub fn lines(
        &mut self,
        render_fn: &impl Fn(&str) -> Vec<ratatui::text::Line<'static>>,
    ) -> Vec<ratatui::text::Line<'static>> {
        let mut out = Vec::new();
        for (src, lines) in &mut self.paragraphs {
            if lines.is_empty() {
                *lines = render_fn(src);
            }
            out.extend(lines.iter().cloned());
        }
        if !self.tail.is_empty() {
            out.extend(render_fn(&self.tail));
        }
        out
    }

    /// Clear all cached paragraph renders (e.g. after toggle collapse).
    /// Source text is preserved; re-rendering will rebuild caches.
    pub fn invalidate_renders(&mut self) {
        for (src, lines) in &mut self.paragraphs {
            let _ = src; // keep source
            lines.clear();
        }
    }

    /// Re-render any paragraph whose cached lines are empty (after `invalidate_renders`).
    pub fn ensure_rendered(
        &mut self,
        render_fn: &impl Fn(&str) -> Vec<ratatui::text::Line<'static>>,
    ) {
        for (src, lines) in &mut self.paragraphs {
            if lines.is_empty() {
                *lines = render_fn(src);
            }
        }
    }

    /// Split completed paragraphs off the tail.
    /// A paragraph boundary is `\n\n` that is NOT inside a code fence.
    fn split_completed_paragraphs(&mut self) {
        loop {
            let (boundary, fence_state, scanned_to) = self.scan_tail_for_boundary();
            if let Some(offset) = boundary {
                let completed = self.tail[..offset].to_owned();
                self.tail = self.tail[offset + 2..].to_owned();
                self.in_code_fence = fence_state;
                // Reset scan_offset: the split removed bytes before the boundary,
                // so scanned_to is no longer valid. Start from 0 for the new tail.
                self.scan_offset = 0;
                self.paragraphs.push((completed, Vec::new()));
            } else {
                // No more boundaries -- save the final fence state + scan position
                self.in_code_fence = fence_state;
                self.scan_offset = scanned_to;
                break;
            }
        }
    }

    /// Scan `self.tail` starting from `self.scan_offset` for the first `\n\n`
    /// outside a code fence.
    /// Returns `(boundary, fence_state, scanned_to)`.
    fn scan_tail_for_boundary(&self) -> (Option<usize>, bool, usize) {
        let bytes = self.tail.as_bytes();
        let mut in_fence = self.in_code_fence;
        let mut i = self.scan_offset;
        while i < bytes.len() {
            // Check for code fence: line starting with ```
            if (i == 0 || bytes[i - 1] == b'\n') && bytes[i..].starts_with(b"```") {
                in_fence = !in_fence;
            }
            // Check for \n\n paragraph boundary (only outside code fences)
            if !in_fence && i + 1 < bytes.len() && bytes[i] == b'\n' && bytes[i + 1] == b'\n' {
                return (Some(i), in_fence, i);
            }
            i += 1;
        }
        (None, in_fence, i)
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
    /// Per-block render cache for this tool call.
    pub cache: BlockCache,
    /// Inline permission prompt - rendered inside this tool call block.
    pub pending_permission: Option<InlinePermission>,
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

    // App tool_call_index

    fn make_test_app() -> App {
        App::test_default()
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
    fn incr_append_no_paragraph_break() {
        let mut incr = IncrementalMarkdown::default();
        incr.append("line1\nline2\nline3");
        assert_eq!(incr.paragraphs.len(), 0);
        assert_eq!(incr.tail, "line1\nline2\nline3");
    }

    #[test]
    fn incr_append_splits_on_double_newline() {
        let mut incr = IncrementalMarkdown::default();
        incr.append("para1\n\npara2");
        assert_eq!(incr.paragraphs.len(), 1);
        assert_eq!(incr.paragraphs[0].0, "para1");
        assert_eq!(incr.tail, "para2");
    }

    #[test]
    fn incr_append_multiple_paragraphs() {
        let mut incr = IncrementalMarkdown::default();
        incr.append("p1\n\np2\n\np3\n\np4");
        assert_eq!(incr.paragraphs.len(), 3);
        assert_eq!(incr.paragraphs[0].0, "p1");
        assert_eq!(incr.paragraphs[1].0, "p2");
        assert_eq!(incr.paragraphs[2].0, "p3");
        assert_eq!(incr.tail, "p4");
    }

    #[test]
    fn incr_append_incremental_chunks() {
        let mut incr = IncrementalMarkdown::default();
        incr.append("hel");
        incr.append("lo\n");
        incr.append("\nworld");
        assert_eq!(incr.paragraphs.len(), 1);
        assert_eq!(incr.paragraphs[0].0, "hello");
        assert_eq!(incr.tail, "world");
    }

    #[test]
    fn incr_code_fence_preserves_double_newlines() {
        let mut incr = IncrementalMarkdown::default();
        incr.append("before\n\n```\ncode\n\nmore code\n```\n\nafter");
        // "before" split off, then code fence block stays as one paragraph
        assert_eq!(incr.paragraphs.len(), 2);
        assert_eq!(incr.paragraphs[0].0, "before");
        assert_eq!(incr.paragraphs[1].0, "```\ncode\n\nmore code\n```");
        assert_eq!(incr.tail, "after");
    }

    #[test]
    fn incr_code_fence_incremental() {
        let mut incr = IncrementalMarkdown::default();
        incr.append("text\n\n```\nfn main() {\n");
        assert_eq!(incr.paragraphs.len(), 1); // "text" split off
        assert!(incr.in_code_fence); // inside fence
        incr.append("    println!(\"hi\");\n\n}\n```\n\nafter");
        assert!(!incr.in_code_fence); // fence closed
        assert_eq!(incr.tail, "after");
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
        // 3 paragraphs total (2 completed + 1 tail), each has 1 line
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn incr_lines_caches_paragraphs() {
        let mut incr = IncrementalMarkdown::default();
        incr.append("p1\n\np2\n\ntail");
        // First call renders all paragraphs
        let _ = incr.lines(&test_render);
        assert!(!incr.paragraphs[0].1.is_empty());
        assert!(!incr.paragraphs[1].1.is_empty());
        // Second call reuses cached paragraph renders
        let lines = incr.lines(&test_render);
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn incr_ensure_rendered_fills_empty() {
        let mut incr = IncrementalMarkdown::default();
        incr.append("p1\n\np2\n\ntail");
        // Paragraphs have empty renders initially
        assert!(incr.paragraphs[0].1.is_empty());
        incr.ensure_rendered(&test_render);
        assert!(!incr.paragraphs[0].1.is_empty());
        assert!(!incr.paragraphs[1].1.is_empty());
    }

    #[test]
    fn incr_invalidate_clears_renders() {
        let mut incr = IncrementalMarkdown::default();
        incr.append("p1\n\np2\n\ntail");
        incr.ensure_rendered(&test_render);
        assert!(!incr.paragraphs[0].1.is_empty());
        incr.invalidate_renders();
        assert!(incr.paragraphs[0].1.is_empty());
        assert!(incr.paragraphs[1].1.is_empty());
    }

    #[test]
    fn incr_streaming_simulation() {
        // Simulate a realistic streaming scenario
        let mut incr = IncrementalMarkdown::default();
        let chunks = ["Here is ", "some text.\n", "\nNext para", "graph here.\n\n", "Final."];
        for chunk in chunks {
            incr.append(chunk);
        }
        assert_eq!(incr.paragraphs.len(), 2);
        assert_eq!(incr.paragraphs[0].0, "Here is some text.");
        assert_eq!(incr.paragraphs[1].0, "Next paragraph here.");
        assert_eq!(incr.tail, "Final.");
    }

    #[test]
    fn incr_empty_paragraphs() {
        let mut incr = IncrementalMarkdown::default();
        incr.append("\n\n\n\n");
        // Two \n\n boundaries: empty string before first, empty between, remaining empty tail
        assert!(!incr.paragraphs.is_empty());
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
