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
    App, AppStatus, ChatViewport, FocusManager, HelpView, ModeInfo, ModeState, SelectionState,
    TodoItem,
};
use crate::Cli;
use crate::agent::client::{AgentConnection, BridgeClient};
use crate::agent::events::{ClientEvent, TerminalMap};
use crate::agent::model;
use crate::agent::types;
use crate::agent::wire::{BridgeCommand, BridgeEvent, CommandEnvelope, EventEnvelope};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
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

fn resolve_startup_cwd(cli: &Cli) -> PathBuf {
    cli.dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// Create the `App` struct in `Connecting` state. No I/O - returns immediately.
pub fn create_app(cli: &Cli) -> App {
    let cwd = resolve_startup_cwd(cli);

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let terminals: TerminalMap = Rc::new(std::cell::RefCell::new(HashMap::new()));

    let cwd_display = shorten_cwd(&cwd);
    let initial_model_name = "Connecting...".to_owned();

    let mut app = App {
        messages: vec![super::ChatMessage::welcome_with_recent(
            &initial_model_name,
            &cwd_display,
            &[],
        )],
        viewport: ChatViewport::new(),
        input: super::InputState::new(),
        status: AppStatus::Connecting,
        resuming_session_id: None,
        should_quit: false,
        session_id: None,
        conn: None,
        model_name: initial_model_name,
        cwd_raw: cwd.to_string_lossy().to_string(),
        cwd: cwd_display,
        files_accessed: 0,
        mode: None,
        login_hint: None,
        pending_compact_clear: false,
        help_view: HelpView::Keys,
        pending_permission_ids: Vec::new(),
        cancelled_turn_pending_hint: false,
        queued_submission: None,
        pending_cancel_origin: None,
        event_tx,
        event_rx,
        spinner_frame: 0,
        tools_collapsed: true,
        active_task_ids: HashSet::new(),
        terminals,
        force_redraw: false,
        tool_call_index: HashMap::new(),
        todos: Vec::<TodoItem>::new(),
        show_header: true,
        show_todo_panel: false,
        todo_scroll: 0,
        todo_selected: 0,
        focus: FocusManager::default(),
        available_commands: Vec::new(),
        recent_sessions: Vec::new(),
        cached_frame_area: ratatui::layout::Rect::new(0, 0, 0, 0),
        selection: Option::<SelectionState>::None,
        scrollbar_drag: None,
        rendered_chat_lines: Vec::new(),
        rendered_chat_area: ratatui::layout::Rect::new(0, 0, 0, 0),
        rendered_input_lines: Vec::new(),
        rendered_input_area: ratatui::layout::Rect::new(0, 0, 0, 0),
        mention: None,
        slash: None,
        pending_submit: false,
        drain_key_count: 0,
        paste_burst: crate::app::paste_burst::PasteBurstDetector::new(),
        pending_paste_text: String::new(),
        file_cache: None,
        cached_todo_compact: None,
        git_branch: None,
        cached_header_line: None,
        cached_footer_line: None,
        update_check_hint: None,
        session_usage: super::SessionUsageState::default(),
        is_compacting: false,
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

/// Spawn the background bridge task.
#[allow(clippy::too_many_lines)]
pub fn start_connection(app: &App, cli: &Cli) {
    let event_tx = app.event_tx.clone();
    let cwd_raw = app.cwd_raw.clone();
    let bridge_script = cli.bridge_script.clone();
    let yolo = cli.yolo;
    let model_override = cli.model.clone();
    let resume_id = cli.resume.clone();

    let conn_slot: Rc<std::cell::RefCell<Option<ConnectionSlot>>> =
        Rc::new(std::cell::RefCell::new(None));
    let conn_slot_writer = Rc::clone(&conn_slot);

    tokio::task::spawn_local(async move {
        tracing::debug!("starting agent bridge connection task");
        let launcher = match crate::agent::bridge::resolve_bridge_launcher(bridge_script.as_deref())
        {
            Ok(launcher) => launcher,
            Err(err) => {
                tracing::error!("failed to resolve bridge launcher: {err}");
                let _ = event_tx.send(ClientEvent::ConnectionFailed(format!(
                    "Failed to resolve bridge launcher: {err}"
                )));
                return;
            }
        };
        tracing::info!("resolved bridge launcher: {}", launcher.describe());

        let mut bridge = match BridgeClient::spawn(&launcher) {
            Ok(client) => client,
            Err(err) => {
                tracing::error!("failed to spawn bridge process: {err}");
                let _ = event_tx
                    .send(ClientEvent::ConnectionFailed(format!("Failed to spawn bridge: {err}")));
                return;
            }
        };
        tracing::debug!("bridge process spawned");

        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<CommandEnvelope>();
        *conn_slot_writer.borrow_mut() =
            Some(ConnectionSlot { conn: Rc::new(AgentConnection::new(cmd_tx.clone())) });

        let init_cmd = CommandEnvelope {
            request_id: None,
            command: BridgeCommand::Initialize {
                cwd: cwd_raw.clone(),
                metadata: std::collections::BTreeMap::new(),
            },
        };
        if let Err(err) = bridge.send(init_cmd).await {
            tracing::error!("failed to send initialize command to bridge: {err}");
            let _ = event_tx
                .send(ClientEvent::ConnectionFailed(format!("Failed to initialize bridge: {err}")));
            return;
        }
        tracing::debug!("sent initialize command to bridge");

        let create_cmd = if let Some(resume) = resume_id {
            CommandEnvelope {
                request_id: None,
                command: BridgeCommand::LoadSession {
                    session_id: resume,
                    metadata: std::collections::BTreeMap::new(),
                },
            }
        } else {
            CommandEnvelope {
                request_id: None,
                command: BridgeCommand::CreateSession {
                    cwd: cwd_raw.clone(),
                    yolo,
                    model: model_override.clone(),
                    resume: None,
                    metadata: std::collections::BTreeMap::new(),
                },
            }
        };
        if let Err(err) = bridge.send(create_cmd).await {
            tracing::error!("failed to send create/load session command to bridge: {err}");
            let _ = event_tx.send(ClientEvent::ConnectionFailed(format!(
                "Failed to create bridge session: {err}"
            )));
            return;
        }
        tracing::debug!("sent create/load session command to bridge");

        let mut connected_once = false;
        loop {
            tokio::select! {
                Some(cmd) = cmd_rx.recv() => {
                    if let Err(err) = bridge.send(cmd).await {
                        tracing::error!("failed to forward command to bridge: {err}");
                        let _ = event_tx.send(ClientEvent::ConnectionFailed(format!(
                            "Failed to send bridge command: {err}"
                        )));
                        break;
                    }
                }
                event = bridge.recv() => {
                    match event {
                        Ok(Some(envelope)) => {
                            handle_bridge_event(&event_tx, &cmd_tx, &mut connected_once, envelope);
                        }
                        Ok(None) => {
                            tracing::error!("bridge stdout closed unexpectedly");
                            let _ = event_tx.send(ClientEvent::ConnectionFailed(
                                "Bridge process exited unexpectedly".to_owned(),
                            ));
                            break;
                        }
                        Err(err) => {
                            tracing::error!("bridge communication failure: {err}");
                            let _ = event_tx.send(ClientEvent::ConnectionFailed(format!(
                                "Bridge communication failure: {err}"
                            )));
                            break;
                        }
                    }
                }
            }
        }
    });

    CONN_SLOT.with(|slot| {
        debug_assert!(
            slot.borrow().is_none(),
            "CONN_SLOT already populated -- start_connection() called twice?"
        );
        *slot.borrow_mut() = Some(conn_slot);
    });
}

fn handle_bridge_event(
    event_tx: &mpsc::UnboundedSender<ClientEvent>,
    cmd_tx: &mpsc::UnboundedSender<CommandEnvelope>,
    connected_once: &mut bool,
    envelope: EventEnvelope,
) {
    match envelope.event {
        BridgeEvent::Connected { session_id, cwd, model_name, mode, history_updates } => {
            handle_connected_event(
                event_tx,
                connected_once,
                session_id,
                cwd,
                model_name,
                mode,
                history_updates,
            );
        }
        BridgeEvent::AuthRequired { method_name, method_description } => {
            tracing::warn!(
                "bridge reported auth required: method={} desc={}",
                method_name,
                method_description
            );
            let _ = event_tx.send(ClientEvent::AuthRequired { method_name, method_description });
        }
        BridgeEvent::ConnectionFailed { message } => {
            tracing::error!("bridge connection_failed: {message}");
            let _ = event_tx.send(ClientEvent::ConnectionFailed(message));
        }
        BridgeEvent::SessionUpdate { update, .. } => {
            if let Some(update) = map_session_update(update) {
                let _ = event_tx.send(ClientEvent::SessionUpdate(update));
            }
        }
        BridgeEvent::PermissionRequest { session_id, request } => {
            handle_permission_request_event(event_tx, cmd_tx, session_id, request);
        }
        BridgeEvent::TurnComplete { .. } => {
            let _ = event_tx.send(ClientEvent::TurnComplete);
        }
        BridgeEvent::TurnError { message, .. } => {
            tracing::warn!("bridge turn_error: {message}");
            let _ = event_tx.send(ClientEvent::TurnError(message));
        }
        BridgeEvent::SlashError { message, .. } => {
            tracing::warn!("bridge slash_error: {message}");
            let _ = event_tx.send(ClientEvent::SlashCommandError(message));
        }
        BridgeEvent::SessionReplaced { session_id, cwd, model_name, mode, history_updates } => {
            let history_updates = history_updates
                .unwrap_or_default()
                .into_iter()
                .filter_map(map_session_update)
                .collect();
            let _ = event_tx.send(ClientEvent::SessionReplaced {
                session_id: model::SessionId::new(session_id),
                cwd,
                model_name,
                mode: mode.map(convert_mode_state),
                history_updates,
            });
        }
        BridgeEvent::SessionsListed { sessions, next_cursor } => {
            let _ = event_tx.send(ClientEvent::SessionsListed { sessions, next_cursor });
        }
        BridgeEvent::Initialized { .. } => {}
    }
}

fn handle_connected_event(
    event_tx: &mpsc::UnboundedSender<ClientEvent>,
    connected_once: &mut bool,
    session_id: String,
    cwd: String,
    model_name: String,
    mode: Option<types::ModeState>,
    history_updates: Option<Vec<types::SessionUpdate>>,
) {
    tracing::info!("bridge connected: session_id={} cwd={} model={}", session_id, cwd, model_name);
    let mode = mode.map(convert_mode_state);
    let history_updates =
        history_updates.unwrap_or_default().into_iter().filter_map(map_session_update).collect();
    if *connected_once {
        let _ = event_tx.send(ClientEvent::SessionReplaced {
            session_id: model::SessionId::new(session_id),
            cwd,
            model_name,
            mode,
            history_updates,
        });
    } else {
        *connected_once = true;
        let _ = event_tx.send(ClientEvent::Connected {
            session_id: model::SessionId::new(session_id),
            cwd,
            model_name,
            mode,
            history_updates,
        });
    }
}

fn handle_permission_request_event(
    event_tx: &mpsc::UnboundedSender<ClientEvent>,
    cmd_tx: &mpsc::UnboundedSender<CommandEnvelope>,
    session_id: String,
    request: types::PermissionRequest,
) {
    tracing::debug!(
        "bridge permission_request: session_id={} tool_call_id={} options={}",
        session_id,
        request.tool_call.tool_call_id,
        request.options.len()
    );
    let (request, tool_call_id) = map_permission_request(&session_id, request);
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    if event_tx.send(ClientEvent::PermissionRequest { request, response_tx }).is_ok() {
        spawn_permission_response_forwarder(cmd_tx.clone(), response_rx, session_id, tool_call_id);
    }
}

fn spawn_permission_response_forwarder(
    cmd_tx: mpsc::UnboundedSender<CommandEnvelope>,
    response_rx: tokio::sync::oneshot::Receiver<model::RequestPermissionResponse>,
    session_id: String,
    tool_call_id: String,
) {
    tokio::task::spawn_local(async move {
        let Ok(response) = response_rx.await else {
            return;
        };
        let outcome = match response.outcome {
            model::RequestPermissionOutcome::Selected(selected) => {
                let option_id = selected.option_id.clone();
                tracing::debug!(
                    "forward permission_response: session_id={} tool_call_id={} option_id={}",
                    session_id,
                    tool_call_id,
                    option_id
                );
                types::PermissionOutcome::Selected { option_id }
            }
            model::RequestPermissionOutcome::Cancelled => {
                tracing::debug!(
                    "forward permission_response: session_id={} tool_call_id={} outcome=cancelled",
                    session_id,
                    tool_call_id
                );
                types::PermissionOutcome::Cancelled
            }
        };
        let _ = cmd_tx.send(CommandEnvelope {
            request_id: None,
            command: BridgeCommand::PermissionResponse { session_id, tool_call_id, outcome },
        });
    });
}

fn map_session_update(update: types::SessionUpdate) -> Option<model::SessionUpdate> {
    match update {
        types::SessionUpdate::UserMessageChunk { content } => {
            let content = convert_content_block(content)?;
            Some(model::SessionUpdate::UserMessageChunk(model::ContentChunk::new(content)))
        }
        types::SessionUpdate::AgentMessageChunk { content } => {
            let content = convert_content_block(content)?;
            Some(model::SessionUpdate::AgentMessageChunk(model::ContentChunk::new(content)))
        }
        types::SessionUpdate::AgentThoughtChunk { content } => {
            let content = convert_content_block(content)?;
            Some(model::SessionUpdate::AgentThoughtChunk(model::ContentChunk::new(content)))
        }
        types::SessionUpdate::ToolCall { tool_call } => {
            Some(model::SessionUpdate::ToolCall(convert_tool_call(tool_call)))
        }
        types::SessionUpdate::ToolCallUpdate { tool_call_update } => {
            Some(model::SessionUpdate::ToolCallUpdate(convert_tool_call_update(tool_call_update)))
        }
        types::SessionUpdate::Plan { entries } => Some(model::SessionUpdate::Plan(
            model::Plan::new(entries.into_iter().map(convert_plan_entry).collect()),
        )),
        types::SessionUpdate::AvailableCommandsUpdate { commands } => Some(
            model::SessionUpdate::AvailableCommandsUpdate(model::AvailableCommandsUpdate::new(
                commands
                    .into_iter()
                    .map(|cmd| {
                        let mut mapped = model::AvailableCommand::new(cmd.name, cmd.description);
                        if let Some(input_hint) = cmd.input_hint
                            && !input_hint.trim().is_empty()
                        {
                            mapped = mapped.input_hint(input_hint);
                        }
                        mapped
                    })
                    .collect(),
            )),
        ),
        types::SessionUpdate::CurrentModeUpdate { current_mode_id } => {
            Some(model::SessionUpdate::CurrentModeUpdate(model::CurrentModeUpdate::new(
                model::SessionModeId::new(current_mode_id),
            )))
        }
        types::SessionUpdate::ConfigOptionUpdate { .. } => None,
        types::SessionUpdate::UsageUpdate { usage } => {
            Some(model::SessionUpdate::UsageUpdate(model::UsageUpdate {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_read_tokens: usage.cache_read_tokens,
                cache_write_tokens: usage.cache_write_tokens,
                total_cost_usd: usage.total_cost_usd,
                turn_cost_usd: usage.turn_cost_usd,
                context_window: usage.context_window,
                max_output_tokens: usage.max_output_tokens,
            }))
        }
        types::SessionUpdate::SessionStatusUpdate { status } => {
            Some(model::SessionUpdate::SessionStatusUpdate(match status {
                types::SessionStatus::Compacting => model::SessionStatus::Compacting,
                types::SessionStatus::Idle => model::SessionStatus::Idle,
            }))
        }
        types::SessionUpdate::CompactionBoundary { trigger, pre_tokens } => {
            Some(model::SessionUpdate::CompactionBoundary(model::CompactionBoundary {
                trigger: match trigger {
                    types::CompactionTrigger::Manual => model::CompactionTrigger::Manual,
                    types::CompactionTrigger::Auto => model::CompactionTrigger::Auto,
                },
                pre_tokens,
            }))
        }
    }
}

fn map_permission_request(
    session_id: &str,
    request: types::PermissionRequest,
) -> (model::RequestPermissionRequest, String) {
    let tool_call_id = request.tool_call.tool_call_id.clone();
    let tool_call_meta = request.tool_call.meta.clone();
    let tool_call_fields = convert_tool_call_to_fields(request.tool_call);
    let mut tool_call_update = model::ToolCallUpdate::new(tool_call_id.clone(), tool_call_fields);
    if let Some(meta) = tool_call_meta {
        tool_call_update = tool_call_update.meta(meta);
    }
    let options = request
        .options
        .into_iter()
        .map(|opt| {
            let kind = match opt.kind.as_str() {
                "allow_once" => model::PermissionOptionKind::AllowOnce,
                "allow_session" => model::PermissionOptionKind::AllowSession,
                "allow_always" => model::PermissionOptionKind::AllowAlways,
                "reject_always" => model::PermissionOptionKind::RejectAlways,
                "question_choice" => model::PermissionOptionKind::QuestionChoice,
                _ => {
                    tracing::warn!(
                        "unknown permission option kind from bridge; defaulting to reject_once: session_id={} tool_call_id={} option_id={} option_name={} option_kind={}",
                        session_id,
                        tool_call_id,
                        opt.option_id,
                        opt.name,
                        opt.kind
                    );
                    model::PermissionOptionKind::RejectOnce
                }
            };
            model::PermissionOption::new(opt.option_id, opt.name, kind).description(opt.description)
        })
        .collect();
    (
        model::RequestPermissionRequest::new(
            model::SessionId::new(session_id),
            tool_call_update,
            options,
        ),
        tool_call_id,
    )
}

fn convert_content_block(content: types::ContentBlock) -> Option<model::ContentBlock> {
    match content {
        types::ContentBlock::Text { text } => {
            Some(model::ContentBlock::Text(model::TextContent::new(text)))
        }
        // Deferred for parity follow-up per scope.
        types::ContentBlock::Image { .. } => None,
    }
}

fn convert_tool_call(tool_call: types::ToolCall) -> model::ToolCall {
    let types::ToolCall {
        tool_call_id,
        title,
        kind,
        status,
        content,
        raw_input,
        raw_output,
        locations,
        meta,
    } = tool_call;

    let mut tc = model::ToolCall::new(tool_call_id, title)
        .kind(convert_tool_kind(&kind))
        .status(convert_tool_status(&status))
        .content(content.into_iter().filter_map(convert_tool_call_content).collect())
        .locations(
            locations
                .into_iter()
                .map(|loc| {
                    let mut location = model::ToolCallLocation::new(loc.path);
                    if let Some(line) = loc.line.and_then(|line| u32::try_from(line).ok()) {
                        location = location.line(line);
                    }
                    location
                })
                .collect(),
        );

    if let Some(raw_input) = raw_input {
        tc = tc.raw_input(raw_input);
    }

    if let Some(raw_output) = raw_output {
        tc = tc.raw_output(serde_json::Value::String(raw_output));
    }
    if let Some(meta) = meta {
        tc = tc.meta(meta);
    }

    tc
}

fn convert_tool_call_update(update: types::ToolCallUpdate) -> model::ToolCallUpdate {
    let update_meta = update.fields.meta.clone();
    let mut out = model::ToolCallUpdate::new(
        update.tool_call_id,
        convert_tool_call_update_fields(update.fields),
    );
    if let Some(meta) = update_meta {
        out = out.meta(meta);
    }
    out
}

fn convert_tool_call_to_fields(tool_call: types::ToolCall) -> model::ToolCallUpdateFields {
    let mut fields = model::ToolCallUpdateFields::new()
        .title(tool_call.title)
        .kind(convert_tool_kind(&tool_call.kind))
        .status(convert_tool_status(&tool_call.status))
        .content(
            tool_call.content.into_iter().filter_map(convert_tool_call_content).collect::<Vec<_>>(),
        )
        .locations(
            tool_call
                .locations
                .into_iter()
                .map(|loc| {
                    let mut location = model::ToolCallLocation::new(loc.path);
                    if let Some(line) = loc.line.and_then(|line| u32::try_from(line).ok()) {
                        location = location.line(line);
                    }
                    location
                })
                .collect::<Vec<_>>(),
        );

    if let Some(raw_input) = tool_call.raw_input {
        fields = fields.raw_input(raw_input);
    }

    if let Some(raw_output) = tool_call.raw_output {
        fields = fields.raw_output(serde_json::Value::String(raw_output));
    }

    fields
}

fn convert_tool_call_update_fields(
    fields: types::ToolCallUpdateFields,
) -> model::ToolCallUpdateFields {
    let mut out = model::ToolCallUpdateFields::new();

    if let Some(title) = fields.title {
        out = out.title(title);
    }
    if let Some(kind) = fields.kind {
        out = out.kind(convert_tool_kind(&kind));
    }
    if let Some(status) = fields.status {
        out = out.status(convert_tool_status(&status));
    }
    if let Some(content) = fields.content {
        out = out
            .content(content.into_iter().filter_map(convert_tool_call_content).collect::<Vec<_>>());
    }
    if let Some(raw_input) = fields.raw_input {
        out = out.raw_input(raw_input);
    }
    if let Some(raw_output) = fields.raw_output {
        out = out.raw_output(serde_json::Value::String(raw_output));
    }
    if let Some(locations) = fields.locations {
        out = out.locations(
            locations
                .into_iter()
                .map(|loc| {
                    let mut location = model::ToolCallLocation::new(loc.path);
                    if let Some(line) = loc.line.and_then(|line| u32::try_from(line).ok()) {
                        location = location.line(line);
                    }
                    location
                })
                .collect::<Vec<_>>(),
        );
    }

    out
}

fn convert_tool_call_content(
    tool_content: types::ToolCallContent,
) -> Option<model::ToolCallContent> {
    match tool_content {
        types::ToolCallContent::Content { content } => {
            let block = convert_content_block(content)?;
            Some(model::ToolCallContent::Content(model::Content::new(block)))
        }
        types::ToolCallContent::Diff { old_path: _, new_path, old, new } => {
            Some(model::ToolCallContent::Diff(model::Diff::new(new_path, new).old_text(Some(old))))
        }
    }
}

fn convert_tool_kind(kind: &str) -> model::ToolKind {
    match kind {
        "read" => model::ToolKind::Read,
        "edit" => model::ToolKind::Edit,
        "delete" => model::ToolKind::Delete,
        "move" => model::ToolKind::Move,
        "execute" => model::ToolKind::Execute,
        "search" => model::ToolKind::Search,
        "fetch" => model::ToolKind::Fetch,
        "switch_mode" => model::ToolKind::SwitchMode,
        "other" => model::ToolKind::Other,
        _ => model::ToolKind::Think,
    }
}

fn convert_tool_status(status: &str) -> model::ToolCallStatus {
    match status {
        "in_progress" => model::ToolCallStatus::InProgress,
        "completed" => model::ToolCallStatus::Completed,
        "failed" => model::ToolCallStatus::Failed,
        _ => model::ToolCallStatus::Pending,
    }
}

fn convert_plan_entry(entry: types::PlanEntry) -> model::PlanEntry {
    let status = match entry.status.as_str() {
        "in_progress" => model::PlanEntryStatus::InProgress,
        "completed" => model::PlanEntryStatus::Completed,
        _ => model::PlanEntryStatus::Pending,
    };
    model::PlanEntry::new(entry.content, model::PlanEntryPriority::Medium, status)
}

fn convert_mode_state(mode: types::ModeState) -> ModeState {
    let available_modes: Vec<ModeInfo> =
        mode.available_modes.into_iter().map(|m| ModeInfo { id: m.id, name: m.name }).collect();
    ModeState {
        current_mode_id: mode.current_mode_id,
        current_mode_name: mode.current_mode_name,
        available_modes,
    }
}

/// Shared slot for passing `Rc<AgentConnection>` from the background task to the event loop.
pub struct ConnectionSlot {
    pub conn: Rc<AgentConnection>,
}

thread_local! {
    pub static CONN_SLOT: std::cell::RefCell<Option<Rc<std::cell::RefCell<Option<ConnectionSlot>>>>> =
        const { std::cell::RefCell::new(None) };
}

/// Take the connection data from the thread-local slot.
pub(super) fn take_connection_slot() -> Option<ConnectionSlot> {
    CONN_SLOT.with(|slot| slot.borrow().as_ref().and_then(|inner| inner.borrow_mut().take()))
}
