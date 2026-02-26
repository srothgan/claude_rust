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
    App, AppStatus, BlockCache, ChatMessage, IncrementalMarkdown, MessageBlock, MessageRole,
};
use crate::agent::events::ClientEvent;
use crate::agent::model;
use crate::app::slash;
use std::rc::Rc;

pub(super) fn submit_input(app: &mut App) {
    if matches!(app.status, AppStatus::Connecting | AppStatus::Resuming | AppStatus::Error) {
        return;
    }

    // Dismiss any open mention dropdown
    app.mention = None;
    app.slash = None;

    // No connection yet - can't submit
    let text = app.input.text();
    if text.trim().is_empty() {
        return;
    }

    if slash::try_handle_submit(app, &text) {
        return;
    }

    // New turn started by user input: force-stop stale tool calls from older turns
    // so their spinners don't continue during this turn.
    let _ = app.finalize_in_progress_tool_calls(model::ToolCallStatus::Failed);

    let Some(ref conn) = app.conn else { return };

    app.messages.push(ChatMessage {
        role: MessageRole::User,
        blocks: vec![MessageBlock::Text(
            text.clone(),
            BlockCache::default(),
            IncrementalMarkdown::from_complete(&text),
        )],
        usage: None,
    });
    // Create empty assistant message immediately -- message.rs shows thinking indicator
    app.messages.push(ChatMessage {
        role: MessageRole::Assistant,
        blocks: Vec::new(),
        usage: None,
    });
    app.input.clear();
    app.status = AppStatus::Thinking;
    app.viewport.engage_auto_scroll();

    let conn = Rc::clone(conn);
    let Some(sid) = app.session_id.clone() else {
        return;
    };
    let tx = app.event_tx.clone();

    tokio::task::spawn_local(async move {
        match conn.prompt_text(sid.to_string(), text) {
            Ok(resp) => {
                tracing::debug!("Prompt dispatched: stop_reason={:?}", resp.stop_reason);
            }
            Err(e) => {
                let _ = tx.send(ClientEvent::TurnError(e.to_string()));
            }
        }
    });
}
