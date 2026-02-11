// =====
// TESTS: 19
// =====
//
// State transition integration tests.
// Validates multi-event sequences and App state consistency.

use agent_client_protocol as acp;
use claude_rust::acp::client::ClientEvent;
use claude_rust::app::{AppStatus, MessageBlock};
use pretty_assertions::assert_eq;

use crate::helpers::{send_acp_event, test_app};

// --- Full turn lifecycle ---

#[tokio::test]
async fn full_turn_lifecycle_text_only() {
    let mut app = test_app();
    assert!(matches!(app.status, AppStatus::Ready));

    // Agent starts thinking (thought chunk)
    let thought =
        acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("Planning...")));
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::AgentThoughtChunk(thought)),
    );
    assert!(matches!(app.status, AppStatus::Thinking));

    // Agent streams text
    let chunk = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(
        "Here is my answer.",
    )));
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(chunk)),
    );
    assert!(matches!(app.status, AppStatus::Running));

    // Turn completes
    send_acp_event(&mut app, ClientEvent::TurnComplete);
    assert!(matches!(app.status, AppStatus::Ready));
    assert_eq!(app.messages.len(), 1);
}

#[tokio::test]
async fn full_turn_lifecycle_with_tool_calls() {
    let mut app = test_app();

    // Text chunk
    let chunk =
        acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("Let me check.")));
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(chunk)),
    );

    // Tool call
    let tc = acp::ToolCall::new("tc-flow", "Read src/lib.rs")
        .kind(acp::ToolKind::Read)
        .status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    // Tool completes
    let fields = acp::ToolCallUpdateFields::new().status(acp::ToolCallStatus::Completed);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            "tc-flow", fields,
        ))),
    );
    assert!(matches!(app.status, AppStatus::Thinking));

    // More text
    let chunk2 = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(
        " The file looks good.",
    )));
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(chunk2)),
    );

    // Turn completes
    send_acp_event(&mut app, ClientEvent::TurnComplete);
    assert!(matches!(app.status, AppStatus::Ready));
}

// --- TodoWrite handling ---

#[tokio::test]
async fn todowrite_tool_call_updates_todo_list() {
    let mut app = test_app();

    let raw_input = serde_json::json!({
        "todos": [
            {"content": "Fix bug", "status": "in_progress", "activeForm": "Fixing bug"},
            {"content": "Write tests", "status": "pending", "activeForm": "Writing tests"},
        ]
    });

    let mut meta = serde_json::Map::new();
    meta.insert("claudeCode".into(), serde_json::json!({"toolName": "TodoWrite"}));
    let tc = acp::ToolCall::new("todo-1", "TodoWrite")
        .kind(acp::ToolKind::Other)
        .status(acp::ToolCallStatus::InProgress)
        .raw_input(raw_input)
        .meta(meta);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    assert_eq!(app.todos.len(), 2);
    assert_eq!(app.todos[0].content, "Fix bug");
    assert_eq!(app.todos[1].content, "Write tests");
    // show_todo_panel is user-toggled (Ctrl+T), not auto-shown on TodoWrite
    assert!(!app.show_todo_panel);
}

#[tokio::test]
async fn todowrite_all_completed_hides_panel() {
    let mut app = test_app();

    let raw_input = serde_json::json!({
        "todos": [
            {"content": "Done task", "status": "completed", "activeForm": "Done"},
        ]
    });

    let mut meta = serde_json::Map::new();
    meta.insert("claudeCode".into(), serde_json::json!({"toolName": "TodoWrite"}));
    let tc = acp::ToolCall::new("todo-done", "TodoWrite")
        .kind(acp::ToolKind::Other)
        .status(acp::ToolCallStatus::InProgress)
        .raw_input(raw_input)
        .meta(meta);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    assert!(app.todos.is_empty(), "all-completed clears the list");
    assert!(!app.show_todo_panel, "panel hidden when all done");
}

// --- Error recovery ---

