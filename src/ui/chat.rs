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
use crate::ui::message;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Text;
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let mut all_lines = Vec::new();
    for msg in &app.messages {
        let rendered = message::render_message(msg, app);
        for line in rendered.lines {
            all_lines.push(line);
        }
    }

    let content = Text::from(all_lines);
    let content_height = content.height() as u16;
    let viewport_height = area.height;

    if content_height >= viewport_height {
        // Content fills or exceeds viewport — use scroll
        if app.auto_scroll {
            app.scroll_offset = content_height.saturating_sub(viewport_height);
        }

        let paragraph = Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .scroll((app.scroll_offset, 0));

        frame.render_widget(paragraph, area);
    } else {
        // Content shorter than viewport — bottom-align via layout spacer
        let [_spacer, content_area] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(content_height),
        ])
        .areas(area);

        let paragraph = Paragraph::new(content).wrap(Wrap { trim: false });
        frame.render_widget(paragraph, content_area);
    }
}
