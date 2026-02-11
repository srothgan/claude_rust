// =====
// TESTS: 27
// =====
//
// ACP event handling integration tests.
// Validates that ClientEvent variants are correctly processed into App state.

use agent_client_protocol as acp;
use claude_rust::acp::client::ClientEvent;
use claude_rust::app::{AppStatus, BlockCache, MessageBlock, MessageRole};
use pretty_assertions::assert_eq;

use crate::helpers::{send_acp_event, test_app};

// --- AgentMessageChunk ---

#[tokio::test]
async fn text_chunk_creates_assistant_message() {
    let mut app = test_app();
    assert!(app.messages.is_empty());

    let chunk = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("Hello")));
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(chunk)),
    );

    assert_eq!(app.messages.len(), 1);
    assert!(matches!(app.messages[0].role, MessageRole::Assistant));
    if let MessageBlock::Text(t, _) = &app.messages[0].blocks[0] {
        assert_eq!(t, "Hello");
    } else {
        panic!("expected Text block");
    }
}

#[tokio::test]
async fn text_chunk_appends_to_existing_assistant_message() {
    let mut app = test_app();

    let chunk1 = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("Hello")));
    let chunk2 = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(" world")));
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(chunk1)),
    );
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(chunk2)),
    );

    assert_eq!(app.messages.len(), 1, "should still be one message");
    if let MessageBlock::Text(t, _) = &app.messages[0].blocks[0] {
        assert_eq!(t, "Hello world");
    } else {
        panic!("expected Text block");
    }
}

#[tokio::test]
async fn text_chunk_sets_status_to_running() {
    let mut app = test_app();
    app.status = AppStatus::Thinking;

    let chunk = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("Hi")));
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(chunk)),
    );

    assert!(matches!(app.status, AppStatus::Running));
}

#[tokio::test]
async fn multiple_text_chunks_accumulate_in_single_block() {
    let mut app = test_app();
    for i in 0..10 {
        let chunk =
            acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(format!("{i}"))));
        send_acp_event(
            &mut app,
            ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(chunk)),
        );
    }
    assert_eq!(app.messages.len(), 1);
    assert_eq!(app.messages[0].blocks.len(), 1);
    if let MessageBlock::Text(t, _) = &app.messages[0].blocks[0] {
        assert_eq!(t, "0123456789");
    } else {
        panic!("expected Text block");
    }
}

// --- ToolCall ---

#[tokio::test]
async fn tool_call_creates_tool_block_in_assistant_message() {
    let mut app = test_app();
    app.status = AppStatus::Running;

    // First send a text chunk so an assistant message exists
    let chunk = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(
        "Let me read that file.",
    )));
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(chunk)),
    );

    let tc = acp::ToolCall::new("tc-1", "Read src/main.rs")
        .kind(acp::ToolKind::Read)
        .status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    assert_eq!(app.messages.len(), 1, "tool call added to existing message");
    assert_eq!(app.messages[0].blocks.len(), 2, "text + tool call");
    assert!(matches!(app.messages[0].blocks[1], MessageBlock::ToolCall(_)));
}

#[tokio::test]
async fn tool_call_without_prior_message_creates_new_assistant_message() {
    let mut app = test_app();

    let tc = acp::ToolCall::new("tc-1", "Read file")
        .kind(acp::ToolKind::Read)
        .status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    assert_eq!(app.messages.len(), 1);
    assert!(matches!(app.messages[0].role, MessageRole::Assistant));
    assert!(matches!(app.messages[0].blocks[0], MessageBlock::ToolCall(_)));
}

#[tokio::test]
async fn tool_call_is_indexed_for_lookup() {
    let mut app = test_app();

    let tc = acp::ToolCall::new("tc-42", "Read something")
        .kind(acp::ToolKind::Read)
        .status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    assert!(app.tool_call_index.contains_key("tc-42"), "tool call should be indexed");
    let (mi, bi) = app.tool_call_index["tc-42"];
    assert_eq!(mi, 0);
    assert_eq!(bi, 0);
}

// --- TurnComplete ---

#[tokio::test]
async fn turn_complete_sets_status_ready() {
    let mut app = test_app();
    app.status = AppStatus::Thinking;

    send_acp_event(&mut app, ClientEvent::TurnComplete);

    assert!(matches!(app.status, AppStatus::Ready));
}

