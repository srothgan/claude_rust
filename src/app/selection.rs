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

use super::{App, SelectionKind, SelectionState};

pub(crate) fn normalize_selection(
    a: super::SelectionPoint,
    b: super::SelectionPoint,
) -> (super::SelectionPoint, super::SelectionPoint) {
    if (a.row, a.col) <= (b.row, b.col) {
        (a, b)
    } else {
        (b, a)
    }
}

pub(super) fn try_copy_selection(app: &mut App) -> bool {
    let Some(sel) = app.selection else {
        return false;
    };
    let mut text = match sel.kind {
        SelectionKind::Chat => extract_chat_selection(app, sel),
        SelectionKind::Input => extract_input_selection(app, sel),
    };
    if text.trim().is_empty() {
        return false;
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(text);
        return true;
    }
    false
}

fn extract_chat_selection(app: &App, sel: SelectionState) -> String {
    let (start, end) = normalize_selection(sel.start, sel.end);
    let mut out = String::new();
    let lines = &app.rendered_chat_lines;
    for row in start.row..=end.row {
        let line = lines.get(row).map(String::as_str).unwrap_or("");
        let slice = if start.row == end.row {
            slice_by_cols(line, start.col, end.col)
        } else if row == start.row {
            slice_by_cols(line, start.col, line.chars().count())
        } else if row == end.row {
            slice_by_cols(line, 0, end.col)
        } else {
            line.to_string()
        };
        out.push_str(&slice);
        if row != end.row {
            out.push('\n');
        }
    }
    out
}

fn extract_input_selection(app: &App, sel: SelectionState) -> String {
    let (start, end) = normalize_selection(sel.start, sel.end);
    let mut out = String::new();
    let lines = &app.rendered_input_lines;
    for row in start.row..=end.row {
        let line = lines.get(row).map(String::as_str).unwrap_or("");
        let slice = if start.row == end.row {
            slice_by_cols(line, start.col, end.col)
        } else if row == start.row {
            slice_by_cols(line, start.col, line.chars().count())
        } else if row == end.row {
            slice_by_cols(line, 0, end.col)
        } else {
            line.to_string()
        };
        out.push_str(&slice);
        if row != end.row {
            out.push('\n');
        }
    }
    out
}

fn slice_by_cols(text: &str, start_col: usize, end_col: usize) -> String {
    let mut out = String::new();
    for (i, ch) in text.chars().enumerate() {
        if i >= end_col {
            break;
        }
        if i >= start_col {
            out.push(ch);
        }
    }
    out
}
