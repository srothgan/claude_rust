// =====
// TESTS: 3
// =====
//
// Internal-failure integration tests.
// Validate ACP event processing + final UI render output for failed tool calls.

use agent_client_protocol as acp;
use claude_code_rust::acp::client::ClientEvent;
use claude_code_rust::app::MessageBlock;
use pretty_assertions::assert_eq;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::helpers::{send_acp_event, test_app};

#[tokio::test]
async fn failed_tool_call_with_xml_internal_error_renders_internal_banner_and_summary() {
    let mut app = test_app();
    let tool_id = "tc-xml-internal";
    let xml_payload =
        "<error><code>-32603</code><message>Adapter process crashed</message></error>";

    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(
            acp::ToolCall::new(tool_id, "Read file")
                .kind(acp::ToolKind::Read)
                .status(acp::ToolCallStatus::InProgress),
        )),
    );

    let fields = acp::ToolCallUpdateFields::new()
        .status(acp::ToolCallStatus::Failed)
        .content(vec![acp::ToolCallContent::from(xml_payload)]);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            tool_id, fields,
        ))),
    );

    assert_eq!(tool_call_text_payload(&app, tool_id).as_deref(), Some(xml_payload));

    let frame = render_frame_to_string(&mut app, 120, 36);
    assert!(frame.contains("Internal ACP/adapter error"));
    assert!(frame.contains("Adapter process crashed"));
}

#[tokio::test]
async fn failed_tool_call_with_jsonrpc_internal_error_renders_extracted_message() {
    let mut app = test_app();
    let tool_id = "tc-jsonrpc-internal";
    let json_payload =
        r#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"internal rpc fault"}}"#;

    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(
            acp::ToolCall::new(tool_id, "Read file")
                .kind(acp::ToolKind::Read)
                .status(acp::ToolCallStatus::InProgress),
        )),
    );

    let fields = acp::ToolCallUpdateFields::new()
        .status(acp::ToolCallStatus::Failed)
        .content(vec![acp::ToolCallContent::from(json_payload)]);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            tool_id, fields,
        ))),
    );

    let frame = render_frame_to_string(&mut app, 120, 36);
    assert!(frame.contains("Internal ACP/adapter error"));
    assert!(frame.contains("internal rpc fault"));
}

#[tokio::test]
async fn failed_tool_call_with_plain_command_error_keeps_normal_rendering() {
    let mut app = test_app();
    let tool_id = "tc-plain-failure";
    let plain_payload = "bash: definitely_not_a_command: command not found";

    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCall(
            acp::ToolCall::new(tool_id, "Read file")
                .kind(acp::ToolKind::Read)
                .status(acp::ToolCallStatus::InProgress),
        )),
    );

    let fields = acp::ToolCallUpdateFields::new()
        .status(acp::ToolCallStatus::Failed)
        .content(vec![acp::ToolCallContent::from(plain_payload)]);
    send_acp_event(
        &mut app,
        ClientEvent::SessionUpdate(acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            tool_id, fields,
        ))),
    );

    let frame = render_frame_to_string(&mut app, 120, 36);
    assert!(!frame.contains("Internal ACP/adapter error"));
    assert!(frame.contains("command not found"));
}

fn tool_call_text_payload(app: &claude_code_rust::app::App, tool_id: &str) -> Option<String> {
    let (mi, bi) = app.tool_call_index.get(tool_id).copied()?;
    let MessageBlock::ToolCall(tc) = &app.messages.get(mi)?.blocks.get(bi)? else {
        return None;
    };
    tc.content.iter().find_map(|content| match content {
        acp::ToolCallContent::Content(c) => match &c.content {
            acp::ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        },
        _ => None,
    })
}

fn render_frame_to_string(app: &mut claude_code_rust::app::App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("create test terminal");
    terminal.draw(|f| claude_code_rust::ui::render(f, app)).expect("draw frame");

    let mut out = String::new();
    let buffer = terminal.backend().buffer();
    for y in 0..height {
        for x in 0..width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}
