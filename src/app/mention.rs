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

use super::{App, FocusTarget, dialog::DialogState};
use ignore::WalkBuilder;
use std::path::Path;
use std::time::SystemTime;

/// Maximum candidates shown in the dropdown.
pub const MAX_VISIBLE: usize = 8;

/// Maximum total candidates kept after filtering.
const MAX_CANDIDATES: usize = 50;

pub struct MentionState {
    /// Character position (row, col) where the `@` was typed.
    pub trigger_row: usize,
    pub trigger_col: usize,
    /// Current query text after the `@` (e.g. "src/m" from "@src/m").
    pub query: String,
    /// Filtered + sorted candidates.
    pub candidates: Vec<FileCandidate>,
    /// Shared autocomplete dialog navigation state.
    pub dialog: DialogState,
}

#[derive(Clone)]
pub struct FileCandidate {
    /// Relative path from cwd (forward slashes, e.g. "src/main.rs").
    /// Directories have a trailing `/` (e.g. "src/").
    pub rel_path: String,
    /// Depth (number of `/` separators) for grouping.
    pub depth: usize,
    /// Last modified time for sorting within depth groups.
    pub modified: SystemTime,
    /// Whether this candidate is a directory (true) or a file (false).
    pub is_dir: bool,
}

/// Scan all files and directories under `cwd` using the `ignore` crate (respects .gitignore).
/// Returns candidates sorted by depth ascending, then modified descending.
pub fn scan_files(cwd: &str) -> Vec<FileCandidate> {
    let cwd_path = Path::new(cwd);
    let mut candidates = Vec::new();

    let walker = WalkBuilder::new(cwd_path)
        .hidden(false) // include dotfiles like .github/, .gitignore
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();

    for entry in walker.flatten() {
        let is_file = entry.file_type().is_some_and(|ft| ft.is_file());
        let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());

        if !is_file && !is_dir {
            continue;
        }

        let path = entry.path();
        let Ok(rel) = path.strip_prefix(cwd_path) else {
            continue;
        };

        // Convert to forward-slash relative path
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if rel_str.is_empty() {
            continue; // skip the cwd root itself
        }

        // Compute depth before moving rel_str
        let depth = rel_str.matches('/').count();

        // Directories get a trailing `/` for visual distinction
        let rel_path = if is_dir { format!("{rel_str}/") } else { rel_str };
        let modified =
            entry.metadata().ok().and_then(|m| m.modified().ok()).unwrap_or(SystemTime::UNIX_EPOCH);

        candidates.push(FileCandidate { rel_path, depth, modified, is_dir });
    }

    // Sort: directories before files at same depth, then depth ascending,
    // then modified descending within each group
    candidates.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then_with(|| b.is_dir.cmp(&a.is_dir)) // dirs first (true > false)
            .then_with(|| b.modified.cmp(&a.modified))
    });
    candidates
}

/// Filter cached candidates by a query string (case-insensitive substring match).
pub fn filter_candidates(cache: &[FileCandidate], query: &str) -> Vec<FileCandidate> {
    if query.is_empty() {
        return cache.iter().take(MAX_CANDIDATES).cloned().collect();
    }

    let query_lower = query.to_lowercase();
    cache
        .iter()
        .filter(|c| c.rel_path.to_lowercase().contains(&query_lower))
        .take(MAX_CANDIDATES)
        .cloned()
        .collect()
}

/// Detect an `@` mention at the current cursor position.
/// Scans backwards from the cursor to find `@`. The `@` must be preceded by
/// whitespace, a newline, or be at position 0 (to avoid false triggers mid-word).
/// Returns `(trigger_row, trigger_col, query)` where `trigger_col` is the
/// position of the `@` character itself.
pub fn detect_mention_at_cursor(
    lines: &[String],
    cursor_row: usize,
    cursor_col: usize,
) -> Option<(usize, usize, String)> {
    let line = lines.get(cursor_row)?;
    let chars: Vec<char> = line.chars().collect();

    // Scan backwards from cursor_col to find `@`
    let mut i = cursor_col;
    while i > 0 {
        i -= 1;
        let ch = *chars.get(i)?;
        if ch == '@' {
            // Check that `@` is preceded by whitespace or is at start of line
            if i == 0 || chars.get(i - 1).is_some_and(|c| c.is_whitespace()) {
                let query: String = chars[i + 1..cursor_col].iter().collect();
                // Query must not contain whitespace (that would end the mention)
                if query.chars().all(|c| !c.is_whitespace()) {
                    return Some((cursor_row, i, query));
                }
            }
            return None;
        }
        // If we hit whitespace before finding `@`, there's no mention here
        if ch.is_whitespace() {
            return None;
        }
    }
    None
}

