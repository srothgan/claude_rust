use claude_code_rust::acp::client::ClientEvent;
use claude_code_rust::app::App;

/// Build a minimal `App` for integration testing.
/// No real ACP connection, no TUI -- just state.
pub fn test_app() -> App {
    App::test_default()
}

/// Helper: send an ACP event into the app's event handling pipeline.
pub fn send_acp_event(app: &mut App, event: ClientEvent) {
    claude_code_rust::app::handle_acp_event(app, event);
}
