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

use crate::app::{BlockCache, ChatMessage, MessageBlock, MessageRole, ToolCallInfo};
use crate::ui::theme;
use agent_client_protocol as acp;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

const SPINNER_FRAMES: &[char] = &[
    '\u{280B}', '\u{2819}', '\u{2839}', '\u{2838}',
    '\u{283C}', '\u{2834}', '\u{2826}', '\u{2827}',
    '\u{2807}', '\u{280F}',
];

/// Snapshot of the app state needed by the spinner — extracted before
/// the message loop so we don't need `&App` (which conflicts with `&mut msg`).
pub struct SpinnerState {
    pub frame: usize,
    pub is_active: bool,
}

// BlockCache model: version starts at 0, lines is None.
// On invalidate(), version bumps to non-zero. On render, we store lines and
// reset version to 0 (clean). If version != 0, cache is stale → re-render.

/// Render a single chat message into a `Vec<Line>`, using per-block caches.
/// Takes `&mut` so block caches can be updated.
/// `spinner` is only used for the "Thinking..." animation on empty assistant messages.
pub fn render_message(msg: &mut ChatMessage, spinner: &SpinnerState) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    match msg.role {
        MessageRole::User => {
            // "User" label in gray bold
            lines.push(Line::from(Span::styled(
                "User",
                Style::default()
                    .fg(theme::DIM)
                    .add_modifier(Modifier::BOLD),
            )));

            // User message: markdown-rendered with background overlay
            for block in &mut msg.blocks {
                if let MessageBlock::Text(text, cache) = block {
                    lines.extend(render_text_cached(text, cache, Some(theme::USER_MSG_BG)));
                }
            }
        }
        MessageRole::Assistant => {
            // "Claude" label in Rust orange bold
            lines.push(Line::from(Span::styled(
                "Claude",
                Style::default()
                    .fg(theme::ROLE_ASSISTANT)
                    .add_modifier(Modifier::BOLD),
            )));

            // Empty blocks + thinking = show spinner
            if msg.blocks.is_empty() && spinner.is_active {
                let ch = SPINNER_FRAMES[spinner.frame % SPINNER_FRAMES.len()];
                lines.push(Line::from(Span::styled(
                    format!("{ch} Thinking..."),
                    Style::default().fg(theme::DIM),
                )));
                lines.push(Line::default());
                return lines;
            }

            // Render blocks in order with spacing at text<->tool transitions
            let mut prev_was_tool = false;
            for block in &mut msg.blocks {
                match block {
                    MessageBlock::Text(text, cache) => {
                        // Add half-spacing when transitioning from tools back to text
                        if prev_was_tool {
                            lines.push(Line::default());
                        }
                        lines.extend(render_text_cached(text, cache, None));
                        prev_was_tool = false;
                    }
                    MessageBlock::ToolCall(tc) => {
                        // Skip hidden tool calls (subagent children)
                        if tc.hidden {
                            continue;
                        }
                        // Add half-spacing when transitioning from text to tools
                        if !prev_was_tool && lines.len() > 1 {
                            lines.push(Line::default());
                        }
                        lines.extend(render_tool_call_cached(tc));
                        prev_was_tool = true;
                    }
                }
            }
        }
        MessageRole::System => {
            lines.push(Line::from(Span::styled(
                "System",
                Style::default()
                    .fg(theme::ROLE_SYSTEM)
                    .add_modifier(Modifier::BOLD),
            )));
            for block in &mut msg.blocks {
                if let MessageBlock::Text(text, _) = block {
                    for text_line in text.lines() {
                        lines.push(Line::from(text_line.to_string()));
                    }
                }
            }
        }
    }

    // Blank separator between messages
    lines.push(Line::default());

    lines
}

