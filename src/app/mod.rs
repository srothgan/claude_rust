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

mod cache_policy;
mod connect;
mod dialog;
mod events;
mod focus;
pub(crate) mod input;
mod input_submit;
mod keys;
pub(crate) mod mention;
pub(crate) mod paste_burst;
mod permissions;
mod selection;
pub(crate) mod slash;
mod state;
mod terminal;
mod todos;
mod update_check;

// Re-export all public types so `crate::app::App`, `crate::app::BlockCache`, etc. still work.
pub use cache_policy::{
    CacheSplitPolicy, DEFAULT_CACHE_SPLIT_HARD_LIMIT_BYTES, DEFAULT_CACHE_SPLIT_SOFT_LIMIT_BYTES,
    DEFAULT_TOOL_PREVIEW_LIMIT_BYTES, default_cache_split_policy, find_text_split_index,
};
pub use connect::{create_app, start_connection};
pub use events::{handle_client_event, handle_terminal_event};
pub use focus::{FocusManager, FocusOwner, FocusTarget};
pub use input::InputState;
pub(crate) use selection::normalize_selection;
pub use state::{
    App, AppStatus, BlockCache, CancelOrigin, ChatMessage, ChatViewport, HelpView,
    IncrementalMarkdown, InlinePermission, LoginHint, MessageBlock, MessageRole, MessageUsage,
    ModeInfo, ModeState, PasteSessionState, RecentSessionInfo, SelectionKind, SelectionPoint,
    SelectionState, SessionUsageState, TerminalSnapshotMode, TodoItem, TodoStatus, ToolCallInfo,
    WelcomeBlock, is_execute_tool_name,
};
pub use update_check::start_update_check;

use crate::agent::model;
use crossterm::event::{
    EventStream, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use futures::{FutureExt as _, StreamExt};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// TUI event loop
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
pub async fn run_tui(app: &mut App) -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    let mut os_shutdown = Box::pin(wait_for_shutdown_signal());

    // Enable bracketed paste and mouse capture (ignore error on unsupported terminals)
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableBracketedPaste,
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableFocusChange,
        // Enable enhanced keyboard protocol for reliable modifier detection (e.g. Shift+Enter)
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
        )
    );

    let mut events = EventStream::new();
    let tick_duration = Duration::from_millis(16);
    let mut last_render = Instant::now();

    loop {
        // Phase 1: wait for at least one event or the next frame tick
        let time_to_next = tick_duration.saturating_sub(last_render.elapsed());
        tokio::select! {
            Some(Ok(event)) = events.next() => {
                events::handle_terminal_event(app, event);
            }
            Some(event) = app.event_rx.recv() => {
                events::handle_client_event(app, event);
            }
            shutdown = &mut os_shutdown => {
                if let Err(err) = shutdown {
                    tracing::warn!(%err, "OS shutdown signal listener failed");
                }
                app.should_quit = true;
            }
            () = tokio::time::sleep(time_to_next) => {}
        }

        // Phase 2: drain all remaining queued events (non-blocking)
        loop {
            // Try terminal events first (keeps typing responsive)
            if let Some(Some(Ok(event))) = events.next().now_or_never() {
                events::handle_terminal_event(app, event);
                continue;
            }
            // Then client events
            match app.event_rx.try_recv() {
                Ok(event) => {
                    events::handle_client_event(app, event);
                }
                Err(_) => break,
            }
        }

        // Merge and process `Event::Paste` chunks as one paste action.
        if !app.pending_paste_text.is_empty() {
            finalize_pending_paste_event(app);
        }

        // Post-drain paste handling:
        // - while a detected paste burst is still active, defer rendering to avoid
        //   showing raw pasted text before placeholder collapse.
        // - once the burst settles, collapse large paste content to placeholder.
        let suppress_render_for_active_paste = should_suppress_render_for_active_paste(app);
        if app.paste_burst.is_paste() {
            app.pending_submit = false;
            if app.paste_burst.is_settled() {
                finalize_paste_burst(app);
                app.paste_burst.reset();
                app.paste_burst_start = None;
            }
        } else if !app.paste_burst.is_active() {
            app.paste_burst_start = None;
        }

        // Deferred submit: if Enter was pressed and no rapid keys followed
        // (not a paste), strip the trailing newline and submit.
        if app.pending_submit {
            app.pending_submit = false;
            finalize_deferred_submit(app);
        }
        app.drain_key_count = 0;

        if app.should_quit {
            break;
        }
        if suppress_render_for_active_paste {
            continue;
        }

        // Phase 3: render once (only when something changed)
        let is_animating = matches!(
            app.status,
            AppStatus::Connecting | AppStatus::Resuming | AppStatus::Thinking | AppStatus::Running
        ) || app.is_compacting;
        if is_animating {
            app.spinner_frame = app.spinner_frame.wrapping_add(1);
            app.needs_redraw = true;
        }
        // Smooth scroll still settling
        let scroll_delta = (app.viewport.scroll_target as f32 - app.viewport.scroll_pos).abs();
        if scroll_delta >= 0.01 {
            app.needs_redraw = true;
        }
        if terminal::update_terminal_outputs(app) {
            app.needs_redraw = true;
        }
        if app.force_redraw {
            terminal.clear()?;
            app.force_redraw = false;
            app.needs_redraw = true;
        }
        if app.needs_redraw {
            if let Some(ref mut perf) = app.perf {
                perf.next_frame();
            }
            if app.perf.is_some() {
                app.mark_frame_presented(Instant::now());
            }
            #[allow(clippy::drop_non_drop)]
            {
                let timer = app.perf.as_ref().map(|p| p.start("frame_total"));
                let draw_timer = app.perf.as_ref().map(|p| p.start("frame::terminal_draw"));
                terminal.draw(|f| crate::ui::render(f, app))?;
                drop(draw_timer);
                drop(timer);
            }
            app.needs_redraw = false;
            last_render = Instant::now();
        }
    }

    // --- Graceful shutdown ---

    // Dismiss all pending inline permissions (reject via last option)
    for tool_id in std::mem::take(&mut app.pending_permission_ids) {
        if let Some((mi, bi)) = app.tool_call_index.get(&tool_id).copied()
            && let Some(MessageBlock::ToolCall(tc)) =
                app.messages.get_mut(mi).and_then(|m| m.blocks.get_mut(bi))
        {
            let tc = tc.as_mut();
            if let Some(pending) = tc.pending_permission.take()
                && let Some(last_opt) = pending.options.last()
            {
                let _ = pending.response_tx.send(model::RequestPermissionResponse::new(
                    model::RequestPermissionOutcome::Selected(
                        model::SelectedPermissionOutcome::new(last_opt.option_id.clone()),
                    ),
                ));
            }
        }
    }

    // Cancel any active turn and give the adapter a moment to clean up
    if matches!(app.status, AppStatus::Thinking | AppStatus::Running)
        && let Some(ref conn) = app.conn
        && let Some(sid) = app.session_id.clone()
    {
        let _ = conn.cancel(sid.to_string());
    }

    // Restore terminal
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableBracketedPaste,
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableFocusChange,
        PopKeyboardEnhancementFlags
    );
    ratatui::restore();

    Ok(())
}

