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

use tui_textarea::{CursorMove, TextArea};

#[derive(Debug)]
pub struct InputState {
    pub lines: Vec<String>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    /// Monotonically increasing version counter. Bumped on every content or cursor change
    /// so that downstream caches (e.g. wrap result) can detect staleness cheaply.
    pub version: u64,
    /// Stored paste blocks: each entry holds the full text of a large paste (>10 lines).
    /// A placeholder line `[Pasted Text N]` is inserted into `lines` at the paste point.
    /// On `text()`, placeholders are expanded back to the original pasted content.
    pub paste_blocks: Vec<String>,
    editor: TextArea<'static>,
}

/// Prefix/suffix used to identify paste placeholder lines in the input buffer.
const PASTE_PREFIX: &str = "[Pasted Text ";
const PASTE_SUFFIX: &str = "]";
/// Line threshold above which pasted content is collapsed to a placeholder.
pub const PASTE_PLACEHOLDER_LINE_THRESHOLD: usize = 10;

impl InputState {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            version: 0,
            paste_blocks: Vec::new(),
            editor: TextArea::default(),
        }
    }

    #[must_use]
    pub fn text(&self) -> String {
        if self.paste_blocks.is_empty() {
            return self.lines.join("\n");
        }
        // Expand paste placeholders back to their full content
        let mut result = String::new();
        for (i, line) in self.lines.iter().enumerate() {
            if i > 0 {
                result.push('\n');
            }
            if let Some((idx, suffix_end)) = parse_paste_placeholder_with_suffix(line) {
                if let Some(content) = self.paste_blocks.get(idx) {
                    result.push_str(content);
                    if suffix_end < line.len() {
                        result.push_str(&line[suffix_end..]);
                    }
                } else {
                    result.push_str(line);
                }
            } else {
                result.push_str(line);
            }
        }
        result
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.paste_blocks.clear();
        self.version += 1;
        self.rebuild_editor_from_snapshot();
    }

    /// Replace the input with the given text, placing the cursor at the end.
    pub fn set_text(&mut self, text: &str) {
        self.lines = text.split('\n').map(String::from).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = self.lines.len() - 1;
        self.cursor_col = self.lines[self.cursor_row].chars().count();
        self.paste_blocks.clear();
        self.version += 1;
        self.rebuild_editor_from_snapshot();
    }

    pub fn insert_char(&mut self, c: char) {
        let line = &mut self.lines[self.cursor_row];
        let byte_idx = char_to_byte_index(line, self.cursor_col);
        line.insert(byte_idx, c);
        self.cursor_col += 1;
        self.version += 1;
    }

    fn as_textarea(&self) -> TextArea<'static> {
        let mut textarea = TextArea::from(self.lines.clone());
        textarea.move_cursor(CursorMove::Jump(
            u16::try_from(self.cursor_row).unwrap_or(u16::MAX),
            u16::try_from(self.cursor_col).unwrap_or(u16::MAX),
        ));
        textarea
    }

    fn sync_snapshot_from_editor(&mut self) -> bool {
        let (row, col) = self.editor.cursor();
        let lines = self.editor.lines().to_vec();
        if self.lines == lines && self.cursor_row == row && self.cursor_col == col {
            return false;
        }
        self.lines = lines;
        self.cursor_row = row;
        self.cursor_col = col;
        self.version += 1;
        true
    }

    fn rebuild_editor_from_snapshot(&mut self) {
        self.editor = self.as_textarea();
    }

    pub fn sync_textarea_engine(&mut self) {
        self.rebuild_editor_from_snapshot();
    }

    fn ensure_editor_synced_from_snapshot(&mut self) {
        if self.editor.cursor() != (self.cursor_row, self.cursor_col)
            || self.editor.lines() != self.lines.as_slice()
        {
            self.rebuild_editor_from_snapshot();
        }
    }

    fn apply_textarea_edit(&mut self, edit: impl FnOnce(&mut TextArea<'_>)) -> bool {
        self.ensure_editor_synced_from_snapshot();
        edit(&mut self.editor);
        self.sync_snapshot_from_editor()
    }

    pub fn textarea_insert_char(&mut self, c: char) -> bool {
        self.apply_textarea_edit(|textarea| {
            textarea.insert_char(c);
        })
    }

    pub fn textarea_insert_newline(&mut self) -> bool {
        self.apply_textarea_edit(|textarea| {
            textarea.insert_newline();
        })
    }

    pub fn textarea_delete_char_before(&mut self) -> bool {
        self.apply_textarea_edit(|textarea| {
            let _ = textarea.delete_char();
        })
    }

    pub fn textarea_delete_char_after(&mut self) -> bool {
        self.apply_textarea_edit(|textarea| {
            let _ = textarea.delete_next_char();
        })
    }

    pub fn textarea_move_left(&mut self) -> bool {
        self.apply_textarea_edit(|textarea| {
            textarea.move_cursor(CursorMove::Back);
        })
    }

    pub fn textarea_move_right(&mut self) -> bool {
        self.apply_textarea_edit(|textarea| {
            textarea.move_cursor(CursorMove::Forward);
        })
    }

    pub fn textarea_move_up(&mut self) -> bool {
        self.apply_textarea_edit(|textarea| {
            textarea.move_cursor(CursorMove::Up);
        })
    }

    pub fn textarea_move_down(&mut self) -> bool {
        self.apply_textarea_edit(|textarea| {
            textarea.move_cursor(CursorMove::Down);
        })
    }

    pub fn textarea_move_home(&mut self) -> bool {
        self.apply_textarea_edit(|textarea| {
            textarea.move_cursor(CursorMove::Head);
        })
    }

    pub fn textarea_move_end(&mut self) -> bool {
        self.apply_textarea_edit(|textarea| {
            textarea.move_cursor(CursorMove::End);
        })
    }

    pub fn textarea_undo(&mut self) -> bool {
        self.apply_textarea_edit(|textarea| {
            let _ = textarea.undo();
        })
    }

    pub fn textarea_redo(&mut self) -> bool {
        self.apply_textarea_edit(|textarea| {
            let _ = textarea.redo();
        })
    }

    pub fn textarea_move_word_left(&mut self) -> bool {
        self.apply_textarea_edit(|textarea| {
            textarea.move_cursor(CursorMove::WordBack);
        })
    }

    pub fn textarea_move_word_right(&mut self) -> bool {
        self.apply_textarea_edit(|textarea| {
            textarea.move_cursor(CursorMove::WordForward);
        })
    }

    pub fn textarea_delete_word_before(&mut self) -> bool {
        self.apply_textarea_edit(|textarea| {
            let _ = textarea.delete_word();
        })
    }

    pub fn textarea_delete_word_after(&mut self) -> bool {
        self.apply_textarea_edit(|textarea| {
            let _ = textarea.delete_next_word();
        })
    }

    pub fn insert_newline(&mut self) {
        let line = &mut self.lines[self.cursor_row];
        let byte_idx = char_to_byte_index(line, self.cursor_col);
        let rest = line[byte_idx..].to_string();
        line.truncate(byte_idx);
        self.cursor_row += 1;
        self.lines.insert(self.cursor_row, rest);
        self.cursor_col = 0;
        self.version += 1;
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            if c == '\n' || c == '\r' {
                self.insert_newline();
            } else {
                self.insert_char(c);
            }
        }
    }

    /// Insert a large paste as a compact placeholder line.
    /// The full text is stored in `paste_blocks` and expanded by `text()` on submit.
    /// Returns the placeholder label for display purposes.
    pub fn insert_paste_block(&mut self, text: &str) -> String {
        let idx = self.paste_blocks.len();
        let placeholder = paste_placeholder_label(idx, count_text_lines(text));
        self.paste_blocks.push(text.to_owned());

        // Insert the placeholder at the current cursor position.
        // If cursor is mid-line, split the current line around the placeholder.
        let current_line = &mut self.lines[self.cursor_row];
        let byte_idx = char_to_byte_index(current_line, self.cursor_col);
        let after = current_line[byte_idx..].to_string();
        current_line.truncate(byte_idx);
        let before_empty = current_line.is_empty();

        if before_empty {
            // Replace current empty/start-of-line with placeholder
            current_line.clone_from(&placeholder);
            if !after.is_empty() {
                self.lines.insert(self.cursor_row + 1, after);
            }
            self.cursor_col = placeholder.chars().count();
        } else {
            // Insert placeholder on a new line after the current content
            self.cursor_row += 1;
            self.lines.insert(self.cursor_row, placeholder.clone());
            if !after.is_empty() {
                self.lines.insert(self.cursor_row + 1, after);
            }
            self.cursor_col = placeholder.chars().count();
        }

        self.version += 1;
        placeholder
    }

    /// Append text to the paste block under the cursor if the cursor currently
    /// sits at the end of a standalone placeholder line.
    ///
    /// This handles terminals that deliver one clipboard paste as multiple
    /// `Event::Paste` chunks.
    pub fn append_to_active_paste_block(&mut self, text: &str) -> bool {
        let Some(current_line) = self.lines.get(self.cursor_row) else {
            return false;
        };
        let Some(idx) = parse_paste_placeholder(current_line) else {
            return false;
        };
        // Only merge chunks when cursor is at the end of that placeholder line.
        if self.cursor_col != current_line.chars().count() {
            return false;
        }

        let Some(block) = self.paste_blocks.get_mut(idx) else {
            return false;
        };
        block.push_str(text);

        self.lines[self.cursor_row] = paste_placeholder_label(idx, count_text_lines(block));
        self.cursor_col = self.lines[self.cursor_row].chars().count();
        self.version += 1;
        true
    }

    pub fn delete_char_before(&mut self) {
        if self.cursor_col > 0 {
            let line = &mut self.lines[self.cursor_row];
            self.cursor_col -= 1;
            let byte_idx = char_to_byte_index(line, self.cursor_col);
            line.remove(byte_idx);
            self.version += 1;
        } else if self.cursor_row > 0 {
            let removed = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].chars().count();
            self.lines[self.cursor_row].push_str(&removed);
            self.version += 1;
        }
    }

    pub fn delete_char_after(&mut self) {
        let line_len = self.lines[self.cursor_row].chars().count();
        if self.cursor_col < line_len {
            let line = &mut self.lines[self.cursor_row];
            let byte_idx = char_to_byte_index(line, self.cursor_col);
            line.remove(byte_idx);
            self.version += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor_row + 1);
            self.lines[self.cursor_row].push_str(&next);
            self.version += 1;
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
            self.version += 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].chars().count();
            self.version += 1;
        }
    }

    pub fn move_right(&mut self) {
        let line_len = self.lines[self.cursor_row].chars().count();
        if self.cursor_col < line_len {
            self.cursor_col += 1;
            self.version += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
            self.version += 1;
        }
    }

    pub fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            let line_len = self.lines[self.cursor_row].chars().count();
            self.cursor_col = self.cursor_col.min(line_len);
            self.version += 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            let line_len = self.lines[self.cursor_row].chars().count();
            self.cursor_col = self.cursor_col.min(line_len);
            self.version += 1;
        }
    }

    pub fn move_home(&mut self) {
        self.cursor_col = 0;
        self.version += 1;
    }

    pub fn move_end(&mut self) {
        self.cursor_col = self.lines[self.cursor_row].chars().count();
        self.version += 1;
    }

    #[must_use]
    pub fn line_count(&self) -> u16 {
        u16::try_from(self.lines.len()).unwrap_or(u16::MAX)
    }
}

