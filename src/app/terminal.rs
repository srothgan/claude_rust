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

use super::{App, MessageBlock};

/// Snapshot terminal output buffers into ToolCallInfo for rendering.
/// Called each frame so in-progress Execute tool calls show live output.
///
/// The output_buffer is append-only (never cleared). The adapter's
/// `terminal_output` uses a cursor to track what it already returned.
/// We simply snapshot the full buffer for display each frame.
pub(super) fn update_terminal_outputs(app: &mut App) {
    let terminals = app.terminals.borrow();
    if terminals.is_empty() {
        return;
    }

    for msg in &mut app.messages {
        for block in &mut msg.blocks {
            if let MessageBlock::ToolCall(tc) = block {
                let tc = tc.as_mut();
                if let Some(ref tid) = tc.terminal_id
                    && let Some(terminal) = terminals.get(tid.as_str())
                {
                    let buf = terminal
                        .output_buffer
                        .lock()
                        .expect("output buffer lock poisoned");
                    let current_len = buf.len();
                    if current_len == 0 || current_len == tc.terminal_output_len {
                        continue;
                    }
                    let snapshot = String::from_utf8_lossy(&buf).to_string();
                    drop(buf);

                    tc.terminal_output = Some(snapshot);
                    tc.terminal_output_len = current_len;
                    tc.cache.invalidate();
                }
            }
        }
    }
}
