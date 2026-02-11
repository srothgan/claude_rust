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
    pub header_top_sep: Rect,
    pub header: Rect,
    pub header_bot_sep: Rect,
    pub body: Rect,
    /// Area for the todo panel (zero-height when hidden or no todos).
    pub todo: Rect,
    pub input_sep: Rect,
    pub input: Rect,
    pub input_bottom_sep: Rect,
    pub help: Rect,
    pub footer: Option<Rect>,
}

pub fn compute(
    area: Rect,
    input_lines: u16,
    show_header: bool,
    todo_height: u16,
    help_height: u16,
) -> AppLayout {
    let input_height = input_lines.max(1);
    let header_height: u16 = u16::from(show_header);
    let header_bot_sep_height: u16 = u16::from(show_header);
    let zero = Rect::new(area.x, area.y, area.width, 0);

    if area.height < 8 {
        // Ultra-compact: no header, no separator, no footer, no todo
        let [body, input, input_bottom_sep, help] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(input_height),
            Constraint::Length(1),
            Constraint::Length(help_height),
        ])
        .areas(area);
        AppLayout {
            header_top_sep: zero,
            header: zero,
            header_bot_sep: zero,
            body,
            todo: zero,
            input_sep: Rect::new(area.x, input.y, area.width, 0),
            input,
            input_bottom_sep,
            help,
            footer: None,
        }
    } else {
        let [
            header_top_sep,
            header,
            header_bot_sep,
            body,
            todo,
            input_sep,
            input,
            input_bottom_sep,
            help,
            footer,
        ] = Layout::vertical([
            Constraint::Length(header_bot_sep_height),
            Constraint::Length(header_height),
            Constraint::Length(header_bot_sep_height),
            Constraint::Min(3),
            Constraint::Length(todo_height),
            Constraint::Length(1),
            Constraint::Length(input_height),
            Constraint::Length(1),
            Constraint::Length(help_height),
            Constraint::Length(1),
        ])
        .areas(area);
        AppLayout {
            header_top_sep,
            header,
            header_bot_sep,
            body,
            todo,
            input_sep,
            input,
            input_bottom_sep,
            help,
            footer: Some(footer),
        }
    }
}

#[cfg(test)]
mod tests {
    // =====
    // TESTS: 33
    // =====

    use super::*;
    use pretty_assertions::assert_eq;

    fn area(w: u16, h: u16) -> Rect {
        Rect::new(0, 0, w, h)
    }

    /// Sum all layout area heights (handles optional footer).
    fn total_height(layout: &AppLayout) -> u16 {
        layout.header_top_sep.height
            + layout.header.height
            + layout.header_bot_sep.height
            + layout.body.height
            + layout.todo.height
            + layout.input_sep.height
            + layout.input.height
            + layout.input_bottom_sep.height
            + layout.help.height
            + layout.footer.map_or(0, |f| f.height)
    }

    /// Collect all non-zero-height areas in top-to-bottom order.
    fn visible_areas(layout: &AppLayout) -> Vec<Rect> {
        let mut areas = vec![
            layout.header_top_sep,
            layout.header,
            layout.header_bot_sep,
            layout.body,
            layout.todo,
            layout.input_sep,
            layout.input,
            layout.input_bottom_sep,
            layout.help,
        ];
        if let Some(f) = layout.footer {
            areas.push(f);
        }
        areas.into_iter().filter(|r| r.height > 0).collect()
    }

    /// Assert no vertical overlap and areas are in ascending y-order.
    fn assert_no_overlap_and_ordered(layout: &AppLayout) {
        let areas = visible_areas(layout);
        for i in 1..areas.len() {
            let prev = areas[i - 1];
            let curr = areas[i];
            assert!(
                prev.y + prev.height <= curr.y,
                "Area {i}-1 ({prev:?}) overlaps or is not before area {i} ({curr:?})"
            );
        }
    }

    // Layout (normal terminal)

    #[test]
    fn normal_terminal_with_header() {
        let layout = compute(area(80, 24), 1, true, 0, 0);
        assert!(layout.footer.is_some());
        assert_eq!(layout.header.height, 1);
        assert_eq!(layout.header_bot_sep.height, 1);
        assert!(layout.body.height >= 3);
        assert_eq!(layout.input_sep.height, 1);
        assert_eq!(layout.input.height, 1);
        assert_eq!(layout.input_bottom_sep.height, 1);
        assert_eq!(layout.footer.unwrap().height, 1);
    }

    #[test]
    fn normal_all_areas_sum_to_total() {
        let layout = compute(area(80, 24), 1, true, 3, 2);
        assert_eq!(total_height(&layout), 24);
    }

