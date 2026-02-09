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

pub struct App {
    pub messages: Vec<ChatMessage>,
    pub scroll_offset: usize,
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
    pub permission_pending: Option<PendingPermission>,
    pub event_tx: mpsc::UnboundedSender<ClientEvent>,
    pub event_rx: mpsc::UnboundedReceiver<ClientEvent>,
    pub spinner_frame: usize,
    /// Session-level default for tool call collapsed state.
    /// Toggled by Ctrl+O — new tool calls inherit this value.
    pub tools_collapsed: bool,
    /// IDs of Task tool calls currently InProgress — their children get hidden.
    /// Use `has_active_tasks()`, `insert_active_task()`, `remove_active_task()`.
    pub(super) active_task_ids: HashSet<String>,
    /// Shared terminal process map — used to snapshot output on completion.
    pub terminals: crate::acp::client::TerminalMap,
    /// Force a full terminal clear on next render frame.
    pub force_redraw: bool,
    /// O(1) lookup: tool_call_id → (message_index, block_index).
    /// Use `lookup_tool_call()`, `index_tool_call()`.
    pub(super) tool_call_index: HashMap<String, (usize, usize)>,
}

impl App {
    /// Returns true if any Task tool calls are currently in-progress.
    #[must_use]
    pub fn has_active_tasks(&self) -> bool {
        !self.active_task_ids.is_empty()
    }

    /// Track a Task tool call as active (in-progress subagent).
    pub fn insert_active_task(&mut self, id: String) {
        self.active_task_ids.insert(id);
    }

    /// Remove a Task tool call from the active set (completed/failed).
    pub fn remove_active_task(&mut self, id: &str) {
        self.active_task_ids.remove(id);
    }

    /// Look up the (message_index, block_index) for a tool call ID.
    #[must_use]
    pub fn lookup_tool_call(&self, id: &str) -> Option<(usize, usize)> {
        self.tool_call_index.get(id).copied()
    }

    /// Register a tool call's position in the message/block arrays.
    pub fn index_tool_call(&mut self, id: String, msg_idx: usize, block_idx: usize) {
        self.tool_call_index.insert(id, (msg_idx, block_idx));
    }
}

#[derive(Debug)]
pub enum AppStatus {
    Ready,
    Thinking,
    Running,
    Error,
}

pub struct ChatMessage {
    pub role: MessageRole,
    pub blocks: Vec<MessageBlock>,
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
        if self.version == 0 {
            self.lines.as_ref()
        } else {
            None
        }
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
    ToolCall(ToolCallInfo),
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
    /// The actual Claude Code tool name from meta.claudeCode.toolName
    /// (e.g. "Task", "Glob", "mcp__acp__Read", "WebSearch")
    pub claude_tool_name: Option<String>,
    /// Hidden tool calls are subagent children — not rendered directly.
    pub hidden: bool,
    /// Terminal ID if this is an Execute tool call with a running/completed terminal.
    pub terminal_id: Option<String>,
    /// The shell command that was executed (e.g. "echo hello && ls -la").
    pub terminal_command: Option<String>,
    /// Snapshot of terminal output, updated each frame while InProgress.
    pub terminal_output: Option<String>,
    /// Length of terminal buffer at last snapshot — used to skip O(n) re-snapshots
    /// when the buffer hasn't grown.
    pub terminal_output_len: usize,
    /// Per-block render cache for this tool call.
    pub cache: BlockCache,
}

pub struct PendingPermission {
    pub request: acp::RequestPermissionRequest,
    pub response_tx: tokio::sync::oneshot::Sender<acp::RequestPermissionResponse>,
    pub selected_index: usize,
}
