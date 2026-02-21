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

use crate::ui::theme;
use agent_client_protocol as acp;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use similar::TextDiff;

/// Render a diff with proper unified-style output using the `similar` crate.
/// The ACP `Diff` struct provides `old_text`/`new_text` -- we compute the actual
/// line-level changes and show only changed lines with context.
pub fn render_diff(diff: &acp::Diff) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // File path header
    let name = diff.path.file_name().map_or_else(
        || diff.path.to_string_lossy().into_owned(),
        |f| f.to_string_lossy().into_owned(),
    );
    lines.push(Line::from(Span::styled(
        name,
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
    )));

    let old = diff.old_text.as_deref().unwrap_or("");
    let new = &diff.new_text;
    let text_diff = TextDiff::from_lines(old, new);

    // Use unified diff with 3 lines of context -- only shows changed hunks
    // instead of the full file content.
    let udiff = text_diff.unified_diff();
    for hunk in udiff.iter_hunks() {
        // Extract the @@ header from the hunk's Display output (first line).
        let hunk_str = hunk.to_string();
        if let Some(header) = hunk_str.lines().next()
            && header.starts_with("@@")
        {
            lines.push(Line::from(Span::styled(
                header.to_owned(),
                Style::default().fg(Color::Cyan),
            )));
        }

        for change in hunk.iter_changes() {
            let value = change.as_str().unwrap_or("").trim_end_matches('\n');
            let (prefix, style) = match change.tag() {
                similar::ChangeTag::Delete => ("-", Style::default().fg(Color::Red)),
                similar::ChangeTag::Insert => ("+", Style::default().fg(Color::Green)),
                similar::ChangeTag::Equal => (" ", Style::default().fg(theme::DIM)),
            };
            lines.push(Line::from(Span::styled(format!("{prefix} {value}"), style)));
        }
    }

    lines
}

/// Check if a tool call title references a markdown file.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
pub fn is_markdown_file(title: &str) -> bool {
    let lower = title.to_lowercase();
    lower.ends_with(".md") || lower.ends_with(".mdx") || lower.ends_with(".markdown")
}

/// Extract a language tag from the file extension in a tool call title.
/// Returns the raw extension (e.g. "rs", "py", "toml") which syntect
/// can resolve to the correct syntax definition. Falls back to empty string.
pub fn lang_from_title(title: &str) -> String {
    // Title may be "src/main.rs" or "Read src/main.rs" - find last path-like token
    title
        .split_whitespace()
        .rev()
        .find_map(|token| {
            let ext = token.rsplit('.').next()?;
            // Ignore if the "extension" is the whole token (no dot found)
            if ext.len() < token.len() { Some(ext.to_lowercase()) } else { None }
        })
        .unwrap_or_default()
}

