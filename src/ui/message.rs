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

use crate::app::{BlockCache, ChatMessage, MessageBlock, MessageRole};
use crate::ui::tables;
use crate::ui::theme;
use crate::ui::tool_call;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

const SPINNER_FRAMES: &[char] = &[
    '\u{280B}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283C}', '\u{2834}', '\u{2826}', '\u{2827}',
    '\u{2807}', '\u{280F}',
];

/// Snapshot of the app state needed by the spinner -- extracted before
/// the message loop so we don't need `&App` (which conflicts with `&mut msg`).
#[derive(Clone, Copy)]
pub struct SpinnerState {
    pub frame: usize,
    pub is_active: bool,
    /// True when this is the last message in the conversation.
    /// Thinking spinners only render on the last assistant message.
    pub is_last_message: bool,
    /// True when the agent is thinking mid-turn (all tool calls finished,
    /// waiting for next action). Shows a trailing spinner after existing blocks.
    pub is_thinking_mid_turn: bool,
}

/// Render a single chat message into a `Vec<Line>`, using per-block caches.
/// Takes `&mut` so block caches can be updated.
/// `spinner` is only used for the "Thinking..." animation on empty assistant messages.
pub fn render_message(
    msg: &mut ChatMessage,
    spinner: &SpinnerState,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    match msg.role {
        MessageRole::User => {
            // "User" label in gray bold
            out.push(Line::from(Span::styled(
                "User",
                Style::default().fg(theme::DIM).add_modifier(Modifier::BOLD),
            )));

            // User message: markdown-rendered with background overlay
            for block in &mut msg.blocks {
                if let MessageBlock::Text(text, cache) = block {
                    render_text_cached(text, cache, width, Some(theme::USER_MSG_BG), true, out);
                }
            }
        }
        MessageRole::Assistant => {
            // "Claude" label in Rust orange bold
            out.push(Line::from(Span::styled(
                "Claude",
                Style::default().fg(theme::ROLE_ASSISTANT).add_modifier(Modifier::BOLD),
            )));

            // Empty blocks + thinking = show spinner (only on the last message)
            if msg.blocks.is_empty() && spinner.is_active && spinner.is_last_message {
                let ch = SPINNER_FRAMES[spinner.frame % SPINNER_FRAMES.len()];
                out.push(Line::from(Span::styled(
                    format!("{ch} Thinking..."),
                    Style::default().fg(theme::DIM),
                )));
                out.push(Line::default());
                return;
            }

            // Render blocks in order with spacing at text<->tool transitions
            let mut prev_was_tool = false;
            for block in &mut msg.blocks {
                match block {
                    MessageBlock::Text(text, cache) => {
                        // Add half-spacing when transitioning from tools back to text
                        if prev_was_tool {
                            out.push(Line::default());
                        }
                        render_text_cached(text, cache, width, None, false, out);
                        prev_was_tool = false;
                    }
                    MessageBlock::ToolCall(tc) => {
                        let tc = tc.as_mut();
                        // Skip hidden tool calls (subagent children)
                        if tc.hidden {
                            continue;
                        }
                        // Add half-spacing when transitioning from text to tools
                        if !prev_was_tool && out.len() > 1 {
                            out.push(Line::default());
                        }
                        tool_call::render_tool_call_cached(tc, width, spinner.frame, out);
                        prev_was_tool = true;
                    }
                }
            }

            // Trailing "Thinking..." spinner when all tool calls finished mid-turn
            if spinner.is_thinking_mid_turn {
                out.push(Line::default());
                let ch = SPINNER_FRAMES[spinner.frame % SPINNER_FRAMES.len()];
                out.push(Line::from(Span::styled(
                    format!("{ch} Thinking..."),
                    Style::default().fg(theme::DIM),
                )));
            }
        }
    }

    // Blank separator between messages
    out.push(Line::default());
}

/// Preprocess markdown that `tui_markdown` doesn't handle well.
/// Headings (`# Title`) become `**Title**` (bold) with a blank line before.
/// Handles variations: `#Title`, `#  Title`, `  ## Title  `, etc.
/// Links are left as-is -- `tui_markdown` handles `[title](url)` natively.
fn preprocess_markdown(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            // Strip all leading '#' characters
            let after_hashes = trimmed.trim_start_matches('#');
            // Extract heading content (trim spaces between # and text, and trailing)
            let content = after_hashes.trim();
            if !content.is_empty() {
                // Blank line before heading for visual separation
                if !result.is_empty() && !result.ends_with("\n\n") {
                    result.push('\n');
                }
                result.push_str("**");
                result.push_str(content);
                result.push_str("**\n");
                continue;
            }
        }
        result.push_str(line);
        result.push('\n');
    }
    if !text.ends_with('\n') {
        result.pop();
    }
    result
}

