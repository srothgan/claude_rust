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

use crate::app::App;
use crate::ui::theme;
use agent_client_protocol::PermissionOptionKind;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// Border color for the permission dialog
const BORDER_COLOR: Color = Color::Rgb(100, 100, 100);

/// How many lines the dialog needs (excluding border top/bottom).
/// 1 blank + N options + 1 blank + 1 hint = N + 3
pub fn required_height(app: &App) -> u16 {
    match &app.permission_pending {
        Some(p) => {
            // border(1) + blank(1) + options + blank(1) + hint(1) + border(1)
            let options = p.request.options.len() as u16;
            options + 5
        }
        None => 0,
    }
}

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let pending = match &app.permission_pending {
        Some(p) => p,
        None => return,
    };

    let title = pending
        .request
        .tool_call
        .fields
        .title
        .as_deref()
        .unwrap_or("Permission Required");

    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BORDER_COLOR))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                title,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]));

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::default());

    for (i, opt) in pending.request.options.iter().enumerate() {
        let is_selected = i == pending.selected_index;
        let is_allow = matches!(
            opt.kind,
            PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways
        );

        let (icon, icon_color) = if is_allow {
            ("\u{2713}", Color::Green) // ✓
        } else {
            ("\u{2717}", Color::Red) // ✗
        };

        let mut spans = Vec::new();

        // Selection indicator
        if is_selected {
            spans.push(Span::styled(
                " \u{25b8} ",
                Style::default()
                    .fg(theme::RUST_ORANGE)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::raw("   "));
        }

        // Icon
        spans.push(Span::styled(
            format!("{icon} "),
            Style::default().fg(icon_color),
        ));

        // Option name
        let name_style = if is_selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(Span::styled(&opt.name, name_style));

        // Keyboard shortcut hint
        let shortcut = match opt.kind {
            PermissionOptionKind::AllowOnce => " (y)",
            PermissionOptionKind::AllowAlways => " (a)",
            PermissionOptionKind::RejectOnce => " (n)",
            PermissionOptionKind::RejectAlways => " (N)",
            _ => "",
        };
        spans.push(Span::styled(shortcut, Style::default().fg(theme::DIM)));

        lines.push(Line::from(spans));
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " \u{2191}\u{2193} select  enter confirm  esc reject",
        Style::default().fg(theme::DIM),
    )));

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}
