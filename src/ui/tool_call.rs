// Claude Code Rust - A native Rust terminal interface for Claude Code
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

use crate::agent::model::{self as model, PermissionOptionKind};
use crate::app::{InlinePermission, ToolCallInfo};
use crate::ui::diff::{is_markdown_file, lang_from_title, render_diff, strip_outer_code_fence};
use crate::ui::markdown;
use crate::ui::theme;
use ansi_to_tui::IntoText as _;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Spinner frames as `&'static str` for use in `status_icon` return type.
const SPINNER_STRS: &[&str] = &[
    "\u{280B}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283C}", "\u{2834}", "\u{2826}", "\u{2827}",
    "\u{2807}", "\u{280F}",
];

/// Max visible output lines for Execute/Bash tool calls.
/// Total box height = 1 (title) + 1 (command) + this + 1 (bottom border) = 15.
/// TODO: make configurable (see ROADMAP.md)
const TERMINAL_MAX_LINES: usize = 12;

pub fn status_icon(status: model::ToolCallStatus, spinner_frame: usize) -> (&'static str, Color) {
    match status {
        model::ToolCallStatus::Pending => ("\u{25CB}", theme::RUST_ORANGE),
        model::ToolCallStatus::InProgress => {
            let s = SPINNER_STRS[spinner_frame % SPINNER_STRS.len()];
            (s, theme::RUST_ORANGE)
        }
        model::ToolCallStatus::Completed => (theme::ICON_COMPLETED, theme::RUST_ORANGE),
        model::ToolCallStatus::Failed => (theme::ICON_FAILED, theme::STATUS_ERROR),
    }
}

/// Render a tool call with caching. Only re-renders when cache is stale.
///
/// For Execute/Bash tool calls, the cache stores **content only** (command, output,
/// permissions) without border decoration. Borders are applied at render time using
/// the current width, so they always fill the terminal correctly after resize.
/// Height for Execute = `content_lines + 2` (title border + bottom border).
///
/// For other tool calls, in-progress calls split title (re-rendered each frame for
/// spinner) from body (cached). Completed calls cache title + body together.
pub fn render_tool_call_cached(
    tc: &mut ToolCallInfo,
    width: u16,
    spinner_frame: usize,
    out: &mut Vec<Line<'static>>,
) {
    let is_execute = tc.is_execute_tool();

    // Execute/Bash: two-layer rendering (cache content, apply borders at render time)
    if is_execute {
        // Ensure content is cached
        if tc.cache.get().is_none() {
            crate::perf::mark("tc::cache_miss_execute");
            let _t = crate::perf::start("tc::render_exec");
            let content = render_execute_content(tc);
            tc.cache.store(content);
        } else {
            crate::perf::mark("tc::cache_hit_execute");
        }
        // Apply borders at render time with current width
        if let Some(content) = tc.cache.get() {
            let bordered = render_execute_with_borders(tc, content, width, spinner_frame);
            out.extend(bordered);
        }
        return;
    }

    // Non-Execute tool calls: existing caching strategy
    let is_in_progress =
        matches!(tc.status, model::ToolCallStatus::InProgress | model::ToolCallStatus::Pending);

    // Completed/failed: full cache (title + body together)
    if !is_in_progress {
        if let Some(cached_lines) = tc.cache.get() {
            crate::perf::mark_with("tc::cache_hit", "lines", cached_lines.len());
            out.extend_from_slice(cached_lines);
            return;
        }
        crate::perf::mark("tc::cache_miss");
        let _t = crate::perf::start("tc::render");
        let fresh = render_tool_call(tc, width, spinner_frame);
        tc.cache.store(fresh);
        if let Some(stored) = tc.cache.get() {
            out.extend_from_slice(stored);
        }
        return;
    }

    // In-progress: re-render only the title line (spinner), cache the body.
    let fresh_title = render_tool_call_title(tc, width, spinner_frame);
    out.push(fresh_title);

    // Body: use cache if valid, otherwise render and cache.
    if let Some(cached_body) = tc.cache.get() {
        crate::perf::mark_with("tc::cache_hit_body", "lines", cached_body.len());
        out.extend_from_slice(cached_body);
    } else {
        crate::perf::mark("tc::cache_miss_body");
        let _t = crate::perf::start("tc::render_body");
        let body = render_tool_call_body(tc);
        tc.cache.store(body);
        if let Some(stored) = tc.cache.get() {
            out.extend_from_slice(stored);
        }
    }
}

