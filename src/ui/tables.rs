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

use super::markdown;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Cell, Row, Table, Widget};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub struct TableBlock {
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

enum MarkdownBlock {
    Text(String),
    Table(TableBlock),
}

pub fn render_markdown_with_tables(
    text: &str,
    width: u16,
    bg: Option<Color>,
) -> Vec<Line<'static>> {
    let blocks = split_markdown_tables(text);
    let mut out: Vec<Line<'static>> = Vec::new();
    for block in blocks {
        match block {
            MarkdownBlock::Text(chunk) => {
                if chunk.trim().is_empty() {
                    continue;
                }
                out.extend(markdown::render_markdown_safe(&chunk, bg));
            }
            MarkdownBlock::Table(table) => {
                if !out.is_empty() {
                    out.push(Line::default());
                }
                out.extend(render_table_lines(&table, width, bg));
                out.push(Line::default());
            }
        }
    }
    out
}

fn split_markdown_tables(text: &str) -> Vec<MarkdownBlock> {
    let mut blocks: Vec<MarkdownBlock> = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0usize;
    let mut current_text = String::new();

    while i < lines.len() {
        let line = lines[i];
        let next = lines.get(i + 1).copied().unwrap_or("");
        if looks_like_table_header(line) && looks_like_table_separator(next) {
            if !current_text.is_empty() {
                blocks.push(MarkdownBlock::Text(current_text.clone()));
                current_text.clear();
            }
            let header = parse_table_row(line);
            i += 2; // skip header + separator
            let mut rows: Vec<Vec<String>> = Vec::new();
            while i < lines.len() {
                let row_line = lines[i];
                if row_line.trim().is_empty() || !row_line.contains('|') {
                    break;
                }
                rows.push(parse_table_row(row_line));
                i += 1;
            }
            blocks.push(MarkdownBlock::Table(TableBlock { header, rows }));
            continue;
        }

        current_text.push_str(line);
        current_text.push('\n');
        i += 1;
    }

    if !current_text.is_empty() {
        blocks.push(MarkdownBlock::Text(current_text));
    }

    blocks
}

fn looks_like_table_header(line: &str) -> bool {
    line.contains('|') && !line.trim().is_empty()
}

fn looks_like_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return false;
    }
    let mut has_dash = false;
    for ch in trimmed.chars() {
        match ch {
            '-' => has_dash = true,
            '|' | ':' | ' ' | '\t' => {}
            _ => return false,
        }
    }
    has_dash
}

fn parse_table_row(line: &str) -> Vec<String> {
    let mut parts: Vec<&str> = line.split('|').collect();
    if parts.first().is_some_and(|s| s.trim().is_empty()) {
        parts.remove(0);
    }
    if parts.last().is_some_and(|s| s.trim().is_empty()) {
        parts.pop();
    }
    parts.into_iter().map(|s| s.trim().to_owned()).collect()
}

#[allow(clippy::cast_possible_truncation, clippy::similar_names)]
fn render_table_lines(table: &TableBlock, width: u16, bg: Option<Color>) -> Vec<Line<'static>> {
    let cols =
        std::cmp::max(table.header.len(), table.rows.iter().map(Vec::len).max().unwrap_or(0));
    if cols == 0 || width == 0 {
        return Vec::new();
    }

    let inner_width = width as usize;
    if inner_width == 0 {
        return Vec::new();
    }

    let mut widths = vec![1usize; cols];
    for (i, cell) in table.header.iter().enumerate() {
        widths[i] = widths[i].max(UnicodeWidthStr::width(cell.as_str()));
    }
    for row in &table.rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(UnicodeWidthStr::width(cell.as_str()));
        }
    }

    let available = inner_width;
    let spacing = 3usize;
    let mut total = widths.iter().sum::<usize>() + spacing.saturating_mul(cols.saturating_sub(1));
    while total > available {
        if let Some((idx, _)) =
            widths.iter().enumerate().filter(|(_, w)| **w > 1).max_by_key(|(_, w)| *w)
        {
            widths[idx] -= 1;
            total = widths.iter().sum::<usize>() + spacing.saturating_mul(cols.saturating_sub(1));
        } else {
            break;
        }
    }

    let mut header_cells: Vec<Cell<'static>> = Vec::with_capacity(cols);
    let mut header_height = 1u16;
    let mut header_style = Style::default().add_modifier(Modifier::BOLD);
    if let Some(bg_color) = bg {
        header_style = header_style.bg(bg_color);
    }
    for (i, width) in widths.iter().enumerate().take(cols) {
        let text = table.header.get(i).map_or("", String::as_str);
        let lines = wrap_inline_markdown(text, *width, header_style);
        header_height = header_height.max(lines.len() as u16);
        header_cells.push(Cell::from(Text::from(lines)));
    }
    let header = Row::new(header_cells).height(header_height).style(Style::default());

    let mut rows: Vec<Row<'static>> = Vec::with_capacity(table.rows.len());
    let mut rows_height = 0u16;
    for row in &table.rows {
        let mut cells: Vec<Cell<'static>> = Vec::with_capacity(cols);
        let mut row_style = Style::default();
        if let Some(bg_color) = bg {
            row_style = row_style.bg(bg_color);
        }
        let mut row_height = 1u16;
        for (i, width) in widths.iter().enumerate().take(cols) {
            let text = row.get(i).map_or("", String::as_str);
            let lines = wrap_inline_markdown(text, *width, row_style);
            row_height = row_height.max(lines.len() as u16);
            cells.push(Cell::from(Text::from(lines)));
        }
        rows.push(Row::new(cells).height(row_height).style(Style::default()));
        rows_height = rows_height.saturating_add(row_height);
    }

    let constraints: Vec<Constraint> =
        widths.iter().map(|w| Constraint::Length(*w as u16)).collect();

    let table_widget = Table::new(rows, constraints).header(header).column_spacing(spacing as u16);

    let height = header_height.saturating_add(rows_height);
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    table_widget.render(area, &mut buffer);

    buffer_to_lines(&buffer, area, bg)
}

