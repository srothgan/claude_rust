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

use crate::acp::client::{ClientEvent, ClaudeClient, TerminalMap};
use crate::acp::connection;
use crate::Cli;
use agent_client_protocol::{self as acp, Agent as _};
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent,
    MouseEventKind,
};
use futures::StreamExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tokio::process::Child;
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// App state types
// ---------------------------------------------------------------------------

pub struct App {
    pub messages: Vec<ChatMessage>,
    pub scroll_offset: u16,
    pub auto_scroll: bool,
    pub input: InputState,
    pub status: AppStatus,
    pub should_quit: bool,
    pub session_id: Option<acp::SessionId>,
    pub model_name: String,
    pub cwd: String,
    pub cwd_raw: String,
    pub files_accessed: usize,
    pub tokens_used: (u64, u64),
    pub permission_pending: Option<PendingPermission>,
    pub event_tx: mpsc::UnboundedSender<ClientEvent>,
    pub event_rx: mpsc::UnboundedReceiver<ClientEvent>,
    pub spinner_frame: usize,
    /// Session-level default for tool call collapsed state.
    /// Toggled by Ctrl+O — new tool calls inherit this value.
    pub tools_collapsed: bool,
    /// IDs of Task tool calls currently InProgress — their children get hidden.
    pub active_task_ids: Vec<String>,
    /// Shared terminal process map — used to snapshot output on completion.
    pub terminals: crate::acp::client::TerminalMap,
}

pub enum AppStatus {
    Ready,
    Thinking,
    Running(String),
    Error(String),
}

pub struct ChatMessage {
    pub role: MessageRole,
    pub blocks: Vec<MessageBlock>,
    pub timestamp: Instant,
}

/// Cached rendered lines for a block. Stores a version counter so the cache
/// is only recomputed when the block content actually changes.
#[derive(Default)]
pub struct BlockCache {
    pub version: u64,
    pub lines: Option<Vec<ratatui::text::Line<'static>>>,
}

impl BlockCache {
    /// Bump the version to invalidate cached lines.
    pub fn invalidate(&mut self) {
        self.version += 1;
    }
}

/// Ordered content block — text and tool calls interleaved as they arrive.
pub enum MessageBlock {
    Text(String, BlockCache),
    ToolCall(ToolCallInfo),
}

pub enum MessageRole {
    User,
    Assistant,
    System,
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
    /// Per-block render cache for this tool call.
    pub cache: BlockCache,
}

pub struct PendingPermission {
    pub request: acp::RequestPermissionRequest,
    pub response_tx: tokio::sync::oneshot::Sender<acp::RequestPermissionResponse>,
    pub selected_index: usize,
}

// ---------------------------------------------------------------------------
// Custom text input state
// ---------------------------------------------------------------------------

pub struct InputState {
    pub lines: Vec<String>,
    pub cursor_row: usize,
    pub cursor_col: usize,
}

impl InputState {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.cursor_row = 0;
        self.cursor_col = 0;
    }

    pub fn insert_char(&mut self, c: char) {
        let line = &mut self.lines[self.cursor_row];
        let byte_idx = char_to_byte_index(line, self.cursor_col);
        line.insert(byte_idx, c);
        self.cursor_col += 1;
    }

    pub fn insert_newline(&mut self) {
        let line = &mut self.lines[self.cursor_row];
        let byte_idx = char_to_byte_index(line, self.cursor_col);
        let rest = line[byte_idx..].to_string();
        line.truncate(byte_idx);
        self.cursor_row += 1;
        self.lines.insert(self.cursor_row, rest);
        self.cursor_col = 0;
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            if c == '\n' || c == '\r' {
                self.insert_newline();
            } else {
                self.insert_char(c);
            }
        }
    }

    pub fn delete_char_before(&mut self) {
        if self.cursor_col > 0 {
            let line = &mut self.lines[self.cursor_row];
            self.cursor_col -= 1;
            let byte_idx = char_to_byte_index(line, self.cursor_col);
            line.remove(byte_idx);
        } else if self.cursor_row > 0 {
            let removed = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].chars().count();
            self.lines[self.cursor_row].push_str(&removed);
        }
    }

    pub fn delete_char_after(&mut self) {
        let line_len = self.lines[self.cursor_row].chars().count();
        if self.cursor_col < line_len {
            let line = &mut self.lines[self.cursor_row];
            let byte_idx = char_to_byte_index(line, self.cursor_col);
            line.remove(byte_idx);
        } else if self.cursor_row + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor_row + 1);
            self.lines[self.cursor_row].push_str(&next);
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].chars().count();
        }
    }

    pub fn move_right(&mut self) {
        let line_len = self.lines[self.cursor_row].chars().count();
        if self.cursor_col < line_len {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
    }

    pub fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            let line_len = self.lines[self.cursor_row].chars().count();
            self.cursor_col = self.cursor_col.min(line_len);
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            let line_len = self.lines[self.cursor_row].chars().count();
            self.cursor_col = self.cursor_col.min(line_len);
        }
    }

    pub fn move_home(&mut self) {
        self.cursor_col = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor_col = self.lines[self.cursor_row].chars().count();
    }

    pub fn line_count(&self) -> u16 {
        self.lines.len() as u16
    }
}

