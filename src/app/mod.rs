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

mod input;
mod state;

// Re-export all public types so `crate::app::App`, `crate::app::BlockCache`, etc. still work.
pub use input::InputState;
pub use state::{
    App, AppStatus, BlockCache, ChatMessage, InlinePermission, MessageBlock, MessageRole, ModeInfo,
    ModeState, ToolCallInfo,
};

use crate::Cli;
use crate::acp::client::{ClaudeClient, ClientEvent, TerminalMap};
use crate::acp::connection;
use agent_client_protocol::{self as acp, Agent as _};
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use futures::{FutureExt as _, StreamExt};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tokio::process::Child;
use tokio::sync::mpsc;

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
                        .fs(acp::FileSystemCapability::new()
                            .read_text_file(true)
                            .write_text_file(true))
                        .terminal(true),
                )
                .client_info(acp::Implementation::new(
                    "claude-rust",
                    env!("CARGO_PKG_VERSION"),
                )),
        )
        .await?;

    tracing::info!("Connected to agent: {:?}", init_response);

    // Helper: authenticate if needed and retry the given async operation.
    async fn authenticate_and_retry<F, Fut, T>(
        conn: &acp::ClientSideConnection,
        init_response: &acp::InitializeResponse,
        f: F,
    ) -> anyhow::Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, agent_client_protocol::Error>>,
    {
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
        Ok(f().await?)
    }

    // Create or resume session
    let (session_id, resp_models, resp_modes) = if let Some(ref sid) = cli.resume {
        // --resume <session_id>: load existing session
        eprintln!("Resuming session {}...", sid);
        let session_id = acp::SessionId::new(sid.as_str());
        let load_req = acp::LoadSessionRequest::new(session_id.clone(), &cwd);
        let resp = match conn.load_session(load_req).await {
            Ok(resp) => resp,
            Err(err) if err.code == acp::ErrorCode::AuthRequired => {
                let cwd = cwd.clone();
                let sid = session_id.clone();
                authenticate_and_retry(&conn, &init_response, || {
                    conn.load_session(acp::LoadSessionRequest::new(sid, &cwd))
                })
                .await?
            }
            Err(err) => return Err(err.into()),
        };
        (session_id, resp.models, resp.modes)
    } else {
        // New session (with auth retry)
        match conn.new_session(acp::NewSessionRequest::new(&cwd)).await {
            Ok(resp) => (resp.session_id, resp.models, resp.modes),
            Err(err) if err.code == acp::ErrorCode::AuthRequired => {
                let cwd = cwd.clone();
                let resp = authenticate_and_retry(&conn, &init_response, || {
                    conn.new_session(acp::NewSessionRequest::new(&cwd))
                })
                .await?;
                (resp.session_id, resp.models, resp.modes)
            }
            Err(err) => return Err(err.into()),
        }
    };

    // Extract model name from session response
    let mut model_name = resp_models
        .as_ref()
        .and_then(|m| {
            m.available_models
                .iter()
                .find(|info| info.model_id == m.current_model_id)
                .map(|info| info.name.clone())
        })
        .unwrap_or_else(|| "Unknown model".to_string());

    // --model override: switch after session creation
    if let Some(ref model_str) = cli.model {
        conn.set_session_model(acp::SetSessionModelRequest::new(
            session_id.clone(),
            acp::ModelId::new(model_str.as_str()),
        ))
        .await?;
        model_name = model_str.clone();
    }

    // Extract mode state from session response
    let mut mode = resp_modes.map(|ms| {
        let current_id = ms.current_mode_id.to_string();
        let available: Vec<ModeInfo> = ms
            .available_modes
            .iter()
            .map(|m| ModeInfo {
                id: m.id.to_string(),
                name: m.name.clone(),
            })
            .collect();
        let current_name = available
            .iter()
            .find(|m| m.id == current_id)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| current_id.clone());
        ModeState {
            current_mode_id: current_id,
            current_mode_name: current_name,
            available_modes: available,
        }
    });

    // Log available modes for debugging
    if let Some(ref m) = mode {
        tracing::info!(
            "Available modes: {:?}",
            m.available_modes.iter().map(|m| &m.id).collect::<Vec<_>>()
        );
        tracing::info!("Current mode: {}", m.current_mode_id);
    }

    // --yolo: switch to bypass-permissions mode via the adapter
    if cli.yolo {
        if let Some(ref m) = mode {
            // Find a bypass/yolo mode — try common IDs
            let yolo_mode = m
                .available_modes
                .iter()
                .find(|mi| mi.id == "bypassPermissions" || mi.id == "dontAsk");
            if let Some(target) = yolo_mode {
                let target_id = target.id.clone();
                let target_name = target.name.clone();
                let mode_id = acp::SessionModeId::new(target_id.as_str());
                conn.set_session_mode(acp::SetSessionModeRequest::new(session_id.clone(), mode_id))
                    .await?;
                tracing::info!("YOLO: switched to mode '{}'", target_id);
                // Update local mode state to reflect the switch
                if let Some(ref mut ms) = mode {
                    ms.current_mode_id = target_id;
                    ms.current_mode_name = target_name;
                }
            } else {
                tracing::warn!(
                    "YOLO: no bypass-permissions or do-not-ask mode found in available modes"
                );
            }
        }
    }

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
        mode,
        pending_permission_ids: Vec::new(),
        event_tx,
        event_rx,
        spinner_frame: 0,
        tools_collapsed: true,
        active_task_ids: HashSet::new(),
        terminals: std::rc::Rc::clone(&terminals),
        force_redraw: false,
        tool_call_index: HashMap::new(),
    };

    Ok((app, conn, child, terminals))
}