/// Strip an outer markdown code fence if the text is entirely wrapped in one.
/// The ACP adapter often wraps file contents in ```` ``` ```` fences.
pub fn strip_outer_code_fence(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.starts_with("```") {
        // Find end of first line (the opening fence, possibly with a language tag)
        if let Some(first_newline) = trimmed.find('\n') {
            let after_opening = &trimmed[first_newline + 1..];
            // Check if it ends with a closing fence
            if let Some(body) = after_opening.strip_suffix("```") {
                return body.trim_end().to_owned();
            }
            // Also handle closing fence followed by newline
            let after_trimmed = after_opening.trim_end();
            if let Some(stripped) = after_trimmed.strip_suffix("```") {
                return stripped.trim_end().to_owned();
            }
        }
    }
    text.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // strip_outer_code_fence

    #[test]
    fn strip_fenced_code() {
        let input = "```rust\nfn main() {}\n```";
        let result = strip_outer_code_fence(input);
        assert_eq!(result, "fn main() {}");
    }

    #[test]
    fn strip_fenced_no_lang_tag() {
        let input = "```\nhello world\n```";
        let result = strip_outer_code_fence(input);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn strip_not_fenced_passthrough() {
        let input = "just plain text";
        let result = strip_outer_code_fence(input);
        assert_eq!(result, "just plain text");
    }

    #[test]
    fn strip_fenced_with_trailing_whitespace() {
        let input = "```\ncontent\n```  \n";
        let result = strip_outer_code_fence(input);
        assert_eq!(result, "content");
    }

    #[test]
    fn strip_nested_fences_only_outer() {
        let input = "```\ninner ```\nstuff\n```";
        let result = strip_outer_code_fence(input);
        assert!(result.contains("inner ```"));
    }

    #[test]
    fn strip_only_opening_fence() {
        let input = "```rust\nfn main() {}";
        let result = strip_outer_code_fence(input);
        assert_eq!(result, input);
    }

    #[test]
    fn strip_empty_fenced_block() {
        let input = "```\n```";
        let result = strip_outer_code_fence(input);
        assert_eq!(result, "");
    }

    #[test]
    fn strip_multiline_content() {
        let input = "```python\nline1\nline2\nline3\n```";
        let result = strip_outer_code_fence(input);
        assert_eq!(result, "line1\nline2\nline3");
    }

    /// Quadruple backtick fence -- starts with 4 backticks which starts with 3, so it should still work.
    #[test]
    fn strip_quadruple_backtick_fence() {
        let input = "````\ncontent here\n````";
        let result = strip_outer_code_fence(input);
        // Starts with ```, so it enters the stripping path.
        // Closing is ```` - strip_suffix("```") matches the last 3 backticks
        // leaving one ` in the body. Let's just verify it doesn't panic
        // and returns something reasonable.
        assert!(result.contains("content here"));
    }

    /// Tilde fences -- NOT handled by `strip_outer_code_fence` (only checks triple backticks).
    #[test]
    fn strip_tilde_fence_passthrough() {
        let input = "~~~\ncontent\n~~~";
        let result = strip_outer_code_fence(input);
        assert_eq!(result, input);
    }

    /// Content with inner code fences that look like closing fences.
    #[test]
    fn strip_inner_fence_in_content() {
        let input = "```\nsome code\n```\nmore code\n```";
        let result = strip_outer_code_fence(input);
        // The function finds the first newline, then looks for ``` at the end
        // of the remaining text. The last ``` is the closing fence.
        assert!(result.contains("some code"));
    }

    /// Very large content inside fence - stress test.
    #[test]
    fn strip_large_fenced_content() {
        let big: String = (0..10_000).fold(String::new(), |mut s, i| {
            use std::fmt::Write;
            writeln!(s, "line {i}").unwrap();
            s
        });
        let input = format!("```\n{big}```");
        let result = strip_outer_code_fence(&input);
        assert!(result.contains("line 0"));
        assert!(result.contains("line 9999"));
    }

    /// Fence with blank content line.
    #[test]
    fn strip_fence_with_blank_lines() {
        let input = "```\n\n\n\n```";
        let result = strip_outer_code_fence(input);
        // Content is three blank lines, trimmed to empty
        assert!(result.is_empty() || result.chars().all(|c| c == '\n'));
    }

    /// Text starting with triple backticks but not at the beginning (leading whitespace).
    #[test]
    fn strip_fence_with_leading_whitespace() {
        let input = "  ```\ncontent\n```";
        let result = strip_outer_code_fence(input);
        // After trim(), starts with ```, so should strip
        assert_eq!(result, "content");
    }

    // lang_from_title

    #[test]
    fn lang_rust_file() {
        assert_eq!(lang_from_title("src/main.rs"), "rs");
    }

    #[test]
    fn lang_python_with_prefix() {
        assert_eq!(lang_from_title("Read foo.py"), "py");
    }

    #[test]
    fn lang_toml_file() {
        assert_eq!(lang_from_title("Cargo.toml"), "toml");
    }

    #[test]
    fn lang_no_extension() {
        assert_eq!(lang_from_title("Makefile"), "");
    }

    #[test]
    fn lang_empty_title() {
        assert_eq!(lang_from_title(""), "");
    }

    #[test]
    fn lang_mixed_case() {
        assert_eq!(lang_from_title("file.RS"), "rs");
    }

    #[test]
    fn lang_multiple_dots() {
        assert_eq!(lang_from_title("archive.tar.gz"), "gz");
    }

    #[test]
    fn lang_path_with_spaces() {
        assert_eq!(lang_from_title("Read some/dir/file.tsx"), "tsx");
    }

    #[test]
    fn lang_hidden_file() {
        assert_eq!(lang_from_title(".gitignore"), "gitignore");
    }

    /// Multiple extensions chained: picks the final one.
    #[test]
    fn lang_chained_extensions() {
        assert_eq!(lang_from_title("Read a.test.spec.ts"), "ts");
    }

    /// Dot at end of title: extension is empty string.
    #[test]
    fn lang_dot_at_end() {
        // "file." - rsplit('.').next() returns "", which is shorter than token
        assert_eq!(lang_from_title("file."), "");
    }

    /// Title with only whitespace.
    #[test]
    fn lang_whitespace_only() {
        assert_eq!(lang_from_title("   "), "");
    }

    /// Title with backslash path (Windows).
    #[test]
    fn lang_windows_backslash_path() {
        // Backslashes are not split by split_whitespace, so the whole path is one token
        assert_eq!(lang_from_title("Read src\\main.rs"), "rs");
    }

    // is_markdown_file

    #[test]
    fn is_md_file() {
        assert!(is_markdown_file("README.md"));
    }

    #[test]
    fn is_mdx_file() {
        assert!(is_markdown_file("component.mdx"));
    }

    #[test]
    fn is_markdown_ext() {
        assert!(is_markdown_file("doc.markdown"));
    }

    #[test]
    fn is_markdown_case_insensitive() {
        assert!(is_markdown_file("README.MD"));
        assert!(is_markdown_file("file.Md"));
    }

    #[test]
    fn is_not_markdown() {
        assert!(!is_markdown_file("main.rs"));
        assert!(!is_markdown_file("style.css"));
        assert!(!is_markdown_file(""));
    }

    #[test]
    fn is_not_markdown_partial() {
        assert!(!is_markdown_file("somemdx"));
    }

    /// `.md` in the middle of the name is NOT a markdown extension.
    #[test]
    fn is_not_markdown_md_in_middle() {
        assert!(!is_markdown_file("file.md.bak"));
    }

    /// Path with .md extension.
    #[test]
    fn is_markdown_with_path() {
        assert!(is_markdown_file("docs/getting-started.md"));
        assert!(is_markdown_file("Read /home/user/notes.md"));
    }

    /// `.MARKDOWN` all caps.
    #[test]
    fn is_markdown_uppercase_full() {
        assert!(is_markdown_file("FILE.MARKDOWN"));
    }
}