/// Ensure tool call caches are up-to-date and return visual wrapped height at `width`.
/// Returns `(height, lines_wrapped_for_measurement)`.
pub fn measure_tool_call_height_cached(
    tc: &mut ToolCallInfo,
    width: u16,
    spinner_frame: usize,
    layout_generation: u64,
) -> (usize, usize) {
    if tc.cache_measurement_key_matches(width, layout_generation) {
        crate::perf::mark("tc_measure_fast_path_hits");
        return (tc.last_measured_height, 0);
    }
    crate::perf::mark("tc_measure_recompute_count");

    let is_execute = tc.is_execute_tool();
    if is_execute {
        if tc.cache.get().is_none() {
            let content = render_execute_content(tc);
            tc.cache.store(content);
        }
        if let Some(content) = tc.cache.get() {
            let bordered = render_execute_with_borders(tc, content, width, spinner_frame);
            let h = Paragraph::new(Text::from(bordered.clone()))
                .wrap(Wrap { trim: false })
                .line_count(width);
            tc.cache.set_height(h, width);
            tc.record_measured_height(width, h, layout_generation);
            return (h, bordered.len());
        }
        tc.record_measured_height(width, 0, layout_generation);
        return (0, 0);
    }

    let is_in_progress =
        matches!(tc.status, model::ToolCallStatus::InProgress | model::ToolCallStatus::Pending);

    if !is_in_progress {
        if let Some(h) = tc.cache.height_at(width) {
            tc.record_measured_height(width, h, layout_generation);
            return (h, 0);
        }
        if let Some(h) = tc.cache.measure_and_set_height(width) {
            tc.record_measured_height(width, h, layout_generation);
            return (h, tc.cache.get().map_or(0, Vec::len));
        }
        let fresh = render_tool_call(tc, width, spinner_frame);
        let h =
            Paragraph::new(Text::from(fresh.clone())).wrap(Wrap { trim: false }).line_count(width);
        tc.cache.store(fresh);
        tc.cache.set_height(h, width);
        tc.record_measured_height(width, h, layout_generation);
        return (h, tc.cache.get().map_or(0, Vec::len));
    }

    // In-progress non-execute: title is dynamic, body is cached separately.
    let title = render_tool_call_title(tc, width, spinner_frame);
    let title_h =
        Paragraph::new(Text::from(vec![title])).wrap(Wrap { trim: false }).line_count(width);

    if let Some(body_h) = tc.cache.height_at(width) {
        let total = title_h + body_h;
        tc.record_measured_height(width, total, layout_generation);
        return (total, 1);
    }
    if let Some(body_h) = tc.cache.measure_and_set_height(width) {
        let total = title_h + body_h;
        tc.record_measured_height(width, total, layout_generation);
        return (total, tc.cache.get().map_or(1, |b| b.len() + 1));
    }

    let body = render_tool_call_body(tc);
    let body_h =
        Paragraph::new(Text::from(body.clone())).wrap(Wrap { trim: false }).line_count(width);
    tc.cache.store(body);
    tc.cache.set_height(body_h, width);
    let total = title_h + body_h;
    tc.record_measured_height(width, total, layout_generation);
    (total, tc.cache.get().map_or(1, |b| b.len() + 1))
}

/// Render just the title line for a non-Execute tool call (the line containing the spinner icon).
/// Used for in-progress tool calls where only the spinner changes each frame.
/// Execute tool calls are handled separately via `render_execute_with_borders`.
fn render_tool_call_title(tc: &ToolCallInfo, _width: u16, spinner_frame: usize) -> Line<'static> {
    let (icon, icon_color) = status_icon(tc.status, spinner_frame);
    let (kind_icon, _kind_name) = theme::tool_name_label(&tc.sdk_tool_name);

    let mut title_spans = vec![
        Span::styled(format!("  {icon} "), Style::default().fg(icon_color)),
        Span::styled(
            format!("{kind_icon} "),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
    ];

    title_spans.extend(markdown_inline_spans(&tc.title));

    Line::from(title_spans)
}

/// Render the body lines (everything after the title) for a non-Execute tool call.
/// Used for in-progress tool calls where the body is cached separately from the title.
/// Execute tool calls are handled separately via `render_execute_with_borders`.
fn render_tool_call_body(tc: &ToolCallInfo) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    render_standard_body(tc, &mut lines);
    lines
}

/// Render a complete non-Execute tool call (title + body).
/// Execute tool calls are handled separately via `render_execute_with_borders`.
fn render_tool_call(tc: &ToolCallInfo, width: u16, spinner_frame: usize) -> Vec<Line<'static>> {
    let title = render_tool_call_title(tc, width, spinner_frame);
    let mut lines = vec![title];
    render_standard_body(tc, &mut lines);
    lines
}

/// Render the body (everything after the title line) of a standard (non-Execute) tool call.
fn render_standard_body(tc: &ToolCallInfo, lines: &mut Vec<Line<'static>>) {
    let pipe_style = Style::default().fg(theme::DIM);
    let has_permission = tc.pending_permission.is_some();

    // Diffs (Edit tool) are always shown -- user needs to see changes
    let has_diff = tc.content.iter().any(|c| matches!(c, model::ToolCallContent::Diff(_)));

    if tc.content.is_empty() && !has_permission {
        return;
    }

    // Force expanded when permission is pending (user needs to see context)
    // TODO(agent-sdk): force failed/error tool calls to single-line collapsed summary
    // even when content is large, to avoid long noisy failure blocks by default.
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
        // Expanded: render full content with | prefix on each line
        let mut content_lines = render_tool_content(tc);

        // Append inline permission controls if pending
        if let Some(ref perm) = tc.pending_permission {
            content_lines.extend(render_permission_lines(tc, perm));
        }

        let last_idx = content_lines.len().saturating_sub(1);
        for (i, content_line) in content_lines.into_iter().enumerate() {
            let prefix = if i == last_idx {
                "  \u{2514}\u{2500} " // corner
            } else {
                "  \u{2502}  " // pipe
            };
            let mut spans = vec![Span::styled(prefix.to_owned(), pipe_style)];
            spans.extend(content_line.spans);
            lines.push(Line::from(spans));
        }
    }
}