// ---------------------------------------------------------------------------
// TUI event loop
// ---------------------------------------------------------------------------

pub async fn run_tui(app: &mut App, conn: Rc<acp::ClientSideConnection>) -> anyhow::Result<()> {
    let mut terminal = ratatui::init();

    // Enable bracketed paste and mouse capture (ignore error on unsupported terminals)
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableBracketedPaste,
        crossterm::event::EnableMouseCapture
    );

    let mut events = EventStream::new();
    let tick_duration = Duration::from_millis(33);
    let mut last_render = Instant::now();

    loop {
        // Phase 1: wait for at least one event or the next frame tick
        let time_to_next = tick_duration.saturating_sub(last_render.elapsed());
        tokio::select! {
            Some(Ok(event)) = events.next() => {
                handle_terminal_event(app, &conn, event);
            }
            Some(event) = app.event_rx.recv() => {
                handle_acp_event(app, event);
            }
            _ = tokio::time::sleep(time_to_next) => {}
        }

        // Phase 2: drain all remaining queued events (non-blocking)
        loop {
            // Try terminal events first (keeps typing responsive)
            if let Some(Some(Ok(event))) = events.next().now_or_never() {
                handle_terminal_event(app, &conn, event);
                continue;
            }
            // Then ACP events
            match app.event_rx.try_recv() {
                Ok(event) => {
                    handle_acp_event(app, event);
                    continue;
                }
                Err(_) => break,
            }
        }

        if app.should_quit {
            break;
        }

        // Phase 3: render once
        if matches!(app.status, AppStatus::Thinking | AppStatus::Running) {
            app.spinner_frame = app.spinner_frame.wrapping_add(1);
        }
        update_terminal_outputs(app);
        if app.force_redraw {
            terminal.clear()?;
            app.force_redraw = false;
        }
        terminal.draw(|f| crate::ui::render(f, app))?;
        last_render = Instant::now();
    }

    // --- Graceful shutdown ---

    // Dismiss all pending inline permissions (reject via last option)
    for tool_id in std::mem::take(&mut app.pending_permission_ids) {
        if let Some((mi, bi)) = app.tool_call_index.get(&tool_id).copied() {
            if let Some(MessageBlock::ToolCall(tc)) =
                app.messages.get_mut(mi).and_then(|m| m.blocks.get_mut(bi))
            {
                if let Some(pending) = tc.pending_permission.take() {
                    if let Some(last_opt) = pending.options.last() {
                        let _ = pending.response_tx.send(acp::RequestPermissionResponse::new(
                            acp::RequestPermissionOutcome::Selected(
                                acp::SelectedPermissionOutcome::new(last_opt.option_id.clone()),
                            ),
                        ));
                    }
                }
            }
        }
    }

    // Cancel any active turn and give the adapter a moment to clean up
    if matches!(app.status, AppStatus::Thinking | AppStatus::Running) {
        if let Some(sid) = app.session_id.clone() {
            let _ = conn.cancel(acp::CancelNotification::new(sid)).await;
        }
    }

    // Restore terminal
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
                        let buf = terminal
                            .output_buffer
                            .lock()
                            .expect("output buffer lock poisoned");
                        let current_len = buf.len();
                        if current_len == 0 || current_len == tc.terminal_output_len {
                            continue;
                        }
                        let snapshot = String::from_utf8_lossy(&buf).to_string();
                        drop(buf);

                        tc.terminal_output = Some(snapshot);
                        tc.terminal_output_len = current_len;
                        tc.cache.invalidate();
                    }
                }
            }
        }
    }
}

