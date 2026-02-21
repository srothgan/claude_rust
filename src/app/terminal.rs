// claude_rust - A native Rust terminal interface for Claude Code
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

use super::{App, MessageBlock};

/// Snapshot terminal output buffers into `ToolCallInfo` for rendering.
/// Called each frame so in-progress Execute tool calls show live output.
///
/// The `output_buffer` is append-only (never cleared). The adapter's
/// `terminal_output` uses a cursor to track what it already returned.
/// We simply snapshot the full buffer for display each frame.
pub(super) fn update_terminal_outputs(app: &mut App) -> bool {
    let _t = app.perf.as_ref().map(|p| p.start("terminal::update"));
    let terminals = app.terminals.borrow();
    if terminals.is_empty() {
        return false;
    }

    let mut changed = false;

    // Use the indexed terminal tool calls instead of scanning all messages/blocks.
    for &(ref tid, mi, bi) in &app.terminal_tool_calls {
        let Some(terminal) = terminals.get(tid.as_str()) else {
            continue;
        };
        let Some(MessageBlock::ToolCall(tc)) =
            app.messages.get_mut(mi).and_then(|m| m.blocks.get_mut(bi))
        else {
            continue;
        };
        let tc = tc.as_mut();

        // Clone raw bytes under the lock, then convert outside
        // the critical section to avoid blocking output writers.
        let raw = {
            let Ok(buf) = terminal.output_buffer.lock() else {
                continue;
            };
            let current_len = buf.len();
            if current_len == 0 || current_len == tc.terminal_output_len {
                continue;
            }
            tc.terminal_output_len = current_len;
            buf.clone()
        };
        let snapshot = String::from_utf8_lossy(&raw).to_string();

        tc.terminal_output = Some(snapshot);
        tc.cache.invalidate();
        changed = true;
    }

    changed
}
