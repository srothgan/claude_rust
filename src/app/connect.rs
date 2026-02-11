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

use super::{App, AppStatus, ModeInfo, ModeState, SelectionState, TodoItem};
use crate::Cli;
use crate::acp::client::{ClaudeClient, TerminalMap};
use crate::acp::connection;
use agent_client_protocol::{self as acp, Agent as _};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use tokio::process::Child;
use tokio::sync::mpsc;

/// Connect to the ACP adapter, handshake, authenticate, and create a session.
/// Runs before `ratatui::init()` so errors print to stderr normally.
/// Returns `(App, Rc<Connection>, Child, TerminalMap)`. The `Child` handle must be
/// kept alive for the adapter process lifetime -- dropping it kills the process.
/// The `TerminalMap` is used for cleanup on exit.
#[allow(clippy::too_many_lines, clippy::items_after_statements, clippy::similar_names)]
pub async fn connect(
    cli: Cli,
    npx_path: PathBuf,
) -> anyhow::Result<(App, Rc<acp::ClientSideConnection>, Child, TerminalMap)> {
    let cwd = match cli.dir {
        Some(dir) => dir,
        None => std::env::current_dir()?,
    };

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (client, terminals) = ClaudeClient::new(event_tx.clone(), cli.yolo, cwd.clone());

    eprintln!("Spawning ACP adapter...");
    let adapter = connection::spawn_adapter(client, &npx_path, &cwd).await?;
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
                .client_info(acp::Implementation::new("claude-rust", env!("CARGO_PKG_VERSION"))),
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
        conn.authenticate(acp::AuthenticateRequest::new(method.id.clone())).await?;
        Ok(f().await?)
    }

    // Create or resume session
    let (session_id, resp_models, resp_modes) = if let Some(ref sid) = cli.resume {
        // --resume <session_id>: load existing session
        eprintln!("Resuming session {sid}...");
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
        .unwrap_or_else(|| "Unknown model".to_owned());

    // --model override: switch after session creation
    if let Some(ref model_str) = cli.model {
        conn.set_session_model(acp::SetSessionModelRequest::new(
            session_id.clone(),
            acp::ModelId::new(model_str.as_str()),
        ))
        .await?;
        model_name.clone_from(model_str);
    }

    // Extract mode state from session response
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

    // Log available modes for debugging
    if let Some(ref m) = mode {
        tracing::info!(
            "Available modes: {:?}",
            m.available_modes.iter().map(|m| &m.id).collect::<Vec<_>>()
        );
        tracing::info!("Current mode: {}", m.current_mode_id);
    }

    // --yolo: switch to bypass-permissions mode via the adapter
    if cli.yolo
        && let Some(ref mut ms) = mode
    {
        let target_id = "bypassPermissions".to_owned();
        let mode_id = acp::SessionModeId::new(target_id.as_str());
        conn.set_session_mode(acp::SetSessionModeRequest::new(session_id.clone(), mode_id)).await?;
        tracing::info!("YOLO: switched to mode '{}'", target_id);
        // Update local mode state to reflect the switch
        let target_name = ms
            .available_modes
            .iter()
            .find(|mi| mi.id == target_id)
            .map_or_else(|| target_id.clone(), |mi| mi.name.clone());
        ms.current_mode_id = target_id;
        ms.current_mode_name = target_name;
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
        scroll_target: 0,
        scroll_pos: 0.0,
        auto_scroll: true,
        input: super::InputState::new(),
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
        todos: Vec::<TodoItem>::new(),
        show_todo_panel: false,
        todo_scroll: 0,
        available_commands: Vec::new(),
        cached_frame_area: ratatui::layout::Rect::new(0, 0, 0, 0),
        selection: Option::<SelectionState>::None,
        rendered_chat_lines: Vec::new(),
        rendered_chat_area: ratatui::layout::Rect::new(0, 0, 0, 0),
        rendered_input_lines: Vec::new(),
        rendered_input_area: ratatui::layout::Rect::new(0, 0, 0, 0),
        mention: None,
        file_cache: None,
        cached_welcome_lines: None,
        input_wrap_cache: None,
        cached_todo_compact: None,
        cached_header_line: None,
        cached_footer_line: None,
    };

    Ok((app, conn, child, terminals))
}
