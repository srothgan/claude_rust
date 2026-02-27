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

use crate::agent::bridge::BridgeLauncher;
use crate::agent::wire::{BridgeCommand, CommandEnvelope, EventEnvelope};
use anyhow::Context as _;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, BufWriter};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout};
use tokio::sync::mpsc;

pub struct BridgeClient {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: tokio::io::Lines<BufReader<ChildStdout>>,
}

impl BridgeClient {
    pub fn spawn(launcher: &BridgeLauncher) -> anyhow::Result<Self> {
        let mut child = launcher
            .command()
            .spawn()
            .with_context(|| format!("failed to spawn bridge process: {}", launcher.describe()))?;

        let stdin = child.stdin.take().context("bridge stdin not available")?;
        let stdout = child.stdout.take().context("bridge stdout not available")?;
        let stderr = child.stderr.take().context("bridge stderr not available")?;
        Self::spawn_stderr_logger(stderr);

        Ok(Self { child, stdin: BufWriter::new(stdin), stdout: BufReader::new(stdout).lines() })
    }

    fn spawn_stderr_logger(stderr: ChildStderr) {
        tokio::task::spawn_local(async move {
            let mut lines = BufReader::new(stderr).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => Self::log_bridge_stderr_line(&line),
                    Ok(None) => break,
                    Err(err) => {
                        tracing::error!("failed to read bridge stderr: {err}");
                        break;
                    }
                }
            }
        });
    }

    fn log_bridge_stderr_line(line: &str) {
        // The bridge uses a structured "[sdk <verb>]" prefix format.
        // Extract an explicit level from it; fall back to debug for ordinary chatter.
        let lower = line.to_ascii_lowercase();
        if lower.contains("[sdk error]") || lower.starts_with("error") || lower.contains("panic") {
            tracing::error!("bridge stderr: {line}");
        } else if lower.contains("[sdk warn]") || lower.starts_with("warn") {
            tracing::warn!("bridge stderr: {line}");
        } else {
            tracing::debug!("bridge stderr: {line}");
        }
    }

    pub async fn send(&mut self, envelope: CommandEnvelope) -> anyhow::Result<()> {
        let line =
            serde_json::to_string(&envelope).context("failed to serialize bridge command")?;
        self.stdin.write_all(line.as_bytes()).await.context("failed to write bridge command")?;
        self.stdin.write_all(b"\n").await.context("failed to write bridge newline")?;
        self.stdin.flush().await.context("failed to flush bridge stdin")?;
        Ok(())
    }

    pub async fn recv(&mut self) -> anyhow::Result<Option<EventEnvelope>> {
        let Some(line) = self.stdout.next_line().await.context("failed to read bridge stdout")?
        else {
            return Ok(None);
        };
        let event: EventEnvelope =
            serde_json::from_str(&line).context("failed to decode bridge event json")?;
        Ok(Some(event))
    }

    pub async fn shutdown(&mut self) -> anyhow::Result<()> {
        self.send(CommandEnvelope { request_id: None, command: BridgeCommand::Shutdown }).await?;
        Ok(())
    }

    pub async fn wait(mut self) -> anyhow::Result<std::process::ExitStatus> {
        self.child.wait().await.context("failed to wait for bridge process")
    }
}

#[derive(Clone)]
pub struct AgentConnection {
    command_tx: mpsc::UnboundedSender<CommandEnvelope>,
}

#[derive(Debug, Clone)]
pub struct PromptResponse {
    pub stop_reason: String,
}

impl AgentConnection {
    #[must_use]
    pub fn new(command_tx: mpsc::UnboundedSender<CommandEnvelope>) -> Self {
        Self { command_tx }
    }

    pub fn prompt_text(&self, session_id: String, text: String) -> anyhow::Result<PromptResponse> {
        self.send(CommandEnvelope {
            request_id: None,
            command: BridgeCommand::Prompt {
                session_id,
                chunks: vec![crate::agent::types::PromptChunk {
                    kind: "text".to_owned(),
                    value: serde_json::Value::String(text),
                }],
            },
        })?;
        Ok(PromptResponse { stop_reason: "end_turn".to_owned() })
    }

    pub fn cancel(&self, session_id: String) -> anyhow::Result<()> {
        self.send(CommandEnvelope {
            request_id: None,
            command: BridgeCommand::CancelTurn { session_id },
        })
    }

    pub fn set_mode(&self, session_id: String, mode: String) -> anyhow::Result<()> {
        self.send(CommandEnvelope {
            request_id: None,
            command: BridgeCommand::SetMode { session_id, mode },
        })
    }

    pub fn set_model(&self, session_id: String, model: String) -> anyhow::Result<()> {
        self.send(CommandEnvelope {
            request_id: None,
            command: BridgeCommand::SetModel { session_id, model },
        })
    }

    pub fn new_session(
        &self,
        cwd: String,
        yolo: bool,
        model: Option<String>,
    ) -> anyhow::Result<()> {
        self.send(CommandEnvelope {
            request_id: None,
            command: BridgeCommand::NewSession { cwd, yolo, model },
        })
    }

    pub fn load_session(&self, session_id: String) -> anyhow::Result<()> {
        self.send(CommandEnvelope {
            request_id: None,
            command: BridgeCommand::LoadSession {
                session_id,
                metadata: std::collections::BTreeMap::new(),
            },
        })
    }

    fn send(&self, envelope: CommandEnvelope) -> anyhow::Result<()> {
        self.command_tx.send(envelope).map_err(|_| anyhow::anyhow!("bridge command channel closed"))
    }
}
