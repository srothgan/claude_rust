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

use crate::agent::types;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandEnvelope {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(flatten)]
    pub command: BridgeCommand,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum BridgeCommand {
    Initialize {
        cwd: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        metadata: BTreeMap<String, serde_json::Value>,
    },
    CreateSession {
        cwd: String,
        yolo: bool,
        model: Option<String>,
        resume: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        metadata: BTreeMap<String, serde_json::Value>,
    },
    LoadSession {
        session_id: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        metadata: BTreeMap<String, serde_json::Value>,
    },
    Prompt {
        session_id: String,
        chunks: Vec<types::PromptChunk>,
    },
    CancelTurn {
        session_id: String,
    },
    SetModel {
        session_id: String,
        model: String,
    },
    SetMode {
        session_id: String,
        mode: String,
    },
    NewSession {
        cwd: String,
        yolo: bool,
        model: Option<String>,
    },
    PermissionResponse {
        session_id: String,
        tool_call_id: String,
        outcome: types::PermissionOutcome,
    },
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(flatten)]
    pub event: BridgeEvent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum BridgeEvent {
    Connected {
        session_id: String,
        cwd: String,
        model_name: String,
        mode: Option<types::ModeState>,
        history_updates: Option<Vec<types::SessionUpdate>>,
    },
    AuthRequired {
        method_name: String,
        method_description: String,
    },
    ConnectionFailed {
        message: String,
    },
    SessionUpdate {
        session_id: String,
        update: types::SessionUpdate,
    },
    PermissionRequest {
        session_id: String,
        request: types::PermissionRequest,
    },
    TurnComplete {
        session_id: String,
    },
    TurnError {
        session_id: String,
        message: String,
    },
    SlashError {
        session_id: String,
        message: String,
    },
    SessionReplaced {
        session_id: String,
        cwd: String,
        model_name: String,
        mode: Option<types::ModeState>,
        history_updates: Option<Vec<types::SessionUpdate>>,
    },
    Initialized {
        result: types::InitializeResult,
    },
    SessionsListed {
        sessions: Vec<types::SessionListEntry>,
        next_cursor: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::{BridgeCommand, BridgeEvent, CommandEnvelope, EventEnvelope};
    use crate::agent::types;

    #[test]
    fn command_envelope_roundtrip_json() {
        let env = CommandEnvelope {
            request_id: Some("req-1".to_owned()),
            command: BridgeCommand::SetMode {
                session_id: "s1".to_owned(),
                mode: "plan".to_owned(),
            },
        };
        let json = serde_json::to_string(&env).expect("serialize");
        let decoded: CommandEnvelope = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, env);
    }

    #[test]
    fn event_envelope_roundtrip_json() {
        let env = EventEnvelope {
            request_id: None,
            event: BridgeEvent::SessionUpdate {
                session_id: "session-1".to_owned(),
                update: types::SessionUpdate::CurrentModeUpdate {
                    current_mode_id: "default".to_owned(),
                },
            },
        };
        let json = serde_json::to_string(&env).expect("serialize");
        let decoded: EventEnvelope = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, env);
    }
}