/// Convert a character index to a byte index within a string.
fn char_to_byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

// ---------------------------------------------------------------------------
// Pre-TUI connection phase
// ---------------------------------------------------------------------------

/// Connect to the ACP adapter, handshake, authenticate, and create a session.
/// Runs before ratatui::init() so errors print to stderr normally.
/// Returns `(App, Rc<Connection>, Child, TerminalMap)`. The `Child` handle must be
/// kept alive for the adapter process lifetime — dropping it kills the process.
/// The `TerminalMap` is used for cleanup on exit.
pub async fn connect(
    cli: Cli,
    npx_path: PathBuf,
) -> anyhow::Result<(App, Rc<acp::ClientSideConnection>, Child, TerminalMap)> {
    let cwd = cli
        .dir
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (client, terminals) = ClaudeClient::new(event_tx.clone(), cli.yolo, cwd.clone());

    eprintln!("Spawning ACP adapter...");
    let adapter = connection::spawn_adapter(client, &npx_path).await?;
    let child = adapter.child;
    let conn = Rc::new(adapter.connection);

    // Initialize handshake
    let init_response = conn
        .initialize(
            acp::InitializeRequest::new(acp::ProtocolVersion::LATEST)
                .client_capabilities(
                    acp::ClientCapabilities::new()
                        .fs(
                            acp::FileSystemCapability::new()
                                .read_text_file(true)
                                .write_text_file(true),
                        )
                        .terminal(true),
                )
                .client_info(acp::Implementation::new(
                    "claude-rust",
                    env!("CARGO_PKG_VERSION"),
                )),
        )
        .await?;

    tracing::info!("Connected to agent: {:?}", init_response);

    // TODO: Detect actual model from session/turn response (see ROADMAP.md)
    let model_name = "Opus 4.6".to_string();

    // Try to create a session. If AuthRequired, authenticate first.
    let session_id = match conn.new_session(acp::NewSessionRequest::new(&cwd)).await {
        Ok(resp) => resp.session_id,
        Err(err) if err.code == acp::ErrorCode::AuthRequired => {
            tracing::info!("Authentication required, triggering auth flow...");

            let method = init_response.auth_methods.first().ok_or_else(|| {
                anyhow::anyhow!(
                    "Agent requires authentication but advertised no auth methods.\n\
                     Try running `claude /login` first."
                )
            })?;

            eprintln!(
                "Authentication required. Method: {} ({})",
                method.name,
                method.description.as_deref().unwrap_or("no description")
            );

            conn.authenticate(acp::AuthenticateRequest::new(method.id.clone()))
                .await?;

            let resp = conn.new_session(acp::NewSessionRequest::new(&cwd)).await?;
            resp.session_id
        }
        Err(err) => return Err(err.into()),
    };

    tracing::info!("Session created: {:?}", session_id);

    // Shorten cwd for display: use ~ for home dir
    let cwd_display = {
        let cwd_str = cwd.to_string_lossy().to_string();
        if let Some(home) = dirs::home_dir() {
            let home_str = home.to_string_lossy().to_string();
            if cwd_str.starts_with(&home_str) {
                format!("~{}", &cwd_str[home_str.len()..])
            } else {
                cwd_str
            }
        } else {
            cwd_str
        }
    };

    let app = App {
        messages: Vec::new(),
        scroll_offset: 0,
        auto_scroll: true,
        input: InputState::new(),
        status: AppStatus::Ready,
        should_quit: false,
        session_id: Some(session_id),
        model_name,
        cwd_raw: cwd.to_string_lossy().to_string(),
        cwd: cwd_display,
        files_accessed: 0,
        tokens_used: (0, 0),
        permission_pending: None,
        event_tx,
        event_rx,
        spinner_frame: 0,
        tools_collapsed: true,
        active_task_ids: Vec::new(),
        terminals: std::rc::Rc::clone(&terminals),
    };

    Ok((app, conn, child, terminals))
}

