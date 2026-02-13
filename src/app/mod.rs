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

mod connect;
mod events;
mod input;
mod input_submit;
pub(crate) mod mention;
mod permissions;
mod selection;
mod state;
mod terminal;
mod todos;

// Re-export all public types so `crate::app::App`, `crate::app::BlockCache`, etc. still work.
pub use connect::{create_app, start_connection};
pub use events::{handle_acp_event, handle_terminal_event};
pub use input::InputState;
pub(crate) use selection::normalize_selection;
pub use state::{
    App, AppStatus, BlockCache, ChatMessage, ChatViewport, IncrementalMarkdown, InlinePermission,
    InputWrapCache, LoginHint, MessageBlock, MessageRole, ModeInfo, ModeState, SelectionKind,
    SelectionPoint, SelectionState, TodoItem, TodoStatus, ToolCallInfo,
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

        if app.should_quit {
            break;
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
            #[allow(clippy::drop_non_drop)]
            {
                let timer = app.perf.as_ref().map(|p| p.start("frame_total"));
                terminal.draw(|f| crate::ui::render(f, app))?;
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