    #[test]
    fn normal_no_header() {
        let layout = compute(area(80, 24), 1, false, 0, 0);
        assert_eq!(layout.header.height, 0);
        assert_eq!(layout.header_bot_sep.height, 0);
        assert!(layout.footer.is_some());
    }

    // Layout

    #[test]
    fn ultra_compact_no_header_no_footer() {
        let layout = compute(area(80, 6), 1, true, 0, 0);
        assert_eq!(layout.header.height, 0);
        assert!(layout.footer.is_none());
        assert_eq!(layout.todo.height, 0);
    }

    #[test]
    fn ultra_compact_areas_sum_to_total() {
        let layout = compute(area(80, 6), 1, true, 0, 0);
        assert_eq!(total_height(&layout), 6);
    }

    #[test]
    fn todo_panel_gets_requested_height() {
        let layout = compute(area(80, 24), 1, true, 5, 0);
        assert_eq!(layout.todo.height, 5);
    }

    #[test]
    fn zero_todo_height_produces_zero_area() {
        let layout = compute(area(80, 24), 1, true, 0, 0);
        assert_eq!(layout.todo.height, 0);
    }

    #[test]
    fn help_gets_requested_height() {
        let layout = compute(area(80, 24), 1, true, 0, 4);
        assert_eq!(layout.help.height, 4);
    }

    #[test]
    fn multi_line_input() {
        let layout = compute(area(80, 24), 5, true, 0, 0);
        assert_eq!(layout.input.height, 5);
    }

    #[test]
    fn input_lines_zero_clamped_to_one() {
        let layout = compute(area(80, 24), 0, true, 0, 0);
        assert_eq!(layout.input.height, 1);
    }

    // Layout

    #[test]
    fn ultra_compact_threshold_exactly_8() {
        let layout = compute(area(80, 8), 1, true, 0, 0);
        assert!(layout.footer.is_some());
    }

    #[test]
    fn ultra_compact_threshold_7() {
        let layout = compute(area(80, 7), 1, true, 0, 0);
        assert!(layout.footer.is_none());
    }

    #[test]
    fn large_terminal() {
        let layout = compute(area(200, 100), 3, true, 5, 2);
        assert_eq!(total_height(&layout), 100);
        assert!(layout.body.height >= 3);
    }

    #[test]
    fn width_carries_through() {
        let layout = compute(area(120, 24), 1, true, 0, 0);
        assert_eq!(layout.header.width, 120);
        assert_eq!(layout.body.width, 120);
        assert_eq!(layout.input.width, 120);
    }

    #[test]
    fn no_overlap_between_areas() {
        let layout = compute(area(80, 24), 2, true, 3, 1);
        assert_no_overlap_and_ordered(&layout);
    }

    #[test]
    fn everything_maxed_out() {
        let layout = compute(area(80, 24), 3, true, 5, 3);
        assert!(layout.body.height >= 3);
        assert_eq!(total_height(&layout), 24);
    }

    // offset areas

    /// Area starting at non-zero x/y — layout should respect the offset.
    #[test]
    fn offset_area_respects_origin() {
        let r = Rect::new(10, 5, 80, 24);
        let layout = compute(r, 1, true, 0, 0);
        // All areas should have x=10 and width=80
        assert_eq!(layout.header.x, 10);
        assert_eq!(layout.body.x, 10);
        assert_eq!(layout.input.x, 10);
        assert_eq!(layout.body.width, 80);
        // First area starts at y=5 (top separator), header at y=6
        assert_eq!(layout.header_top_sep.y, 5);
        assert_eq!(layout.header.y, 6);
        assert_eq!(total_height(&layout), 24);
    }

    /// Compact mode with offset area.
    #[test]
    fn offset_area_compact() {
        let r = Rect::new(5, 10, 60, 6);
        let layout = compute(r, 1, true, 0, 0);
        assert!(layout.footer.is_none());
        assert_eq!(layout.body.x, 5);
        assert_eq!(total_height(&layout), 6);
    }

    // degenerate sizes

    /// Zero-height area — everything gets zero or minimal height.
    #[test]
    fn zero_height_area() {
        let layout = compute(area(80, 0), 1, true, 0, 0);
        // Ultra-compact path (0 < 8), areas should still be valid (not panicking)
        assert!(layout.footer.is_none());
    }

    /// Height = 1 — absolute minimum.
    #[test]
    fn height_one() {
        let layout = compute(area(80, 1), 1, true, 0, 0);
        assert!(layout.footer.is_none());
        assert_eq!(total_height(&layout), 1);
    }