fn handle_terminal_event(app: &mut App, conn: &Rc<acp::ClientSideConnection>, event: Event) {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            if !app.pending_permission_ids.is_empty() {
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

const MOUSE_SCROLL_LINES: usize = 3;

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

fn handle_normal_key(app: &mut App, conn: &Rc<acp::ClientSideConnection>, key: KeyEvent) {
    match (key.code, key.modifiers) {
        // Ctrl+C: quit (graceful shutdown handles cancel + cleanup)
        (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        // Esc: cancel current turn if thinking/running
        (KeyCode::Esc, _) => {
            if matches!(app.status, AppStatus::Thinking | AppStatus::Running) {
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
        // Ctrl+L: force full terminal redraw
        (KeyCode::Char('l'), m) if m.contains(KeyModifiers::CONTROL) => {
            app.force_redraw = true;
        }
        // Shift+Tab: cycle session mode
        (KeyCode::BackTab, _) => {
            if let Some(ref mode) = app.mode {
                if mode.available_modes.len() > 1 {
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
                        .map(|m| ModeInfo {
                            id: m.id.clone(),
                            name: m.name.clone(),
                        })
                        .collect();
                    app.mode = Some(ModeState {
                        current_mode_id: next_id,
                        current_mode_name: next_name,
                        available_modes: modes,
                    });
                }
            }
        }
        // Editing
        (KeyCode::Backspace, _) => app.input.delete_char_before(),
        (KeyCode::Delete, _) => app.input.delete_char_after(),
        // Printable characters
        (KeyCode::Char(c), _) => app.input.insert_char(c),
        _ => {}
    }
}

/// Look up the tool call that currently has keyboard focus for its permission.
/// This is the first entry in `pending_permission_ids`.
/// Returns mutable reference to its `ToolCallInfo`.
fn get_focused_permission_tc(app: &mut App) -> Option<&mut ToolCallInfo> {
    let tool_id = app.pending_permission_ids.first()?;
    let (mi, bi) = app.tool_call_index.get(tool_id).copied()?;
    match app.messages.get_mut(mi)?.blocks.get_mut(bi)? {
        MessageBlock::ToolCall(tc) if tc.pending_permission.is_some() => Some(tc),
        _ => None,
    }
}

/// Set the `focused` flag on a permission at the given index in `pending_permission_ids`.
/// Also invalidates the tool call's render cache.
fn set_permission_focused(app: &mut App, queue_index: usize, focused: bool) {
    let Some(tool_id) = app.pending_permission_ids.get(queue_index) else {
        return;
    };
    let Some((mi, bi)) = app.tool_call_index.get(tool_id).copied() else {
        return;
    };
    if let Some(MessageBlock::ToolCall(tc)) =
        app.messages.get_mut(mi).and_then(|m| m.blocks.get_mut(bi))
    {
        if let Some(ref mut perm) = tc.pending_permission {
            perm.focused = focused;
        }
        tc.cache.invalidate();
    }
}

fn handle_permission_key(app: &mut App, key: KeyEvent) {
    let option_count = get_focused_permission_tc(app)
        .and_then(|tc| tc.pending_permission.as_ref())
        .map(|p| p.options.len())
        .unwrap_or(0);

    match key.code {
        // Up / Down: cycle focus between pending permissions
        KeyCode::Up | KeyCode::Down if app.pending_permission_ids.len() > 1 => {
            // Unfocus the current (first) permission
            set_permission_focused(app, 0, false);

            if key.code == KeyCode::Down {
                // Move first to end (rotate forward)
                let first = app.pending_permission_ids.remove(0);
                app.pending_permission_ids.push(first);
            } else {
                // Move last to front (rotate backward)
                let last = app.pending_permission_ids.pop().unwrap();
                app.pending_permission_ids.insert(0, last);
            }

            // Focus the new first permission
            set_permission_focused(app, 0, true);
            // Scroll to the newly focused permission's tool call
            app.auto_scroll = true;
        }
        KeyCode::Left => {
            if let Some(tc) = get_focused_permission_tc(app) {
                if let Some(ref mut p) = tc.pending_permission {
                    p.selected_index = p.selected_index.saturating_sub(1);
                    tc.cache.invalidate();
                }
            }
        }
        KeyCode::Right => {
            if let Some(tc) = get_focused_permission_tc(app) {
                if let Some(ref mut p) = tc.pending_permission {
                    if p.selected_index + 1 < option_count {
                        p.selected_index += 1;
                        tc.cache.invalidate();
                    }
                }
            }
        }
        KeyCode::Enter => {
            respond_permission(app, None);
        }
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            respond_permission(app, Some(0));
        }
        KeyCode::Char('a') | KeyCode::Char('A') => {
            if option_count > 1 {
                respond_permission(app, Some(1));
            }
        }
        KeyCode::Char('n') | KeyCode::Char('N') => {
            if option_count > 0 {
                respond_permission(app, Some(option_count - 1));
            }
        }
        KeyCode::Esc => {
            if option_count > 0 {
                respond_permission(app, Some(option_count - 1));
            }
        }
        _ => {}
    }
}

fn respond_permission(app: &mut App, override_index: Option<usize>) {
    if app.pending_permission_ids.is_empty() {
        return;
    }
    // Remove the focused (first) permission from the queue.
    let tool_id = app.pending_permission_ids.remove(0);

    let Some((mi, bi)) = app.tool_call_index.get(&tool_id).copied() else {
        return;
    };
    let Some(MessageBlock::ToolCall(tc)) = app
        .messages
        .get_mut(mi)
        .and_then(|m| m.blocks.get_mut(bi))
    else {
        return;
    };
    if let Some(pending) = tc.pending_permission.take() {
        let idx = override_index.unwrap_or(pending.selected_index);
        if let Some(opt) = pending.options.get(idx) {
            let _ = pending
                .response_tx
                .send(acp::RequestPermissionResponse::new(
                    acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(
                        opt.option_id.clone(),
                    )),
                ));
        }
        tc.cache.invalidate();
    }

    // Focus the next permission in the queue (now at index 0), if any.
    set_permission_focused(app, 0, true);
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
    });
    // Create empty assistant message immediately — message.rs shows thinking indicator
    app.messages.push(ChatMessage {
        role: MessageRole::Assistant,
        blocks: Vec::new(),
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
                tracing::debug!("PromptResponse: stop_reason={:?}", resp.stop_reason);
                let _ = tx.send(ClientEvent::TurnComplete);
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
            let tool_id = request.tool_call.tool_call_id.to_string();
            if let Some((mi, bi)) = app.lookup_tool_call(&tool_id) {
                if let Some(MessageBlock::ToolCall(tc)) =
                    app.messages.get_mut(mi).and_then(|m| m.blocks.get_mut(bi))
                {
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
                // Tool call not found — reject by selecting last option
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
        }
        ClientEvent::TurnError(msg) => {
            tracing::error!("Turn error: {msg}");
            app.status = AppStatus::Error;
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

    // Quick check: if title doesn't contain any part of cwd, skip normalization
    // Use the first path component of cwd as a heuristic
    let cwd_start = cwd_raw
        .split(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or(cwd_raw);
    if !title.contains(cwd_start) {
        return title.to_string();
    }

    // Normalize both to forward slashes for matching
    let cwd_norm = cwd_raw.replace('\\', "/");
    let title_norm = title.replace('\\', "/");

    // Ensure cwd ends with slash so we strip the separator too
    let with_sep = if cwd_norm.ends_with('/') {
        cwd_norm
    } else {
        format!("{cwd_norm}/")
    };

    if title_norm.contains(&with_sep) {
        return title_norm.replace(&with_sep, "");
    }
    title_norm
}

/// Return a human-readable name for a SessionUpdate variant (for debug logging).
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
            .map(|s| s.to_string())
    });

    let is_task = claude_tool_name.as_deref() == Some("Task");

    // Subagent children are never hidden — they need to be visible so
    // permission prompts render and the user can interact with them.
    let hidden = false;

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

    // Attach to current assistant message — update existing or add new
    let msg_idx = app.messages.len().saturating_sub(1);
    let existing_pos = app.lookup_tool_call(&tool_info.id);
    let is_assistant = app
        .messages
        .last()
        .is_some_and(|m| matches!(m.role, MessageRole::Assistant));

    if is_assistant {
        if let Some((mi, bi)) = existing_pos {
            if let Some(MessageBlock::ToolCall(existing)) =
                app.messages.get_mut(mi).and_then(|m| m.blocks.get_mut(bi))
            {
                existing.title = tool_info.title.clone();
                existing.status = tool_info.status;
                existing.content = tool_info.content.clone();
                existing.kind = tool_info.kind;
                existing.claude_tool_name = tool_info.claude_tool_name.clone();
                existing.cache.invalidate();
            }
        } else if let Some(last) = app.messages.last_mut() {
            let block_idx = last.blocks.len();
            let tc_id = tool_info.id.clone();
            last.blocks.push(MessageBlock::ToolCall(tool_info));
            app.index_tool_call(tc_id, msg_idx, block_idx);
        }
    }

    app.status = AppStatus::Running;
    if !hidden {
        app.files_accessed += 1;
    }
}

fn handle_session_update(app: &mut App, update: acp::SessionUpdate) {
    tracing::debug!("SessionUpdate variant: {}", session_update_name(&update));
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
                            last.blocks
                                .push(MessageBlock::Text(text.text.clone(), BlockCache::default()));
                        }
                        return;
                    }
                }
                app.messages.push(ChatMessage {
                    role: MessageRole::Assistant,
                    blocks: vec![MessageBlock::Text(text.text.clone(), BlockCache::default())],
                });
            }
        }
        acp::SessionUpdate::ToolCall(tc) => {
            handle_tool_call(app, tc);
        }
        acp::SessionUpdate::ToolCallUpdate(tcu) => {
            // Find and update the tool call by id (in-place)
            let id_str = tcu.tool_call_id.to_string();
            let has_content = tcu.fields.content.as_ref().map(|c| c.len()).unwrap_or(0);
            tracing::debug!(
                "ToolCallUpdate: id={id_str} new_title={:?} new_status={:?} content_blocks={has_content}",
                tcu.fields.title,
                tcu.fields.status
            );

            // If this is a Task completing, remove from active list
            if matches!(
                tcu.fields.status,
                Some(acp::ToolCallStatus::Completed) | Some(acp::ToolCallStatus::Failed)
            ) {
                app.remove_active_task(&id_str);
            }

            if let Some((mi, bi)) = app.lookup_tool_call(&id_str) {
                if let Some(MessageBlock::ToolCall(tc)) =
                    app.messages.get_mut(mi).and_then(|m| m.blocks.get_mut(bi))
                {
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
                    if let Some(ref meta) = tcu.meta {
                        if let Some(name) = meta
                            .get("claudeCode")
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
                }
            } else {
                tracing::warn!("ToolCallUpdate: id={id_str} not found in index");
            }
        }
        acp::SessionUpdate::UserMessageChunk(_) => {
            // Our own message echoed back — we already display it
        }
        acp::SessionUpdate::AgentThoughtChunk(chunk) => {
            tracing::debug!("Agent thought: {:?}", chunk);
        }
        acp::SessionUpdate::Plan(plan) => {
            tracing::debug!("Plan update: {:?}", plan);
        }
        acp::SessionUpdate::AvailableCommandsUpdate(cmds) => {
            tracing::debug!(
                "Available commands: {} commands",
                cmds.available_commands.len()
            );
        }
        acp::SessionUpdate::CurrentModeUpdate(update) => {
            if let Some(ref mut mode) = app.mode {
                let mode_id = update.current_mode_id.to_string();
                if let Some(info) = mode.available_modes.iter().find(|m| m.id == mode_id) {
                    mode.current_mode_name = info.name.clone();
                    mode.current_mode_id = mode_id;
                } else {
                    mode.current_mode_name = mode_id.clone();
                    mode.current_mode_id = mode_id;
                }
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