async fn wait_for_shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            sigint = tokio::signal::ctrl_c() => {
                sigint?;
            }
            _ = sigterm.recv() => {}
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}

/// Finalize queued `Event::Paste` chunks for this drain cycle.
fn finalize_pending_paste_event(app: &mut App) {
    let pasted = std::mem::take(&mut app.pending_paste_text);
    if pasted.is_empty() {
        return;
    }

    let session = app.pending_paste_session.take().unwrap_or_else(|| {
        let id = app.next_paste_session_id;
        app.next_paste_session_id = app.next_paste_session_id.saturating_add(1);
        state::PasteSessionState {
            id,
            start: SelectionPoint { row: app.input.cursor_row, col: app.input.cursor_col },
            placeholder_index: None,
        }
    });

    if session.placeholder_index.is_none() {
        let end = SelectionPoint { row: app.input.cursor_row, col: app.input.cursor_col };
        strip_input_range(app, session.start, end);
    }

    let appended = session
        .placeholder_index
        .and_then(|session_idx| {
            let current_line = app.input.lines.get(app.input.cursor_row)?;
            let current_idx =
                input::parse_paste_placeholder_before_cursor(current_line, app.input.cursor_col)?;
            (current_idx == session_idx).then_some(())
        })
        .is_some()
        && app.input.append_to_active_paste_block(&pasted);
    if appended {
        app.active_paste_session = Some(session);
        return;
    }

    let char_count = input::count_text_chars(&pasted);
    if char_count > input::PASTE_PLACEHOLDER_CHAR_THRESHOLD {
        app.input.insert_paste_block(&pasted);
        let idx = app.input.lines.get(app.input.cursor_row).and_then(|line| {
            input::parse_paste_placeholder_before_cursor(line, app.input.cursor_col)
        });
        app.active_paste_session =
            Some(state::PasteSessionState { placeholder_index: idx, ..session });
    } else {
        app.input.insert_str(&pasted);
        app.active_paste_session = None;
    }
}

