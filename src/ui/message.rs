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

use crate::app::{
    BlockCache, ChatMessage, InlinePermission, MessageBlock, MessageRole, ToolCallInfo,
};
use crate::ui::tables;
use crate::ui::theme;
use agent_client_protocol::{self as acp, PermissionOptionKind};
use ansi_to_tui::IntoText as _;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use similar::TextDiff;

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

// BlockCache model: version starts at 0, lines is None.
// On invalidate(), version bumps to non-zero. On render, we store lines and
// reset version to 0 (clean). If version != 0, cache is stale → re-render.

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
                        render_tool_call_cached(tc, width, spinner.frame, out);
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

/// Render a tool call with caching. Only re-renders when cache is stale.
/// `InProgress` tool calls skip caching because the icon color pulses each frame.
fn render_tool_call_cached(
    tc: &mut ToolCallInfo,
    width: u16,
    spinner_frame: usize,
    out: &mut Vec<Line<'static>>,
) {
    let is_in_progress =
        matches!(tc.status, acp::ToolCallStatus::InProgress | acp::ToolCallStatus::Pending);

    // Skip cache for in-progress tool calls (icon pulses)
    if !is_in_progress && let Some(cached_lines) = tc.cache.get() {
        out.extend_from_slice(cached_lines);
        return;
    }

    let fresh = render_tool_call(tc, width, spinner_frame);

    // Only cache completed/failed tool calls
    if is_in_progress {
        // In-progress: move directly, no caching needed.
        out.extend(fresh);
    } else {
        // Store first, then extend from stored ref to avoid double-clone.
        tc.cache.store(fresh);
        if let Some(stored) = tc.cache.get() {
            out.extend_from_slice(stored);
        }
    }
}

/// Max visible output lines for Execute/Bash tool calls.
/// Total box height = 1 (title) + 1 (command) + this + 1 (bottom border) = 15.
/// TODO: make configurable (see ROADMAP.md)
const TERMINAL_MAX_LINES: usize = 12;

/// Render inline permission options on a single compact line.
/// Format: `▸ ✓ Allow once (y)  ·  ✓ Allow always (a)  ·  ✗ Reject (n)`
/// Unfocused permissions are dimmed to indicate they don't have keyboard input.
fn render_permission_lines(perm: &InlinePermission) -> Vec<Line<'static>> {
    // Unfocused permissions: show a dimmed "waiting for focus" line
    if !perm.focused {
        return vec![
            Line::default(),
            Line::from(Span::styled(
                "  \u{25cb} Waiting for input\u{2026} (\u{2191}\u{2193} to focus)",
                Style::default().fg(theme::DIM),
            )),
        ];
    }

    let mut spans: Vec<Span<'static>> = Vec::new();
    let dot = Span::styled("  \u{00b7}  ", Style::default().fg(theme::DIM));

    for (i, opt) in perm.options.iter().enumerate() {
        let is_selected = i == perm.selected_index;
        let is_allow =
            matches!(opt.kind, PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways);

        let (icon, icon_color) = if is_allow {
            ("\u{2713}", Color::Green) // ✓
        } else {
            ("\u{2717}", Color::Red) // ✗
        };

        // Separator between options
        if i > 0 {
            spans.push(dot.clone());
        }

        // Selection indicator
        if is_selected {
            spans.push(Span::styled(
                "\u{25b8} ",
                Style::default().fg(theme::RUST_ORANGE).add_modifier(Modifier::BOLD),
            ));
        }

        spans.push(Span::styled(format!("{icon} "), Style::default().fg(icon_color)));

        let name_style = if is_selected {
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(Span::styled(opt.name.clone(), name_style));

        let shortcut = match opt.kind {
            PermissionOptionKind::AllowOnce => " (y)",
            PermissionOptionKind::AllowAlways => " (a)",
            PermissionOptionKind::RejectOnce => " (n)",
            PermissionOptionKind::RejectAlways => " (N)",
            _ => "",
        };
        spans.push(Span::styled(shortcut, Style::default().fg(theme::DIM)));
    }

    vec![
        Line::default(),
        Line::from(spans),
        Line::from(Span::styled(
            "\u{2190}\u{2192} select  \u{2191}\u{2193} next  enter confirm  esc reject",
            Style::default().fg(theme::DIM),
        )),
    ]
}

