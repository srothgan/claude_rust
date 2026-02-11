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

use crate::acp::client::ClientEvent;
use agent_client_protocol as acp;
use std::collections::{HashMap, HashSet};
use tokio::sync::mpsc;

use super::input::InputState;
use super::mention;

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

#[allow(clippy::struct_excessive_bools)]
pub struct App {
    pub messages: Vec<ChatMessage>,
    /// Rendered scroll offset (rounded from `scroll_pos`).
    pub scroll_offset: usize,
    /// Target scroll offset requested by user input or auto-scroll.
    pub scroll_target: usize,
    /// Smooth scroll position (fractional) for animation.
    pub scroll_pos: f32,
    pub auto_scroll: bool,
    pub input: InputState,
    pub status: AppStatus,
    pub should_quit: bool,
    pub session_id: Option<acp::SessionId>,
    pub model_name: String,
    pub cwd: String,
    pub cwd_raw: String,
    pub files_accessed: usize,
    pub mode: Option<ModeState>,
    /// Tool call IDs with pending permission prompts, ordered by arrival.
    /// The first entry is the "focused" permission that receives keyboard input.
    /// Up / Down arrow keys cycle focus through the list.
    pub pending_permission_ids: Vec<String>,
    pub event_tx: mpsc::UnboundedSender<ClientEvent>,
    pub event_rx: mpsc::UnboundedReceiver<ClientEvent>,
    pub spinner_frame: usize,
    /// Session-level default for tool call collapsed state.
    /// Toggled by Ctrl+O — new tool calls inherit this value.
    pub tools_collapsed: bool,
    /// IDs of Task tool calls currently `InProgress` -- their children get hidden.
    /// Use `insert_active_task()`, `remove_active_task()`.
    pub(super) active_task_ids: HashSet<String>,
    /// Shared terminal process map — used to snapshot output on completion.
    pub terminals: crate::acp::client::TerminalMap,
    /// Force a full terminal clear on next render frame.
    pub force_redraw: bool,
    /// O(1) lookup: `tool_call_id` -> `(message_index, block_index)`.
    /// Use `lookup_tool_call()`, `index_tool_call()`.
    pub(super) tool_call_index: HashMap<String, (usize, usize)>,
    /// Current todo list from Claude's `TodoWrite` tool calls.
    pub todos: Vec<TodoItem>,
    /// Whether the todo panel is expanded (true) or shows compact status line (false).
    /// Toggled by Ctrl+T.
    pub show_todo_panel: bool,
    /// Scroll offset for the expanded todo panel (capped at 5 visible lines).
    pub todo_scroll: usize,
    /// Commands advertised by the agent via `AvailableCommandsUpdate`.
    pub available_commands: Vec<acp::AvailableCommand>,
    /// Last known frame area (for mouse selection mapping).
    pub cached_frame_area: ratatui::layout::Rect,
    /// Current selection state for mouse-based selection.
    pub selection: Option<SelectionState>,
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
    /// Cached file list from cwd (scanned on first `@` trigger).
    pub file_cache: Option<Vec<mention::FileCandidate>>,
    /// Cached welcome text lines (populated once, never changes after init).
    pub cached_welcome_lines: Option<Vec<ratatui::text::Line<'static>>>,
    /// Cached input wrap result (keyed by input version + width).
    pub input_wrap_cache: Option<InputWrapCache>,
    /// Cached todo compact line (invalidated on `set_todos()`).
    pub cached_todo_compact: Option<ratatui::text::Line<'static>>,
    /// Current git branch (refreshed on focus gain + turn complete).
    pub git_branch: Option<String>,
    /// Cached header line (invalidated when git branch changes).
    pub cached_header_line: Option<ratatui::text::Line<'static>>,
    /// Cached footer line (invalidated on mode change).
    pub cached_footer_line: Option<ratatui::text::Line<'static>>,
}

impl App {
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
}

#[derive(Debug)]
pub enum AppStatus {
    Ready,
    Thinking,
    Running,
    Error,
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

pub struct ChatMessage {
    pub role: MessageRole,
    pub blocks: Vec<MessageBlock>,
    /// Approximate wrapped visual height (in terminal rows) at `cached_visual_width`.
    /// Computed from the `Line` objects produced by `render_message` using
    /// `Paragraph::line_count(width)`. Used for viewport culling -- messages outside
    /// the visible window skip rendering and just contribute this height to the offset.
    pub cached_visual_height: usize,
    /// The viewport width used to compute `cached_visual_height`.
    /// When the terminal is resized, heights are recomputed.
    pub cached_visual_width: u16,
}

/// Cached result of `wrap_lines_and_cursor()` for the input field.
/// Keyed by input version + width so the expensive wrapping runs at most once per frame.
pub struct InputWrapCache {
    pub version: u64,
    pub content_width: u16,
    pub wrapped_lines: Vec<ratatui::text::Line<'static>>,
    pub cursor_pos: Option<(u16, u16)>,
}

/// Cached rendered lines for a block. Stores a version counter so the cache
/// is only recomputed when the block content actually changes.
///
/// Fields are private — use `invalidate()` to mark stale, `is_stale()` to check,
/// `get()` to read cached lines, and `store()` to populate.
#[derive(Default)]
pub struct BlockCache {
    version: u64,
    lines: Option<Vec<ratatui::text::Line<'static>>>,
}

impl BlockCache {
    /// Bump the version to invalidate cached lines.
    pub fn invalidate(&mut self) {
        self.version += 1;
    }

