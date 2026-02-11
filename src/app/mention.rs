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

use super::App;
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
    /// Index into `candidates` of the highlighted item.
    pub selected: usize,
    /// Scroll offset for the dropdown (when candidates > max visible).
    pub scroll_offset: usize,
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
        selected: 0,
        scroll_offset: 0,
    });
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
        // Clamp selection to new candidate count
        if mention.candidates.is_empty() {
            mention.selected = 0;
            mention.scroll_offset = 0;
        } else {
            mention.selected = mention.selected.min(mention.candidates.len() - 1);
            clamp_scroll(mention);
        }
    }
}

/// Confirm the selected candidate: replace `@query` in input with `@rel_path`.
pub fn confirm_selection(app: &mut App) {
    let Some(mention) = app.mention.take() else {
        return;
    };

    let Some(candidate) = mention.candidates.get(mention.selected) else {
        return;
    };

    let rel_path = candidate.rel_path.clone();
    let trigger_row = mention.trigger_row;
    let trigger_col = mention.trigger_col;

    // The `@` is at trigger_col, the query extends to cursor position.
    // Replace from trigger_col (the `@`) through the current query with `@rel_path `.
    let line = &mut app.input.lines[trigger_row];
    let chars: Vec<char> = line.chars().collect();

    // Calculate current end of the mention (trigger_col + 1 for `@` + query length)
    let mention_end = trigger_col + 1 + mention.query.chars().count();

    // Rebuild the line: before_@ + @rel_path + space + after_query
    let before: String = chars[..trigger_col].iter().collect();
    let after: String = chars[mention_end..].iter().collect();
    let replacement = format!("@{rel_path} ");

    let new_line = format!("{before}{replacement}{after}");
    let new_cursor_col = trigger_col + replacement.chars().count();

    app.input.lines[trigger_row] = new_line;
    app.input.cursor_col = new_cursor_col;
}

/// Deactivate mention autocomplete.
pub fn deactivate(app: &mut App) {
    app.mention = None;
}

/// Move selection up in the candidate list.
pub fn move_up(app: &mut App) {
    if let Some(ref mut mention) = app.mention {
        if mention.candidates.is_empty() {
            return;
        }
        if mention.selected == 0 {
            mention.selected = mention.candidates.len() - 1;
        } else {
            mention.selected -= 1;
        }
        clamp_scroll(mention);
    }
}

/// Move selection down in the candidate list.
pub fn move_down(app: &mut App) {
    if let Some(ref mut mention) = app.mention {
        if mention.candidates.is_empty() {
            return;
        }
        mention.selected = (mention.selected + 1) % mention.candidates.len();
        clamp_scroll(mention);
    }
}

/// Ensure `scroll_offset` keeps `selected` visible within the `MAX_VISIBLE` window.
fn clamp_scroll(mention: &mut MentionState) {
    if mention.selected < mention.scroll_offset {
        mention.scroll_offset = mention.selected;
    } else if mention.selected >= mention.scroll_offset + MAX_VISIBLE {
        mention.scroll_offset = mention.selected + 1 - MAX_VISIBLE;
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
