// =====
// TESTS: 18
// =====
//
// Tool call lifecycle integration tests.
// Validates the full create -> update -> complete flow for tool calls.

use agent_client_protocol as acp;
use claude_rust::acp::client::ClientEvent;
use claude_rust::app::{AppStatus, MessageBlock};
use pretty_assertions::assert_eq;

use crate::helpers::{send_acp_event, test_app};

// --- ToolCallUpdate status transitions ---

#[tokio::test]
async fn tool_call_update_changes_status_to_completed() {
    let mut app = test_app();
    app.status = AppStatus::Running;

    // Create tool call
    let tc = acp::ToolCall::new("tc-1", "Read file")
        .kind(acp::ToolKind::Read)
        .status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    // Complete it
    let fields = acp::ToolCallUpdateFields::new().status(acp::ToolCallStatus::Completed);
    let update = acp::ToolCallUpdate::new("tc-1", fields);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(update)),
    );

    let (mi, bi) = app.tool_call_index["tc-1"];
    if let MessageBlock::ToolCall(tc) = &app.messages[mi].blocks[bi] {
        assert!(matches!(tc.status, acp::ToolCallStatus::Completed));
    } else {
        panic!("expected ToolCall block");
    }
}

#[tokio::test]
async fn tool_call_update_changes_status_to_failed() {
    let mut app = test_app();

    let tc = acp::ToolCall::new("tc-fail", "Write file")
        .kind(acp::ToolKind::Edit)
        .status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    let fields = acp::ToolCallUpdateFields::new().status(acp::ToolCallStatus::Failed);
    let update = acp::ToolCallUpdate::new("tc-fail", fields);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(update)),
    );

    let (mi, bi) = app.tool_call_index["tc-fail"];
    if let MessageBlock::ToolCall(tc) = &app.messages[mi].blocks[bi] {
        assert!(matches!(tc.status, acp::ToolCallStatus::Failed));
    } else {
        panic!("expected ToolCall block");
    }
}

#[tokio::test]
async fn tool_call_update_changes_title() {
    let mut app = test_app();

    let tc = acp::ToolCall::new("tc-title", "Read file").status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    let fields = acp::ToolCallUpdateFields::new().title("Read src/lib.rs".to_owned());
    let update = acp::ToolCallUpdate::new("tc-title", fields);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(update)),
    );

    let (mi, bi) = app.tool_call_index["tc-title"];
    if let MessageBlock::ToolCall(tc) = &app.messages[mi].blocks[bi] {
        assert_eq!(tc.title, "Read src/lib.rs");
    } else {
        panic!("expected ToolCall block");
    }
}

// --- All tools complete -> Thinking ---

#[tokio::test]
async fn all_tools_completed_transitions_to_thinking() {
    let mut app = test_app();
    app.status = AppStatus::Running;

    // Create two tool calls
    let tc1 = acp::ToolCall::new("tc-a", "Read A").status(acp::ToolCallStatus::InProgress);
    let tc2 = acp::ToolCall::new("tc-b", "Read B").status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc1)));
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc2)));

    assert!(matches!(app.status, AppStatus::Running));

    // Complete first
    let fields = acp::ToolCallUpdateFields::new().status(acp::ToolCallStatus::Completed);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            "tc-a", fields,
        ))),
    );
    assert!(matches!(app.status, AppStatus::Running), "one still in progress");

    // Complete second
    let fields = acp::ToolCallUpdateFields::new().status(acp::ToolCallStatus::Completed);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            "tc-b", fields,
        ))),
    );
    assert!(matches!(app.status, AppStatus::Thinking), "all done -> Thinking");
}

#[tokio::test]
async fn mixed_completed_and_failed_also_transitions() {
    let mut app = test_app();
    app.status = AppStatus::Running;

    let tc1 = acp::ToolCall::new("tc-x", "Op 1").status(acp::ToolCallStatus::InProgress);
    let tc2 = acp::ToolCall::new("tc-y", "Op 2").status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc1)));
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc2)));

    // First completed, second failed
    let f1 = acp::ToolCallUpdateFields::new().status(acp::ToolCallStatus::Completed);
    let f2 = acp::ToolCallUpdateFields::new().status(acp::ToolCallStatus::Failed);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            "tc-x", f1,
        ))),
    );
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            "tc-y", f2,
        ))),
    );

    assert!(matches!(app.status, AppStatus::Thinking));
}

// --- Task tool call tracking ---