fn render_tool_call(tc: &ToolCallInfo, width: u16, spinner_frame: usize) -> Vec<Line<'static>> {
    // Execute/Bash tool calls get a distinct rendering
    if matches!(tc.kind, acp::ToolKind::Execute) {
        return render_execute_tool_call(tc, width, spinner_frame);
    }

    let (icon, icon_color) = status_icon(tc.status, spinner_frame);
    let (kind_icon, _kind_name) = theme::tool_kind_label(tc.kind, tc.claude_tool_name.as_deref());

    let mut title_spans = vec![
        Span::styled(format!("  {icon} "), Style::default().fg(icon_color)),
        Span::styled(
            format!("{kind_icon} "),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
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

    let pipe_style = Style::default().fg(theme::DIM);
    let has_permission = tc.pending_permission.is_some();

    // Diffs (Edit tool) are always shown — user needs to see changes
    let has_diff = tc.content.iter().any(|c| matches!(c, acp::ToolCallContent::Diff(_)));

    if tc.content.is_empty() && !has_permission {
        return lines;
    }

    // Force expanded when permission is pending (user needs to see context)
    let effectively_collapsed = tc.collapsed && !has_diff && !has_permission;

    if effectively_collapsed {
        // Collapsed: show summary + ctrl+o hint
        let summary = content_summary(tc);
        lines.push(Line::from(vec![
            Span::styled("  \u{2514}\u{2500} ", pipe_style),
            Span::styled(summary, Style::default().fg(theme::DIM)),
            Span::styled("  ctrl+o to expand", Style::default().fg(theme::DIM)),
        ]));
    } else {
        // Expanded: render full content with │ prefix on each line
        let mut content_lines = render_tool_content(tc);

        // Append inline permission controls if pending
        if let Some(ref perm) = tc.pending_permission {
            content_lines.extend(render_permission_lines(perm));
        }

        let last_idx = content_lines.len().saturating_sub(1);
        for (i, content_line) in content_lines.into_iter().enumerate() {
            let prefix = if i == last_idx {
                "  \u{2514}\u{2500} " // └─
            } else {
                "  \u{2502}  " // │
            };
            let mut spans = vec![Span::styled(prefix.to_owned(), pipe_style)];
            spans.extend(content_line.spans);
            lines.push(Line::from(spans));
        }
    }

    lines
}

/// Spinner frames as `&'static str` for use in `status_icon` return type.
const SPINNER_STRS: &[&str] = &[
    "\u{280B}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283C}", "\u{2834}", "\u{2826}", "\u{2827}",
    "\u{2807}", "\u{280F}",
];

fn status_icon(status: acp::ToolCallStatus, spinner_frame: usize) -> (&'static str, Color) {
    match status {
        acp::ToolCallStatus::Pending | acp::ToolCallStatus::InProgress => {
            let s = SPINNER_STRS[spinner_frame % SPINNER_STRS.len()];
            (s, theme::RUST_ORANGE)
        }
        acp::ToolCallStatus::Completed => (theme::ICON_COMPLETED, theme::RUST_ORANGE),
        acp::ToolCallStatus::Failed => (theme::ICON_FAILED, theme::STATUS_ERROR),
        _ => ("?", theme::DIM),
    }
}

/// Render an Execute/Bash tool call as a bordered terminal box:
///
///   ╭─ ✓ Bash  title ───────────────────────╮
///   │ $ command
///   │ output line 1
///   │ ...
///   ╰───────────────────────────────────────╯
///
/// Left border on all content lines, right border only on top/bottom rules.
/// Top/bottom rules stretch to the full terminal width.
/// Output is capped at `TERMINAL_MAX_LINES` (tail).
fn render_execute_tool_call(
    tc: &ToolCallInfo,
    width: u16,
    spinner_frame: usize,
) -> Vec<Line<'static>> {
    let (status_icon_str, icon_color) = status_icon(tc.status, spinner_frame);
    let border = Style::default().fg(theme::DIM);
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Available width minus the 2-char left indent ("  ")
    let inner_w = (width as usize).saturating_sub(2);

    // ── Top border: ╭─ ⏵ Bash  title ──────────────────────────╮
    // Fixed parts: "╭─" (2) + " ⏵ " (3) + "Bash " (5) + title + " " (1) + rule + "╮" (1)
    let label_overhead = 2 + 3 + 5 + 1 + 1; // chars consumed by fixed parts
    let title_len = tc.title.chars().count();
    let fill = inner_w.saturating_sub(label_overhead + title_len);
    let top_fill: String = "\u{2500}".repeat(fill);
    lines.push(Line::from(vec![
        Span::styled("  \u{256D}\u{2500}", border),
        Span::styled(format!(" {status_icon_str} "), Style::default().fg(icon_color)),
        Span::styled("Bash ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(tc.title.clone(), Style::default().fg(Color::White)),
        Span::styled(format!(" {top_fill}\u{256E}"), border),
    ]));

    // ── Command line: │ $ command
    if let Some(ref cmd) = tc.terminal_command {
        lines.push(Line::from(vec![
            Span::styled("  \u{2502} ", border),
            Span::styled(
                "$ ",
                Style::default().fg(theme::RUST_ORANGE).add_modifier(Modifier::BOLD),
            ),
            Span::styled(cmd.clone(), Style::default().fg(Color::Yellow)),
        ]));
    }

    // ── Output lines (capped) ──
    let mut body_lines: Vec<Line<'static>> = Vec::new();

    if let Some(ref output) = tc.terminal_output {
        let raw_lines: Vec<Line<'static>> = if let Ok(ansi_text) = output.as_bytes().into_text() {
            ansi_text
                .lines
                .into_iter()
                .map(|line| {
                    let owned: Vec<Span<'static>> = line
                        .spans
                        .into_iter()
                        .map(|s| Span::styled(s.content.into_owned(), s.style))
                        .collect();
                    Line::from(owned)
                })
                .collect()
        } else {
            output.lines().map(|l| Line::from(l.to_owned())).collect()
        };

        let total = raw_lines.len();
        if total > TERMINAL_MAX_LINES {
            let skipped = total - TERMINAL_MAX_LINES;
            body_lines.push(Line::from(Span::styled(
                format!("... {skipped} lines hidden ..."),
                Style::default().fg(theme::DIM),
            )));
            body_lines.extend(raw_lines.into_iter().skip(skipped));
        } else {
            body_lines = raw_lines;
        }
    } else if matches!(tc.status, acp::ToolCallStatus::InProgress) {
        body_lines.push(Line::from(Span::styled("running...", Style::default().fg(theme::DIM))));
    }

    for content_line in body_lines {
        let mut spans = vec![Span::styled("  \u{2502} ", border)];
        spans.extend(content_line.spans);
        lines.push(Line::from(spans));
    }

    // ── Inline permission controls (inside the box) ──
    if let Some(ref perm) = tc.pending_permission {
        for perm_line in render_permission_lines(perm) {
            let mut spans = vec![Span::styled("  \u{2502} ", border)];
            spans.extend(perm_line.spans);
            lines.push(Line::from(spans));
        }
    }

    // ── Bottom border: ╰────────────────────────────────────────╯
    // "╰" (1) + rule + "╯" (1) = inner_w
    let bottom_fill: String = "\u{2500}".repeat(inner_w.saturating_sub(2));
    lines.push(Line::from(Span::styled(format!("  \u{2570}{bottom_fill}\u{256F}"), border)));

    lines
}