#[tokio::test]
async fn error_then_new_turn_recovers() {
    let mut app = test_app();

    send_acp_event(&mut app, ClientEvent::TurnError("timeout".into()));
    assert!(matches!(app.status, AppStatus::Error));

    // New text chunk (simulates user retry) starts fresh
    let chunk =
        acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("Retry answer")));
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(chunk)),
    );
    assert!(matches!(app.status, AppStatus::Running));
}

// --- Message accumulation ---

#[tokio::test]
async fn chunks_across_turns_append_to_last_assistant_message() {
    let mut app = test_app();

    // First turn
    let c1 = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("Turn 1")));
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(c1)));
    send_acp_event(&mut app, ClientEvent::TurnComplete);
    assert_eq!(app.messages.len(), 1);

    // Second turn: chunks append to the last assistant message (no user message between turns)
    let c2 = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("Turn 2")));
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(c2)));

    // Still one message — consecutive assistant chunks always merge
    assert_eq!(app.messages.len(), 1);
    if let MessageBlock::Text(t, _) =
        &app.messages.last().expect("message").blocks.last().expect("block")
    {
        assert!(t.contains("Turn 1"), "first turn text present");
        assert!(t.contains("Turn 2"), "second turn text appended");
    }
}

#[tokio::test]
async fn tool_call_content_update() {
    let mut app = test_app();

    let tc = acp::ToolCall::new("tc-content", "Read file").status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    // Update with content
    let content = vec![acp::ToolCallContent::from("file contents here")];
    let fields =
        acp::ToolCallUpdateFields::new().content(content).status(acp::ToolCallStatus::Completed);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            "tc-content",
            fields,
        ))),
    );

    let (mi, bi) = app.tool_call_index["tc-content"];
    if let MessageBlock::ToolCall(tc) = &app.messages[mi].blocks[bi] {
        assert!(!tc.content.is_empty(), "content should be set");
    } else {
        panic!("expected ToolCall block");
    }
}

// --- Auto-scroll ---

#[tokio::test]
async fn auto_scroll_maintained_during_streaming() {
    let mut app = test_app();
    assert!(app.auto_scroll);

    for _ in 0..20 {
        let chunk =
            acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("More text. ")));
        send_acp_event(
            &mut app,
            ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(chunk)),
        );
    }

    assert!(app.auto_scroll, "auto_scroll should stay true during streaming");
}

// --- Stress: many tool calls in one turn ---

#[tokio::test]
async fn stress_many_tool_calls_in_one_turn() {
    let mut app = test_app();
    app.status = AppStatus::Running;

    for i in 0..50 {
        let tc = acp::ToolCall::new(format!("stress-{i}"), format!("Op {i}"))
            .status(acp::ToolCallStatus::InProgress);
        send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));
    }

    assert_eq!(app.tool_call_index.len(), 50);

    // Complete all
    for i in 0..50 {
        let fields = acp::ToolCallUpdateFields::new().status(acp::ToolCallStatus::Completed);
        send_acp_event(
            &mut app,
            ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(
                acp::ToolCallUpdate::new(format!("stress-{i}"), fields),
            )),
        );
    }

    assert!(matches!(app.status, AppStatus::Thinking));
}

// --- CurrentModeUpdate ---

#[tokio::test]
async fn mode_update_switches_active_mode() {
    let mut app = test_app();

    // Initialize with two modes, "code" active
    app.mode = Some(claude_rust::app::ModeState {
        current_mode_id: "code".into(),
        current_mode_name: "Code".into(),
        available_modes: vec![
            claude_rust::app::ModeInfo { id: "code".into(), name: "Code".into() },
            claude_rust::app::ModeInfo { id: "plan".into(), name: "Plan".into() },
        ],
    });

    // CurrentModeUpdate switches to "plan"
    let update = acp::CurrentModeUpdate::new("plan");
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::CurrentModeUpdate(update)),
    );

    let mode = app.mode.as_ref().expect("mode should still exist");
    assert_eq!(mode.current_mode_id, "plan");
    assert_eq!(mode.current_mode_name, "Plan", "name resolved from available_modes");
    assert_eq!(mode.available_modes.len(), 2, "available_modes unchanged");
}

