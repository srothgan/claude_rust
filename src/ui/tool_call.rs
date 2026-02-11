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

use crate::app::{InlinePermission, ToolCallInfo};
use crate::ui::diff::{is_markdown_file, lang_from_title, render_diff, strip_outer_code_fence};
use crate::ui::theme;
use agent_client_protocol::{self as acp, PermissionOptionKind};
use ansi_to_tui::IntoText as _;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Spinner frames as `&'static str` for use in `status_icon` return type.
const SPINNER_STRS: &[&str] = &[
    "\u{280B}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283C}", "\u{2834}", "\u{2826}", "\u{2827}",
    "\u{2807}", "\u{280F}",
];

/// Max visible output lines for Execute/Bash tool calls.
/// Total box height = 1 (title) + 1 (command) + this + 1 (bottom border) = 15.
/// TODO: make configurable (see ROADMAP.md)
const TERMINAL_MAX_LINES: usize = 12;

pub fn status_icon(status: acp::ToolCallStatus, spinner_frame: usize) -> (&'static str, Color) {
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

/// Render a tool call with caching. Only re-renders when cache is stale.
/// `InProgress` tool calls skip caching because the icon color pulses each frame.
pub fn render_tool_call_cached(
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

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

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