/// Render Execute/Bash content lines WITHOUT any border decoration.
/// This is width-independent and safe to cache across resizes.
/// Returns: command line + output lines + permission lines (no border prefixes).
fn render_execute_content(tc: &ToolCallInfo) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Command line (no border prefix)
    if let Some(ref cmd) = tc.terminal_command {
        lines.push(Line::from(vec![
            Span::styled(
                "$ ",
                Style::default().fg(theme::RUST_ORANGE).add_modifier(Modifier::BOLD),
            ),
            Span::styled(cmd.clone(), Style::default().fg(Color::Yellow)),
        ]));
    }

    // Output lines (capped, no border prefix)
    let mut body_lines: Vec<Line<'static>> = Vec::new();

    if let Some(ref output) = tc.terminal_output {
        if matches!(tc.status, model::ToolCallStatus::Failed)
            && let Some(first_line) = failed_execute_first_line(output)
        {
            body_lines.push(Line::from(Span::styled(
                first_line,
                Style::default().fg(theme::STATUS_ERROR),
            )));
        } else {
            let raw_lines: Vec<Line<'static>> = if let Ok(ansi_text) = output.as_bytes().into_text()
            {
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
        }
    } else if matches!(tc.status, model::ToolCallStatus::InProgress) {
        body_lines.push(Line::from(Span::styled("running...", Style::default().fg(theme::DIM))));
    }

    lines.extend(body_lines);

    // Inline permission controls (no border prefix)
    if let Some(ref perm) = tc.pending_permission {
        lines.extend(render_permission_lines(tc, perm));
    }

    lines
}