/// Activate mention autocomplete after the user types `@`.
pub fn activate(app: &mut App) {
    let detection =
        detect_mention_at_cursor(&app.input.lines, app.input.cursor_row, app.input.cursor_col);

    let Some((trigger_row, trigger_col, query)) = detection else {
        return;
    };
    // Scan files if cache is empty
    if app.file_cache.is_none() {
        app.file_cache = Some(scan_files(&app.cwd_raw));
    }

    let candidates =
        app.file_cache.as_ref().map(|cache| filter_candidates(cache, &query)).unwrap_or_default();

    app.mention = Some(MentionState {
        trigger_row,
        trigger_col,
        query,
        candidates,
        dialog: DialogState::default(),
    });
    app.slash = None;
    app.claim_focus_target(FocusTarget::Mention);
}

/// Update the query and re-filter candidates while mention is active.
pub fn update_query(app: &mut App) {
    let detection =
        detect_mention_at_cursor(&app.input.lines, app.input.cursor_row, app.input.cursor_col);

    let Some((trigger_row, trigger_col, query)) = detection else {
        deactivate(app);
        return;
    };
    let candidates =
        app.file_cache.as_ref().map(|cache| filter_candidates(cache, &query)).unwrap_or_default();

    if let Some(ref mut mention) = app.mention {
        mention.trigger_row = trigger_row;
        mention.trigger_col = trigger_col;
        mention.query = query;
        mention.candidates = candidates;
        mention.dialog.clamp(mention.candidates.len(), MAX_VISIBLE);
    }
}

/// Keep mention state in sync with the current cursor location.
/// - If cursor is inside a valid `@mention` token, activate/update autocomplete.
/// - Otherwise, deactivate mention autocomplete.
pub fn sync_with_cursor(app: &mut App) {
    let in_mention =
        detect_mention_at_cursor(&app.input.lines, app.input.cursor_row, app.input.cursor_col)
            .is_some();
    match (in_mention, app.mention.is_some()) {
        (true, true) => update_query(app),
        (true, false) => activate(app),
        (false, true) => deactivate(app),
        (false, false) => {}
    }
}

/// Confirm the selected candidate: replace `@query` in input with `@rel_path`.
pub fn confirm_selection(app: &mut App) {
    let Some(mention) = app.mention.take() else {
        return;
    };
    app.release_focus_target(FocusTarget::Mention);

    let Some(candidate) = mention.candidates.get(mention.dialog.selected) else {
        return;
    };

    let rel_path = candidate.rel_path.clone();
    let trigger_row = mention.trigger_row;
    let trigger_col = mention.trigger_col;

    // Replace the full mention token (from `@` to the next whitespace),
    // so editing in the middle of an existing mention correctly rewrites
    // the entire path instead of only the prefix before the cursor.
    let line = &mut app.input.lines[trigger_row];
    let chars: Vec<char> = line.chars().collect();
    if trigger_col >= chars.len() || chars[trigger_col] != '@' {
        return;
    }

    // Find token end: first whitespace after `@...`
    let mention_end =
        (trigger_col + 1..chars.len()).find(|&i| chars[i].is_whitespace()).unwrap_or(chars.len());

    // Rebuild the line: before_@ + @rel_path + optional trailing space + after_token
    let before: String = chars[..trigger_col].iter().collect();
    let after: String = chars[mention_end..].iter().collect();
    let replacement =
        if after.is_empty() { format!("@{rel_path} ") } else { format!("@{rel_path}") };

    let new_line = format!("{before}{replacement}{after}");
    let new_cursor_col = trigger_col + replacement.chars().count();

    app.input.lines[trigger_row] = new_line;
    app.input.cursor_col = new_cursor_col;
    app.input.version += 1;
    app.input.sync_textarea_engine();
}

/// Deactivate mention autocomplete.
pub fn deactivate(app: &mut App) {
    app.mention = None;
    if app.slash.is_none() {
        app.release_focus_target(FocusTarget::Mention);
    }
}

/// Move selection up in the candidate list.
pub fn move_up(app: &mut App) {
    if let Some(ref mut mention) = app.mention {
        mention.dialog.move_up(mention.candidates.len(), MAX_VISIBLE);
    }
}

/// Move selection down in the candidate list.
pub fn move_down(app: &mut App) {
    if let Some(ref mut mention) = app.mention {
        mention.dialog.move_down(mention.candidates.len(), MAX_VISIBLE);
    }
}

