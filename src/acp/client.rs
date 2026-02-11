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

use agent_client_protocol::{self as acp};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

/// Convert an `std::io::Error` into an `acp::Error` with the appropriate JSON-RPC
/// error code and the original message attached as data.
#[allow(clippy::needless_pass_by_value)]
fn io_err(e: std::io::Error) -> acp::Error {
    acp::Error::internal_error().data(serde_json::Value::String(e.to_string()))
}

/// Messages sent from the ACP Client impl to the App/UI layer.
pub enum ClientEvent {
    /// Session update notification (streaming text, tool calls, etc.)
    SessionUpdate(acp::SessionUpdate),
    /// Permission request that needs user input.
    PermissionRequest {
        request: acp::RequestPermissionRequest,
        response_tx: tokio::sync::oneshot::Sender<acp::RequestPermissionResponse>,
    },
    /// A prompt turn completed successfully.
    TurnComplete,
    /// A prompt turn failed with an error.
    TurnError(String),
}

/// Shared handle to all spawned terminal processes.
/// Kept accessible so the app can kill them on exit.
pub type TerminalMap = Rc<RefCell<HashMap<String, TerminalProcess>>>;

pub struct ClaudeClient {
    event_tx: mpsc::UnboundedSender<ClientEvent>,
    auto_approve: bool,
    terminals: TerminalMap,
    cwd: PathBuf,
}

pub struct TerminalProcess {
    child: tokio::process::Child,
    /// Accumulated stdout+stderr — append-only, never cleared.
    /// Shared with background reader tasks via Arc.
    pub(crate) output_buffer: Arc<Mutex<Vec<u8>>>,
    /// Byte offset: how much of `output_buffer` has already been returned
    /// by `terminal_output` polls. Only the adapter advances this.
    output_cursor: usize,
    /// The shell command that was executed (e.g. "echo hello && ls -la").
    pub(crate) command: String,
}

/// Spawn a background task that reads from an async reader into a shared buffer.
fn spawn_output_reader(
    mut reader: impl tokio::io::AsyncRead + Unpin + 'static,
    buffer: Arc<Mutex<Vec<u8>>>,
) {
    tokio::task::spawn_local(async move {
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut buf) = buffer.lock() {
                        buf.extend_from_slice(&chunk[..n]);
                    } else {
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!("terminal output reader error: {e}");
                    break;
                }
            }
        }
    });
}

impl ClaudeClient {
    pub fn new(
        event_tx: mpsc::UnboundedSender<ClientEvent>,
        auto_approve: bool,
        cwd: PathBuf,
    ) -> (Self, TerminalMap) {
        let terminals = Rc::new(RefCell::new(HashMap::new()));
        (Self { event_tx, auto_approve, terminals: Rc::clone(&terminals), cwd }, terminals)
    }
}

/// Kill all spawned terminal child processes. Call on app exit.
pub fn kill_all_terminals(terminals: &TerminalMap) {
    let mut map = terminals.borrow_mut();
    for (_, terminal) in map.iter_mut() {
        // start_kill is synchronous — sends the kill signal without awaiting
        let _ = terminal.child.start_kill();
    }
    map.clear();
}

#[async_trait::async_trait(?Send)]
impl acp::Client for ClaudeClient {
    async fn request_permission(
        &self,
        req: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        if self.auto_approve {
            let allow_option = req
                .options
                .iter()
                .find(|o| {
                    matches!(
                        o.kind,
                        acp::PermissionOptionKind::AllowOnce
                            | acp::PermissionOptionKind::AllowAlways
                    )
                })
                .ok_or_else(|| {
                    acp::Error::internal_error()
                        .data(serde_json::Value::String("No allow option found".into()))
                })?;

            return Ok(acp::RequestPermissionResponse::new(
                acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(
                    allow_option.option_id.clone(),
                )),
            ));
        }

        // Send to UI and wait for user response
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        self.event_tx.send(ClientEvent::PermissionRequest { request: req, response_tx }).map_err(
            |_| {
                acp::Error::internal_error()
                    .data(serde_json::Value::String("Event channel closed".into()))
            },
        )?;