#[tokio::test]
async fn mode_update_unknown_id_uses_id_as_name() {
    let mut app = test_app();

    app.mode = Some(claude_rust::app::ModeState {
        current_mode_id: "code".into(),
        current_mode_name: "Code".into(),
        available_modes: vec![claude_rust::app::ModeInfo {
            id: "code".into(),
            name: "Code".into(),
        }],
    });

    // Update with an ID not in available_modes
    let update = acp::CurrentModeUpdate::new("unknown-mode");
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::CurrentModeUpdate(update)),
    );

    let mode = app.mode.as_ref().unwrap();
    assert_eq!(mode.current_mode_id, "unknown-mode");
    assert_eq!(mode.current_mode_name, "unknown-mode", "falls back to ID as name");
}

#[tokio::test]
async fn mode_update_without_mode_state_is_noop() {
    let mut app = test_app();
    assert!(app.mode.is_none());

    let update = acp::CurrentModeUpdate::new("plan-mode");
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::CurrentModeUpdate(update)),
    );

    // No crash, mode stays None since no ModeState was initialized
    assert!(app.mode.is_none());
}

// --- Edge cases: interleaved events ---

#[tokio::test]
async fn text_between_tool_calls_creates_separate_blocks() {
    let mut app = test_app();

    let c1 = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("Before tool")));
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(c1)));

    let tc = acp::ToolCall::new("tc-inter", "Read file").status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    let c2 = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("After tool")));
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(c2)));

    let tc2 = acp::ToolCall::new("tc-inter2", "Write file").status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc2)));

    let c3 = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("Final text")));
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(c3)));

    // Should be: Text, ToolCall, Text, ToolCall, Text = 5 blocks
    assert_eq!(app.messages.len(), 1);
    assert_eq!(app.messages[0].blocks.len(), 5);
    assert!(matches!(app.messages[0].blocks[0], MessageBlock::Text(..)));
    assert!(matches!(app.messages[0].blocks[1], MessageBlock::ToolCall(_)));
    assert!(matches!(app.messages[0].blocks[2], MessageBlock::Text(..)));
    assert!(matches!(app.messages[0].blocks[3], MessageBlock::ToolCall(_)));
    assert!(matches!(app.messages[0].blocks[4], MessageBlock::Text(..)));
}

#[tokio::test]
async fn rapid_turn_complete_then_new_streaming() {
    let mut app = test_app();

    // First turn
    let c1 = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("Turn 1")));
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(c1)));
    send_acp_event(&mut app, ClientEvent::TurnComplete);
    assert!(matches!(app.status, AppStatus::Ready));
    assert_eq!(app.files_accessed, 0);

    // Immediately start second turn
    let c2 = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("Turn 2")));
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(c2)));
    assert!(matches!(app.status, AppStatus::Running));

    let tc = acp::ToolCall::new("tc-t2", "Read file").status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));
    assert_eq!(app.files_accessed, 1);

    send_acp_event(&mut app, ClientEvent::TurnComplete);
    assert!(matches!(app.status, AppStatus::Ready));
    assert_eq!(app.files_accessed, 0, "reset again on second TurnComplete");
}

