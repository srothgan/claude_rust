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

//! Re-exports of commonly used ACP types for convenience.

pub use agent_client_protocol::{
    // Connection & handshake
    ClientSideConnection, InitializeRequest, InitializeResponse,
    // Client trait
    Client,
    // Agent trait (for calling into the adapter)
    Agent,
    // Session management
    NewSessionRequest, NewSessionResponse, SessionId,
    LoadSessionRequest, LoadSessionResponse,
    // Prompting
    PromptRequest, PromptResponse, ContentBlock, TextContent, StopReason,
    // Streaming updates
    SessionNotification, SessionUpdate,
    // Permissions
    RequestPermissionRequest, RequestPermissionResponse,
    RequestPermissionOutcome, SelectedPermissionOutcome,
    PermissionOption, PermissionOptionKind,
    // Capabilities
    ClientCapabilities, FileSystemCapability, Implementation,
    // Authentication
    AuthenticateRequest, AuthenticateResponse,
    // File operations
    ReadTextFileRequest, ReadTextFileResponse,
    WriteTextFileRequest, WriteTextFileResponse,
    // Terminal operations
    CreateTerminalRequest, CreateTerminalResponse,
    TerminalOutputRequest, TerminalOutputResponse,
    KillTerminalCommandRequest, KillTerminalCommandResponse,
    WaitForTerminalExitRequest, WaitForTerminalExitResponse,
    ReleaseTerminalRequest, ReleaseTerminalResponse,
    // Tool calls
    ToolCall, ToolCallUpdate, ToolCallStatus,
    // Protocol version
    ProtocolVersion,
    // Terminal exit
    TerminalExitStatus,
    // Session models (unstable_session_model)
    ModelId, SessionModelState, ModelInfo,
    SetSessionModelRequest, SetSessionModelResponse,
    // Session modes
    SessionModeState, SessionMode, SessionModeId,
    SetSessionModeRequest, SetSessionModeResponse,
    // Session usage (unstable_session_usage) — logged only, not displayed yet
    UsageUpdate, Cost,
    // Session notifications
    CurrentModeUpdate,
    // Cancel
    CancelNotification,
    // Error codes
    ErrorCode,
};