/// After a paste burst is detected (rapid key events), clean up the pasted
/// content in the newly pasted range only.
fn finalize_paste_burst(app: &mut App) {
    let Some(start) = app.paste_burst_start else {
        return;
    };
    let end = SelectionPoint { row: app.input.cursor_row, col: app.input.cursor_col };
    if cursor_gt(start, end) {
        return;
    }

    let Some(start_offset) = cursor_to_byte_offset(&app.input.lines, start) else {
        return;
    };
    let Some(end_offset) = cursor_to_byte_offset(&app.input.lines, end) else {
        return;
    };
    if start_offset > end_offset {
        return;
    }

    let raw = app.input.lines.join("\n");
    if end_offset > raw.len() {
        return;
    }

    let pasted_range = &raw[start_offset..end_offset];
    let normalized = input::trim_trailing_line_breaks(pasted_range);
    if normalized.is_empty() {
        let mut merged = String::with_capacity(
            raw.len().saturating_sub(end_offset.saturating_sub(start_offset)),
        );
        merged.push_str(&raw[..start_offset]);
        merged.push_str(&raw[end_offset..]);
        apply_merged_input_snapshot(app, &merged, start_offset);
        return;
    }

    if input::count_text_chars(normalized) > input::PASTE_PLACEHOLDER_CHAR_THRESHOLD {
        let normalized = normalized.to_owned();
        let mut merged = String::with_capacity(
            raw.len().saturating_sub(end_offset.saturating_sub(start_offset)),
        );
        merged.push_str(&raw[..start_offset]);
        merged.push_str(&raw[end_offset..]);
        apply_merged_input_snapshot(app, &merged, start_offset);
        app.input.insert_paste_block(&normalized);
        return;
    }

    let normalized = normalized.to_owned();
    let mut merged = String::with_capacity(
        raw.len().saturating_sub(end_offset.saturating_sub(start_offset)) + normalized.len(),
    );
    merged.push_str(&raw[..start_offset]);
    merged.push_str(&normalized);
    merged.push_str(&raw[end_offset..]);
    let cursor_offset = start_offset + normalized.len();
    apply_merged_input_snapshot(app, &merged, cursor_offset);
}

fn cursor_gt(a: SelectionPoint, b: SelectionPoint) -> bool {
    a.row > b.row || (a.row == b.row && a.col > b.col)
}

fn cursor_to_byte_offset(lines: &[String], cursor: SelectionPoint) -> Option<usize> {
    let line = lines.get(cursor.row)?;
    let mut offset = 0usize;
    for prior in &lines[..cursor.row] {
        offset = offset.saturating_add(prior.len().saturating_add(1));
    }
    Some(offset.saturating_add(char_to_byte_index(line, cursor.col)))
}

fn char_to_byte_index(text: &str, char_idx: usize) -> usize {
    text.char_indices().nth(char_idx).map_or(text.len(), |(i, _)| i)
}

