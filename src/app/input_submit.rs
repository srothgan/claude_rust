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

use super::{App, AppStatus, BlockCache, ChatMessage, MessageBlock, MessageRole};
use crate::acp::client::ClientEvent;
use agent_client_protocol::{self as acp, Agent as _};
use std::rc::Rc;

pub(super) fn submit_input(app: &mut App, conn: &Rc<acp::ClientSideConnection>) {
    let text = app.input.text();
    if text.trim().is_empty() {
        return;
    }

    app.messages.push(ChatMessage {
        role: MessageRole::User,
        blocks: vec![MessageBlock::Text(text.clone(), BlockCache::default())],
    });
    // Create empty assistant message immediately -- message.rs shows thinking indicator
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