#[tokio::test]
async fn turn_complete_resets_files_accessed() {
    let mut app = test_app();
    app.files_accessed = 5;

    send_acp_event(&mut app, ClientEvent::TurnComplete);

    assert_eq!(app.files_accessed, 0);
}

// --- TurnError ---

#[tokio::test]
async fn turn_error_sets_status_error() {
    let mut app = test_app();
    app.status = AppStatus::Thinking;

    send_acp_event(&mut app, ClientEvent::TurnError("something broke".into()));

    assert!(matches!(app.status, AppStatus::Error));
}

#[tokio::test]
async fn turn_error_does_not_create_message() {
    let mut app = test_app();

    send_acp_event(&mut app, ClientEvent::TurnError("connection lost".into()));

    // TurnError only sets status — error is logged, not shown in chat
    assert!(app.messages.is_empty(), "TurnError should not create a message");
}

// --- AgentThoughtChunk ---

#[tokio::test]
async fn agent_thought_sets_thinking_status() {
    let mut app = test_app();
    app.status = AppStatus::Ready;

    let chunk =
        acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("Let me think...")));
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::AgentThoughtChunk(chunk)),
    );

    assert!(matches!(app.status, AppStatus::Thinking));
}

#[tokio::test]
async fn agent_thought_chunks_are_not_stored_in_messages() {
    let mut app = test_app();

    for i in 0..5 {
        let chunk = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(
            format!("Thought {i}"),
        )));
        send_acp_event(
            &mut app,
            ClientEvent::SessionUpdate(acp::SessionUpdate::AgentThoughtChunk(chunk)),
        );
    }

    // Thoughts only set status, they are not stored in messages
    assert!(app.messages.is_empty(), "thought chunks should not create messages");
    assert!(matches!(app.status, AppStatus::Thinking));
}

// --- AvailableCommandsUpdate ---

#[tokio::test]
async fn available_commands_update_stores_commands() {
    let mut app = test_app();
    assert!(app.available_commands.is_empty());

    let cmd = acp::AvailableCommand::new("/help", "Show help");
    let update = acp::AvailableCommandsUpdate::new(vec![cmd]);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::AvailableCommandsUpdate(update)),
    );

    assert_eq!(app.available_commands.len(), 1);
}

// --- Edge cases: AgentMessageChunk ---

#[tokio::test]
async fn non_text_content_block_is_ignored() {
    let mut app = test_app();

    let chunk = acp::ContentChunk::new(acp::ContentBlock::Image(acp::ImageContent::new(
        "base64data",
        "image/png",
    )));
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(chunk)),
    );

    // Image content blocks are silently skipped
    // NOTE: update this test when image block support is added
    assert!(app.messages.is_empty());
    assert!(matches!(app.status, AppStatus::Ready), "status unchanged for non-text");
}

#[tokio::test]
async fn text_chunk_after_tool_call_creates_new_text_block() {
    let mut app = test_app();

    // Text -> ToolCall -> Text should produce [Text, ToolCall, Text]
    let c1 = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("Before")));
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(c1)));

    let tc = acp::ToolCall::new("tc-mid", "Read file").status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    let c2 = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("After")));
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(c2)));

    assert_eq!(app.messages[0].blocks.len(), 3);
    assert!(matches!(app.messages[0].blocks[0], MessageBlock::Text(..)));
    assert!(matches!(app.messages[0].blocks[1], MessageBlock::ToolCall(_)));
    // Text after a tool call becomes a NEW text block (can't append to ToolCall)
    assert!(matches!(app.messages[0].blocks[2], MessageBlock::Text(..)));
}

#[tokio::test]
async fn empty_text_chunk_still_creates_message() {
    let mut app = test_app();

    let chunk = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("")));
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(chunk)),
    );

    // Even empty text creates a message and sets Running
    assert_eq!(app.messages.len(), 1);
    assert!(matches!(app.status, AppStatus::Running));
}

#[tokio::test]
async fn text_chunk_after_user_message_creates_new_assistant_message() {
    let mut app = test_app();

    // Simulate a user message already in the chat
    app.messages.push(claude_rust::app::ChatMessage {
        role: MessageRole::User,
        blocks: vec![MessageBlock::Text("user question".into(), BlockCache::default())],
        cached_visual_height: 0,
        cached_visual_width: 0,
    });

    let chunk = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("answer")));
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(chunk)),
    );

    // Should NOT append to user message — creates new assistant message
    assert_eq!(app.messages.len(), 2);
    assert!(matches!(app.messages[0].role, MessageRole::User));
    assert!(matches!(app.messages[1].role, MessageRole::Assistant));
}

