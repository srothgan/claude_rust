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

use ratatui::style::Color;

// Accent
pub const RUST_ORANGE: Color = Color::Rgb(244, 118, 0);

// UI chrome
pub const DIM: Color = Color::DarkGray;
pub const PROMPT_CHAR: &str = "❯";
pub const SEPARATOR_CHAR: &str = "─";

// Model family colors
pub const MODEL_SONNET: Color = Color::Magenta;
pub const MODEL_OPUS: Color = Color::Red;
pub const MODEL_HAIKU: Color = Color::Green;

// Role header colors
pub const ROLE_USER: Color = Color::White;
pub const ROLE_ASSISTANT: Color = RUST_ORANGE;
pub const ROLE_SYSTEM: Color = Color::Yellow;

// User message background
pub const USER_MSG_BG: Color = Color::Rgb(40, 44, 52);

// Tool kind colors (muted, no bright green/cyan)
pub const TOOL_READ: Color = Color::White;
pub const TOOL_EDIT: Color = Color::White;
pub const TOOL_EXECUTE: Color = Color::White;
pub const TOOL_SEARCH: Color = Color::White;

// Tool status icons
pub const ICON_PENDING: &str = "◌";
pub const ICON_RUNNING: &str = "⏵";
pub const ICON_COMPLETED: &str = "✓";
pub const ICON_FAILED: &str = "✗";

// Status colors (used by tool call rendering in message.rs)
pub const STATUS_RUNNING: Color = Color::Cyan;
pub const STATUS_ERROR: Color = Color::Red;

/// Return a color for the model based on its name.
pub fn model_color(name: &str) -> Color {
    let lower = name.to_lowercase();
    if lower.contains("opus") {
        MODEL_OPUS
    } else if lower.contains("haiku") {
        MODEL_HAIKU
    } else if lower.contains("sonnet") {
        MODEL_SONNET
    } else {
        RUST_ORANGE
    }
}

/// Tool kind icon + label pair. Monochrome Unicode symbols.
pub fn tool_kind_label(kind: agent_client_protocol::ToolKind) -> (&'static str, &'static str) {
    use agent_client_protocol::ToolKind;
    match kind {
        ToolKind::Read => ("⬚", "Read"),
        ToolKind::Edit => ("▣", "Edit"),
        ToolKind::Delete => ("▣", "Delete"),
        ToolKind::Move => ("⇄", "Move"),
        ToolKind::Search => ("⌕", "Find"),
        ToolKind::Execute => ("⟩", "Bash"),
        ToolKind::Think => ("❖", "Think"),
        ToolKind::Fetch => ("↯", "Fetch"),
        ToolKind::SwitchMode => ("⊕", "Mode"),
        _ => ("○", "Tool"),
    }
}

/// Map a tool kind to its color.
pub fn tool_kind_color(kind: agent_client_protocol::ToolKind) -> Color {
    match kind {
        agent_client_protocol::ToolKind::Read => TOOL_READ,
        agent_client_protocol::ToolKind::Edit => TOOL_EDIT,
        agent_client_protocol::ToolKind::Execute => TOOL_EXECUTE,
        _ => TOOL_SEARCH,
    }
}
