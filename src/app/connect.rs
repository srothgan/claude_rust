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

use super::{
    App, AppStatus, ChatViewport, FocusManager, ModeInfo, ModeState, SelectionState, TodoItem,
};
use crate::Cli;
use crate::acp::client::{ClaudeClient, ClientEvent, TerminalMap};
use crate::acp::connection;
use agent_client_protocol::{self as acp, Agent as _};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;
use tokio::sync::mpsc;

/// Shorten cwd for display: use `~` for the home directory prefix.
fn shorten_cwd(cwd: &std::path::Path) -> String {
    let cwd_str = cwd.to_string_lossy().to_string();
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy().to_string();
        if cwd_str.starts_with(&home_str) {
            return format!("~{}", &cwd_str[home_str.len()..]);
        }
    }
    cwd_str
}

/// Create the `App` struct in `Connecting` state. No I/O — returns immediately.
pub fn create_app(cli: &Cli) -> App {
    let cwd = cli
        .dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let terminals: TerminalMap = Rc::new(std::cell::RefCell::new(HashMap::new()));

    let cwd_display = shorten_cwd(&cwd);
    let initial_model_name = "Connecting...".to_owned();

    let mut app = App {
        messages: vec![super::ChatMessage::welcome(&initial_model_name, &cwd_display)],
        viewport: ChatViewport::new(),
        input: super::InputState::new(),
        status: AppStatus::Connecting,
        should_quit: false,
        session_id: None,
        conn: None,
        adapter_child: None,
        model_name: initial_model_name,
        cwd_raw: cwd.to_string_lossy().to_string(),
        cwd: cwd_display,
        files_accessed: 0,
        mode: None,
        login_hint: None,
        pending_permission_ids: Vec::new(),
        cancelled_turn_pending_hint: false,
        event_tx,
        event_rx,
        spinner_frame: 0,
        tools_collapsed: true,
        active_task_ids: HashSet::new(),
        terminals,
        force_redraw: false,
        tool_call_index: HashMap::new(),
        todos: Vec::<TodoItem>::new(),
        show_todo_panel: false,
        todo_scroll: 0,
        todo_selected: 0,
        focus: FocusManager::default(),
        available_commands: Vec::new(),
        cached_frame_area: ratatui::layout::Rect::new(0, 0, 0, 0),
        selection: Option::<SelectionState>::None,
        rendered_chat_lines: Vec::new(),
        rendered_chat_area: ratatui::layout::Rect::new(0, 0, 0, 0),
        rendered_input_lines: Vec::new(),
        rendered_input_area: ratatui::layout::Rect::new(0, 0, 0, 0),
        mention: None,
        pending_submit: false,
        drain_key_count: 0,
        paste_burst: crate::app::paste_burst::PasteBurstDetector::new(),
        pending_paste_text: String::new(),
        file_cache: None,
        input_wrap_cache: None,
        cached_todo_compact: None,
        git_branch: None,
        cached_header_line: None,
        cached_footer_line: None,
        terminal_tool_calls: Vec::new(),
        needs_redraw: true,
        perf: cli
            .perf_log
            .as_deref()
            .and_then(|path| crate::perf::PerfLogger::open(path, cli.perf_append)),
        fps_ema: None,
        last_frame_at: None,
    };

    app.refresh_git_branch();
    app
}