/// Render a text block with caching. Only calls tui_markdown when cache is stale.
/// `bg` is an optional background color overlay (used for user messages).
fn render_text_cached(
    text: &str,
    cache: &mut BlockCache,
    bg: Option<Color>,
) -> Vec<Line<'static>> {
    // Check if cache is fresh (version 0 means lines were stored and not invalidated since)
    if let Some(ref cached_lines) = cache.lines {
        if cache.version == 0 {
            return cached_lines.clone();
        }
    }

    // Cache miss — render from markdown
    let rendered = tui_markdown::from_str(text);
    let fresh: Vec<Line<'static>> = rendered
        .lines
        .into_iter()
        .map(|line| {
            let owned_spans: Vec<Span<'static>> = line
                .spans
                .into_iter()
                .map(|s| {
                    let style = if let Some(bg_color) = bg {
                        s.style.bg(bg_color)
                    } else {
                        s.style
                    };
                    Span::styled(s.content.into_owned(), style)
                })
                .collect();
            Line::from(owned_spans).style(line.style)
        })
        .collect();

    cache.lines = Some(fresh.clone());
    cache.version = 0; // Mark as clean
    fresh
}

/// Render a tool call with caching. Only re-renders when cache is stale.
fn render_tool_call_cached(tc: &mut ToolCallInfo) -> Vec<Line<'static>> {
    if let Some(ref cached_lines) = tc.cache.lines {
        if tc.cache.version == 0 {
            return cached_lines.clone();
        }
    }

    let fresh = render_tool_call(tc);
    tc.cache.lines = Some(fresh.clone());
    tc.cache.version = 0;
    fresh
}

fn render_tool_call(tc: &ToolCallInfo) -> Vec<Line<'static>> {
    let (icon, icon_color) = match tc.status {
        acp::ToolCallStatus::Pending => (theme::ICON_PENDING, theme::DIM),
        acp::ToolCallStatus::InProgress => (theme::ICON_RUNNING, theme::RUST_ORANGE),
        acp::ToolCallStatus::Completed => (theme::ICON_COMPLETED, theme::RUST_ORANGE),
        acp::ToolCallStatus::Failed => (theme::ICON_FAILED, theme::STATUS_ERROR),
        _ => ("?", theme::DIM),
    };

    let (kind_icon, _kind_name) = theme::tool_kind_label(tc.kind, tc.claude_tool_name.as_deref());

    let mut title_spans = vec![
        Span::styled(format!("  {icon} "), Style::default().fg(icon_color)),
        Span::styled(
            format!("{kind_icon} "),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    // Render the title with markdown (handles backticks, bold, etc.)
    let rendered = tui_markdown::from_str(&tc.title);
    if let Some(first_line) = rendered.lines.into_iter().next() {
        for span in first_line.spans {
            title_spans.push(Span::styled(span.content.into_owned(), span.style));
        }
    }

    let mut lines = vec![Line::from(title_spans)];

    // Content area below the title with corner bracket prefix
    let pipe_style = Style::default().fg(theme::DIM);

    if tc.content.is_empty() {
        return lines;
    }

    // Diffs (Edit tool) are always shown — user needs to see changes
    let has_diff = tc.content.iter().any(|c| matches!(c, acp::ToolCallContent::Diff(_)));

    if tc.collapsed && !has_diff {
        // Collapsed: show summary + ctrl+o hint
        let summary = content_summary(tc);
        lines.push(Line::from(vec![
            Span::styled("  \u{2514}\u{2500} ", pipe_style),
            Span::styled(summary, Style::default().fg(theme::DIM)),
            Span::styled("  ctrl+o to expand", Style::default().fg(theme::DIM)),
        ]));
    } else {
        // Expanded: render full content with │ prefix on each line
        let content_lines = render_tool_content(tc);
        let last_idx = content_lines.len().saturating_sub(1);
        for (i, content_line) in content_lines.into_iter().enumerate() {
            let prefix = if i == last_idx {
                "  \u{2514}\u{2500} " // └─
            } else {
                "  \u{2502}  "         // │
            };
            let mut spans = vec![Span::styled(prefix.to_string(), pipe_style)];
            spans.extend(content_line.spans);
            lines.push(Line::from(spans));
        }
    }

    lines
}

/// One-line summary for collapsed tool calls.
fn content_summary(tc: &ToolCallInfo) -> String {
    for content in &tc.content {
        match content {
            acp::ToolCallContent::Diff(diff) => {
                let name = diff.path
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_else(|| diff.path.to_string_lossy().into_owned());
                return name;
            }
            acp::ToolCallContent::Content(c) => {
                if let acp::ContentBlock::Text(text) = &c.content {
                    let stripped = strip_outer_code_fence(&text.text);
                    let first = stripped.lines().next().unwrap_or("");
                    return if first.len() > 60 {
                        format!("{}...", &first[..57])
                    } else {
                        first.to_string()
                    };
                }
            }
            acp::ToolCallContent::Terminal(term) => {
                return format!("terminal {}", term.terminal_id);
            }
            _ => {}
        }
    }
    String::new()
}

/// Render the full content of a tool call as lines.
fn render_tool_content(tc: &ToolCallInfo) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    for content in &tc.content {
        match content {
            acp::ToolCallContent::Diff(diff) => {
                lines.extend(render_diff(diff));
            }
            acp::ToolCallContent::Content(c) => {
                if let acp::ContentBlock::Text(text) = &c.content {
                    let stripped = strip_outer_code_fence(&text.text);
                    let md_source = if is_markdown_file(&tc.title) {
                        // Render markdown files as-is
                        stripped
                    } else {
                        // Wrap code files in a fenced block for syntax highlighting
                        let lang = lang_from_title(&tc.title);
                        format!("```{lang}\n{stripped}\n```")
                    };
                    let rendered = tui_markdown::from_str(&md_source);
                    for line in rendered.lines {
                        let owned: Vec<Span<'static>> = line
                            .spans
                            .into_iter()
                            .map(|s| Span::styled(s.content.into_owned(), s.style))
                            .collect();
                        lines.push(Line::from(owned));
                    }
                }
            }
            acp::ToolCallContent::Terminal(term) => {
                lines.push(Line::from(Span::styled(
                    format!("terminal {}", term.terminal_id),
                    Style::default().fg(theme::DIM),
                )));
            }
            _ => {}
        }
    }

    lines
}