    /// Height = 2.
    #[test]
    fn height_two() {
        let layout = compute(area(80, 2), 1, true, 0, 0);
        assert_eq!(total_height(&layout), 2);
    }

    /// Width = 1 — very narrow terminal.
    #[test]
    fn width_one() {
        let layout = compute(Rect::new(0, 0, 1, 24), 1, true, 0, 0);
        assert_eq!(layout.body.width, 1);
        assert_eq!(layout.input.width, 1);
        assert_eq!(total_height(&layout), 24);
    }

    /// Width = 0.
    #[test]
    fn width_zero() {
        let layout = compute(area(0, 24), 1, true, 0, 0);
        assert_eq!(layout.body.width, 0);
        assert_eq!(total_height(&layout), 24);
    }

    // input exceeds available space

    /// Input requests more lines than the terminal has rows.
    #[test]
    fn input_larger_than_terminal() {
        let layout = compute(area(80, 10), 50, true, 0, 0);
        // Layout should still produce valid areas that sum to total
        assert_eq!(total_height(&layout), 10);
        // Input gets what it asks, but body gets squeezed
        // (ratatui Layout handles this with Min constraint)
    }

    /// Todo + help + input together exceed available space.
    #[test]
    fn competing_constraints_squeeze_body() {
        let layout = compute(area(80, 12), 3, true, 4, 3);
        // header(1) + sep(1) + body(min 3) + todo(4) + sep(1) + input(3) + sep(1) + help(3) + footer(1) = 18
        // Only 12 available — body gets squeezed below min, or ratatui adjusts
        assert_eq!(total_height(&layout), 12);
    }

    // compact mode with extras

    /// Ultra-compact with `help_height` > 0.
    #[test]
    fn compact_with_help() {
        let layout = compute(area(80, 6), 1, true, 0, 2);
        assert!(layout.footer.is_none());
        assert_eq!(layout.help.height, 2);
        assert_eq!(total_height(&layout), 6);
    }

    /// Ultra-compact with multi-line input.
    #[test]
    fn compact_with_multiline_input() {
        let layout = compute(area(80, 7), 3, true, 0, 0);
        assert!(layout.footer.is_none());
        assert_eq!(layout.input.height, 3);
        assert_eq!(total_height(&layout), 7);
    }

    // ordering invariants

    /// In normal mode, areas must be in strict top-to-bottom order.
    #[test]
    fn normal_mode_y_ordering() {
        let layout = compute(area(80, 30), 2, true, 3, 1);
        assert_no_overlap_and_ordered(&layout);
    }

    /// In compact mode, areas must be in strict top-to-bottom order.
    #[test]
    fn compact_mode_y_ordering() {
        let layout = compute(area(80, 6), 1, true, 0, 1);
        assert_no_overlap_and_ordered(&layout);
    }

    /// Footer (when present) must be at the very bottom.
    #[test]
    fn footer_at_bottom() {
        let layout = compute(area(80, 24), 1, true, 0, 0);
        let footer = layout.footer.unwrap();
        assert_eq!(footer.y + footer.height, 24);
    }

    /// Body starts immediately after header separator.
    #[test]
    fn body_follows_header_bot_sep() {
        let layout = compute(area(80, 24), 1, true, 0, 0);
        assert_eq!(
            layout.body.y,
            layout.header.y + layout.header.height + layout.header_bot_sep.height
        );
    }

    // stress / parametric

    /// Verify invariants hold for many terminal sizes.
    #[test]
    fn parametric_sizes_invariants() {
        for h in [1, 2, 3, 5, 7, 8, 10, 15, 24, 50, 100] {
            for w in [1, 10, 80, 200] {
                let layout = compute(Rect::new(0, 0, w, h), 1, true, 0, 0);
                assert_eq!(total_height(&layout), h, "Height mismatch for {w}x{h}");
                for a in visible_areas(&layout) {
                    assert_eq!(a.width, w, "Width mismatch in area {a:?} for {w}x{h}");
                }
            }
        }
    }

    /// Verify invariants with various input/todo/help combinations.
    #[test]
    fn parametric_features_invariants() {
        for input in [0, 1, 3, 10] {
            for todo in [0, 2, 5] {
                for help in [0, 1, 3] {
                    let layout = compute(area(80, 30), input, true, todo, help);
                    assert_eq!(
                        total_height(&layout),
                        30,
                        "Height mismatch for input={input} todo={todo} help={help}"
                    );
                    assert_no_overlap_and_ordered(&layout);
                }
            }
        }
    }
}