/// Spawn the background connection task. Uses `spawn_local` so it runs on the
/// same `LocalSet` as the TUI — `Rc<Connection>` stays on one thread.
///
/// On success, stores the connection in `app.conn` via a shared slot and sends
/// `ClientEvent::Connected`. On auth error, sends `ClientEvent::AuthRequired`.
/// On failure, sends `ClientEvent::ConnectionFailed`.
#[allow(clippy::too_many_lines, clippy::items_after_statements, clippy::similar_names)]
pub fn start_connection(app: &App, cli: &Cli, launchers: Vec<connection::AdapterLauncher>) {
    let event_tx = app.event_tx.clone();
    let terminals = Rc::clone(&app.terminals);
    let cwd_raw = app.cwd_raw.clone();
    let cwd = PathBuf::from(&cwd_raw);
    let yolo = cli.yolo;
    let model_override = cli.model.clone();
    let resume_id = cli.resume.clone();

    // Rc<Connection> is !Send, so it can't be sent through the mpsc channel.
    // Instead, the task deposits it into a thread-local slot, then signals
    // via ClientEvent::Connected. The event handler calls take_connection_slot().
    let conn_slot: Rc<std::cell::RefCell<Option<ConnectionSlot>>> =
        Rc::new(std::cell::RefCell::new(None));
    let conn_slot_writer = Rc::clone(&conn_slot);

    tokio::task::spawn_local(async move {
        let result = connect_impl(
            &event_tx,
            &terminals,
            &cwd,
            &launchers,
            yolo,
            model_override.as_deref(),
            resume_id.as_deref(),
        )
        .await;

        match result {
            Ok((conn, child, session_id, model_name, mode)) => {
                // Deposit connection + child in the shared slot
                *conn_slot_writer.borrow_mut() =
                    Some(ConnectionSlot { conn: Rc::clone(&conn), child });
                let _ = event_tx.send(ClientEvent::Connected { session_id, model_name, mode });
            }
            Err(ConnectError::AuthRequired { method_name, method_description }) => {
                let _ =
                    event_tx.send(ClientEvent::AuthRequired { method_name, method_description });
            }
            Err(ConnectError::Failed(msg)) => {
                let _ = event_tx.send(ClientEvent::ConnectionFailed(msg));
            }
        }
    });

    // Store the slot in a thread-local so handle_acp_event can retrieve the
    // Rc<Connection> when ClientEvent::Connected arrives. This is safe because
    // start_connection() must only be called once per app lifetime.
    CONN_SLOT.with(|slot| {
        debug_assert!(
            slot.borrow().is_none(),
            "CONN_SLOT already populated -- start_connection() called twice?"
        );
        *slot.borrow_mut() = Some(conn_slot);
    });
}

/// Shared slot for passing `Rc<Connection>` from the background task to the event loop.
pub struct ConnectionSlot {
    pub conn: Rc<acp::ClientSideConnection>,
    pub child: tokio::process::Child,
}

// Thread-local storage for the connection slot. Used by start_connection() to deposit
// and by handle_acp_event() to retrieve the Rc<Connection>.
thread_local! {
    pub static CONN_SLOT: std::cell::RefCell<Option<Rc<std::cell::RefCell<Option<ConnectionSlot>>>>> =
        const { std::cell::RefCell::new(None) };
}

/// Take the connection data from the thread-local slot. Called once when
/// `ClientEvent::Connected` is received.
pub(super) fn take_connection_slot() -> Option<ConnectionSlot> {
    CONN_SLOT.with(|slot| slot.borrow().as_ref().and_then(|inner| inner.borrow_mut().take()))
}

/// Internal error type for the connection task.
enum ConnectError {
    AuthRequired { method_name: String, method_description: String },
    Failed(String),
}

/// The actual connection logic, extracted from the old `connect()`.
/// Runs inside `spawn_local` — can use `Rc`, `!Send` types freely.
#[allow(clippy::too_many_lines, clippy::similar_names)]
async fn connect_impl(
    event_tx: &mpsc::UnboundedSender<ClientEvent>,
    terminals: &crate::acp::client::TerminalMap,
    cwd: &std::path::Path,
    launchers: &[connection::AdapterLauncher],
    yolo: bool,
    model_override: Option<&str>,
    resume_id: Option<&str>,
) -> Result<
    (
        Rc<acp::ClientSideConnection>,
        tokio::process::Child,
        acp::SessionId,
        String,
        Option<ModeState>,
    ),
    ConnectError,
> {
    if launchers.is_empty() {
        return Err(ConnectError::Failed("No adapter launchers configured".into()));
    }

    let mut failures = Vec::new();
    for launcher in launchers {
        let started = Instant::now();
        tracing::info!("Connecting with adapter launcher: {}", launcher.describe());
        match connect_with_launcher(
            event_tx,
            terminals,
            cwd,
            launcher,
            yolo,
            model_override,
            resume_id,
        )
        .await
        {
            Ok(result) => {
                tracing::info!("Connected via {} in {:?}", launcher.describe(), started.elapsed());
                return Ok(result);
            }
            Err(auth_required @ ConnectError::AuthRequired { .. }) => {
                return Err(auth_required);
            }
            Err(ConnectError::Failed(msg)) => {
                tracing::warn!("Launcher {} failed: {}", launcher.describe(), msg);
                failures.push(format!("{}: {msg}", launcher.describe()));
            }
        }
    }

    Err(ConnectError::Failed(format!("All adapter launchers failed: {}", failures.join(" | "))))
}

#[allow(clippy::too_many_lines, clippy::similar_names)]
async fn connect_with_launcher(
    event_tx: &mpsc::UnboundedSender<ClientEvent>,
    terminals: &crate::acp::client::TerminalMap,
    cwd: &std::path::Path,
    launcher: &connection::AdapterLauncher,
    yolo: bool,
    model_override: Option<&str>,
    resume_id: Option<&str>,
) -> Result<
    (
        Rc<acp::ClientSideConnection>,
        tokio::process::Child,
        acp::SessionId,
        String,
        Option<ModeState>,
    ),
    ConnectError,
