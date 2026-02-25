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

use ratatui::style::Color;

// Accent
pub const RUST_ORANGE: Color = Color::Rgb(244, 118, 0);

// UI chrome
pub const DIM: Color = Color::DarkGray;
pub const PROMPT_CHAR: &str = "\u{276f}";
pub const SEPARATOR_CHAR: &str = "\u{2500}";

// Role header colors
pub const ROLE_ASSISTANT: Color = RUST_ORANGE;

// User message background
pub const USER_MSG_BG: Color = Color::Rgb(40, 44, 52);

// Tool status icons
pub const ICON_COMPLETED: &str = "\u{2713}";
pub const ICON_FAILED: &str = "\u{2717}";

// Status colors
pub const STATUS_ERROR: Color = Color::Red;
pub const SLASH_COMMAND: Color = Color::LightMagenta;

/// SDK tool icon + label pair. Monochrome Unicode symbols.
/// Unknown tool names fall back to a generic Tool label.
pub fn tool_name_label(sdk_tool_name: &str) -> (&'static str, &'static str) {
    match sdk_tool_name {
        "Read" => ("\u{2b1a}", "Read"),
        "Write" => ("\u{25a3}", "Write"),
        "Edit" => ("\u{25a3}", "Edit"),
        "MultiEdit" => ("\u{25a3}", "MultiEdit"),
        "NotebookEdit" => ("\u{25a3}", "NotebookEdit"),
        "Delete" => ("\u{25a3}", "Delete"),
        "Move" => ("\u{21c4}", "Move"),
        "Glob" => ("\u{2315}", "Glob"),
        "Grep" => ("\u{2315}", "Grep"),
        "LS" => ("\u{2315}", "LS"),
        "Bash" => ("\u{27e9}", "Bash"),
        "Task" => ("\u{25c7}", "Task"),
        "WebFetch" => ("\u{2295}", "WebFetch"),
        "WebSearch" => ("\u{2295}", "WebSearch"),
        "ExitPlanMode" => ("\u{2299}", "ExitPlanMode"),
        "TodoWrite" => ("\u{25cc}", "TodoWrite"),
        "Config" => ("\u{2299}", "Config"),
        "EnterWorktree" => ("\u{21c4}", "EnterWorktree"),
        _ => ("\u{25cb}", "Tool"),
    }
}