impl Default for InputState {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert a character index to a byte index within a string.
fn char_to_byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map_or(s.len(), |(i, _)| i)
}

/// Count logical lines for text containing mixed `\n`, `\r`, and `\r\n` endings.
#[must_use]
pub fn count_text_lines(text: &str) -> usize {
    // Count universal newlines (\n, \r, and \r\n as a single break).
    let mut lines = 1;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => lines += 1,
            b'\r' => {
                lines += 1;
                if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    lines
}

/// Trim trailing line-break characters (`\n` and `\r`) from text.
#[must_use]
pub fn trim_trailing_line_breaks(mut text: &str) -> &str {
    while let Some(stripped) = text.strip_suffix('\n').or_else(|| text.strip_suffix('\r')) {
        text = stripped;
    }
    text
}

fn paste_placeholder_label(idx: usize, line_count: usize) -> String {
    format!("{PASTE_PREFIX}{} - {line_count} lines{PASTE_SUFFIX}", idx + 1)
}

/// Parse a placeholder at the start of a line.
///
/// Returns `(paste_block_index, placeholder_end_byte_index)`.
pub fn parse_paste_placeholder_with_suffix(line: &str) -> Option<(usize, usize)> {
    let rest = line.strip_prefix(PASTE_PREFIX)?;
    let close_rel = rest.find(PASTE_SUFFIX)?;
    let rest = &rest[..close_rel];
    let num_str = rest.split(" - ").next()?;
    let n: usize = num_str.parse().ok()?;
    if n == 0 {
        return None;
    }
    let end = PASTE_PREFIX.len() + close_rel + PASTE_SUFFIX.len();
    Some((n - 1, end))
}

/// Check if a line is a paste placeholder. Returns the 0-based paste block index if so.
pub fn parse_paste_placeholder(line: &str) -> Option<usize> {
    let (idx, suffix_end) = parse_paste_placeholder_with_suffix(line)?;
    if suffix_end == line.len() { Some(idx) } else { None }
}

#[cfg(test)]
mod tests {
    // =====
    // TESTS: 83
    // =====

    use super::*;
    use pretty_assertions::assert_eq;

    // char_to_byte_index

    #[test]
    fn char_to_byte_index_ascii() {
        assert_eq!(char_to_byte_index("hello", 0), 0);
        assert_eq!(char_to_byte_index("hello", 2), 2);
        assert_eq!(char_to_byte_index("hello", 5), 5); // past end
    }

    #[test]
    fn char_to_byte_index_multibyte_utf8() {
        // 'e' with accent: 2 bytes in UTF-8
        let s = "cafe\u{0301}"; // "cafe" + combining accent = 5 chars, but accent is 2 bytes
        assert_eq!(char_to_byte_index(s, 4), 4); // the combining char starts at byte 4
    }

    #[test]
    fn char_to_byte_index_emoji() {
        let s = "\u{1F600}hello"; // grinning face (4 bytes) + "hello"
        assert_eq!(char_to_byte_index(s, 0), 0);
        assert_eq!(char_to_byte_index(s, 1), 4); // after emoji
    }

    #[test]
    fn char_to_byte_index_beyond_string() {
        assert_eq!(char_to_byte_index("ab", 10), 2); // returns s.len()
    }

    #[test]
    fn char_to_byte_index_empty_string() {
        assert_eq!(char_to_byte_index("", 0), 0);
        assert_eq!(char_to_byte_index("", 5), 0);
    }

    // InputState::new / Default

    #[test]
    fn new_creates_empty_state() {
        let input = InputState::new();
        assert_eq!(input.lines, vec![String::new()]);
        assert_eq!(input.cursor_row, 0);
        assert_eq!(input.cursor_col, 0);
        assert_eq!(input.version, 0);
    }

    #[test]
    fn default_equals_new() {
        let a = InputState::new();
        let b = InputState::default();
        assert_eq!(a.lines, b.lines);
        assert_eq!(a.cursor_row, b.cursor_row);
        assert_eq!(a.cursor_col, b.cursor_col);
        assert_eq!(a.version, b.version);
    }

    // text()

    #[test]
    fn text_single_empty_line() {
        let input = InputState::new();
        assert_eq!(input.text(), "");
    }

    #[test]
    fn text_joins_lines_with_newline() {
        let mut input = InputState::new();
        input.insert_str("line1\nline2\nline3");
        assert_eq!(input.text(), "line1\nline2\nline3");
    }

    // is_empty()

    #[test]
    fn is_empty_true_for_new() {
        assert!(InputState::new().is_empty());
    }

    #[test]
    fn is_empty_false_after_insert() {
        let mut input = InputState::new();
        input.insert_char('a');
        assert!(!input.is_empty());
    }

    #[test]
    fn is_empty_false_for_empty_multiline() {
        // Two empty lines: not considered "empty" by the method
        let mut input = InputState::new();
        input.insert_newline();
        assert!(!input.is_empty());
    }

    // clear()

    #[test]
    fn clear_resets_to_empty() {
        let mut input = InputState::new();
        input.insert_str("hello\nworld");
        let v_before = input.version;
        input.clear();
        assert!(input.is_empty());
        assert_eq!(input.cursor_row, 0);
        assert_eq!(input.cursor_col, 0);
        assert!(input.version > v_before);
    }

    // insert_char

    #[test]
    fn insert_char_ascii() {
        let mut input = InputState::new();
        input.insert_char('h');
        input.insert_char('i');
        assert_eq!(input.lines[0], "hi");
        assert_eq!(input.cursor_col, 2);
    }

    #[test]
    fn insert_char_unicode_emoji() {
        let mut input = InputState::new();
        input.insert_char('\u{1F600}'); // grinning face
        assert_eq!(input.cursor_col, 1);
        assert_eq!(input.lines[0], "\u{1F600}");
    }

    #[test]
    fn insert_char_cjk() {
        let mut input = InputState::new();
        input.insert_char('\u{4F60}'); // Chinese "ni"
        input.insert_char('\u{597D}'); // Chinese "hao"
        assert_eq!(input.lines[0], "\u{4F60}\u{597D}");
        assert_eq!(input.cursor_col, 2);
    }

    #[test]
    fn insert_char_mid_line() {
        let mut input = InputState::new();
        input.insert_str("ac");
        input.move_left(); // cursor at col 1
        input.insert_char('b');
        assert_eq!(input.lines[0], "abc");
        assert_eq!(input.cursor_col, 2);
    }

    #[test]
    fn insert_char_bumps_version() {
        let mut input = InputState::new();
        let v = input.version;
        input.insert_char('x');
        assert!(input.version > v);
    }

    // insert_newline

    #[test]
    fn insert_newline_at_end() {
        let mut input = InputState::new();
        input.insert_str("hello");
        input.insert_newline();
        assert_eq!(input.lines, vec!["hello", ""]);
        assert_eq!(input.cursor_row, 1);
        assert_eq!(input.cursor_col, 0);
    }

    #[test]
    fn insert_newline_mid_line() {
        let mut input = InputState::new();
        input.insert_str("helloworld");
        // Move cursor to position 5
        input.cursor_col = 5;
        input.insert_newline();
        assert_eq!(input.lines, vec!["hello", "world"]);
        assert_eq!(input.cursor_row, 1);
        assert_eq!(input.cursor_col, 0);
    }

    #[test]
    fn insert_newline_at_start() {
        let mut input = InputState::new();
        input.insert_str("hello");
        input.move_home();
        input.insert_newline();
        assert_eq!(input.lines, vec!["", "hello"]);
    }

    // insert_str

    #[test]
    fn insert_str_multiline() {
        let mut input = InputState::new();
        input.insert_str("line1\nline2\nline3");
        assert_eq!(input.lines, vec!["line1", "line2", "line3"]);
        assert_eq!(input.cursor_row, 2);
        assert_eq!(input.cursor_col, 5);
    }

    #[test]
    fn insert_str_with_carriage_returns() {
        let mut input = InputState::new();
        input.insert_str("a\rb\rc");
        // \r treated same as \n
        assert_eq!(input.lines, vec!["a", "b", "c"]);
    }

    #[test]
    fn insert_str_empty() {
        let mut input = InputState::new();
        let v = input.version;
        input.insert_str("");
        assert_eq!(input.version, v); // no mutation
    }

    // delete_char_before (backspace)

    #[test]
    fn backspace_mid_line() {
        let mut input = InputState::new();
        input.insert_str("abc");
        input.delete_char_before();
        assert_eq!(input.lines[0], "ab");
        assert_eq!(input.cursor_col, 2);
    }

    #[test]
    fn backspace_start_of_line_joins() {
        let mut input = InputState::new();
        input.insert_str("hello\nworld");
        // cursor at row 1, col 5. Move to start of row 1.
        input.move_home();
        input.delete_char_before();
        assert_eq!(input.lines, vec!["helloworld"]);
        assert_eq!(input.cursor_row, 0);
        assert_eq!(input.cursor_col, 5); // at the join point
    }

    #[test]
    fn backspace_start_of_buffer_noop() {
        let mut input = InputState::new();
        input.insert_str("hi");
        input.move_home();
        let v = input.version;
        input.delete_char_before(); // should do nothing
        assert_eq!(input.lines[0], "hi");
        assert_eq!(input.version, v); // no version bump
    }

    #[test]
    fn backspace_unicode() {
        let mut input = InputState::new();
        input.insert_char('\u{1F600}');
        input.insert_char('x');
        input.delete_char_before();
        assert_eq!(input.lines[0], "\u{1F600}");
    }

    // delete_char_after (delete key)

    #[test]
    fn delete_mid_line() {
        let mut input = InputState::new();
        input.insert_str("abc");
        input.move_home();
        input.delete_char_after();
        assert_eq!(input.lines[0], "bc");
        assert_eq!(input.cursor_col, 0);
    }

    #[test]
    fn delete_end_of_line_joins_next() {
        let mut input = InputState::new();
        input.insert_str("hello\nworld");
        input.cursor_row = 0;
        input.cursor_col = 5; // end of "hello"
        input.delete_char_after();
        assert_eq!(input.lines, vec!["helloworld"]);
    }

    #[test]
    fn delete_end_of_buffer_noop() {
        let mut input = InputState::new();
        input.insert_str("hi");
        // cursor at end of last line
        let v = input.version;
        input.delete_char_after();
        assert_eq!(input.lines[0], "hi");
        assert_eq!(input.version, v);
    }

    // Navigation: move_left, move_right

    #[test]
    fn move_left_within_line() {
        let mut input = InputState::new();
        input.insert_str("abc");
        input.move_left();
        assert_eq!(input.cursor_col, 2);
    }

    #[test]
    fn move_left_wraps_to_previous_line() {
        let mut input = InputState::new();
        input.insert_str("ab\ncd");
        input.move_home(); // at col 0, row 1
        input.move_left();
        assert_eq!(input.cursor_row, 0);
        assert_eq!(input.cursor_col, 2); // end of "ab"
    }

    #[test]
    fn move_left_at_origin_noop() {
        let mut input = InputState::new();
        input.insert_char('a');
        input.move_home();
        let v = input.version;
        input.move_left();
        assert_eq!(input.cursor_col, 0);
        assert_eq!(input.cursor_row, 0);
        assert_eq!(input.version, v); // no change
    }

    #[test]
    fn move_right_within_line() {
        let mut input = InputState::new();
        input.insert_str("abc");
        input.move_home();
        input.move_right();
        assert_eq!(input.cursor_col, 1);
    }

    #[test]
    fn move_right_wraps_to_next_line() {
        let mut input = InputState::new();
        input.insert_str("ab\ncd");
        input.cursor_row = 0;
        input.cursor_col = 2; // end of "ab"
        input.move_right();
        assert_eq!(input.cursor_row, 1);
        assert_eq!(input.cursor_col, 0);
    }

    #[test]
    fn move_right_at_end_noop() {
        let mut input = InputState::new();
        input.insert_str("ab");
        let v = input.version;
        input.move_right(); // already at end
        assert_eq!(input.version, v);
    }

    // Navigation: move_up, move_down

    #[test]
    fn move_up_clamps_col() {
        let mut input = InputState::new();
        input.insert_str("ab\nhello");
        // cursor at row 1, col 5 ("hello" end)
        input.move_up();
        assert_eq!(input.cursor_row, 0);
        assert_eq!(input.cursor_col, 2); // clamped to "ab" length
    }

    #[test]
    fn move_up_at_top_noop() {
        let mut input = InputState::new();
        input.insert_str("hello");
        let v = input.version;
        input.move_up();
        assert_eq!(input.cursor_row, 0);
        assert_eq!(input.version, v);
    }

    #[test]
    fn move_down_clamps_col() {
        let mut input = InputState::new();
        input.insert_str("hello\nab");
        input.cursor_row = 0;
        input.cursor_col = 5;
        input.move_down();
        assert_eq!(input.cursor_row, 1);
        assert_eq!(input.cursor_col, 2); // clamped to "ab" length
    }

    #[test]
    fn move_down_at_bottom_noop() {
        let mut input = InputState::new();
        input.insert_str("hello");
        let v = input.version;
        input.move_down();
        assert_eq!(input.version, v);
    }

    // Navigation: move_home, move_end

    #[test]
    fn move_home_sets_col_zero() {
        let mut input = InputState::new();
        input.insert_str("hello");
        input.move_home();
        assert_eq!(input.cursor_col, 0);
    }

    #[test]
    fn move_end_sets_col_to_line_len() {
        let mut input = InputState::new();
        input.insert_str("hello");
        input.move_home();
        input.move_end();
        assert_eq!(input.cursor_col, 5);
    }

    #[test]
    fn move_home_always_bumps_version() {
        let mut input = InputState::new();
        input.insert_str("hello");
        input.move_home(); // col was 5, now 0
        let v = input.version;
        input.move_home(); // col already 0, but still bumps
        assert!(input.version > v);
    }

    // line_count

    #[test]
    fn line_count_single() {
        assert_eq!(InputState::new().line_count(), 1);
    }

    #[test]
    fn line_count_multi() {
        let mut input = InputState::new();
        input.insert_str("a\nb\nc");
        assert_eq!(input.line_count(), 3);
    }

    // version counter

    #[test]
    fn version_increments_on_every_mutation() {
        let mut input = InputState::new();
        let mut v = input.version;

        input.insert_char('a');
        assert!(input.version > v);
        v = input.version;

        input.insert_newline();
        assert!(input.version > v);
        v = input.version;

        input.delete_char_before();
        assert!(input.version > v);
        v = input.version;

        input.move_left();
        assert!(input.version > v);
        v = input.version;

        input.clear();
        assert!(input.version > v);
    }

    #[test]
    fn rapid_insert_delete_cycle() {
        let mut input = InputState::new();
        for _ in 0..100 {
            input.insert_char('x');
        }
        assert_eq!(input.lines[0].len(), 100);
        for _ in 0..100 {
            input.delete_char_before();
        }
        assert!(input.is_empty());
    }

    #[test]
    fn mixed_unicode_operations() {
        let mut input = InputState::new();
        // Insert mixed: ASCII, emoji, CJK
        input.insert_str("hi\u{1F600}\u{4F60}");
        assert_eq!(input.cursor_col, 4); // h, i, emoji, CJK
        input.move_home();
        input.move_right(); // past 'h'
        input.move_right(); // past 'i'
        input.delete_char_after(); // delete emoji
        assert_eq!(input.lines[0], "hi\u{4F60}");
    }

    #[test]
    fn multiline_editing_stress() {
        let mut input = InputState::new();
        // Create 10 lines
        for i in 0..10 {
            input.insert_str(&format!("line{i}"));
            if i < 9 {
                input.insert_newline();
            }
        }
        assert_eq!(input.lines.len(), 10);

        // Navigate to middle and delete lines by joining
        input.cursor_row = 5;
        input.cursor_col = 0;
        input.delete_char_before(); // join line 5 with line 4
        assert_eq!(input.lines.len(), 9);

        // Text should be coherent
        let text = input.text();
        assert!(text.contains("line4line5"));
    }

    #[test]
    fn insert_str_with_only_newlines() {
        let mut input = InputState::new();
        input.insert_str("\n\n\n");
        assert_eq!(input.lines, vec!["", "", "", ""]);
        assert_eq!(input.cursor_row, 3);
        assert_eq!(input.cursor_col, 0);
    }

    #[test]
    fn cursor_clamping_on_vertical_nav() {
        let mut input = InputState::new();
        input.insert_str("long line here\nab\nmedium line");
        // cursor at row 2, col 11 (end of "medium line")
        input.move_up(); // to row 1 "ab", col clamped to 2
        assert_eq!(input.cursor_col, 2);
        input.move_up(); // to row 0 "long line here", col stays 2
        assert_eq!(input.cursor_col, 2);
        input.move_end(); // col = 14
        input.move_down(); // to row 1 "ab", col clamped to 2
        assert_eq!(input.cursor_col, 2);
    }

    // weird inputs

    #[test]
    fn insert_tab_character() {
        let mut input = InputState::new();
        input.insert_char('\t');
        assert_eq!(input.lines[0], "\t");
        assert_eq!(input.cursor_col, 1);
    }

    #[test]
    fn insert_null_byte() {
        let mut input = InputState::new();
        input.insert_char('\0');
        assert_eq!(input.lines[0].len(), 1);
        assert_eq!(input.cursor_col, 1);
    }

    #[test]
    fn insert_control_chars() {
        let mut input = InputState::new();
        // Bell, backspace-char (not the key), escape
        input.insert_char('\x07');
        input.insert_char('\x08');
        input.insert_char('\x1B');
        assert_eq!(input.cursor_col, 3);
        assert_eq!(input.lines[0].chars().count(), 3);
    }

    #[test]
    fn windows_crlf_line_endings() {
        // \r\n should produce TWO newlines (each triggers insert_newline)
        let mut input = InputState::new();
        input.insert_str("a\r\nb");
        // \r -> newline, \n -> another newline
        assert_eq!(input.lines, vec!["a", "", "b"]);
    }

    #[test]
    fn insert_zero_width_joiner_sequence() {
        // Family emoji: man + ZWJ + woman + ZWJ + girl
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let mut input = InputState::new();
        input.insert_str(family);
        // Each code point is a separate char as far as Rust is concerned
        assert_eq!(input.cursor_col, family.chars().count());
        assert_eq!(input.text(), family);
    }

    #[test]
    fn insert_flag_emoji() {
        // Regional indicator symbols for US flag
        let flag = "\u{1F1FA}\u{1F1F8}";
        let mut input = InputState::new();
        input.insert_str(flag);
        assert_eq!(input.cursor_col, 2); // two chars
        assert_eq!(input.text(), flag);
    }

    #[test]
    fn insert_combining_diacritical_marks() {
        // e + combining acute + combining cedilla
        let mut input = InputState::new();
        input.insert_char('e');
        input.insert_char('\u{0301}'); // combining acute
        input.insert_char('\u{0327}'); // combining cedilla
        assert_eq!(input.cursor_col, 3);
        // Delete the last combining mark
        input.delete_char_before();
        assert_eq!(input.cursor_col, 2);
        assert_eq!(input.lines[0], "e\u{0301}");
    }

    #[test]
    fn insert_right_to_left_text() {
        // Arabic text
        let arabic = "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}";
        let mut input = InputState::new();
        input.insert_str(arabic);
        assert_eq!(input.cursor_col, 5);
        assert_eq!(input.text(), arabic);
        // Navigate and delete should still work
        input.move_home();
        input.delete_char_after();
        assert_eq!(input.cursor_col, 0);
        assert_eq!(input.lines[0].chars().count(), 4);
    }

    #[test]
    fn insert_very_long_single_line() {
        let mut input = InputState::new();
        let long_str: String = "x".repeat(10_000);
        input.insert_str(&long_str);
        assert_eq!(input.cursor_col, 10_000);
        assert_eq!(input.lines[0].len(), 10_000);
        // Navigate to middle
        input.move_home();
        for _ in 0..5000 {
            input.move_right();
        }
        assert_eq!(input.cursor_col, 5000);
        // Insert in the middle
        input.insert_char('Y');
        assert_eq!(input.lines[0].len(), 10_001);
    }

    #[test]
    fn insert_many_short_lines() {
        let mut input = InputState::new();
        for i in 0..500 {
            input.insert_str(&format!("{i}"));
            input.insert_newline();
        }
        assert_eq!(input.lines.len(), 501); // 500 newlines + 1 trailing empty
        assert_eq!(input.cursor_row, 500);
    }

    // rapid key combinations

    #[test]
    fn type_then_backspace_all_then_retype() {
        let mut input = InputState::new();
        input.insert_str("hello world");
        // Backspace everything
        for _ in 0..11 {
            input.delete_char_before();
        }
        assert!(input.is_empty());
        assert_eq!(input.cursor_col, 0);
        // Type again
        input.insert_str("new text");
        assert_eq!(input.text(), "new text");
    }

    #[test]
    fn alternating_insert_and_navigate() {
        let mut input = InputState::new();
        // Simulate: type 'a', left, type 'b', left, type 'c' -> "cba"
        input.insert_char('a');
        input.move_left();
        input.insert_char('b');
        input.move_left();
        input.insert_char('c');
        assert_eq!(input.lines[0], "cba");
        assert_eq!(input.cursor_col, 1); // after 'c'
    }

    #[test]
    fn home_end_rapid_cycle() {
        let mut input = InputState::new();
        input.insert_str("hello");
        for _ in 0..50 {
            input.move_home();
            assert_eq!(input.cursor_col, 0);
            input.move_end();
            assert_eq!(input.cursor_col, 5);
        }
    }

    #[test]
    fn left_right_round_trip_preserves_position() {
        let mut input = InputState::new();
        input.insert_str("abcdef");
        input.move_home();
        input.move_right();
        input.move_right();
        input.move_right(); // at col 3
        let col = input.cursor_col;
        // Go left 2 then right 2 -- should be back at same spot
        input.move_left();
        input.move_left();
        input.move_right();
        input.move_right();
        assert_eq!(input.cursor_col, col);
    }

    #[test]
    fn up_down_round_trip_with_short_line() {
        let mut input = InputState::new();
        input.insert_str("longline\na\nlongline");
        input.cursor_row = 0;
        input.cursor_col = 7; // end-ish of first line
        input.move_down(); // to "a" -- col clamped to 1
        assert_eq!(input.cursor_col, 1);
        input.move_down(); // to "longline" -- col stays at 1 (not restored to 7)
        assert_eq!(input.cursor_col, 1);
    }

    #[test]
    fn newline_then_immediate_backspace() {
        let mut input = InputState::new();
        input.insert_str("hello");
        input.insert_newline();
        assert_eq!(input.lines.len(), 2);
        input.delete_char_before(); // should rejoin
        assert_eq!(input.lines.len(), 1);
        assert_eq!(input.lines[0], "hello");
        assert_eq!(input.cursor_col, 5);
    }

    #[test]
    fn delete_forward_through_multiple_line_joins() {
        let mut input = InputState::new();
        input.insert_str("a\nb\nc\nd");
        assert_eq!(input.lines.len(), 4);
        // Go to very start
        input.cursor_row = 0;
        input.cursor_col = 0;
        // Move to col 1 (after 'a'), then delete forward repeatedly
        input.move_right(); // past 'a'
        input.delete_char_after(); // join "a" + "b" -> "ab"
        assert_eq!(input.lines[0], "ab");
        input.move_right(); // past 'b'
        input.delete_char_after(); // join "ab" + "c" -> "abc"
        assert_eq!(input.lines[0], "abc");
        input.move_right(); // past 'c'
        input.delete_char_after(); // join "abc" + "d" -> "abcd"
        assert_eq!(input.lines, vec!["abcd"]);
    }

    #[test]
    fn backspace_collapses_all_lines_to_one() {
        let mut input = InputState::new();
        input.insert_str("a\nb\nc\nd\ne");
        assert_eq!(input.lines.len(), 5);
        // Cursor is at end of last line. Backspace everything.
        let total_chars = input.text().len(); // includes \n chars
        for _ in 0..total_chars {
            input.delete_char_before();
        }
        assert!(input.is_empty());
        assert_eq!(input.lines.len(), 1);
        assert_eq!(input.cursor_row, 0);
        assert_eq!(input.cursor_col, 0);
    }

    // interleaved actions

    #[test]
    fn type_on_multiple_lines_then_clear() {
        let mut input = InputState::new();
        input.insert_str("line1\nline2\nline3");
        input.move_up();
        input.move_home();
        input.insert_str("prefix_");
        assert_eq!(input.lines[1], "prefix_line2");
        input.clear();
        assert!(input.is_empty());
        assert_eq!(input.cursor_row, 0);
    }

    #[test]
    fn insert_between_emoji() {
        let mut input = InputState::new();
        input.insert_char('\u{1F600}');
        input.insert_char('\u{1F601}');
        // cursor at col 2, after both emoji
        input.move_left(); // between the two emoji, col 1
        input.insert_char('X');
        assert_eq!(input.lines[0], "\u{1F600}X\u{1F601}");
        assert_eq!(input.cursor_col, 2);
    }

    #[test]
    fn delete_char_after_on_multibyte_boundary() {
        let mut input = InputState::new();
        input.insert_str("\u{1F600}\u{1F601}\u{1F602}");
        input.move_home();
        input.move_right(); // after first emoji
        input.delete_char_after(); // delete second emoji
        assert_eq!(input.lines[0], "\u{1F600}\u{1F602}");
    }

    #[test]
    fn text_consistent_after_every_operation() {
        let mut input = InputState::new();

        input.insert_str("hello");
        assert_eq!(input.text(), "hello");

        input.insert_newline();
        assert_eq!(input.text(), "hello\n");

        input.insert_str("world");
        assert_eq!(input.text(), "hello\nworld");

        input.move_up();
        input.move_end();
        input.insert_char('!');
        assert_eq!(input.text(), "hello!\nworld");

        input.delete_char_before();
        assert_eq!(input.text(), "hello\nworld");

        input.move_down();
        input.move_home();
        input.delete_char_before(); // join lines
        assert_eq!(input.text(), "helloworld");

        input.clear();
        assert_eq!(input.text(), "");
    }

    #[test]
    fn navigate_through_empty_lines() {
        let mut input = InputState::new();
        input.insert_str("\n\n\n");
        // 4 empty lines, cursor at row 3
        assert_eq!(input.cursor_row, 3);
        input.move_up();
        assert_eq!(input.cursor_row, 2);
        assert_eq!(input.cursor_col, 0);
        input.move_up();
        input.move_up();
        assert_eq!(input.cursor_row, 0);
        // Insert on the first empty line
        input.insert_char('x');
        assert_eq!(input.lines[0], "x");
        assert_eq!(input.lines.len(), 4);
    }

    #[test]
    fn insert_str_into_middle_of_existing_content() {
        let mut input = InputState::new();
        input.insert_str("hd");
        input.move_left(); // between h and d
        input.insert_str("ello worl");
        assert_eq!(input.lines[0], "hello world");
    }

    #[test]
    fn multiline_paste_into_middle_of_line() {
        let mut input = InputState::new();
        input.insert_str("start end");
        // Move cursor to col 6 (between "start " and "end")
        input.move_home();
        for _ in 0..6 {
            input.move_right();
        }
        input.insert_str("line1\nline2\nline3 ");
        assert_eq!(input.lines[0], "start line1");
        assert_eq!(input.lines[1], "line2");
        assert_eq!(input.lines[2], "line3 end");
        assert_eq!(input.cursor_row, 2);
    }

    #[test]
    fn version_never_wraps_in_reasonable_use() {
        let mut input = InputState::new();
        // After 1000 operations version should be 1000
        for _ in 0..500 {
            input.insert_char('a');
            input.delete_char_before();
        }
        assert_eq!(input.version, 1000);
    }

    #[test]
    fn mixed_cr_and_lf_in_paste() {
        let mut input = InputState::new();
        // Mix of \r, \n, and \r\n
        input.insert_str("a\rb\nc\r\nd");
        // \r -> newline, \n -> newline, \r -> newline, \n -> newline
        // So: "a", "", "b", "", "c", "", "", "d" -- no wait, let me think again
        // \r -> newline (line "a" done, new line), b -> char, \n -> newline,
        // c -> char, \r -> newline, \n -> newline, d -> char
        // lines: ["a", "b", "c", "", "d"]
        assert_eq!(input.lines[0], "a");
        assert_eq!(input.lines.last().unwrap(), "d");
        // The key point: it doesn't crash and 'd' ends up somewhere
        assert!(input.text().contains('d'));
    }

    #[test]
    fn parse_placeholder_with_trailing_suffix_text() {
        let line = "[Pasted Text 2 - 42 lines]tail";
        let parsed = parse_paste_placeholder_with_suffix(line).unwrap();
        assert_eq!(parsed.0, 1);
        assert_eq!(&line[..parsed.1], "[Pasted Text 2 - 42 lines]");
    }

    #[test]
    fn text_expands_placeholder_even_with_trailing_text() {
        let mut input = InputState::new();
        input.insert_paste_block("line1\nline2");
        input.lines[0].push_str(" + extra");
        input.cursor_col = input.lines[0].chars().count();
        assert_eq!(input.text(), "line1\nline2 + extra");
    }

    #[test]
    fn append_to_active_paste_block_merges_chunks_and_updates_label() {
        let mut input = InputState::new();
        let original = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk";
        input.insert_paste_block(original);
        assert!(input.append_to_active_paste_block("\nl\nm"));
        assert_eq!(input.lines[0], "[Pasted Text 1 - 13 lines]");
        assert_eq!(input.text(), "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm");
    }

    #[test]
    fn append_to_active_paste_block_rejects_dirty_placeholder_line() {
        let mut input = InputState::new();
        input.insert_paste_block("a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk");
        input.lines[0].push_str("tail");
        input.cursor_col = input.lines[0].chars().count();
        assert!(!input.append_to_active_paste_block("x"));
    }

    #[test]
    fn count_text_lines_handles_mixed_line_endings() {
        assert_eq!(count_text_lines("a\r\nb\nc\rd"), 4);
        assert_eq!(count_text_lines("single"), 1);
        assert_eq!(count_text_lines("x\r\n"), 2);
    }

    #[test]
    fn trim_trailing_line_breaks_handles_crlf_and_lf() {
        assert_eq!(trim_trailing_line_breaks("a\r\n\r\n"), "a");
        assert_eq!(trim_trailing_line_breaks("a\n\n"), "a");
        assert_eq!(trim_trailing_line_breaks("a\r\r"), "a");
        assert_eq!(trim_trailing_line_breaks("a"), "a");
    }
}