fn byte_offset_to_cursor(text: &str, byte_offset: usize) -> SelectionPoint {
    let mut row = 0usize;
    let mut col = 0usize;
    let mut seen = 0usize;
    for ch in text.chars() {
        let ch_len = ch.len_utf8();
        if seen + ch_len > byte_offset {
            break;
        }
        seen += ch_len;
        if ch == '\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    SelectionPoint { row, col }
}

fn apply_merged_input_snapshot(app: &mut App, merged: &str, cursor_offset: usize) {
    let mut lines: Vec<String> = merged.split('\n').map(ToOwned::to_owned).collect();
    if lines.is_empty() {
        lines.push(String::new());
    }
    let mut cursor = byte_offset_to_cursor(merged, cursor_offset.min(merged.len()));
    if cursor.row >= lines.len() {
        cursor.row = lines.len().saturating_sub(1);
        cursor.col = lines[cursor.row].chars().count();
    } else {
        cursor.col = cursor.col.min(lines[cursor.row].chars().count());
    }

    app.input.lines = lines;
    app.input.cursor_row = cursor.row;
    app.input.cursor_col = cursor.col;
    app.input.version += 1;
    app.input.sync_textarea_engine();
}

fn strip_input_range(app: &mut App, start: SelectionPoint, end: SelectionPoint) {
    if cursor_gt(start, end) || start == end {
        return;
    }
    let Some(start_offset) = cursor_to_byte_offset(&app.input.lines, start) else {
        return;
    };
    let Some(end_offset) = cursor_to_byte_offset(&app.input.lines, end) else {
        return;
    };
    if start_offset >= end_offset {
        return;
    }
    let raw = app.input.lines.join("\n");
    if end_offset > raw.len() {
        return;
    }
    let mut merged = String::with_capacity(raw.len().saturating_sub(end_offset - start_offset));
    merged.push_str(&raw[..start_offset]);
    merged.push_str(&raw[end_offset..]);
    apply_merged_input_snapshot(app, &merged, start_offset);
}

/// Finalize a deferred Enter: strip trailing empty lines that were optimistically
/// inserted by the deferred-submit Enter handler, then submit the input.
fn finalize_deferred_submit(app: &mut App) {
    // Remove trailing empty lines added by deferred Enter presses.
    while app.input.lines.len() > 1 && app.input.lines.last().is_some_and(String::is_empty) {
        app.input.lines.pop();
    }
    // Place cursor at end of last line
    app.input.cursor_row = app.input.lines.len().saturating_sub(1);
    app.input.cursor_col = app.input.lines.last().map_or(0, |l| l.chars().count());
    app.input.version += 1;
    app.input.sync_textarea_engine();

    input_submit::submit_input(app);
}

#[inline]
fn should_suppress_render_for_active_paste(app: &App) -> bool {
    app.paste_burst.is_paste() && app.paste_burst.is_active()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::Event;

    #[test]
    fn pending_paste_chunks_are_merged_before_threshold_check() {
        let mut app = App::test_default();
        let first = "a".repeat(700);
        let second = "b".repeat(401);
        events::handle_terminal_event(&mut app, Event::Paste(first.clone()));
        events::handle_terminal_event(&mut app, Event::Paste(second.clone()));

        // Not applied until post-drain finalization.
        assert!(app.input.is_empty());
        assert!(!app.pending_paste_text.is_empty());

        finalize_pending_paste_event(&mut app);

        assert_eq!(app.input.lines, vec!["[Pasted Text 1 - 1101 chars]"]);
        assert_eq!(app.input.text(), format!("{first}{second}"));
    }

    #[test]
    fn pending_paste_chunk_appends_to_same_session_placeholder() {
        let mut app = App::test_default();
        app.input.insert_paste_block("a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk");
        app.active_paste_session = Some(state::PasteSessionState {
            id: 7,
            start: SelectionPoint { row: 0, col: 0 },
            placeholder_index: Some(0),
        });
        app.pending_paste_session = app.active_paste_session;
        app.pending_paste_text = "\nl\nm".to_owned();

        finalize_pending_paste_event(&mut app);

        assert_eq!(app.input.lines, vec!["[Pasted Text 1 - 25 chars]"]);
        assert_eq!(app.input.text(), "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm");
    }

    #[test]
    fn pending_paste_exact_1000_chars_stays_inline() {
        let mut app = App::test_default();
        app.pending_paste_text = "x".repeat(1000);

        finalize_pending_paste_event(&mut app);

        assert_eq!(app.input.lines, vec!["x".repeat(1000)]);
    }

    #[test]
    fn pending_paste_1001_chars_becomes_placeholder() {
        let mut app = App::test_default();
        app.pending_paste_text = "x".repeat(1001);

        finalize_pending_paste_event(&mut app);

        assert_eq!(app.input.lines, vec!["[Pasted Text 1 - 1001 chars]"]);
        assert_eq!(app.input.text(), "x".repeat(1001));
    }

    #[test]
    fn pending_paste_session_isolation_prevents_unintended_append() {
        let mut app = App::test_default();
        app.pending_paste_text = "a".repeat(1001);
        finalize_pending_paste_event(&mut app);
        events::handle_terminal_event(
            &mut app,
            Event::Key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('v'),
                crossterm::event::KeyModifiers::CONTROL,
            )),
        );

        app.pending_paste_text = "b".repeat(1001);
        finalize_pending_paste_event(&mut app);

        assert_eq!(
            app.input.lines,
            vec!["[Pasted Text 1 - 1001 chars][Pasted Text 2 - 1001 chars]"]
        );
        assert_eq!(app.input.text(), format!("{}{}", "a".repeat(1001), "b".repeat(1001)));
    }

    #[test]
    fn burst_finalization_is_limited_to_newly_pasted_range() {
        let mut app = App::test_default();
        app.input.set_text("beforeafter");
        app.input.cursor_col = 6; // between "before" and "after"
        app.paste_burst_start = Some(SelectionPoint { row: 0, col: 6 });
        app.input.insert_str(&"x".repeat(1001));

        finalize_paste_burst(&mut app);

        assert_eq!(app.input.lines, vec!["before[Pasted Text 1 - 1001 chars]after"]);
        assert_eq!(app.input.text(), format!("before{}after", "x".repeat(1001)));
    }

    #[test]
    fn render_not_suppressed_for_active_non_paste_burst() {
        let mut app = App::test_default();
        let _ = app.paste_burst.on_key_event(app.input.lines.len());

        assert!(!should_suppress_render_for_active_paste(&app));
        assert!(app.paste_burst.is_active());
        assert!(!app.paste_burst.is_paste());
    }

    #[test]
    fn render_suppressed_for_active_confirmed_paste_burst() {
        let mut app = App::test_default();
        for _ in 0..4 {
            let _ = app.paste_burst.on_key_event(app.input.lines.len());
        }

        assert!(should_suppress_render_for_active_paste(&app));
        assert!(app.paste_burst.is_active());
        assert!(app.paste_burst.is_paste());
    }
}