// --- Edge cases: ToolCall ---

#[tokio::test]
async fn duplicate_tool_call_id_updates_existing() {
    let mut app = test_app();

    // First: send text so assistant message exists
    let chunk = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("text")));
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::AgentMessageChunk(chunk)),
    );

    let tc1 = acp::ToolCall::new("dup-1", "Read file v1").status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc1)));

    // Same ID arrives again with different title
    let tc2 = acp::ToolCall::new("dup-1", "Read file v2").status(acp::ToolCallStatus::Completed);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc2)));

    // Should update in-place, not create a second block
    assert_eq!(app.messages[0].blocks.len(), 2, "text + one tool call, not two");
    if let MessageBlock::ToolCall(tc) = &app.messages[0].blocks[1] {
        assert_eq!(tc.title, "Read file v2");
        assert!(matches!(tc.status, acp::ToolCallStatus::Completed));
    } else {
        panic!("expected ToolCall block");
    }
}

#[tokio::test]
async fn tool_call_after_user_message_creates_assistant_message() {
    let mut app = test_app();

    app.messages.push(claude_rust::app::ChatMessage {
        role: MessageRole::User,
        blocks: vec![MessageBlock::Text("question".into(), BlockCache::default())],
        cached_visual_height: 0,
        cached_visual_width: 0,
    });

    let tc =
        acp::ToolCall::new("tc-after-user", "Read file").status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    // Should create new assistant message, not attach to user message
    assert_eq!(app.messages.len(), 2);
    assert!(matches!(app.messages[1].role, MessageRole::Assistant));
    assert!(app.tool_call_index.contains_key("tc-after-user"));
}

#[tokio::test]
async fn tool_call_increments_files_accessed() {
    let mut app = test_app();
    assert_eq!(app.files_accessed, 0);

    let tc = acp::ToolCall::new("tc-count", "Read file").status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    assert_eq!(app.files_accessed, 1);
}

// --- Edge cases: ToolCallUpdate ---

#[tokio::test]
async fn update_for_unknown_tool_call_is_silent_noop() {
    let mut app = test_app();

    let fields = acp::ToolCallUpdateFields::new().status(acp::ToolCallStatus::Completed);
    let update = acp::ToolCallUpdate::new("nonexistent-id", fields);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(update)),
    );

    // No crash, no messages created
    assert!(app.messages.is_empty());
    assert!(app.tool_call_index.is_empty());
}

#[tokio::test]
async fn update_with_empty_fields_is_noop() {
    let mut app = test_app();

    let tc =
        acp::ToolCall::new("tc-empty-update", "Read file").status(acp::ToolCallStatus::InProgress);
    send_acp_event(&mut app, ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(tc)));

    // Send update with no fields set
    let fields = acp::ToolCallUpdateFields::new();
    let update = acp::ToolCallUpdate::new("tc-empty-update", fields);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(update)),
    );

    // Tool call should be unchanged
    let (mi, bi) = app.tool_call_index["tc-empty-update"];
    if let MessageBlock::ToolCall(tc) = &app.messages[mi].blocks[bi] {
        assert_eq!(tc.title, "Read file");
        assert!(matches!(tc.status, acp::ToolCallStatus::InProgress));
    } else {
        panic!("expected ToolCall block");
    }
}

// --- Edge cases: TurnComplete/TurnError ---

#[tokio::test]
async fn double_turn_complete_stays_ready() {
    let mut app = test_app();
    app.status = AppStatus::Thinking;

    send_acp_event(&mut app, ClientEvent::TurnComplete);
    assert!(matches!(app.status, AppStatus::Ready));

    send_acp_event(&mut app, ClientEvent::TurnComplete);
    assert!(matches!(app.status, AppStatus::Ready), "double TurnComplete is harmless");
}

#[tokio::test]
async fn turn_error_after_turn_complete() {
    let mut app = test_app();
    app.status = AppStatus::Thinking;

    send_acp_event(&mut app, ClientEvent::TurnComplete);
    assert!(matches!(app.status, AppStatus::Ready));

    send_acp_event(&mut app, ClientEvent::TurnError("late error".into()));
    assert!(matches!(app.status, AppStatus::Error), "error overwrites ready");
}