> {
    let client = ClaudeClient::with_terminals(
        event_tx.clone(),
        yolo,
        cwd.to_path_buf(),
        Rc::clone(terminals),
    );

    let adapter_start = Instant::now();
    let adapter = connection::spawn_adapter(client, launcher, cwd)
        .await
        .map_err(|e| ConnectError::Failed(format!("Failed to spawn adapter: {e}")))?;
    tracing::debug!("Spawned adapter via {} in {:?}", launcher.describe(), adapter_start.elapsed());
    let child = adapter.child;
    let conn = Rc::new(adapter.connection);

    // Initialize handshake
    let handshake_start = Instant::now();
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
                .client_info(acp::Implementation::new("claude-rust", env!("CARGO_PKG_VERSION"))),
        )
        .await
        .map_err(|e| ConnectError::Failed(format!("Handshake failed: {e}")))?;
    tracing::debug!(
        "Handshake via {} completed in {:?}",
        launcher.describe(),
        handshake_start.elapsed()
    );

    tracing::info!("Connected to agent: {:?}", init_response);

    // Create or resume session — on AuthRequired, signal back instead of blocking
    let session_result = if let Some(sid) = resume_id {
        let session_id = acp::SessionId::new(sid);
        let load_req = acp::LoadSessionRequest::new(session_id.clone(), cwd);
        match conn.load_session(load_req).await {
            Ok(resp) => Ok((session_id, resp.models, resp.modes)),
            Err(err) if err.code == acp::ErrorCode::AuthRequired => {
                return Err(auth_required_error(&init_response));
            }
            Err(err) => Err(err),
        }
    } else {
        match conn.new_session(acp::NewSessionRequest::new(cwd)).await {
            Ok(resp) => Ok((resp.session_id, resp.models, resp.modes)),
            Err(err) if err.code == acp::ErrorCode::AuthRequired => {
                return Err(auth_required_error(&init_response));
            }
            Err(err) => Err(err),
        }
    };

    let (session_id, resp_models, resp_modes) = session_result
        .map_err(|e| ConnectError::Failed(format!("Session creation failed: {e}")))?;

    // Extract model name
    let mut model_name = resp_models
        .as_ref()
        .and_then(|m| {
            m.available_models
                .iter()
                .find(|info| info.model_id == m.current_model_id)
                .map(|info| info.name.clone())
        })
        .unwrap_or_else(|| "Unknown model".to_owned());

    // --model override
    if let Some(model_str) = model_override {
        conn.set_session_model(acp::SetSessionModelRequest::new(
            session_id.clone(),
            acp::ModelId::new(model_str),
        ))
        .await
        .map_err(|e| ConnectError::Failed(format!("Model switch failed: {e}")))?;
        model_str.clone_into(&mut model_name);
    }

    // Extract mode state
    let mut mode = resp_modes.map(|ms| {
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

    if let Some(ref m) = mode {
        tracing::info!(
            "Available modes: {:?}",
            m.available_modes.iter().map(|m| &m.id).collect::<Vec<_>>()
        );
        tracing::info!("Current mode: {}", m.current_mode_id);
    }

    // --yolo: switch to bypass-permissions mode
    if yolo && let Some(ref mut ms) = mode {
        let target_id = "bypassPermissions".to_owned();
        let mode_id = acp::SessionModeId::new(target_id.as_str());
        conn.set_session_mode(acp::SetSessionModeRequest::new(session_id.clone(), mode_id))
            .await
            .map_err(|e| ConnectError::Failed(format!("Mode switch failed: {e}")))?;
        tracing::info!("YOLO: switched to mode '{}'", target_id);
        let target_name = ms
            .available_modes
            .iter()
            .find(|mi| mi.id == target_id)
            .map_or_else(|| target_id.clone(), |mi| mi.name.clone());
        ms.current_mode_id = target_id;
        ms.current_mode_name = target_name;
    }

    tracing::info!("Session created: {:?}", session_id);

    Ok((conn, child, session_id, model_name, mode))
}

/// Build a `ConnectError::AuthRequired` from the adapter's init response.
fn auth_required_error(init_response: &acp::InitializeResponse) -> ConnectError {
    let method = init_response.auth_methods.first();
    ConnectError::AuthRequired {
        method_name: method.map_or_else(|| "unknown".into(), |m| m.name.clone()),
        method_description: method
            .and_then(|m| m.description.clone())
            .unwrap_or_else(|| "Sign in to continue".into()),
    }
}
