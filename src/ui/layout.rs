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

use ratatui::layout::{Constraint, Layout, Rect};

pub struct AppLayout {
    pub header: Rect,
    pub header_sep: Rect,
    pub body: Rect,
    pub input_sep: Rect,
    pub permission: Option<Rect>,
    pub input: Rect,
    pub footer: Option<Rect>,
}

pub fn compute(area: Rect, input_lines: u16, permission_height: u16, show_header: bool) -> AppLayout {
    let input_height = input_lines.max(1);
    let header_height: u16 = if show_header { 1 } else { 0 };
    let header_sep_height: u16 = if show_header { 1 } else { 0 };

    if area.height < 8 {
        // Ultra-compact: no header, no separator, no footer, no permission
        let [body, input] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(input_height),
        ])
        .areas(area);
        AppLayout {
            header: Rect::new(area.x, area.y, area.width, 0),
            header_sep: Rect::new(area.x, area.y, area.width, 0),
            body,
            input_sep: Rect::new(area.x, input.y, area.width, 0),
            permission: None,
            input,
            footer: None,
        }
    } else if permission_height > 0 {
        let [header, header_sep, body, input_sep, permission, input, _spacer, footer] = Layout::vertical([
            Constraint::Length(header_height),
            Constraint::Length(header_sep_height),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(permission_height),
            Constraint::Length(input_height),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);
        AppLayout {
            header,
            header_sep,
            body,
            input_sep,
            permission: Some(permission),
            input,
            footer: Some(footer),
        }
    } else {
        let [header, header_sep, body, input_sep, input, _spacer, footer] = Layout::vertical([
            Constraint::Length(header_height),
            Constraint::Length(header_sep_height),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(input_height),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);
        AppLayout {
            header,
            header_sep,
            body,
            input_sep,
            permission: None,
            input,
            footer: Some(footer),
        }
    }
}