/// One-line summary for collapsed tool calls.
fn content_summary(tc: &ToolCallInfo) -> String {
    // For Execute tool calls, show last non-empty line of terminal output
    if tc.terminal_id.is_some() {
        if let Some(ref output) = tc.terminal_output {
            let last = output.lines().rev().find(|l| !l.trim().is_empty());
            if let Some(line) = last {
                return if line.chars().count() > 80 {
                    let truncated: String = line.chars().take(77).collect();
                    format!("{truncated}...")
                } else {
                    line.to_owned()
                };
            }
        }
        return if matches!(tc.status, acp::ToolCallStatus::InProgress) {
            "running...".to_owned()
        } else {
            String::new()
        };
    }

    for content in &tc.content {
        match content {
            acp::ToolCallContent::Diff(diff) => {
                let name = diff.path.file_name().map_or_else(
                    || diff.path.to_string_lossy().into_owned(),
                    |f| f.to_string_lossy().into_owned(),
                );
                return name;
            }
            acp::ToolCallContent::Content(c) => {
                if let acp::ContentBlock::Text(text) = &c.content {
                    let stripped = strip_outer_code_fence(&text.text);
                    let first = stripped.lines().next().unwrap_or("");
                    return if first.chars().count() > 60 {
                        let truncated: String = first.chars().take(57).collect();
                        format!("{truncated}...")
                    } else {
                        first.to_owned()
                    };
                }
            }
            _ => {}
        }
    }
    String::new()
}