// ---------------------------------------------------------------------------
// TUI event loop
// ---------------------------------------------------------------------------

pub async fn run_tui(
    app: &mut App,
    conn: Rc<acp::ClientSideConnection>,
) -> anyhow::Result<()> {
    let mut terminal = ratatui::init();

    // Enable bracketed paste and mouse capture (ignore error on unsupported terminals)
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableBracketedPaste,
        crossterm::event::EnableMouseCapture
    );

    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(16));

    loop {
        tokio::select! {
            Some(Ok(event)) = events.next() => {
                handle_terminal_event(app, &conn, event);
            }
            Some(event) = app.event_rx.recv() => {
                handle_acp_event(app, event);
            }
            _ = tick.tick() => {
                if matches!(app.status, AppStatus::Thinking | AppStatus::Running(_)) {
                    app.spinner_frame = app.spinner_frame.wrapping_add(1);
                }
                update_terminal_outputs(app);
                terminal.draw(|f| crate::ui::render(f, app))?;
            }
        }

        if app.should_quit {
            break;
        }
    }

    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableBracketedPaste,
        crossterm::event::DisableMouseCapture
    );
    ratatui::restore();

    Ok(())
}

// ---------------------------------------------------------------------------
// Terminal event handling
// ---------------------------------------------------------------------------

/// Snapshot terminal output buffers into ToolCallInfo for rendering.
/// Called each frame so in-progress Execute tool calls show live output.
///
/// The output_buffer is append-only (never cleared). The adapter's
/// `terminal_output` uses a cursor to track what it already returned.
/// We simply snapshot the full buffer for display each frame.
fn update_terminal_outputs(app: &mut App) {
    let terminals = app.terminals.borrow();
    if terminals.is_empty() {
        return;
    }

    for msg in &mut app.messages {
        for block in &mut msg.blocks {
            if let MessageBlock::ToolCall(tc) = block {
                if let Some(ref tid) = tc.terminal_id {
                    if let Some(terminal) = terminals.get(tid.as_str()) {
                        let buf = terminal.output_buffer.lock().unwrap();
                        if buf.is_empty() {
                            continue;
                        }
                        let snapshot = String::from_utf8_lossy(&buf).to_string();
                        drop(buf);

                        if tc.terminal_output.as_deref() != Some(&snapshot) {
                            tc.terminal_output = Some(snapshot);
                            tc.cache.invalidate();
                        }
                    }
                }
            }
        }
    }
}

fn handle_terminal_event(
    app: &mut App,
    conn: &Rc<acp::ClientSideConnection>,
    event: Event,
) {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            if app.permission_pending.is_some() {
                handle_permission_key(app, key);
            } else {
                handle_normal_key(app, conn, key);
            }
        }
        Event::Mouse(mouse) => {
            handle_mouse_event(app, mouse);
        }
        Event::Paste(text) => {
            app.input.insert_str(&text);
        }
        // Resize is handled automatically by ratatui
        _ => {}
    }
}

const MOUSE_SCROLL_LINES: u16 = 3;

fn handle_mouse_event(app: &mut App, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            app.scroll_offset = app.scroll_offset.saturating_sub(MOUSE_SCROLL_LINES);
            app.auto_scroll = false;
        }
        MouseEventKind::ScrollDown => {
            app.scroll_offset = app.scroll_offset.saturating_add(MOUSE_SCROLL_LINES);
            // auto_scroll re-engagement handled by chat::render clamping
        }
        _ => {}
    }
}

