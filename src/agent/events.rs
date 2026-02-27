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

use crate::agent::error_handling::TurnErrorClass;
use crate::agent::model;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

/// Messages sent from the backend bridge path to the App/UI layer.
pub enum ClientEvent {
    /// Session update notification (streaming text, tool calls, etc.)
    SessionUpdate(model::SessionUpdate),
    /// Permission request that needs user input.
    PermissionRequest {
        request: model::RequestPermissionRequest,
        response_tx: tokio::sync::oneshot::Sender<model::RequestPermissionResponse>,
    },
    /// A prompt turn completed successfully.
    TurnComplete,
    /// `cancel` notification was accepted by the bridge.
    TurnCancelled,
    /// A prompt turn failed with an error.
    TurnError(String),
    /// A prompt turn failed with bridge-provided classification metadata.
    TurnErrorClassified { message: String, class: TurnErrorClass },
    /// Background connection completed successfully.
    Connected {
        session_id: model::SessionId,
        cwd: String,
        model_name: String,
        mode: Option<crate::app::ModeState>,
        history_updates: Vec<model::SessionUpdate>,
    },
    /// Background connection failed.
    ConnectionFailed(String),
    /// Authentication is required before a session can be created.
    AuthRequired { method_name: String, method_description: String },
    /// Slash-command execution failed with a user-facing error.
    SlashCommandError(String),
    /// Custom slash command replaced the active session.
    SessionReplaced {
        session_id: model::SessionId,
        cwd: String,
        model_name: String,
        mode: Option<crate::app::ModeState>,
        history_updates: Vec<model::SessionUpdate>,
    },
    /// Recent persisted sessions discovered by the bridge.
    SessionsListed {
        sessions: Vec<crate::agent::types::SessionListEntry>,
        next_cursor: Option<String>,
    },
    /// Startup update check found a newer published version.
    UpdateAvailable { latest_version: String, current_version: String },
}

/// Shared handle to all spawned terminal processes.
pub type TerminalMap = Rc<RefCell<HashMap<String, TerminalProcess>>>;

/// Minimal terminal process state used by UI snapshot rendering.
pub struct TerminalProcess {
    pub child: Option<tokio::process::Child>,
    /// Accumulated stdout+stderr - append-only, never cleared.
    pub output_buffer: Arc<Mutex<Vec<u8>>>,
    /// The shell command that was executed.
    pub command: String,
}

/// Kill all spawned terminal child processes. Call on app exit.
pub fn kill_all_terminals(terminals: &TerminalMap) {
    let mut map = terminals.borrow_mut();
    for (_, terminal) in map.iter_mut() {
        if let Some(child) = terminal.child.as_mut() {
            let _ = child.start_kill();
        }
    }
    map.clear();
}