fn buffer_to_lines(buffer: &Buffer, area: Rect, bg: Option<Color>) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(area.height as usize);
    for y in 0..area.height {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut current_style: Option<Style> = None;
        let mut current_text = String::new();
        for x in 0..area.width {
            let cell = &buffer[(area.x + x, area.y + y)];
            let mut style = cell.style();
            if let Some(bg_color) = bg {
                style = style.bg(bg_color);
            }
            let symbol = cell.symbol();
            if current_style.is_some_and(|s| s == style) {
                current_text.push_str(symbol);
            } else {
                if !current_text.is_empty() {
                    spans.push(Span::styled(
                        current_text.clone(),
                        current_style.unwrap_or_default(),
                    ));
                    current_text.clear();
                }
                current_style = Some(style);
                current_text.push_str(symbol);
            }
        }
        if !current_text.is_empty() {
            spans.push(Span::styled(current_text, current_style.unwrap_or_default()));
        }
        lines.push(Line::from(spans));
    }
    lines
}

fn wrap_inline_markdown(text: &str, width: usize, base_style: Style) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    if text.is_empty() {
        return vec![Line::from(Span::styled(String::new(), base_style))];
    }

    let chunks = parse_inline_chunks(text, base_style);
    wrap_chunks_to_lines(&chunks, width)
}

struct StyledChunk {
    text: String,
    style: Style,
}

fn parse_inline_chunks(text: &str, base_style: Style) -> Vec<StyledChunk> {
    let mut chunks: Vec<StyledChunk> = Vec::new();
    let mut current = String::new();
    let mut style_stack: Vec<Style> = vec![base_style];
    let mut current_style = base_style;

    let flush_current = |chunks: &mut Vec<StyledChunk>, current: &mut String, style: Style| {
        if !current.is_empty() {
            chunks.push(StyledChunk { text: std::mem::take(current), style });
        }
    };

    let options = Options::ENABLE_STRIKETHROUGH;
    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(tag) => {
                flush_current(&mut chunks, &mut current, current_style);
                let next = match tag {
                    Tag::Strong => current_style.add_modifier(Modifier::BOLD),
                    Tag::Emphasis => current_style.add_modifier(Modifier::ITALIC),
                    Tag::Strikethrough => current_style.add_modifier(Modifier::CROSSED_OUT),
                    _ => current_style,
                };
                style_stack.push(next);
                current_style = next;
            }
            Event::End(tag) => {
                flush_current(&mut chunks, &mut current, current_style);
                match tag {
                    TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough => {
                        style_stack.pop();
                        current_style = *style_stack.last().unwrap_or(&base_style);
                    }
                    _ => {}
                }
            }
            Event::Text(t) => current.push_str(&t),
            Event::Code(t) => {
                flush_current(&mut chunks, &mut current, current_style);
                let code_style = current_style.add_modifier(Modifier::REVERSED);
                chunks.push(StyledChunk { text: t.into_string(), style: code_style });
            }
            Event::SoftBreak => current.push(' '),
            Event::HardBreak => current.push('\n'),
            _ => {}
        }
    }
    flush_current(&mut chunks, &mut current, current_style);

    if chunks.is_empty() {
        chunks.push(StyledChunk { text: String::new(), style: base_style });
    }

    chunks
}

fn wrap_chunks_to_lines(chunks: &[StyledChunk], width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut line_spans: Vec<Span<'static>> = Vec::new();
    let mut line_width = 0usize;
    let mut span_text = String::new();
    let mut span_style: Option<Style> = None;

    let flush_span = |line_spans: &mut Vec<Span<'static>>,
                      span_text: &mut String,
                      span_style: &mut Option<Style>| {
        if !span_text.is_empty() {
            let style = span_style.unwrap_or_default();
            line_spans.push(Span::styled(std::mem::take(span_text), style));
        }
    };

    let flush_line = |lines: &mut Vec<Line<'static>>,
                      line_spans: &mut Vec<Span<'static>>,
                      span_text: &mut String,
                      span_style: &mut Option<Style>,
                      line_width: &mut usize| {
        flush_span(line_spans, span_text, span_style);
        lines.push(Line::from(std::mem::take(line_spans)));
        *line_width = 0;
    };

    for chunk in chunks {
        let style = chunk.style;
        for ch in chunk.text.chars() {
            if ch == '\n' {
                flush_line(
                    &mut lines,
                    &mut line_spans,
                    &mut span_text,
                    &mut span_style,
                    &mut line_width,
                );
                span_style = None;
                continue;
            }

            let w = UnicodeWidthChar::width(ch).unwrap_or(0);
            if width > 0 && line_width + w > width && line_width > 0 {
                flush_line(
                    &mut lines,
                    &mut line_spans,
                    &mut span_text,
                    &mut span_style,
                    &mut line_width,
                );
                span_style = None;
            }

            if span_style.is_none() || span_style.is_some_and(|s| s != style) {
                flush_span(&mut line_spans, &mut span_text, &mut span_style);
                span_style = Some(style);
            }
            span_text.push(ch);
            line_width = line_width.saturating_add(w);
        }
    }

    flush_line(&mut lines, &mut line_spans, &mut span_text, &mut span_style, &mut line_width);

    if lines.is_empty() {
        lines.push(Line::default());
    }

    lines
}