/// Apply Execute/Bash box borders around pre-rendered content lines.
/// This is called at render time with the current width, so borders always
/// fill the terminal correctly even after resize.
fn render_execute_with_borders(
    tc: &ToolCallInfo,
    content: &[Line<'static>],
    width: u16,
    spinner_frame: usize,
) -> Vec<Line<'static>> {
    let border = Style::default().fg(theme::DIM);
    let inner_w = (width as usize).saturating_sub(2);
    let mut out = Vec::with_capacity(content.len() + 2);

    // Top border with status icon and title
    let (status_icon_str, icon_color) = status_icon(tc.status, spinner_frame);
    let (_tool_icon, tool_label) = theme::tool_name_label(&tc.sdk_tool_name);
    let line_budget = width as usize;
    let left_prefix = vec![
        Span::styled("  \u{256D}\u{2500}", border),
        Span::styled(format!(" {status_icon_str} "), Style::default().fg(icon_color)),
        Span::styled(
            format!("{tool_label} "),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
    ];
    let prefix_w = spans_width(&left_prefix);
    let right_border_w = 1; // "╮"
    // Reserve at least one fill char so the border looks continuous.
    let title_max_w = line_budget.saturating_sub(prefix_w + right_border_w + 1);
    let title_spans = truncate_spans_to_width(markdown_inline_spans(&tc.title), title_max_w);
    let title_w = spans_width(&title_spans);
    let fill_w = line_budget.saturating_sub(prefix_w + title_w + right_border_w);
    let top_fill: String = "\u{2500}".repeat(fill_w);

    let mut top = left_prefix;
    top.extend(title_spans);
    top.push(Span::styled(format!("{top_fill}\u{256E}"), border));
    out.push(Line::from(top));

    // Content lines with left border prefix
    for line in content {
        let mut spans = vec![Span::styled("  \u{2502} ", border)];
        spans.extend(line.spans.iter().cloned());
        out.push(Line::from(spans));
    }

    // Bottom border
    let bottom_fill: String = "\u{2500}".repeat(inner_w.saturating_sub(2));
    out.push(Line::from(Span::styled(format!("  \u{2570}{bottom_fill}\u{256F}"), border)));

    out
}

/// Render inline permission options on a single compact line.
/// Options are dynamic and include shortcuts only when applicable.
/// Unfocused permissions are dimmed to indicate they don't have keyboard input.
fn render_permission_lines(tc: &ToolCallInfo, perm: &InlinePermission) -> Vec<Line<'static>> {
    if is_question_permission(perm, tc) {
        return render_question_permission_lines(tc, perm);
    }

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
        let is_allow = matches!(
            opt.kind,
            PermissionOptionKind::AllowOnce
                | PermissionOptionKind::AllowSession
                | PermissionOptionKind::AllowAlways
        );

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
        let mut name_spans = markdown_inline_spans(&opt.name);
        if name_spans.is_empty() {
            spans.push(Span::styled(opt.name.clone(), name_style));
        } else {
            for span in &mut name_spans {
                span.style = span.style.patch(name_style);
            }
            spans.extend(name_spans);
        }

        let shortcut = match opt.kind {
            PermissionOptionKind::AllowOnce => " (Ctrl+y)",
            PermissionOptionKind::AllowSession | PermissionOptionKind::AllowAlways => " (Ctrl+a)",
            PermissionOptionKind::RejectOnce => " (Ctrl+n)",
            PermissionOptionKind::RejectAlways | PermissionOptionKind::QuestionChoice => "",
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

fn is_question_permission(perm: &InlinePermission, tc: &ToolCallInfo) -> bool {
    tc.is_ask_question_tool()
        || perm.options.iter().all(|opt| matches!(opt.kind, PermissionOptionKind::QuestionChoice))
}

#[derive(Default)]
struct AskQuestionMeta {
    header: Option<String>,
    question: Option<String>,
    question_index: Option<usize>,
    total_questions: Option<usize>,
}

fn parse_ask_question_meta(raw_input: Option<&serde_json::Value>) -> AskQuestionMeta {
    let Some(raw) = raw_input else {
        return AskQuestionMeta::default();
    };

    let question = raw
        .get("questions")
        .and_then(serde_json::Value::as_array)
        .and_then(|items| items.first())
        .and_then(serde_json::Value::as_object);

    AskQuestionMeta {
        header: question
            .and_then(|q| q.get("header"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        question: question
            .and_then(|q| q.get("question"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        question_index: raw
            .get("question_index")
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| usize::try_from(n).ok()),
        total_questions: raw
            .get("total_questions")
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| usize::try_from(n).ok()),
    }
}

fn render_question_permission_lines(
    tc: &ToolCallInfo,
    perm: &InlinePermission,
) -> Vec<Line<'static>> {
    let meta = parse_ask_question_meta(tc.raw_input.as_ref());
    let header = meta.header.unwrap_or_else(|| "Question".to_owned());
    let question_text = meta.question.unwrap_or_else(|| tc.title.clone());
    let progress = match (meta.question_index, meta.total_questions) {
        (Some(index), Some(total)) if total > 0 => format!(" ({}/{total})", index + 1),
        _ => String::new(),
    };

    let mut lines = vec![
        Line::default(),
        Line::from(vec![
            Span::styled("  ? ", Style::default().fg(theme::RUST_ORANGE)),
            Span::styled(
                format!("{header}{progress}"),
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
        ]),
    ];

    for row in question_text.lines() {
        lines.push(Line::from(vec![Span::styled(
            format!("    {row}"),
            Style::default().fg(Color::Gray),
        )]));
    }

    if !perm.focused {
        lines.push(Line::from(Span::styled(
            "  waiting for input... (Up/Down to focus)",
            Style::default().fg(theme::DIM),
        )));
        return lines;
    }

    let horizontal = perm.options.len() <= 3
        && perm.options.iter().all(|opt| {
            opt.description.as_deref().is_none_or(str::is_empty) && opt.name.chars().count() <= 20
        });

    if horizontal {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (i, opt) in perm.options.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled("  |  ", Style::default().fg(theme::DIM)));
            }
            let selected = i == perm.selected_index;
            if selected {
                spans.push(Span::styled(
                    "▸ ",
                    Style::default().fg(theme::RUST_ORANGE).add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled("  ", Style::default().fg(theme::DIM)));
            }
            let style = if selected {
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            spans.push(Span::styled(opt.name.clone(), style));
        }
        lines.push(Line::from(spans));
    } else {
        for (i, opt) in perm.options.iter().enumerate() {
            let selected = i == perm.selected_index;
            let bullet = if selected { "  ▸ " } else { "  ○ " };
            let name_style = if selected {
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            lines.push(Line::from(vec![
                Span::styled(
                    bullet,
                    if selected {
                        Style::default().fg(theme::RUST_ORANGE)
                    } else {
                        Style::default().fg(theme::DIM)
                    },
                ),
                Span::styled(opt.name.clone(), name_style),
            ]));
            if let Some(desc) = opt.description.as_ref().map(|d| d.trim()).filter(|d| !d.is_empty())
            {
                lines.push(Line::from(Span::styled(
                    format!("      {desc}"),
                    Style::default().fg(theme::DIM),
                )));
            }
        }
    }

    lines.push(Line::from(Span::styled(
        "  Left/Right or Up/Down select  Enter confirm  Esc cancel",
        Style::default().fg(theme::DIM),
    )));
    lines
}

fn markdown_inline_spans(input: &str) -> Vec<Span<'static>> {
    markdown::render_markdown_safe(input, None).into_iter().next().map_or_else(Vec::new, |line| {
        line.spans.into_iter().map(|s| Span::styled(s.content.into_owned(), s.style)).collect()
    })
}

fn spans_width(spans: &[Span<'static>]) -> usize {
    spans.iter().map(|s| UnicodeWidthStr::width(s.content.as_ref())).sum()
}

fn truncate_spans_to_width(spans: Vec<Span<'static>>, max_width: usize) -> Vec<Span<'static>> {
    if max_width == 0 {
        return Vec::new();
    }
    if spans_width(&spans) <= max_width {
        return spans;
    }

    let keep_width = max_width.saturating_sub(1);
    let mut used = 0usize;
    let mut out: Vec<Span<'static>> = Vec::new();

    for span in spans {
        if used >= keep_width {
            break;
        }
        let mut chunk = String::new();
        for ch in span.content.chars() {
            let w = UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + w > keep_width {
                break;
            }
            chunk.push(ch);
            used += w;
        }
        if !chunk.is_empty() {
            out.push(Span::styled(chunk, span.style));
        }
    }
    out.push(Span::styled("\u{2026}", Style::default().fg(theme::DIM)));
    out
}

/// One-line summary for collapsed tool calls.
fn content_summary(tc: &ToolCallInfo) -> String {
    // For Execute tool calls, show last non-empty line of terminal output
    if tc.terminal_id.is_some() {
        if let Some(ref output) = tc.terminal_output {
            if matches!(tc.status, model::ToolCallStatus::Failed)
                && let Some(first_line) = failed_execute_first_line(output)
            {
                return if first_line.chars().count() > 80 {
                    let truncated: String = first_line.chars().take(77).collect();
                    format!("{truncated}...")
                } else {
                    first_line
                };
            }
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
        return if matches!(tc.status, model::ToolCallStatus::InProgress) {
            "running...".to_owned()
        } else {
            String::new()
        };
    }

    for content in &tc.content {
        match content {
            model::ToolCallContent::Diff(diff) => {
                let name = diff.path.file_name().map_or_else(
                    || diff.path.to_string_lossy().into_owned(),
                    |f| f.to_string_lossy().into_owned(),
                );
                return name;
            }
            model::ToolCallContent::Content(c) => {
                if let model::ContentBlock::Text(text) = &c.content {
                    let stripped = strip_outer_code_fence(&text.text);
                    if matches!(tc.status, model::ToolCallStatus::Failed)
                        && let Some(msg) = extract_tool_use_error_message(&stripped)
                    {
                        return msg;
                    }
                    let first = stripped.lines().next().unwrap_or("");
                    return if first.chars().count() > 60 {
                        let truncated: String = first.chars().take(57).collect();
                        format!("{truncated}...")
                    } else {
                        first.to_owned()
                    };
                }
            }
            model::ToolCallContent::Terminal(_) => {}
        }
    }
    String::new()
}

/// Render the full content of a tool call as lines.
fn render_tool_content(tc: &ToolCallInfo) -> Vec<Line<'static>> {
    let is_execute = tc.is_execute_tool();
    let mut lines: Vec<Line<'static>> = Vec::new();

    // For Execute tool calls with terminal output, render the live output
    if is_execute {
        if let Some(ref output) = tc.terminal_output {
            if matches!(tc.status, model::ToolCallStatus::Failed)
                && let Some(first_line) = failed_execute_first_line(output)
            {
                lines.push(Line::from(Span::styled(
                    first_line,
                    Style::default().fg(theme::STATUS_ERROR),
                )));
            } else if let Ok(ansi_text) = output.as_bytes().into_text() {
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
        } else if matches!(tc.status, model::ToolCallStatus::InProgress) {
            lines.push(Line::from(Span::styled("running...", Style::default().fg(theme::DIM))));
        }
        debug_failed_tool_render(tc);
        return lines;
    }

    for content in &tc.content {
        match content {
            model::ToolCallContent::Diff(diff) => {
                lines.extend(render_diff(diff));
            }
            model::ToolCallContent::Content(c) => {
                if let model::ContentBlock::Text(text) = &c.content {
                    let stripped = strip_outer_code_fence(&text.text);
                    if matches!(tc.status, model::ToolCallStatus::Failed)
                        && let Some(msg) = extract_tool_use_error_message(&stripped)
                    {
                        lines.extend(render_tool_use_error_content(&msg));
                        continue;
                    }
                    if matches!(tc.status, model::ToolCallStatus::Failed)
                        && looks_like_internal_error(&stripped)
                    {
                        lines.extend(render_internal_failure_content(&stripped));
                        continue;
                    }
                    let md_source = if is_markdown_file(&tc.title) {
                        stripped
                    } else {
                        let lang = lang_from_title(&tc.title);
                        format!("```{lang}\n{stripped}\n```")
                    };
                    for line in markdown::render_markdown_safe(&md_source, None) {
                        let owned: Vec<Span<'static>> = line
                            .spans
                            .into_iter()
                            .map(|s| Span::styled(s.content.into_owned(), s.style))
                            .collect();
                        lines.push(Line::from(owned));
                    }
                }
            }
            model::ToolCallContent::Terminal(_) => {}
        }
    }

    debug_failed_tool_render(tc);
    lines
}

fn failed_execute_first_line(output: &str) -> Option<String> {
    if let Some(msg) = extract_tool_use_error_message(output) {
        return Some(msg);
    }
    output.lines().find(|line| !line.trim().is_empty()).map(str::trim).map(str::to_owned)
}

fn render_internal_failure_content(payload: &str) -> Vec<Line<'static>> {
    let summary = summarize_internal_error(payload);
    let mut lines = vec![Line::from(Span::styled(
        "Internal bridge/adapter error",
        Style::default().fg(theme::STATUS_ERROR).add_modifier(Modifier::BOLD),
    ))];
    if !summary.is_empty() {
        lines.push(Line::from(Span::styled(summary, Style::default().fg(theme::STATUS_ERROR))));
    }
    lines
}

fn render_tool_use_error_content(message: &str) -> Vec<Line<'static>> {
    message
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            Line::from(Span::styled(line.to_owned(), Style::default().fg(theme::STATUS_ERROR)))
        })
        .collect()
}

fn debug_failed_tool_render(tc: &ToolCallInfo) {
    if !matches!(tc.status, model::ToolCallStatus::Failed) {
        return;
    }

    let Some(text_payload) = tc.content.iter().find_map(|content| match content {
        model::ToolCallContent::Content(c) => match &c.content {
            model::ContentBlock::Text(t) => Some(t.text.as_str().to_owned()),
            model::ContentBlock::Image(_) => None,
        },
        _ => None,
    }) else {
        // Skip generic command failures that only have terminal stderr/stdout.
        // We want bridge/adapter-style structured error payloads here.
        return;
    };
    if !looks_like_internal_error(&text_payload) {
        return;
    }
    let text_preview = summarize_internal_error(&text_payload);

    let terminal_preview = tc
        .terminal_output
        .as_deref()
        .map_or_else(|| "<no terminal output>".to_owned(), preview_for_log);

    tracing::debug!(
        tool_call_id = %tc.id,
        title = %tc.title,
        sdk_tool_name = %tc.sdk_tool_name,
        content_blocks = tc.content.len(),
        text_preview = %text_preview,
        terminal_preview = %terminal_preview,
        "Failed tool call render payload"
    );
}

fn preview_for_log(input: &str) -> String {
    const LIMIT: usize = 240;
    let mut out = String::new();
    for (i, ch) in input.chars().enumerate() {
        if i >= LIMIT {
            out.push_str("...");
            break;
        }
        out.push(ch);
    }
    out.replace('\n', "\\n")
}

fn looks_like_internal_error(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    has_internal_error_keywords(&lower)
        || looks_like_json_rpc_error_shape(&lower)
        || looks_like_xml_error_shape(&lower)
}

fn has_internal_error_keywords(lower: &str) -> bool {
    [
        "internal error",
        "adapter",
        "bridge",
        "json-rpc",
        "rpc",
        "protocol error",
        "transport",
        "handshake failed",
        "session creation failed",
        "connection closed",
        "event channel closed",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn looks_like_json_rpc_error_shape(lower: &str) -> bool {
    (lower.contains("\"jsonrpc\"") && lower.contains("\"error\""))
        || lower.contains("\"code\":-32603")
        || lower.contains("\"code\": -32603")
}

fn looks_like_xml_error_shape(lower: &str) -> bool {
    let has_error_node = lower.contains("<error") || lower.contains("<fault");
    let has_detail_node = lower.contains("<message>") || lower.contains("<code>");
    has_error_node && has_detail_node
}

fn extract_tool_use_error_message(input: &str) -> Option<String> {
    extract_xml_tag_value(input, "tool_use_error")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn summarize_internal_error(input: &str) -> String {
    if let Some(msg) = extract_xml_tag_value(input, "message") {
        return preview_for_log(msg);
    }
    if let Some(msg) = extract_json_string_field(input, "message") {
        return preview_for_log(&msg);
    }
    let fallback = input.lines().find(|line| !line.trim().is_empty()).unwrap_or(input);
    preview_for_log(fallback.trim())
}

fn extract_xml_tag_value<'a>(input: &'a str, tag: &str) -> Option<&'a str> {
    let lower = input.to_ascii_lowercase();
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = lower.find(&open)? + open.len();
    let end = start + lower[start..].find(&close)?;
    let value = input[start..end].trim();
    (!value.is_empty()).then_some(value)
}

fn extract_json_string_field(input: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\"");
    let start = input.find(&needle)? + needle.len();
    let rest = input[start..].trim_start();
    let colon_idx = rest.find(':')?;
    let mut chars = rest[colon_idx + 1..].trim_start().chars();
    if chars.next()? != '"' {
        return None;
    }

    let mut escaped = false;
    let mut out = String::new();
    for ch in chars {
        if escaped {
            let mapped = match ch {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                '"' => '"',
                '\\' => '\\',
                _ => ch,
            };
            out.push(mapped);
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some(out),
            _ => out.push(ch),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::BlockCache;
    use pretty_assertions::assert_eq;

    fn test_tool_call(
        id: &str,
        sdk_tool_name: &str,
        status: model::ToolCallStatus,
    ) -> ToolCallInfo {
        ToolCallInfo {
            id: id.to_owned(),
            title: id.to_owned(),
            sdk_tool_name: sdk_tool_name.to_owned(),
            raw_input: None,
            status,
            content: Vec::new(),
            collapsed: false,
            hidden: false,
            terminal_id: None,
            terminal_command: None,
            terminal_output: None,
            terminal_output_len: 0,
            terminal_bytes_seen: 0,
            terminal_snapshot_mode: crate::app::TerminalSnapshotMode::AppendOnly,
            render_epoch: 0,
            layout_epoch: 0,
            last_measured_width: 0,
            last_measured_height: 0,
            last_measured_layout_epoch: 0,
            last_measured_layout_generation: 0,
            cache: BlockCache::default(),
            pending_permission: None,
        }
    }

    // status_icon

    #[test]
    fn status_icon_pending() {
        let (icon, color) = status_icon(model::ToolCallStatus::Pending, 0);
        assert!(!icon.is_empty());
        assert_eq!(color, theme::RUST_ORANGE);
    }

    #[test]
    fn status_icon_in_progress() {
        let (icon, color) = status_icon(model::ToolCallStatus::InProgress, 3);
        assert!(!icon.is_empty());
        assert_eq!(color, theme::RUST_ORANGE);
    }

    #[test]
    fn status_icon_completed() {
        let (icon, color) = status_icon(model::ToolCallStatus::Completed, 0);
        assert_eq!(icon, theme::ICON_COMPLETED);
        assert_eq!(color, theme::RUST_ORANGE);
    }

    #[test]
    fn status_icon_failed() {
        let (icon, color) = status_icon(model::ToolCallStatus::Failed, 0);
        assert_eq!(icon, theme::ICON_FAILED);
        assert_eq!(color, theme::STATUS_ERROR);
    }

    #[test]
    fn status_icon_spinner_wraps() {
        let (icon_a, _) = status_icon(model::ToolCallStatus::InProgress, 0);
        let (icon_b, _) = status_icon(model::ToolCallStatus::InProgress, SPINNER_STRS.len());
        assert_eq!(icon_a, icon_b);
    }

    #[test]
    fn status_icon_all_spinner_frames_valid() {
        for i in 0..SPINNER_STRS.len() {
            let (icon, _) = status_icon(model::ToolCallStatus::InProgress, i);
            assert!(!icon.is_empty());
        }
    }

    /// Spinner frames are all distinct.
    #[test]
    fn status_icon_spinner_frames_distinct() {
        let frames: Vec<&str> = (0..SPINNER_STRS.len())
            .map(|i| status_icon(model::ToolCallStatus::InProgress, i).0)
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
        let (icon, _) = status_icon(model::ToolCallStatus::Pending, 999_999);
        assert!(!icon.is_empty());
    }

    #[test]
    fn truncate_spans_adds_ellipsis_when_needed() {
        let spans = vec![Span::raw("abcdefghijklmnopqrstuvwxyz")];
        let out = truncate_spans_to_width(spans, 8);
        let rendered: String = out.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(rendered, "abcdefg\u{2026}");
        assert!(spans_width(&out) <= 8);
    }

    #[test]
    fn markdown_inline_spans_removes_markdown_syntax() {
        let spans = markdown_inline_spans("**Allow** _once_");
        let rendered: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(rendered.contains("Allow"));
        assert!(rendered.contains("once"));
        assert!(!rendered.contains('*'));
        assert!(!rendered.contains('_'));
    }

    #[test]
    fn execute_top_border_does_not_wrap_for_long_title() {
        let tc = ToolCallInfo {
            id: "tc-1".into(),
            title: "echo very long command title with markdown **bold** and path /a/b/c/d/e/f"
                .into(),
            sdk_tool_name: "Bash".into(),
            raw_input: None,
            status: model::ToolCallStatus::Pending,
            content: Vec::new(),
            collapsed: false,
            hidden: false,
            terminal_id: None,
            terminal_command: None,
            terminal_output: None,
            terminal_output_len: 0,
            terminal_bytes_seen: 0,
            terminal_snapshot_mode: crate::app::TerminalSnapshotMode::AppendOnly,
            render_epoch: 0,
            layout_epoch: 0,
            last_measured_width: 0,
            last_measured_height: 0,
            last_measured_layout_epoch: 0,
            last_measured_layout_generation: 0,
            cache: BlockCache::default(),
            pending_permission: None,
        };

        let rendered = render_execute_with_borders(&tc, &[], 80, 0);
        let top = rendered.first().expect("top border line");
        assert!(spans_width(&top.spans) <= 80);
    }

    #[test]
    fn execute_measure_fast_path_reuses_cached_height() {
        let mut tc = test_tool_call("tc-fast", "Bash", model::ToolCallStatus::InProgress);
        tc.terminal_command = Some("echo hi".to_owned());
        tc.terminal_output = Some("hello\nworld".to_owned());

        let (h1, lines1) = measure_tool_call_height_cached(&mut tc, 80, 0, 1);
        assert!(h1 > 0);
        assert!(lines1 > 0);

        let (h2, lines2) = measure_tool_call_height_cached(&mut tc, 80, 4, 1);
        assert_eq!(h2, h1);
        assert_eq!(lines2, 0);
    }

    #[test]
    fn execute_measure_recomputes_on_layout_generation_change() {
        let mut tc = test_tool_call("tc-layout-gen", "Bash", model::ToolCallStatus::InProgress);
        tc.terminal_command = Some("echo hi".to_owned());
        tc.terminal_output = Some("hello".to_owned());

        let (_, first_lines) = measure_tool_call_height_cached(&mut tc, 80, 0, 1);
        assert!(first_lines > 0);
        let (_, second_lines) = measure_tool_call_height_cached(&mut tc, 80, 0, 2);
        assert!(second_lines > 0);
    }

    #[test]
    fn layout_dirty_invalidates_measure_fast_path() {
        let mut tc = test_tool_call("tc-dirty", "Read", model::ToolCallStatus::Completed);
        tc.content = vec![model::ToolCallContent::from("one line")];

        let (_, first_lines) = measure_tool_call_height_cached(&mut tc, 80, 0, 1);
        assert!(first_lines > 0);
        let (_, fast_lines) = measure_tool_call_height_cached(&mut tc, 80, 0, 1);
        assert_eq!(fast_lines, 0);

        tc.mark_tool_call_layout_dirty();
        let (_, recompute_lines) = measure_tool_call_height_cached(&mut tc, 80, 0, 1);
        assert!(recompute_lines > 0);
    }

    #[test]
    fn internal_error_detection_accepts_xml_payload() {
        let payload =
            "<error><code>-32603</code><message>Adapter process crashed</message></error>";
        assert!(looks_like_internal_error(payload));
    }

    #[test]
    fn internal_error_detection_rejects_plain_bash_failure() {
        let payload = "bash: unknown_command: command not found";
        assert!(!looks_like_internal_error(payload));
    }

    #[test]
    fn summarize_internal_error_prefers_xml_message() {
        let payload =
            "<error><code>-32603</code><message>Adapter process crashed</message></error>";
        assert_eq!(summarize_internal_error(payload), "Adapter process crashed");
    }

    #[test]
    fn summarize_internal_error_reads_json_rpc_message() {
        let payload = r#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"internal rpc fault"}}"#;
        assert_eq!(summarize_internal_error(payload), "internal rpc fault");
    }

    #[test]
    fn extract_tool_use_error_message_reads_inner_text() {
        let payload = "<tool_use_error>Sibling tool call errored</tool_use_error>";
        assert_eq!(
            extract_tool_use_error_message(payload).as_deref(),
            Some("Sibling tool call errored")
        );
    }

    #[test]
    fn render_tool_use_error_content_shows_only_inner_text_lines() {
        let lines = render_tool_use_error_content("Line A\nLine B");
        let rendered: Vec<String> = lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(rendered, vec!["Line A", "Line B"]);
    }

    #[test]
    fn content_summary_only_extracts_tool_use_error_for_failed_execute() {
        let tc = ToolCallInfo {
            id: "tc-1".into(),
            title: "Bash".into(),
            sdk_tool_name: "Bash".into(),
            raw_input: None,
            status: model::ToolCallStatus::Completed,
            content: Vec::new(),
            collapsed: true,
            hidden: false,
            terminal_id: Some("term-1".into()),
            terminal_command: Some("echo done".into()),
            terminal_output: Some("<tool_use_error>bad</tool_use_error>\ndone".into()),
            terminal_output_len: 0,
            terminal_bytes_seen: 0,
            terminal_snapshot_mode: crate::app::TerminalSnapshotMode::AppendOnly,
            render_epoch: 0,
            layout_epoch: 0,
            last_measured_width: 0,
            last_measured_height: 0,
            last_measured_layout_epoch: 0,
            last_measured_layout_generation: 0,
            cache: BlockCache::default(),
            pending_permission: None,
        };
        assert_eq!(content_summary(&tc), "done");
    }

    #[test]
    fn content_summary_extracts_tool_use_error_for_failed_execute() {
        let tc = ToolCallInfo {
            id: "tc-1".into(),
            title: "Bash".into(),
            sdk_tool_name: "Bash".into(),
            raw_input: None,
            status: model::ToolCallStatus::Failed,
            content: Vec::new(),
            collapsed: true,
            hidden: false,
            terminal_id: Some("term-1".into()),
            terminal_command: Some("echo done".into()),
            terminal_output: Some("<tool_use_error>bad</tool_use_error>\ndone".into()),
            terminal_output_len: 0,
            terminal_bytes_seen: 0,
            terminal_snapshot_mode: crate::app::TerminalSnapshotMode::AppendOnly,
            render_epoch: 0,
            layout_epoch: 0,
            last_measured_width: 0,
            last_measured_height: 0,
            last_measured_layout_epoch: 0,
            last_measured_layout_generation: 0,
            cache: BlockCache::default(),
            pending_permission: None,
        };
        assert_eq!(content_summary(&tc), "bad");
    }

    #[test]
    fn content_summary_uses_first_terminal_line_for_failed_execute() {
        let tc = ToolCallInfo {
            id: "tc-2".into(),
            title: "Bash".into(),
            sdk_tool_name: "Bash".into(),
            raw_input: None,
            status: model::ToolCallStatus::Failed,
            content: Vec::new(),
            collapsed: true,
            hidden: false,
            terminal_id: Some("term-2".into()),
            terminal_command: Some("cd path with spaces".into()),
            terminal_output: Some(
                "Exit code 1\n/usr/bin/bash: line 1: cd: too many arguments\nmore detail".into(),
            ),
            terminal_output_len: 0,
            terminal_bytes_seen: 0,
            terminal_snapshot_mode: crate::app::TerminalSnapshotMode::AppendOnly,
            render_epoch: 0,
            layout_epoch: 0,
            last_measured_width: 0,
            last_measured_height: 0,
            last_measured_layout_epoch: 0,
            last_measured_layout_generation: 0,
            cache: BlockCache::default(),
            pending_permission: None,
        };
        assert_eq!(content_summary(&tc), "Exit code 1");
    }

    #[test]
    fn render_execute_content_failed_keeps_single_output_line() {
        let tc = ToolCallInfo {
            id: "tc-3".into(),
            title: "Bash".into(),
            sdk_tool_name: "Bash".into(),
            raw_input: None,
            status: model::ToolCallStatus::Failed,
            content: Vec::new(),
            collapsed: false,
            hidden: false,
            terminal_id: Some("term-3".into()),
            terminal_command: Some("cd path with spaces".into()),
            terminal_output: Some(
                "Exit code 1\n/usr/bin/bash: line 1: cd: too many arguments\nmore detail".into(),
            ),
            terminal_output_len: 0,
            terminal_bytes_seen: 0,
            terminal_snapshot_mode: crate::app::TerminalSnapshotMode::AppendOnly,
            render_epoch: 0,
            layout_epoch: 0,
            last_measured_width: 0,
            last_measured_height: 0,
            last_measured_layout_epoch: 0,
            last_measured_layout_generation: 0,
            cache: BlockCache::default(),
            pending_permission: None,
        };

        let lines = render_execute_content(&tc);
        let rendered: Vec<String> = lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(rendered.len(), 2);
        assert_eq!(rendered[1], "Exit code 1");
    }
}