/// Check if a tool call title references a markdown file.
fn is_markdown_file(title: &str) -> bool {
    let lower = title.to_lowercase();
    lower.ends_with(".md") || lower.ends_with(".mdx") || lower.ends_with(".markdown")
}

/// Extract a language tag from the file extension in a tool call title.
/// Returns the raw extension (e.g. "rs", "py", "toml") which syntect
/// can resolve to the correct syntax definition. Falls back to empty string.
fn lang_from_title(title: &str) -> String {
    // Title may be "src/main.rs" or "Read src/main.rs" — find last path-like token
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
/// The ACP adapter often wraps file contents in ``` fences.
fn strip_outer_code_fence(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.starts_with("```") {
        // Find end of first line (the opening fence, possibly with a language tag)
        if let Some(first_newline) = trimmed.find('\n') {
            let after_opening = &trimmed[first_newline + 1..];
            // Check if it ends with a closing fence
            if let Some(body) = after_opening.strip_suffix("```") {
                return body.trim_end().to_string();
            }
            // Also handle closing fence followed by newline
            let after_trimmed = after_opening.trim_end();
            if after_trimmed.ends_with("```") {
                return after_trimmed[..after_trimmed.len() - 3].trim_end().to_string();
            }
        }
    }
    text.to_string()
}

/// Render a diff with green/red coloring and +/- prefixes.
/// The ACP Diff struct provides old_text/new_text, not a unified diff string.
/// We generate a simple line-based diff from those.
fn render_diff(diff: &acp::Diff) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // File path header
    let name = diff.path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| diff.path.to_string_lossy().into_owned());
    lines.push(Line::from(Span::styled(
        name,
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
    )));

    // Show removed lines from old_text (if any) then added lines from new_text
    if let Some(old) = &diff.old_text {
        for old_line in old.lines() {
            lines.push(Line::from(Span::styled(
                format!("- {old_line}"),
                Style::default().fg(Color::Red),
            )));
        }
    }
    for new_line in diff.new_text.lines() {
        lines.push(Line::from(Span::styled(
            format!("+ {new_line}"),
            Style::default().fg(Color::Green),
        )));
    }

    lines
}
