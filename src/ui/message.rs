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

use crate::app::{App, AppStatus, ChatMessage, MessageBlock, MessageRole, ToolCallInfo};
use crate::ui::theme;
use agent_client_protocol as acp;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};

const SPINNER_FRAMES: &[char] = &[
    '\u{280B}', '\u{2819}', '\u{2839}', '\u{2838}',
    '\u{283C}', '\u{2834}', '\u{2826}', '\u{2827}',
    '\u{2807}', '\u{280F}',
];

/// Render a single chat message into a `Text` block (lines of styled spans).
pub fn render_message(msg: &ChatMessage, app: &App) -> Text<'static> {
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
            for block in &msg.blocks {
                if let MessageBlock::Text(text) = block {
                    let rendered = tui_markdown::from_str(text);
                    for line in rendered.lines {
                        let owned_spans: Vec<Span<'static>> = line
                            .spans
                            .into_iter()
                            .map(|s| {
                                let style = s.style.bg(theme::USER_MSG_BG);
                                Span::styled(s.content.into_owned(), style)
                            })
                            .collect();
                        lines.push(Line::from(owned_spans).style(line.style));
                    }
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
            if msg.blocks.is_empty() && matches!(app.status, AppStatus::Thinking | AppStatus::Running(_)) {
                let ch = SPINNER_FRAMES[app.spinner_frame % SPINNER_FRAMES.len()];
                lines.push(Line::from(Span::styled(
                    format!("{ch} Thinking..."),
                    Style::default().fg(theme::DIM),
                )));
                lines.push(Line::default());
                return Text::from(lines);
            }

            // Render blocks in order with spacing at text<->tool transitions
            let mut prev_was_tool = false;
            for block in &msg.blocks {
                match block {
                    MessageBlock::Text(text) => {
                        // Add half-spacing when transitioning from tools back to text
                        if prev_was_tool {
                            lines.push(Line::default());
                        }
                        let rendered = tui_markdown::from_str(text);
                        for line in rendered.lines {
                            let owned_spans: Vec<Span<'static>> = line
                                .spans
                                .into_iter()
                                .map(|s| Span::styled(s.content.into_owned(), s.style))
                                .collect();
                            lines.push(Line::from(owned_spans).style(line.style));
                        }
                        prev_was_tool = false;
                    }
                    MessageBlock::ToolCall(tc) => {
                        // Add half-spacing when transitioning from text to tools
                        if !prev_was_tool && lines.len() > 1 {
                            lines.push(Line::default());
                        }
                        lines.push(render_tool_call(tc));
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
            for block in &msg.blocks {
                if let MessageBlock::Text(text) = block {
                    for text_line in text.lines() {
                        lines.push(Line::from(text_line.to_string()));
                    }
                }
            }
        }
    }

    // Blank separator between messages
    lines.push(Line::default());

    Text::from(lines)
}

fn render_tool_call(tc: &ToolCallInfo) -> Line<'static> {
    let (icon, icon_color) = match tc.status {
        acp::ToolCallStatus::Pending => (theme::ICON_PENDING, theme::DIM),
        acp::ToolCallStatus::InProgress => (theme::ICON_RUNNING, theme::RUST_ORANGE),
        acp::ToolCallStatus::Completed => (theme::ICON_COMPLETED, theme::RUST_ORANGE),
        acp::ToolCallStatus::Failed => (theme::ICON_FAILED, theme::STATUS_ERROR),
        _ => ("?", theme::DIM),
    };

    let (kind_icon, kind_name) = theme::tool_kind_label(tc.kind);

    let mut spans = vec![
        Span::styled(format!("  {icon} "), Style::default().fg(icon_color)),
        Span::styled(
            format!("{kind_icon} "),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{kind_name}  "),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    // Use tui_markdown to render the title — handles backticks, bold, etc.
    let rendered = tui_markdown::from_str(&tc.title);
    if let Some(first_line) = rendered.lines.into_iter().next() {
        for span in first_line.spans {
            spans.push(Span::styled(span.content.into_owned(), span.style));
        }
    }

    Line::from(spans)
}