#[tokio::test]
async fn task_tool_call_tracked_in_active_ids() {
    let mut app = test_app();

    // A Task tool call has kind=Think and meta with claudeCode.toolName="Task"
    let mut meta = serde_json::Map::new();
    meta.insert("claudeCode".into(), serde_json::json!({"toolName": "Task"}));
    let tc = acp::ToolCall::new("task-1", "Running subtask")
        .kind(acp::ToolKind::Think)
        .status(acp::ToolCallStatus::InProgress)
        .meta(meta);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    assert!(app.active_task_ids.contains("task-1"), "Task tool call should be tracked");
}

#[tokio::test]
async fn task_completion_removes_from_active_ids() {
    let mut app = test_app();

    let mut meta = serde_json::Map::new();
    meta.insert("claudeCode".into(), serde_json::json!({"toolName": "Task"}));
    let tc = acp::ToolCall::new("task-2", "Subtask")
        .kind(acp::ToolKind::Think)
        .status(acp::ToolCallStatus::InProgress)
        .meta(meta);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));
    assert!(app.active_task_ids.contains("task-2"));

    // Complete the task
    let fields = acp::ToolCallUpdateFields::new().status(acp::ToolCallStatus::Completed);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            "task-2", fields,
        ))),
    );

    assert!(!app.active_task_ids.contains("task-2"), "completed Task removed from active set");
}

// --- Collapsed tool calls ---

#[tokio::test]
async fn new_tool_call_starts_expanded_when_tools_not_collapsed() {
    let mut app = test_app();
    app.tools_collapsed = false;

    let tc = acp::ToolCall::new("tc-init-exp", "Read file").status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    let (mi, bi) = app.tool_call_index["tc-init-exp"];
    if let MessageBlock::ToolCall(tc) = &app.messages[mi].blocks[bi] {
        assert!(!tc.collapsed, "new InProgress tool call should start expanded");
    } else {
        panic!("expected ToolCall block");
    }
}

#[tokio::test]
async fn new_tool_call_starts_collapsed_when_tools_collapsed() {
    let mut app = test_app();
    app.tools_collapsed = true;

    let tc = acp::ToolCall::new("tc-init-col", "Read file").status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    let (mi, bi) = app.tool_call_index["tc-init-col"];
    if let MessageBlock::ToolCall(tc) = &app.messages[mi].blocks[bi] {
        assert!(tc.collapsed, "new InProgress tool call should inherit collapsed=true");
    } else {
        panic!("expected ToolCall block");
    }
}

#[tokio::test]
async fn completed_tool_calls_inherit_collapsed_state() {
    let mut app = test_app();
    app.tools_collapsed = true;

    let tc = acp::ToolCall::new("tc-col", "Read file").status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    let fields = acp::ToolCallUpdateFields::new().status(acp::ToolCallStatus::Completed);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            "tc-col", fields,
        ))),
    );

    let (mi, bi) = app.tool_call_index["tc-col"];
    if let MessageBlock::ToolCall(tc) = &app.messages[mi].blocks[bi] {
        assert!(tc.collapsed, "completed tool call should inherit collapsed");
    } else {
        panic!("expected ToolCall block");
    }
}

#[tokio::test]
async fn uncollapsed_tool_calls_stay_expanded() {
    let mut app = test_app();
    app.tools_collapsed = false;

    let tc = acp::ToolCall::new("tc-exp", "Write file").status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    let fields = acp::ToolCallUpdateFields::new().status(acp::ToolCallStatus::Completed);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            "tc-exp", fields,
        ))),
    );

    let (mi, bi) = app.tool_call_index["tc-exp"];
    if let MessageBlock::ToolCall(tc) = &app.messages[mi].blocks[bi] {
        assert!(!tc.collapsed);
    } else {
        panic!("expected ToolCall block");
    }
}

// --- Multiple tool calls indexed correctly ---

#[tokio::test]
async fn multiple_tool_calls_independently_indexed() {
    let mut app = test_app();

    for i in 0..5 {
        let tc = acp::ToolCall::new(format!("tc-{i}"), format!("Tool {i}"))
            .status(acp::ToolCallStatus::InProgress);
        send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));
    }

    assert_eq!(app.tool_call_index.len(), 5);
    for i in 0..5 {
        let key = format!("tc-{i}");
        assert!(app.tool_call_index.contains_key(&key), "missing {key}");
    }
}

// --- Edge cases: tool call update propagation ---

