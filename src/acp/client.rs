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
use tokio::sync::mpsc;

/// Convert an `std::io::Error` into an `acp::Error` with the appropriate JSON-RPC
/// error code and the original message attached as data.
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
    TurnComplete {
        stop_reason: acp::StopReason,
    },
    /// A prompt turn failed with an error.
    TurnError(String),
}

pub struct ClaudeClient {
    event_tx: mpsc::UnboundedSender<ClientEvent>,
    auto_approve: bool,
    terminals: RefCell<HashMap<String, TerminalProcess>>,
    cwd: PathBuf,
}

struct TerminalProcess {
    child: tokio::process::Child,
}

impl ClaudeClient {
    pub fn new(
        event_tx: mpsc::UnboundedSender<ClientEvent>,
        auto_approve: bool,
        cwd: PathBuf,
    ) -> Self {
        Self {
            event_tx,
            auto_approve,
            terminals: RefCell::new(HashMap::new()),
            cwd,
        }
    }
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
                acp::RequestPermissionOutcome::Selected(
                    acp::SelectedPermissionOutcome::new(allow_option.option_id.clone()),
                ),
            ));
        }

        // Send to UI and wait for user response
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        self.event_tx
            .send(ClientEvent::PermissionRequest {
                request: req,
                response_tx,
            })
            .map_err(|_| {
                acp::Error::internal_error()
                    .data(serde_json::Value::String("Event channel closed".into()))
            })?;

        response_rx.await.map_err(|_| {
            acp::Error::internal_error()
                .data(serde_json::Value::String("Permission dialog cancelled".into()))
        })
    }

    async fn session_notification(
        &self,
        notification: acp::SessionNotification,
    ) -> acp::Result<()> {
        self.event_tx
            .send(ClientEvent::SessionUpdate(notification.update))
            .map_err(|_| {
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
            let start = req
                .line
                .map(|l| (l as usize).saturating_sub(1))
                .unwrap_or(0);
            let end = req
                .limit
                .map(|l| (start + l as usize).min(lines.len()))
                .unwrap_or(lines.len());
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
        tokio::fs::write(&req.path, &req.content)
            .await
            .map_err(io_err)?;
        Ok(acp::WriteTextFileResponse::new())
    }

    async fn create_terminal(
        &self,
        req: acp::CreateTerminalRequest,
    ) -> acp::Result<acp::CreateTerminalResponse> {
        let cwd = req.cwd.unwrap_or_else(|| self.cwd.clone());

        let child = tokio::process::Command::new(&req.command)
            .args(&req.args)
            .current_dir(&cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .envs(req.env.iter().map(|e| (&e.name, &e.value)))
            .spawn()
            .map_err(io_err)?;

        let terminal_id = uuid::Uuid::new_v4().to_string();
        self.terminals
            .borrow_mut()
            .insert(terminal_id.clone(), TerminalProcess { child });

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

        let exit_status = match terminal.child.try_wait().map_err(io_err)? {
            Some(status) => {
                let mut es = acp::TerminalExitStatus::new();
                if let Some(code) = status.code() {
                    es = es.exit_code(code as u32);
                }
                Some(es)
            }
            None => None,
        };

        let mut response = acp::TerminalOutputResponse::new(String::new(), false);
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
            terminal.child.kill().await.map_err(io_err)?;
        }
        Ok(acp::KillTerminalCommandResponse::new())
    }

    async fn wait_for_terminal_exit(
        &self,
        req: acp::WaitForTerminalExitRequest,
    ) -> acp::Result<acp::WaitForTerminalExitResponse> {
        let tid = req.terminal_id.to_string();
        let mut terminals = self.terminals.borrow_mut();
        let terminal = terminals.get_mut(tid.as_str()).ok_or_else(|| {
            acp::Error::internal_error()
                .data(serde_json::Value::String("Terminal not found".into()))
        })?;

        let status = terminal.child.wait().await.map_err(io_err)?;
        let mut exit_status = acp::TerminalExitStatus::new();
        if let Some(code) = status.code() {
            exit_status = exit_status.exit_code(code as u32);
        }

        Ok(acp::WaitForTerminalExitResponse::new(exit_status))
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
