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

// Re-export all public types so `crate::app::App`, `crate::app::BlockCache`, etc. still work.
pub use connect::{create_app, start_connection};
pub use events::{handle_acp_event, handle_terminal_event};
pub use focus::{FocusManager, FocusOwner, FocusTarget};
pub use input::InputState;
pub(crate) use selection::normalize_selection;
pub use state::{
    App, AppStatus, BlockCache, ChatMessage, ChatViewport, HelpView, IncrementalMarkdown,
    InlinePermission, InputWrapCache, LoginHint, MessageBlock, MessageRole, ModeInfo, ModeState,
    SelectionKind, SelectionPoint, SelectionState, TodoItem, TodoStatus, ToolCallInfo,
    WelcomeBlock,
};

use agent_client_protocol::{self as acp, Agent as _};
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
                events::handle_acp_event(app, event);
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
            // Then ACP events
            match app.event_rx.try_recv() {
                Ok(event) => {
                    events::handle_acp_event(app, event);
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
        let suppress_render_for_active_paste =
            app.paste_burst.is_paste() && app.paste_burst.is_active();
        if app.paste_burst.is_paste() {
            app.pending_submit = false;
            if app.paste_burst.is_settled() {
                finalize_paste_burst(app);
                app.paste_burst.reset();
            }
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
        let is_animating =
            matches!(app.status, AppStatus::Connecting | AppStatus::Thinking | AppStatus::Running);
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
                let _ = pending.response_tx.send(acp::RequestPermissionResponse::new(
                    acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(
                        last_opt.option_id.clone(),
                    )),
                ));
            }
        }
    }

    // Cancel any active turn and give the adapter a moment to clean up
    if matches!(app.status, AppStatus::Thinking | AppStatus::Running)
        && let Some(ref conn) = app.conn
        && let Some(sid) = app.session_id.clone()
    {
        let _ = conn.cancel(acp::CancelNotification::new(sid)).await;
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

    // Continuation chunk of an already collapsed placeholder.
    if app.input.append_to_active_paste_block(&pasted) {
        return;
    }

    let line_count = input::count_text_lines(&pasted);
    if line_count > input::PASTE_PLACEHOLDER_LINE_THRESHOLD {
        app.input.insert_paste_block(&pasted);
    } else {
        app.input.insert_str(&pasted);
    }
}

/// After a paste burst is detected (rapid key events), clean up the pasted
/// content: strip trailing empty lines and convert large pastes (>10 lines)
/// into a compact placeholder.
fn finalize_paste_burst(app: &mut App) {
    // Work on the fully expanded text so placeholders + trailing chunk artifacts
    // are normalized back into a single coherent paste block.
    let full_text = app.input.text();
    let full_text = input::trim_trailing_line_breaks(&full_text);

    if full_text.is_empty() {
        app.input.clear();
        return;
    }

    let line_count = input::count_text_lines(full_text);
    if line_count > input::PASTE_PLACEHOLDER_LINE_THRESHOLD {
        app.input.clear();
        app.input.insert_paste_block(full_text);
    } else {
        app.input.set_text(full_text);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::Event;

    #[test]
    fn pending_paste_chunks_are_merged_before_threshold_check() {
        let mut app = App::test_default();
        events::handle_terminal_event(&mut app, Event::Paste("a\nb\nc\nd\ne\nf".to_owned()));
        events::handle_terminal_event(&mut app, Event::Paste("\ng\nh\ni\nj\nk".to_owned()));

        // Not applied until post-drain finalization.
        assert!(app.input.is_empty());
        assert!(!app.pending_paste_text.is_empty());

        finalize_pending_paste_event(&mut app);

        assert_eq!(app.input.lines, vec!["[Pasted Text 1 - 11 lines]"]);
        assert_eq!(app.input.text(), "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk");
    }

    #[test]
    fn pending_paste_chunk_appends_to_existing_placeholder() {
        let mut app = App::test_default();
        app.input.insert_paste_block("a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk");
        app.pending_paste_text = "\nl\nm".to_owned();

        finalize_pending_paste_event(&mut app);

        assert_eq!(app.input.lines, vec!["[Pasted Text 1 - 13 lines]"]);
        assert_eq!(app.input.text(), "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm");
    }
}