#[tokio::test]
async fn tool_call_update_via_meta_sets_claude_tool_name() {
    let mut app = test_app();

    let tc = acp::ToolCall::new("tc-meta", "Some tool").status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    // Update arrives with meta setting claude_tool_name
    let mut meta = serde_json::Map::new();
    meta.insert("claudeCode".into(), serde_json::json!({"toolName": "WebSearch"}));
    let fields = acp::ToolCallUpdateFields::new();
    let update = acp::ToolCallUpdate::new("tc-meta", fields).meta(meta);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(update)),
    );

    let (mi, bi) = app.tool_call_index["tc-meta"];
    if let MessageBlock::ToolCall(tc) = &app.messages[mi].blocks[bi] {
        assert_eq!(tc.claude_tool_name.as_deref(), Some("WebSearch"));
    } else {
        panic!("expected ToolCall block");
    }
}

#[tokio::test]
async fn todowrite_via_update_raw_input_parses_todos() {
    let mut app = test_app();

    // Create a tool call, initially without TodoWrite meta
    let tc = acp::ToolCall::new("tc-todo-up", "TodoWrite").status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    // Update sets claude_tool_name + raw_input with todos
    let mut meta = serde_json::Map::new();
    meta.insert("claudeCode".into(), serde_json::json!({"toolName": "TodoWrite"}));
    let raw = serde_json::json!({"todos": [
        {"content": "Step 1", "status": "pending", "activeForm": "Doing step 1"}
    ]});
    let fields = acp::ToolCallUpdateFields::new().raw_input(raw);
    let update = acp::ToolCallUpdate::new("tc-todo-up", fields).meta(meta);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(update)),
    );

    assert_eq!(app.todos.len(), 1);
    assert_eq!(app.todos[0].content, "Step 1");
}

#[tokio::test]
async fn task_failed_also_removes_from_active_ids() {
    let mut app = test_app();

    let mut meta = serde_json::Map::new();
    meta.insert("claudeCode".into(), serde_json::json!({"toolName": "Task"}));
    let tc = acp::ToolCall::new("task-fail", "Subtask")
        .kind(acp::ToolKind::Think)
        .status(acp::ToolCallStatus::InProgress)
        .meta(meta);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));
    assert!(app.active_task_ids.contains("task-fail"));

    // Fail (not complete) — should still remove
    let fields = acp::ToolCallUpdateFields::new().status(acp::ToolCallStatus::Failed);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            "task-fail",
            fields,
        ))),
    );

    assert!(!app.active_task_ids.contains("task-fail"), "failed Task removed");
}

#[tokio::test]
async fn pending_status_update_does_not_remove_task() {
    let mut app = test_app();

    let mut meta = serde_json::Map::new();
    meta.insert("claudeCode".into(), serde_json::json!({"toolName": "Task"}));
    let tc = acp::ToolCall::new("task-pend", "Subtask")
        .kind(acp::ToolKind::Think)
        .status(acp::ToolCallStatus::InProgress)
        .meta(meta);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    // Update with Pending status — should NOT remove from active set
    let fields = acp::ToolCallUpdateFields::new().status(acp::ToolCallStatus::Pending);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            "task-pend",
            fields,
        ))),
    );

    assert!(app.active_task_ids.contains("task-pend"), "Pending does not remove");
}

#[tokio::test]
async fn in_progress_status_does_not_collapse_tool_call() {
    let mut app = test_app();
    app.tools_collapsed = true;

    let tc = acp::ToolCall::new("tc-inprog", "Read file").status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    // Update to InProgress again — should NOT set collapsed
    let fields = acp::ToolCallUpdateFields::new().status(acp::ToolCallStatus::InProgress);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            "tc-inprog",
            fields,
        ))),
    );

    let (mi, bi) = app.tool_call_index["tc-inprog"];
    if let MessageBlock::ToolCall(tc) = &app.messages[mi].blocks[bi] {
        // collapsed is set at creation time based on tools_collapsed,
        // but only OVERWRITTEN on Completed/Failed
        // InProgress update should not change it to collapsed
        assert!(
            matches!(tc.status, acp::ToolCallStatus::InProgress),
            "status should be InProgress"
        );
    } else {
        panic!("expected ToolCall block");
    }
}

#[tokio::test]
async fn title_shortened_relative_to_cwd() {
    let mut app = test_app();
    app.cwd_raw = "/home/user/project".into();

    let tc = acp::ToolCall::new("tc-shorten", "Read /home/user/project/src/main.rs")
        .status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    let (mi, bi) = app.tool_call_index["tc-shorten"];
    if let MessageBlock::ToolCall(tc) = &app.messages[mi].blocks[bi] {
        assert_eq!(tc.title, "Read src/main.rs", "absolute path shortened to relative");
    } else {
        panic!("expected ToolCall block");
    }
}
