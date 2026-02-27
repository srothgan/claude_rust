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

use super::{
    App, AppStatus, BlockCache, CancelOrigin, ChatMessage, IncrementalMarkdown, MessageBlock,
    MessageRole,
};
use crate::agent::events::ClientEvent;
use crate::agent::model;
use crate::app::slash;

pub(super) fn submit_input(app: &mut App) {
    if matches!(app.status, AppStatus::Connecting | AppStatus::Resuming | AppStatus::Error) {
        return;
    }

    // Dismiss any open mention dropdown
    app.mention = None;
    app.slash = None;

    // No connection yet - can't submit
    let text = app.input.text();
    if text.trim().is_empty() {
        return;
    }

    app.input.clear();
    enqueue_submission(app, text);
}

fn is_turn_busy(app: &App) -> bool {
    matches!(app.status, AppStatus::Thinking | AppStatus::Running)
        || app.pending_cancel_origin.is_some()
}

pub(super) fn enqueue_submission(app: &mut App, text: String) {
    if text.trim().is_empty() {
        return;
    }

    // `/cancel` is a manual control action and never goes through queueing.
    if slash::is_cancel_command(&text) {
        dispatch_submission(app, text);
        return;
    }

    if is_turn_busy(app) {
        app.queued_submission = Some(text);
        if let Err(message) = request_cancel(app, CancelOrigin::AutoQueue) {
            tracing::error!("Failed to request queued auto-cancel: {message}");
        }
        return;
    }

    dispatch_submission(app, text);
}

pub(super) fn request_cancel(app: &mut App, origin: CancelOrigin) -> Result<(), String> {
    if !matches!(app.status, AppStatus::Thinking | AppStatus::Running) {
        return Ok(());
    }

    if let Some(existing_origin) = app.pending_cancel_origin {
        if matches!(existing_origin, CancelOrigin::AutoQueue)
            && matches!(origin, CancelOrigin::Manual)
        {
            app.pending_cancel_origin = Some(CancelOrigin::Manual);
            app.cancelled_turn_pending_hint = true;
        }
        return Ok(());
    }

    let Some(ref conn) = app.conn else {
        return Err("not connected yet".to_owned());
    };
    let Some(sid) = app.session_id.clone() else {
        return Err("no active session".to_owned());
    };

    conn.cancel(sid.to_string()).map_err(|e| e.to_string())?;
    app.pending_cancel_origin = Some(origin);
    app.cancelled_turn_pending_hint = matches!(origin, CancelOrigin::Manual);
    let _ = app.event_tx.send(ClientEvent::TurnCancelled);
    Ok(())
}

pub(super) fn drain_queued_submission(app: &mut App) {
    if !matches!(app.status, AppStatus::Ready) || app.pending_cancel_origin.is_some() {
        return;
    }
    let Some(text) = app.queued_submission.take() else {
        return;
    };
    dispatch_submission(app, text);
}

fn dispatch_submission(app: &mut App, text: String) {
    if slash::try_handle_submit(app, &text) {
        return;
    }
    dispatch_prompt_turn(app, text);
}

fn dispatch_prompt_turn(app: &mut App, text: String) {
    // New turn started by user input: force-stop stale tool calls from older turns
    // so their spinners don't continue during this turn.
    let _ = app.finalize_in_progress_tool_calls(model::ToolCallStatus::Failed);

    let Some(conn) = app.conn.clone() else { return };

    app.messages.push(ChatMessage {
        role: MessageRole::User,
        blocks: vec![MessageBlock::Text(
            text.clone(),
            BlockCache::default(),
            IncrementalMarkdown::from_complete(&text),
        )],
        usage: None,
    });
    // Create empty assistant message immediately -- message.rs shows thinking indicator
    app.messages.push(ChatMessage {
        role: MessageRole::Assistant,
        blocks: Vec::new(),
        usage: None,
    });
    app.enforce_history_retention();
    app.status = AppStatus::Thinking;
    app.viewport.engage_auto_scroll();

    let Some(sid) = app.session_id.clone() else {
        return;
    };
    let tx = app.event_tx.clone();
    match conn.prompt_text(sid.to_string(), text) {
        Ok(resp) => {
            tracing::debug!("Prompt dispatched: stop_reason={:?}", resp.stop_reason);
        }
        Err(e) => {
            let _ = tx.send(ClientEvent::TurnError(e.to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::wire::BridgeCommand;

    fn app_with_connection()
    -> (App, tokio::sync::mpsc::UnboundedReceiver<crate::agent::wire::CommandEnvelope>) {
        let mut app = App::test_default();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        app.conn = Some(std::rc::Rc::new(crate::agent::client::AgentConnection::new(tx)));
        app.session_id = Some(model::SessionId::new("session-1"));
        (app, rx)
    }

    #[test]
    fn enqueue_submission_while_running_queues_and_requests_auto_cancel() {
        let (mut app, mut rx) = app_with_connection();
        app.status = AppStatus::Running;

        enqueue_submission(&mut app, "queued prompt".into());

        assert_eq!(app.queued_submission.as_deref(), Some("queued prompt"));
        assert_eq!(app.pending_cancel_origin, Some(CancelOrigin::AutoQueue));
        assert!(matches!(app.status, AppStatus::Running));
        let envelope = rx.try_recv().expect("cancel command should be sent");
        assert!(matches!(
            envelope.command,
            BridgeCommand::CancelTurn { session_id } if session_id == "session-1"
        ));
    }

    #[test]
    fn manual_cancel_promotes_existing_auto_cancel() {
        let (mut app, mut rx) = app_with_connection();
        app.status = AppStatus::Thinking;

        request_cancel(&mut app, CancelOrigin::AutoQueue).expect("auto cancel request");
        request_cancel(&mut app, CancelOrigin::Manual).expect("manual cancel request");

        assert_eq!(app.pending_cancel_origin, Some(CancelOrigin::Manual));
        assert!(app.cancelled_turn_pending_hint);
        let envelope = rx.try_recv().expect("single cancel command should be sent");
        assert!(matches!(
            envelope.command,
            BridgeCommand::CancelTurn { session_id } if session_id == "session-1"
        ));
        assert!(rx.try_recv().is_err(), "manual promotion should not send second cancel");
    }

    #[test]
    fn drain_queued_submission_dispatches_prompt_when_ready() {
        let (mut app, mut rx) = app_with_connection();
        app.status = AppStatus::Ready;
        app.queued_submission = Some("run queued".into());

        drain_queued_submission(&mut app);

        assert!(app.queued_submission.is_none());
        assert!(matches!(app.status, AppStatus::Thinking));
        assert_eq!(app.messages.len(), 2);
        let envelope = rx.try_recv().expect("prompt command should be sent");
        assert!(matches!(
            envelope.command,
            BridgeCommand::Prompt { session_id, .. } if session_id == "session-1"
        ));
    }

    #[test]
    fn submit_input_cancel_command_bypasses_queue() {
        let (mut app, mut rx) = app_with_connection();
        app.status = AppStatus::Running;
        app.input.set_text("/cancel");

        submit_input(&mut app);

        assert!(app.queued_submission.is_none());
        assert_eq!(app.pending_cancel_origin, Some(CancelOrigin::Manual));
        let envelope = rx.try_recv().expect("cancel command should be sent");
        assert!(matches!(
            envelope.command,
            BridgeCommand::CancelTurn { session_id } if session_id == "session-1"
        ));
    }
}
