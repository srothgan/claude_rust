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

// Role header colors
pub const ROLE_ASSISTANT: Color = RUST_ORANGE;

// User message background
pub const USER_MSG_BG: Color = Color::Rgb(40, 44, 52);

// Tool status icons
pub const ICON_PENDING: &str = "◌";
pub const ICON_RUNNING: &str = "⏵";
pub const ICON_COMPLETED: &str = "✓";
pub const ICON_FAILED: &str = "✗";

// Status colors
pub const STATUS_ERROR: Color = Color::Red;

/// Tool kind icon + label pair. Monochrome Unicode symbols.
/// If `claude_tool_name` is provided, override icon/label for specific tools.
pub fn tool_kind_label(
    kind: agent_client_protocol::ToolKind,
    claude_tool_name: Option<&str>,
) -> (&'static str, &'static str) {
    // Override for specific Claude Code tool names
    if let Some(name) = claude_tool_name {
        match name {
            "Task" => return ("◇", "Agent"),
            "WebSearch" => return ("⊕", "Search"),
            "WebFetch" => return ("⊕", "Fetch"),
            _ => {}
        }
    }

    use agent_client_protocol::ToolKind;
    match kind {
        ToolKind::Read => ("⬚", "Read"),
        ToolKind::Edit => ("▣", "Edit"),
        ToolKind::Delete => ("▣", "Delete"),
        ToolKind::Move => ("⇄", "Move"),
        ToolKind::Search => ("⌕", "Find"),
        ToolKind::Execute => ("⟩", "Bash"),
        ToolKind::Think => ("❖", "Think"),
        ToolKind::Fetch => ("⊕", "Fetch"),
        ToolKind::SwitchMode => ("⊙", "Mode"),
        _ => ("○", "Tool"),
    }
}