/// Find all `@path` references in a text string. Returns `(start_byte, end_byte, path)` tuples.
/// A valid `@path` must start after whitespace or at position 0, and extends until
/// the next whitespace or end of string.
pub fn find_mention_spans(text: &str) -> Vec<(usize, usize, String)> {
    let mut spans = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '@' && (i == 0 || chars[i - 1].is_whitespace()) {
            let start = i;
            i += 1; // skip `@`
            // Collect path characters (non-whitespace)
            let path_start = i;
            while i < chars.len() && !chars[i].is_whitespace() {
                i += 1;
            }
            if i > path_start {
                let path: String = chars[path_start..i].iter().collect();
                // Convert char indices to byte offsets
                let byte_start: usize = chars[..start].iter().map(|c| c.len_utf8()).sum();
                let byte_end: usize = chars[..i].iter().map(|c| c.len_utf8()).sum();
                spans.push((byte_start, byte_end, path));
            }
        } else {
            i += 1;
        }
    }

    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;

    #[test]
    fn sync_with_cursor_activates_inside_existing_mention() {
        let mut app = App::test_default();
        app.input.set_text("open @src/main.rs now");
        app.input.cursor_row = 0;
        app.input.cursor_col = "open @src".chars().count();
        app.file_cache = Some(vec![
            FileCandidate {
                rel_path: "src/main.rs".into(),
                depth: 1,
                modified: SystemTime::UNIX_EPOCH,
                is_dir: false,
            },
            FileCandidate {
                rel_path: "tests/integration.rs".into(),
                depth: 1,
                modified: SystemTime::UNIX_EPOCH,
                is_dir: false,
            },
        ]);

        sync_with_cursor(&mut app);

        let mention = app.mention.as_ref().expect("mention should be active");
        assert_eq!(mention.query, "src");
        assert!(!mention.candidates.is_empty());
    }

    #[test]
    fn confirm_selection_replaces_full_existing_token_without_double_space() {
        let mut app = App::test_default();
        app.input.set_text("open @src/main.rs now");
        app.input.cursor_row = 0;
        app.input.cursor_col = "open @src".chars().count();
        app.file_cache = Some(vec![FileCandidate {
            rel_path: "src/lib.rs".into(),
            depth: 1,
            modified: SystemTime::UNIX_EPOCH,
            is_dir: false,
        }]);

        activate(&mut app);
        confirm_selection(&mut app);

        assert_eq!(app.input.lines[0], "open @src/lib.rs now");
        assert!(app.mention.is_none());
    }

    #[test]
    fn confirm_selection_at_end_keeps_trailing_space() {
        let mut app = App::test_default();
        app.input.set_text("@src/mai");
        app.input.cursor_row = 0;
        app.input.cursor_col = app.input.lines[0].chars().count();
        app.file_cache = Some(vec![FileCandidate {
            rel_path: "src/main.rs".into(),
            depth: 1,
            modified: SystemTime::UNIX_EPOCH,
            is_dir: false,
        }]);

        activate(&mut app);
        confirm_selection(&mut app);

        assert_eq!(app.input.lines[0], "@src/main.rs ");
    }

    #[test]
    fn activate_with_empty_query_shows_all_candidates() {
        let mut app = App::test_default();
        app.input.set_text("@");
        app.input.cursor_row = 0;
        app.input.cursor_col = 1;
        app.file_cache = Some(vec![FileCandidate {
            rel_path: "src/main.rs".into(),
            depth: 1,
            modified: SystemTime::UNIX_EPOCH,
            is_dir: false,
        }]);

        activate(&mut app);

        let mention = app.mention.as_ref().expect("mention should be active");
        assert_eq!(mention.query, "");
        assert_eq!(mention.candidates.len(), 1);
    }

    #[test]
    fn update_query_keeps_active_when_query_becomes_empty() {
        let mut app = App::test_default();
        app.input.set_text("@src");
        app.input.cursor_row = 0;
        app.input.cursor_col = app.input.lines[0].chars().count();
        app.file_cache = Some(vec![FileCandidate {
            rel_path: "src/main.rs".into(),
            depth: 1,
            modified: SystemTime::UNIX_EPOCH,
            is_dir: false,
        }]);
        activate(&mut app);
        assert!(app.mention.is_some());

        // Cursor directly after '@' means empty mention query.
        app.input.cursor_col = 1;
        update_query(&mut app);

        let mention = app.mention.as_ref().expect("mention should stay active");
        assert_eq!(mention.query, "");
        assert_eq!(mention.candidates.len(), 1);
    }
}