fn handle_normal_key(
    app: &mut App,
    conn: &Rc<acp::ClientSideConnection>,
    key: KeyEvent,
) {
    match (key.code, key.modifiers) {
        // Ctrl+C: cancel if active, otherwise quit
        (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
            if matches!(app.status, AppStatus::Thinking | AppStatus::Running(_)) {
                // Cancel the active turn first, then quit
                if let Some(sid) = app.session_id.clone() {
                    let conn = Rc::clone(conn);
                    tokio::task::spawn_local(async move {
                        let _ = conn.cancel(acp::CancelNotification::new(sid)).await;
                    });
                }
            }
            app.should_quit = true;
        }
        // Esc: cancel current turn if thinking/running
        (KeyCode::Esc, _) => {
            if matches!(app.status, AppStatus::Thinking | AppStatus::Running(_)) {
                if let Some(sid) = app.session_id.clone() {
                    let conn = Rc::clone(conn);
                    tokio::task::spawn_local(async move {
                        if let Err(e) = conn.cancel(acp::CancelNotification::new(sid)).await {
                            tracing::error!("Failed to send cancel: {e}");
                        }
                    });
                    app.status = AppStatus::Ready;
                }
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
            app.scroll_offset = app.scroll_offset.saturating_sub(1);
            app.auto_scroll = false;
        }
        (KeyCode::Down, m) if m.contains(KeyModifiers::CONTROL) => {
            // Ctrl+Down: scroll chat down (clamped in chat::render)
            app.scroll_offset = app.scroll_offset.saturating_add(1);
        }
        (KeyCode::Up, _) => app.input.move_up(),
        (KeyCode::Down, _) => app.input.move_down(),
        (KeyCode::Home, _) => app.input.move_home(),
        (KeyCode::End, _) => app.input.move_end(),
        // Ctrl+O: toggle expand/collapse on all tool calls
        (KeyCode::Char('o'), m) if m.contains(KeyModifiers::CONTROL) => {
            toggle_all_tool_calls(app);
        }
        // Editing
        (KeyCode::Backspace, _) => app.input.delete_char_before(),
        (KeyCode::Delete, _) => app.input.delete_char_after(),
        // Printable characters
        (KeyCode::Char(c), _) => app.input.insert_char(c),
        _ => {}
    }
}

fn handle_permission_key(app: &mut App, key: KeyEvent) {
    let option_count = app
        .permission_pending
        .as_ref()
        .map(|p| p.request.options.len())
        .unwrap_or(0);

    match key.code {
        KeyCode::Up => {
            if let Some(ref mut p) = app.permission_pending {
                p.selected_index = p.selected_index.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            if let Some(ref mut p) = app.permission_pending {
                if p.selected_index + 1 < option_count {
                    p.selected_index += 1;
                }
            }
        }
        KeyCode::Enter => {
            respond_permission(app, None);
        }
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            // Select first option (typically "Allow once")
            respond_permission(app, Some(0));
        }
        KeyCode::Char('a') | KeyCode::Char('A') => {
            // Select second option if it exists (typically "Allow always")
            if option_count > 1 {
                respond_permission(app, Some(1));
            }
        }
        KeyCode::Char('n') | KeyCode::Char('N') => {
            // Select last option (typically "Reject")
            if option_count > 0 {
                respond_permission(app, Some(option_count - 1));
            }
        }
        KeyCode::Esc => {
            // Reject by selecting last option
            if option_count > 0 {
                respond_permission(app, Some(option_count - 1));
            }
        }
        _ => {}
    }
}

fn respond_permission(app: &mut App, override_index: Option<usize>) {
    if let Some(pending) = app.permission_pending.take() {
        let idx = override_index.unwrap_or(pending.selected_index);
        if let Some(opt) = pending.request.options.get(idx) {
            let _ = pending.response_tx.send(acp::RequestPermissionResponse::new(
                acp::RequestPermissionOutcome::Selected(
                    acp::SelectedPermissionOutcome::new(opt.option_id.clone()),
                ),
            ));
        }
    }
}

/// Toggle the session-level collapsed preference and apply to all tool calls.
fn toggle_all_tool_calls(app: &mut App) {
    app.tools_collapsed = !app.tools_collapsed;
    for msg in &mut app.messages {
        for block in &mut msg.blocks {
            if let MessageBlock::ToolCall(tc) = block {
                tc.collapsed = app.tools_collapsed;
                tc.cache.invalidate();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Prompt submission via spawn_local
// ---------------------------------------------------------------------------

fn submit_input(app: &mut App, conn: &Rc<acp::ClientSideConnection>) {
    let text = app.input.text();
    if text.trim().is_empty() {
        return;
    }

    app.messages.push(ChatMessage {
        role: MessageRole::User,
        blocks: vec![MessageBlock::Text(text.clone(), BlockCache::default())],
        timestamp: Instant::now(),
    });
    // Create empty assistant message immediately — message.rs shows thinking indicator
    app.messages.push(ChatMessage {
        role: MessageRole::Assistant,
        blocks: Vec::new(),
        timestamp: Instant::now(),
    });
    app.input.clear();
    app.status = AppStatus::Thinking;
    app.auto_scroll = true;

    let conn = Rc::clone(conn);
    let sid = app.session_id.clone().unwrap();
    let tx = app.event_tx.clone();

    tokio::task::spawn_local(async move {
        match conn
            .prompt(acp::PromptRequest::new(
                sid,
                vec![acp::ContentBlock::Text(acp::TextContent::new(&text))],
            ))
            .await
        {
            Ok(resp) => {
                let _ = tx.send(ClientEvent::TurnComplete {
                    stop_reason: resp.stop_reason,
                });
            }
            Err(e) => {
                let _ = tx.send(ClientEvent::TurnError(e.to_string()));
            }
        }
    });
}

// ---------------------------------------------------------------------------
// ACP event handling
// ---------------------------------------------------------------------------

fn handle_acp_event(app: &mut App, event: ClientEvent) {
    match event {
        ClientEvent::SessionUpdate(update) => handle_session_update(app, update),
        ClientEvent::PermissionRequest {
            request,
            response_tx,
        } => {
            app.permission_pending = Some(PendingPermission {
                request,
                response_tx,
                selected_index: 0,
            });
        }
        ClientEvent::TurnComplete { .. } => {
            app.status = AppStatus::Ready;
        }
        ClientEvent::TurnError(msg) => {
            app.status = AppStatus::Error(msg);
        }
    }
}

/// Shorten absolute paths in tool titles to relative paths based on cwd.
/// e.g. "Read C:\Users\me\project\src\main.rs" → "Read src/main.rs"
/// Handles both `/` and `\` separators on all platforms since the ACP adapter
/// may use either regardless of the host OS.
fn shorten_tool_title(title: &str, cwd_raw: &str) -> String {
    if cwd_raw.is_empty() {
        return title.to_string();
    }
    // Normalize both to forward slashes for matching
    let cwd_norm = cwd_raw.replace('\\', "/");
    let title_norm = title.replace('\\', "/");

    // Try with trailing slash first (strips the separator too)
    let with_sep = if cwd_norm.ends_with('/') {
        cwd_norm.clone()
    } else {
        format!("{cwd_norm}/")
    };

    if title_norm.contains(&with_sep) {
        return title_norm.replace(&with_sep, "");
    }
    // Fallback: strip cwd without trailing slash (rare, but handles edge cases)
    title_norm.replace(&cwd_norm, "")
}

fn handle_session_update(app: &mut App, update: acp::SessionUpdate) {
    match update {
        acp::SessionUpdate::AgentMessageChunk(chunk) => {
            if let acp::ContentBlock::Text(text) = chunk.content {
                // Append to last text block in current assistant message, or create new
                if let Some(last) = app.messages.last_mut() {
                    if matches!(last.role, MessageRole::Assistant) {
                        // Append to last Text block if it exists, else push new one
                        if let Some(MessageBlock::Text(t, cache)) = last.blocks.last_mut() {
                            t.push_str(&text.text);
                            cache.invalidate();
                        } else {
                            last.blocks.push(MessageBlock::Text(text.text.clone(), BlockCache::default()));
                        }
                        return;
                    }
                }
                app.messages.push(ChatMessage {
                    role: MessageRole::Assistant,
                    blocks: vec![MessageBlock::Text(text.text.clone(), BlockCache::default())],
                    timestamp: Instant::now(),
                });
            }
        }
        acp::SessionUpdate::ToolCall(tc) => {
            let title = tc.title.clone();
            let kind = tc.kind;
            let id_str = tc.tool_call_id.to_string();
            tracing::debug!("ToolCall: id={id_str} title={title} kind={kind:?} status={:?} content_blocks={}", tc.status, tc.content.len());

            // Extract claude_tool_name from meta.claudeCode.toolName
            let claude_tool_name = tc.meta.as_ref().and_then(|m| {
                m.get("claudeCode")
                    .and_then(|v| v.get("toolName"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            });

            let is_task = claude_tool_name.as_deref() == Some("Task");

            // If a Task is active and this is NOT itself a Task, it's a subagent child — hide it
            let hidden = !is_task && !app.active_task_ids.is_empty();

            // Track new Task tool calls as active subagents
            if is_task {
                app.active_task_ids.push(id_str.clone());
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
                cache: BlockCache::default(),
            };

            // Attach to current assistant message — update existing or add new
            if let Some(last) = app.messages.last_mut() {
                if matches!(last.role, MessageRole::Assistant) {
                    // Check if this tool call ID already exists (update in place)
                    let mut found = false;
                    for block in &mut last.blocks {
                        if let MessageBlock::ToolCall(existing) = block {
                            if existing.id == tool_info.id {
                                existing.title = tool_info.title.clone();
                                existing.status = tool_info.status;
                                existing.content = tool_info.content.clone();
                                existing.kind = tool_info.kind;
                                existing.claude_tool_name = tool_info.claude_tool_name.clone();
                                existing.cache.invalidate();
                                found = true;
                                break;
                            }
                        }
                    }
                    if !found {
                        last.blocks.push(MessageBlock::ToolCall(tool_info));
                    }
                }
            }

            app.status = AppStatus::Running(shorten_tool_title(&title, &app.cwd_raw));
            if !hidden {
                app.files_accessed += 1;
            }
        }
        acp::SessionUpdate::ToolCallUpdate(tcu) => {
            // Find and update the tool call by id (in-place)
            let id_str = tcu.tool_call_id.to_string();
            let has_content = tcu.fields.content.as_ref().map(|c| c.len()).unwrap_or(0);
            tracing::debug!("ToolCallUpdate: id={id_str} new_title={:?} new_status={:?} content_blocks={has_content}", tcu.fields.title, tcu.fields.status);

            // If this is a Task completing, remove from active list
            if matches!(tcu.fields.status, Some(acp::ToolCallStatus::Completed) | Some(acp::ToolCallStatus::Failed)) {
                app.active_task_ids.retain(|id| id != &id_str);
            }

            for msg in app.messages.iter_mut().rev() {
                for block in &mut msg.blocks {
                    if let MessageBlock::ToolCall(tc) = block {
                        if tc.id == id_str {
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
                                        // Look up the command from the shared terminal map
                                        if let Some(terminal) = app.terminals.borrow().get(&tid) {
                                            tc.terminal_command = Some(terminal.command.clone());
                                        }
                                        tc.terminal_id = Some(tid);
                                    }
                                }
                                tc.content = content;
                            }
                            // Update claude_tool_name from update meta if present
                            if let Some(ref meta) = tcu.meta {
                                if let Some(name) = meta.get("claudeCode")
                                    .and_then(|v| v.get("toolName"))
                                    .and_then(|v| v.as_str())
                                {
                                    tc.claude_tool_name = Some(name.to_string());
                                }
                            }
                            if matches!(
                                tc.status,
                                acp::ToolCallStatus::Completed | acp::ToolCallStatus::Failed
                            ) {
                                tc.collapsed = app.tools_collapsed;
                            }
                            tc.cache.invalidate();
                            return;
                        }
                    }
                }
            }
            tracing::warn!("ToolCallUpdate: id={id_str} not found in any message");
        }
        _ => {}
    }
}
