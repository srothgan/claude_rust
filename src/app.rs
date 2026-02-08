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

use crate::acp::client::{ClientEvent, ClaudeClient};
use crate::acp::connection;
use crate::Cli;
use agent_client_protocol::{self as acp, Agent as _};
use std::path::PathBuf;
use tokio::io::AsyncBufReadExt;

pub async fn run(cli: Cli, npx_path: PathBuf) -> anyhow::Result<()> {
    let cwd = cli
        .dir
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

    let client = ClaudeClient::new(event_tx, cli.yolo, cwd.clone());

    tracing::info!("Spawning ACP adapter...");
    let adapter = connection::spawn_adapter(client, &npx_path).await?;
    let conn = adapter.connection;

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

    // Try to create a session. If the agent returns AuthRequired, authenticate first.
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

            // Retry session creation after authentication
            let resp = conn.new_session(acp::NewSessionRequest::new(&cwd)).await?;
            resp.session_id
        }
        Err(err) => return Err(err.into()),
    };

    tracing::info!("Session created: {:?}", session_id);

    println!("Claude Rust - connected. Type your message (Ctrl+C to exit):");

    // Spawn event consumer that prints streaming output
    tokio::task::spawn_local(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                ClientEvent::SessionUpdate(update) => match update {
                    acp::SessionUpdate::AgentMessageChunk(chunk) => {
                        if let acp::ContentBlock::Text(text) = chunk.content {
                            print!("{}", text.text);
                        }
                    }
                    acp::SessionUpdate::ToolCall(tc) => {
                        println!("\n[Tool: {} - {:?}]", tc.title, tc.status);
                    }
                    acp::SessionUpdate::ToolCallUpdate(tcu) => {
                        if let Some(title) = &tcu.fields.title {
                            tracing::debug!("Tool update: {}", title);
                        }
                    }
                    _ => {}
                },
                ClientEvent::PermissionRequest {
                    request,
                    response_tx,
                } => {
                    let title = request
                        .tool_call
                        .fields
                        .title
                        .as_deref()
                        .unwrap_or("unknown");
                    println!("\n[Permission] {title} - auto-approving");
                    if let Some(opt) = request.options.first() {
                        let _ = response_tx.send(acp::RequestPermissionResponse::new(
                            acp::RequestPermissionOutcome::Selected(
                                acp::SelectedPermissionOutcome::new(opt.option_id.clone()),
                            ),
                        ));
                    }
                }
            }
        }
    });

    // Read lines from stdin and send as prompts
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        if line.is_empty() {
            continue;
        }

        let response = conn
            .prompt(acp::PromptRequest::new(
                session_id.clone(),
                vec![acp::ContentBlock::Text(acp::TextContent::new(&line))],
            ))
            .await?;

        println!("\n[Turn ended: {:?}]\n", response.stop_reason);
    }

    Ok(())
}