/// Render a text block with caching. Only calls `tui_markdown` when cache is stale.
/// `bg` is an optional background color overlay (used for user messages).
fn render_text_cached(
    text: &str,
    cache: &mut BlockCache,
    width: u16,
    bg: Option<Color>,
    preserve_newlines: bool,
    out: &mut Vec<Line<'static>>,
) {
    if let Some(cached_lines) = cache.get() {
        out.extend_from_slice(cached_lines);
        return;
    }

    // Cache miss — preprocess headings (tui_markdown doesn't handle them well),
    // then render from markdown.
    let mut preprocessed = preprocess_markdown(text);
    if preserve_newlines {
        preprocessed = force_markdown_line_breaks(&preprocessed);
    }
    let fresh: Vec<Line<'static>> = tables::render_markdown_with_tables(&preprocessed, width, bg);

    // Store first, then extend from stored ref to avoid double-clone.
    cache.store(fresh);
    if let Some(stored) = cache.get() {
        out.extend_from_slice(stored);
    }
}

/// Convert single line breaks into hard breaks so user-entered newlines persist.
fn force_markdown_line_breaks(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = String::with_capacity(text.len());
    for (i, line) in lines.iter().enumerate() {
        if !line.is_empty() {
            out.push_str(line);
            out.push_str("  ");
        }
        if i + 1 < lines.len() || text.ends_with('\n') {
            out.push('\n');
        }
    }
    if text.ends_with('\n') {
        // preserve trailing newline
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // preprocess_markdown

    #[test]
    fn preprocess_h1_heading() {
        let result = preprocess_markdown("# Hello");
        assert!(result.contains("**Hello**"));
        assert!(!result.contains('#'));
    }

    #[test]
    fn preprocess_h3_heading() {
        let result = preprocess_markdown("### Deeply Nested");
        assert!(result.contains("**Deeply Nested**"));
    }

    #[test]
    fn preprocess_non_heading_passthrough() {
        let input = "Just normal text\nwith multiple lines";
        let result = preprocess_markdown(input);
        assert_eq!(result, input);
    }

    #[test]
    fn preprocess_mixed_headings_and_text() {
        let input = "# Title\nSome text\n## Subtitle\nMore text";
        let result = preprocess_markdown(input);
        assert!(result.contains("**Title**"));
        assert!(result.contains("Some text"));
        assert!(result.contains("**Subtitle**"));
        assert!(result.contains("More text"));
    }

    #[test]
    fn preprocess_heading_no_space() {
        let result = preprocess_markdown("#Title");
        assert!(result.contains("**Title**"));
    }

    #[test]
    fn preprocess_heading_extra_spaces() {
        let result = preprocess_markdown("#   Spaced Out   ");
        assert!(result.contains("**Spaced Out**"));
    }

    #[test]
    fn preprocess_indented_heading() {
        let result = preprocess_markdown("  ## Indented");
        assert!(result.contains("**Indented**"));
    }

    #[test]
    fn preprocess_empty_heading() {
        let result = preprocess_markdown("# ");
        assert_eq!(result, "# ");
    }

    #[test]
    fn preprocess_empty_string() {
        assert_eq!(preprocess_markdown(""), "");
    }

    #[test]
    fn preprocess_preserves_trailing_newline() {
        let result = preprocess_markdown("hello\n");
        assert!(result.ends_with('\n'));
    }

    #[test]
    fn preprocess_no_trailing_newline() {
        let result = preprocess_markdown("hello");
        assert!(!result.ends_with('\n'));
    }

    #[test]
    fn preprocess_blank_line_before_heading() {
        let input = "text\n\n# Heading";
        let result = preprocess_markdown(input);
        assert!(!result.contains("\n\n\n"));
        assert!(result.contains("**Heading**"));
    }

    #[test]
    fn preprocess_consecutive_headings() {
        let input = "# First\n# Second";
        let result = preprocess_markdown(input);
        assert!(result.contains("**First**"));
        assert!(result.contains("**Second**"));
    }

    #[test]
    fn preprocess_hash_in_code_not_heading() {
        let result = preprocess_markdown("# actual heading");
        assert!(result.contains("**actual heading**"));
    }

    /// H6 heading (6 `#` chars).
    #[test]
    fn preprocess_h6_heading() {
        let result = preprocess_markdown("###### Deep H6");
        assert!(result.contains("**Deep H6**"));
        assert!(!result.contains('#'));
    }

    /// Heading with markdown formatting inside.
    #[test]
    fn preprocess_heading_with_bold_inside() {
        let result = preprocess_markdown("# **bold** and *italic*");
        assert!(result.contains("****bold** and *italic***"));
    }

    /// Heading at end of file with no trailing newline.
    #[test]
    fn preprocess_heading_at_eof_no_newline() {
        let result = preprocess_markdown("text\n# Final");
        assert!(result.contains("**Final**"));
        assert!(!result.ends_with('\n'));
    }

    /// Only hashes with no text: `###` — content after stripping is empty, passthrough.
    #[test]
    fn preprocess_only_hashes() {
        let result = preprocess_markdown("###");
        assert_eq!(result, "###");
    }

    /// Very long heading.
    #[test]
    fn preprocess_very_long_heading() {
        let long_text = "A".repeat(1000);
        let input = format!("# {long_text}");
        let result = preprocess_markdown(&input);
        assert!(result.starts_with("**"));
        assert!(result.contains(&long_text));
    }

    /// Unicode emoji in heading.
    #[test]
    fn preprocess_unicode_heading() {
        let result = preprocess_markdown("# \u{1F680} Launch \u{4F60}\u{597D}");
        assert!(result.contains("**\u{1F680} Launch \u{4F60}\u{597D}**"));
    }

    /// Quoted heading: `> # Heading` — starts with `>` not `#`, so passthrough.
    #[test]
    fn preprocess_blockquote_heading_passthrough() {
        let result = preprocess_markdown("> # Quoted heading");
        // Line starts with `>`, not `#`, so trimmed starts with `>` not `#`
        assert!(!result.contains("**"));
        assert!(result.contains("> # Quoted heading"));
    }

    /// All heading levels in sequence.
    #[test]
    fn preprocess_all_heading_levels() {
        let input = "# H1\n## H2\n### H3\n#### H4\n##### H5\n###### H6";
        let result = preprocess_markdown(input);
        for label in ["H1", "H2", "H3", "H4", "H5", "H6"] {
            assert!(result.contains(&format!("**{label}**")), "missing {label}");
        }
    }

    // force_markdown_line_breaks

    #[test]
    fn force_breaks_adds_trailing_spaces() {
        let result = force_markdown_line_breaks("line1\nline2");
        assert!(result.contains("line1  \n"));
        assert!(result.contains("line2  "));
    }

    #[test]
    fn force_breaks_preserves_trailing_newline() {
        let result = force_markdown_line_breaks("hello\n");
        assert!(result.ends_with('\n'));
    }

    #[test]
    fn force_breaks_empty_lines_no_trailing_spaces() {
        let result = force_markdown_line_breaks("a\n\nb");
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].ends_with("  "));
        assert_eq!(lines[1], "");
        assert!(lines[2].ends_with("  "));
    }

    #[test]
    fn force_breaks_single_line_no_trailing_newline() {
        let result = force_markdown_line_breaks("hello");
        assert_eq!(result, "hello  ");
    }

    #[test]
    fn force_breaks_many_consecutive_empty_lines() {
        let result = force_markdown_line_breaks("a\n\n\nb");
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.len(), 4);
    }

    /// Empty input.
    #[test]
    fn force_breaks_empty_input() {
        let result = force_markdown_line_breaks("");
        assert_eq!(result, "");
    }

    /// Only empty lines.
    #[test]
    fn force_breaks_only_empty_lines() {
        let result = force_markdown_line_breaks("\n\n\n");
        let lines: Vec<&str> = result.lines().collect();
        // All lines are empty, so no trailing spaces added
        for line in &lines {
            assert!(line.is_empty(), "empty line got content: {line:?}");
        }
    }

    /// Line already ending with two spaces — gets two more.
    #[test]
    fn force_breaks_already_has_trailing_spaces() {
        let result = force_markdown_line_breaks("hello  \nworld");
        // "hello  " + "  " = "hello    "
        assert!(result.starts_with("hello    "));
    }

    /// Single newline (no content).
    #[test]
    fn force_breaks_single_newline() {
        let result = force_markdown_line_breaks("\n");
        // One empty line, should stay empty with trailing newline
        assert_eq!(result, "\n");
    }
}