/// Render the full content of a tool call as lines.
fn render_tool_content(tc: &ToolCallInfo) -> Vec<Line<'static>> {
    let is_execute = matches!(tc.kind, acp::ToolKind::Execute);
    let mut lines: Vec<Line<'static>> = Vec::new();

    // For Execute tool calls with terminal output, render the live output
    if is_execute {
        if let Some(ref output) = tc.terminal_output {
            if let Ok(ansi_text) = output.as_bytes().into_text() {
                for line in ansi_text.lines {
                    let owned: Vec<Span<'static>> = line
                        .spans
                        .into_iter()
                        .map(|s| Span::styled(s.content.into_owned(), s.style))
                        .collect();
                    lines.push(Line::from(owned));
                }
            } else {
                for text_line in output.lines() {
                    lines.push(Line::from(text_line.to_owned()));
                }
            }
        } else if matches!(tc.status, acp::ToolCallStatus::InProgress) {
            lines.push(Line::from(Span::styled("running...", Style::default().fg(theme::DIM))));
        }
        return lines;
    }

    for content in &tc.content {
        match content {
            acp::ToolCallContent::Diff(diff) => {
                lines.extend(render_diff(diff));
            }
            acp::ToolCallContent::Content(c) => {
                if let acp::ContentBlock::Text(text) = &c.content {
                    let stripped = strip_outer_code_fence(&text.text);
                    let md_source = if is_markdown_file(&tc.title) {
                        stripped
                    } else {
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
            _ => {}
        }
    }

    lines
}

/// Check if a tool call title references a markdown file.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
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
/// The ACP adapter often wraps file contents in ```` ``` ```` fences.
fn strip_outer_code_fence(text: &str) -> String {
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

/// Render a diff with proper unified-style output using the `similar` crate.
/// The ACP `Diff` struct provides `old_text`/`new_text` -- we compute the actual
/// line-level changes and show only changed lines with context.
fn render_diff(diff: &acp::Diff) -> Vec<Line<'static>> {
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

#[cfg(test)]
mod tests {
    // =====
    // TESTS: 75
    // =====

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

    // preprocess_markdown

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

    // preprocess_markdown

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

    // strip_outer_code_fence

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

    // strip_outer_code_fence

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
        // Closing is ```` — strip_suffix("```") matches the last 3 backticks
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

    /// Very large content inside fence — stress test.
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

    // lang_from_title

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

    // lang_from_title

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
        // "file." — rsplit('.').next() returns "", which is shorter than token
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

    // is_markdown_file

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

    // is_markdown_file

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

    // force_markdown_line_breaks

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

    // force_markdown_line_breaks

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

    // status_icon

    #[test]
    fn status_icon_pending() {
        let (icon, color) = status_icon(acp::ToolCallStatus::Pending, 0);
        assert!(!icon.is_empty());
        assert_eq!(color, theme::RUST_ORANGE);
    }

    #[test]
    fn status_icon_in_progress() {
        let (icon, color) = status_icon(acp::ToolCallStatus::InProgress, 3);
        assert!(!icon.is_empty());
        assert_eq!(color, theme::RUST_ORANGE);
    }

    #[test]
    fn status_icon_completed() {
        let (icon, color) = status_icon(acp::ToolCallStatus::Completed, 0);
        assert_eq!(icon, theme::ICON_COMPLETED);
        assert_eq!(color, theme::RUST_ORANGE);
    }

    #[test]
    fn status_icon_failed() {
        let (icon, color) = status_icon(acp::ToolCallStatus::Failed, 0);
        assert_eq!(icon, theme::ICON_FAILED);
        assert_eq!(color, theme::STATUS_ERROR);
    }

    // status_icon

    #[test]
    fn status_icon_spinner_wraps() {
        let (icon_a, _) = status_icon(acp::ToolCallStatus::InProgress, 0);
        let (icon_b, _) = status_icon(acp::ToolCallStatus::InProgress, SPINNER_STRS.len());
        assert_eq!(icon_a, icon_b);
    }

    #[test]
    fn status_icon_all_spinner_frames_valid() {
        for i in 0..SPINNER_STRS.len() {
            let (icon, _) = status_icon(acp::ToolCallStatus::InProgress, i);
            assert!(!icon.is_empty());
        }
    }

    /// Spinner frames are all distinct.
    #[test]
    fn status_icon_spinner_frames_distinct() {
        let frames: Vec<&str> = (0..SPINNER_STRS.len())
            .map(|i| status_icon(acp::ToolCallStatus::InProgress, i).0)
            .collect();
        for i in 0..frames.len() {
            for j in (i + 1)..frames.len() {
                assert_ne!(frames[i], frames[j], "frames {i} and {j} are identical");
            }
        }
    }

    /// Large spinner frame number wraps correctly.
    #[test]
    fn status_icon_spinner_large_frame() {
        let (icon, _) = status_icon(acp::ToolCallStatus::Pending, 999_999);
        assert!(!icon.is_empty());
    }
}