        response_rx.await.map_err(|_| {
            acp::Error::internal_error()
                .data(serde_json::Value::String("Permission dialog cancelled".into()))
        })
    }

    async fn session_notification(
        &self,
        notification: acp::SessionNotification,
    ) -> acp::Result<()> {
        self.event_tx.send(ClientEvent::SessionUpdate(notification.update)).map_err(|_| {
            acp::Error::internal_error()
                .data(serde_json::Value::String("Event channel closed".into()))
        })?;
        Ok(())
    }

    async fn read_text_file(
        &self,
        req: acp::ReadTextFileRequest,
    ) -> acp::Result<acp::ReadTextFileResponse> {
        let content = tokio::fs::read_to_string(&req.path).await.map_err(io_err)?;

        let filtered = if req.line.is_some() || req.limit.is_some() {
            let lines: Vec<&str> = content.lines().collect();
            let start = req.line.map_or(0, |l| (l as usize).saturating_sub(1));
            let end = req.limit.map_or(lines.len(), |l| (start + l as usize).min(lines.len()));
            lines[start..end].join("\n")
        } else {
            content
        };

        Ok(acp::ReadTextFileResponse::new(filtered))
    }

    async fn write_text_file(
        &self,
        req: acp::WriteTextFileRequest,
    ) -> acp::Result<acp::WriteTextFileResponse> {
        tokio::fs::write(&req.path, &req.content).await.map_err(io_err)?;
        Ok(acp::WriteTextFileResponse::new())
    }

    async fn create_terminal(
        &self,
        req: acp::CreateTerminalRequest,
    ) -> acp::Result<acp::CreateTerminalResponse> {
        let cwd = req.cwd.unwrap_or_else(|| self.cwd.clone());

        // The ACP adapter sends the full shell command as req.command
        // (e.g. "echo hello && ls -la"). We must wrap it in a shell.
        let mut command = if cfg!(windows) {
            let mut c = tokio::process::Command::new("cmd.exe");
            c.arg("/C").arg(&req.command);
            c
        } else {
            let mut c = tokio::process::Command::new("sh");
            c.arg("-c").arg(&req.command);
            c
        };
        // Append any extra args the adapter may send (typically empty)
        command.args(&req.args);

        let mut child = command
            .current_dir(&cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .envs(req.env.iter().map(|e| (&e.name, &e.value)))
            // Force colored output — programs disable colors when stdout is piped.
            // These env vars cover most CLI tools across ecosystems.
            .env("FORCE_COLOR", "1")
            .env("CLICOLOR_FORCE", "1")
            .env("CARGO_TERM_COLOR", "always")
            .spawn()
            .map_err(io_err)?;

        let output_buffer = Arc::new(Mutex::new(Vec::new()));

        // Spawn background tasks to drain stdout and stderr into the shared buffer
        if let Some(stdout) = child.stdout.take() {
            spawn_output_reader(stdout, Arc::clone(&output_buffer));
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_output_reader(stderr, Arc::clone(&output_buffer));
        }

        let terminal_id = uuid::Uuid::new_v4().to_string();
        self.terminals.borrow_mut().insert(
            terminal_id.clone(),
            TerminalProcess {
                child,
                output_buffer,
                output_cursor: 0,
                command: req.command.clone(),
            },
        );

        Ok(acp::CreateTerminalResponse::new(terminal_id))
    }

    async fn terminal_output(
        &self,
        req: acp::TerminalOutputRequest,
    ) -> acp::Result<acp::TerminalOutputResponse> {
        let tid = req.terminal_id.to_string();
        let mut terminals = self.terminals.borrow_mut();
        let terminal = terminals.get_mut(tid.as_str()).ok_or_else(|| {
            acp::Error::internal_error()
                .data(serde_json::Value::String(format!("Terminal not found: {tid}")))
        })?;

        // Return new output since last poll (advance cursor, never clear buffer)
        let output = {
            if let Ok(buf) = terminal.output_buffer.lock() {
                let new_data = &buf[terminal.output_cursor..];
                let data = String::from_utf8_lossy(new_data).to_string();
                terminal.output_cursor = buf.len();
                data
            } else {
                String::new()
            }
        };

        let exit_status = match terminal.child.try_wait().map_err(io_err)? {
            Some(status) => {
                let mut es = acp::TerminalExitStatus::new();
                if let Some(code) = status.code() {
                    es = es.exit_code(code.unsigned_abs());
                }
                Some(es)
            }
            None => None,
        };

        let mut response = acp::TerminalOutputResponse::new(output, false);
        if let Some(es) = exit_status {
            response = response.exit_status(es);
        }
        Ok(response)
    }

    async fn kill_terminal_command(
        &self,
        req: acp::KillTerminalCommandRequest,
    ) -> acp::Result<acp::KillTerminalCommandResponse> {
        let tid = req.terminal_id.to_string();
        let mut terminals = self.terminals.borrow_mut();
        if let Some(terminal) = terminals.get_mut(tid.as_str()) {
            // start_kill sends the signal synchronously — no await needed
            terminal.child.start_kill().map_err(io_err)?;
        }
        Ok(acp::KillTerminalCommandResponse::new())
    }

    async fn wait_for_terminal_exit(
        &self,
        req: acp::WaitForTerminalExitRequest,
    ) -> acp::Result<acp::WaitForTerminalExitResponse> {
        let tid = req.terminal_id.to_string();

        // Poll with try_wait to avoid holding borrow_mut across .await
        loop {
            {
                let mut terminals = self.terminals.borrow_mut();
                let terminal = terminals.get_mut(tid.as_str()).ok_or_else(|| {
                    acp::Error::internal_error()
                        .data(serde_json::Value::String("Terminal not found".into()))
                })?;

                if let Some(status) = terminal.child.try_wait().map_err(io_err)? {
                    let mut exit_status = acp::TerminalExitStatus::new();
                    if let Some(code) = status.code() {
                        exit_status = exit_status.exit_code(code.unsigned_abs());
                    }
                    return Ok(acp::WaitForTerminalExitResponse::new(exit_status));
                }
            } // borrow_mut dropped here

            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    async fn release_terminal(
        &self,
        req: acp::ReleaseTerminalRequest,
    ) -> acp::Result<acp::ReleaseTerminalResponse> {
        let tid = req.terminal_id.to_string();
        self.terminals.borrow_mut().remove(tid.as_str());
        Ok(acp::ReleaseTerminalResponse::new())
    }
}