#[tokio::test]
async fn todowrite_replaces_previous_todos() {
    let mut app = test_app();

    // First TodoWrite with 2 items
    let raw1 = serde_json::json!({"todos": [
        {"content": "Task A", "status": "in_progress", "activeForm": "Doing A"},
        {"content": "Task B", "status": "pending", "activeForm": "Doing B"},
    ]});
    let mut meta1 = serde_json::Map::new();
    meta1.insert("claudeCode".into(), serde_json::json!({"toolName": "TodoWrite"}));
    let tc1 = acp::ToolCall::new("todo-r1", "TodoWrite")
        .status(acp::ToolCallStatus::InProgress)
        .raw_input(raw1)
        .meta(meta1);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc1)));
    assert_eq!(app.todos.len(), 2);

    // Second TodoWrite replaces with 1 item
    let raw2 = serde_json::json!({"todos": [
        {"content": "Task C", "status": "pending", "activeForm": "Doing C"},
    ]});
    let mut meta2 = serde_json::Map::new();
    meta2.insert("claudeCode".into(), serde_json::json!({"toolName": "TodoWrite"}));
    let tc2 = acp::ToolCall::new("todo-r2", "TodoWrite")
        .status(acp::ToolCallStatus::InProgress)
        .raw_input(raw2)
        .meta(meta2);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc2)));

    assert_eq!(app.todos.len(), 1, "second TodoWrite replaces first");
    assert_eq!(app.todos[0].content, "Task C");
}

#[tokio::test]
async fn available_commands_update_replaces_previous() {
    let mut app = test_app();

    let cmd1 = acp::AvailableCommand::new("/help", "Help");
    let cmd2 = acp::AvailableCommand::new("/clear", "Clear");
    let update1 = acp::AvailableCommandsUpdate::new(vec![cmd1, cmd2]);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::AvailableCommandsUpdate(update1)),
    );
    assert_eq!(app.available_commands.len(), 2);

    // New update replaces, not appends
    let cmd3 = acp::AvailableCommand::new("/commit", "Commit");
    let update2 = acp::AvailableCommandsUpdate::new(vec![cmd3]);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::AvailableCommandsUpdate(update2)),
    );
    assert_eq!(app.available_commands.len(), 1, "replaced, not appended");
}

#[tokio::test]
async fn empty_todowrite_clears_todos() {
    let mut app = test_app();

    // Set up some todos first
    let raw1 = serde_json::json!({"todos": [
        {"content": "Task A", "status": "pending", "activeForm": "Doing A"},
    ]});
    let mut meta1 = serde_json::Map::new();
    meta1.insert("claudeCode".into(), serde_json::json!({"toolName": "TodoWrite"}));
    let tc1 = acp::ToolCall::new("todo-e1", "TodoWrite")
        .status(acp::ToolCallStatus::InProgress)
        .raw_input(raw1)
        .meta(meta1);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc1)));
    assert_eq!(app.todos.len(), 1);

    // Empty TodoWrite clears
    let raw2 = serde_json::json!({"todos": []});
    let mut meta2 = serde_json::Map::new();
    meta2.insert("claudeCode".into(), serde_json::json!({"toolName": "TodoWrite"}));
    let tc2 = acp::ToolCall::new("todo-e2", "TodoWrite")
        .status(acp::ToolCallStatus::InProgress)
        .raw_input(raw2)
        .meta(meta2);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc2)));

    assert!(app.todos.is_empty(), "empty todo list clears");
}

#[tokio::test]
async fn error_during_tool_calls_leaves_tool_calls_intact() {
    let mut app = test_app();

    let c = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("working")));
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(c)));

    let tc = acp::ToolCall::new("tc-err", "Read file").status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    send_acp_event(&mut app, ClientEvent::TurnError("crashed".into()));

    assert!(matches!(app.status, AppStatus::Error));
    // Tool call should still be in the index and message
    assert!(app.tool_call_index.contains_key("tc-err"));
    assert_eq!(app.messages.len(), 1);
    assert_eq!(app.messages[0].blocks.len(), 2, "text + tool call preserved");
}

#[tokio::test]
async fn files_accessed_accumulates_across_tool_calls_in_one_turn() {
    let mut app = test_app();

    for i in 0..3 {
        let tc = acp::ToolCall::new(format!("tc-acc-{i}"), format!("Read {i}"))
            .status(acp::ToolCallStatus::InProgress);
        send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));
    }

    assert_eq!(app.files_accessed, 3, "one per tool call");
    send_acp_event(&mut app, ClientEvent::TurnComplete);
    assert_eq!(app.files_accessed, 0, "reset on turn complete");
}