    /// Get a reference to the cached lines, if fresh.
    #[must_use]
    pub fn get(&self) -> Option<&Vec<ratatui::text::Line<'static>>> {
        if self.version == 0 { self.lines.as_ref() } else { None }
    }

    /// Store freshly rendered lines, marking the cache as clean.
    pub fn store(&mut self, lines: Vec<ratatui::text::Line<'static>>) {
        self.lines = Some(lines);
        self.version = 0;
    }
}

/// Ordered content block — text and tool calls interleaved as they arrive.
pub enum MessageBlock {
    Text(String, BlockCache),
    ToolCall(Box<ToolCallInfo>),
}

#[derive(Debug)]
pub enum MessageRole {
    User,
    Assistant,
}

pub struct ToolCallInfo {
    pub id: String,
    pub title: String,
    pub kind: acp::ToolKind,
    pub status: acp::ToolCallStatus,
    pub content: Vec<acp::ToolCallContent>,
    pub collapsed: bool,
    /// The actual Claude Code tool name from `meta.claudeCode.toolName`
    /// (e.g. "Task", "Glob", "`mcp__acp__Read`", "`WebSearch`")
    pub claude_tool_name: Option<String>,
    /// Hidden tool calls are subagent children — not rendered directly.
    pub hidden: bool,
    /// Terminal ID if this is an Execute tool call with a running/completed terminal.
    pub terminal_id: Option<String>,
    /// The shell command that was executed (e.g. "echo hello && ls -la").
    pub terminal_command: Option<String>,
    /// Snapshot of terminal output, updated each frame while `InProgress`.
    pub terminal_output: Option<String>,
    /// Length of terminal buffer at last snapshot — used to skip O(n) re-snapshots
    /// when the buffer hasn't grown.
    pub terminal_output_len: usize,
    /// Per-block render cache for this tool call.
    pub cache: BlockCache,
    /// Inline permission prompt — rendered inside this tool call block.
    pub pending_permission: Option<InlinePermission>,
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

    /// Store twice without invalidating — second store overwrites first.
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

    /// get() called twice returns consistent data.
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

    /// Version counter after many invalidations — verify it doesn't
    /// accidentally wrap to 0 (which would make stale data appear fresh).
    /// With u64, 10K invalidations is nowhere near overflow.
    #[test]
    fn cache_version_no_false_fresh_after_many_invalidations() {
        let mut cache = BlockCache::default();
        cache.store(vec![Line::from("data")]);
        for _ in 0..10_000 {
            cache.invalidate();
        }
        // Cache was invalidated 10K times without re-storing — must be stale
        assert!(cache.get().is_none());
    }

    /// Invalidate, store, invalidate, store — alternating pattern.
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

    // App tool_call_index

    fn make_test_app() -> App {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        App {
            messages: Vec::new(),
            scroll_offset: 0,
            scroll_target: 0,
            scroll_pos: 0.0,
            auto_scroll: true,
            input: InputState::new(),
            status: AppStatus::Ready,
            should_quit: false,
            session_id: None,
            model_name: "test".into(),
            cwd: "/test".into(),
            cwd_raw: "/test".into(),
            files_accessed: 0,
            mode: None,
            pending_permission_ids: Vec::new(),
            event_tx: tx,
            event_rx: rx,
            spinner_frame: 0,
            tools_collapsed: false,
            active_task_ids: Default::default(),
            terminals: Default::default(),
            force_redraw: false,
            tool_call_index: Default::default(),
            todos: Vec::new(),
            show_todo_panel: false,
            todo_scroll: 0,
            available_commands: Vec::new(),
            cached_frame_area: Default::default(),
            selection: None,
            rendered_chat_lines: Vec::new(),
            rendered_chat_area: Default::default(),
            rendered_input_lines: Vec::new(),
            rendered_input_area: Default::default(),
            mention: None,
            file_cache: None,
            cached_welcome_lines: None,
            input_wrap_cache: None,
            cached_todo_compact: None,
            git_branch: None,
            cached_header_line: None,
            cached_footer_line: None,
        }
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

    /// Index same ID twice — second write overwrites first.
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

    /// Insert same ID twice — set deduplicates; one remove clears it.
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

    /// Remove from empty set multiple times — no panic.
    #[test]
    fn active_task_remove_from_empty_repeatedly() {
        let mut app = make_test_app();
        for i in 0..100 {
            app.remove_active_task(&format!("ghost-{i}"));
        }
        assert!(app.active_task_ids.is_empty());
    }
}

/// Permission state stored inline on a `ToolCallInfo`, so the permission
/// controls render inside the tool call block (unified edit/permission UX).
pub struct InlinePermission {
    pub options: Vec<acp::PermissionOption>,
    pub response_tx: tokio::sync::oneshot::Sender<acp::RequestPermissionResponse>,
    pub selected_index: usize,
    /// Whether this permission currently has keyboard focus.
    /// When multiple permissions are pending, only the focused one
    /// shows the selection arrow and accepts Left/Right/Enter input.
    pub focused: bool,
}
