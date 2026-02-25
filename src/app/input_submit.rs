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
    App, AppStatus, BlockCache, ChatMessage, IncrementalMarkdown, MessageBlock, MessageRole,
};
use crate::acp::client::ClientEvent;
use crate::app::{mention, slash};
use agent_client_protocol::{self as acp, Agent as _};
use std::path::Path;
use std::rc::Rc;

pub(super) fn submit_input(app: &mut App) {
    if matches!(app.status, AppStatus::Connecting | AppStatus::Error) {
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

    if slash::try_handle_submit(app, &text) {
        return;
    }

    // New turn started by user input: force-stop stale tool calls from older turns
    // so their spinners don't continue during this turn.
    let _ = app.finalize_in_progress_tool_calls(acp::ToolCallStatus::Failed);

    let Some(ref conn) = app.conn else { return };

    // Build content blocks: text segments + embedded file resources for @mentions
    let content_blocks = build_content_blocks(&text, &app.cwd_raw);

    app.messages.push(ChatMessage {
        role: MessageRole::User,
        blocks: vec![MessageBlock::Text(
            text.clone(),
            BlockCache::default(),
            IncrementalMarkdown::from_complete(&text),
        )],
    });
    // Create empty assistant message immediately -- message.rs shows thinking indicator
    app.messages.push(ChatMessage { role: MessageRole::Assistant, blocks: Vec::new() });
    app.input.clear();
    app.status = AppStatus::Thinking;
    app.viewport.engage_auto_scroll();

    let conn = Rc::clone(conn);
    let Some(sid) = app.session_id.clone() else {
        return;
    };
    let tx = app.event_tx.clone();

    tokio::task::spawn_local(async move {
        match conn.prompt(acp::PromptRequest::new(sid, content_blocks)).await {
            Ok(resp) => {
                tracing::debug!("PromptResponse: stop_reason={:?}", resp.stop_reason);
                let _ = tx.send(ClientEvent::TurnComplete);
            }
            Err(e) => {
                let _ = tx.send(ClientEvent::TurnError(e.to_string()));
            }
        }
    });
}

/// Parse `@path` references in the text and build a mixed list of content blocks.
/// - Files are embedded as `ContentBlock::Resource(EmbeddedResource)` with full contents.
/// - Directories are sent as `ContentBlock::ResourceLink` so the agent can decide what to read.
/// - Invalid paths are kept as plain text.
fn build_content_blocks(text: &str, cwd: &str) -> Vec<acp::ContentBlock> {
    let spans = mention::find_mention_spans(text);

    if spans.is_empty() {
        return vec![acp::ContentBlock::Text(acp::TextContent::new(text))];
    }

    let cwd_path = Path::new(cwd);
    let mut blocks: Vec<acp::ContentBlock> = Vec::new();
    let mut last_end = 0;

    for (start, end, ref_path) in &spans {
        // Add preceding text as a Text block (if any)
        if *start > last_end {
            let preceding = &text[last_end..*start];
            if !preceding.is_empty() {
                blocks.push(acp::ContentBlock::Text(acp::TextContent::new(preceding)));
            }
        }

        // Strip trailing `/` from directory paths for filesystem lookup
        let clean_path = ref_path.trim_end_matches('/');
        let fs_path = cwd_path.join(clean_path);

        if fs_path.is_file() {
            match std::fs::read_to_string(&fs_path) {
                Ok(content) => {
                    let uri = path_to_file_uri(&fs_path);
                    let mime = mime_from_extension(ref_path);

                    let resource_contents =
                        acp::TextResourceContents::new(&content, &uri).mime_type(&mime);
                    let embedded = acp::EmbeddedResource::new(
                        acp::EmbeddedResourceResource::TextResourceContents(resource_contents),
                    );
                    blocks.push(acp::ContentBlock::Resource(embedded));
                }
                Err(e) => {
                    tracing::warn!("Failed to read @{ref_path}: {e}");
                    blocks
                        .push(acp::ContentBlock::Text(acp::TextContent::new(&text[*start..*end])));
                }
            }
        } else if fs_path.is_dir() {
            let uri = path_to_file_uri(&fs_path);
            let link =
                acp::ResourceLink::new(clean_path, &uri).mime_type("inode/directory".to_owned());
            blocks.push(acp::ContentBlock::ResourceLink(link));
        } else {
            // Not a valid file or directory -- keep as plain text
            blocks.push(acp::ContentBlock::Text(acp::TextContent::new(&text[*start..*end])));
        }

        last_end = *end;
    }

    // Add trailing text
    if last_end < text.len() {
        let trailing = &text[last_end..];
        if !trailing.is_empty() {
            blocks.push(acp::ContentBlock::Text(acp::TextContent::new(trailing)));
        }
    }

    blocks
}

/// Build a `file:///` URI from a filesystem path, normalizing to forward slashes.
fn path_to_file_uri(path: &Path) -> String {
    let abs_path = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/");
    format!("file:///{}", abs_path.trim_start_matches('/'))
}

/// Derive a MIME type from a file extension.
fn mime_from_extension(path: &str) -> String {
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();

    match ext.as_str() {
        "rs" => "text/x-rust",
        "py" => "text/x-python",
        "js" | "jsx" => "text/javascript",
        "ts" | "tsx" => "text/typescript",
        "json" => "application/json",
        "toml" => "text/x-toml",
        "yaml" | "yml" => "text/x-yaml",
        "md" => "text/markdown",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "sh" | "bash" => "text/x-shellscript",
        "c" | "h" | "hpp" => "text/x-c",
        "cpp" | "cc" | "cxx" => "text/x-c++",
        "go" => "text/x-go",
        "java" => "text/x-java",
        "rb" => "text/x-ruby",
        "sql" => "text/x-sql",
        "xml" => "text/xml",
        _ => "text/plain",
    }
    .to_owned()
}
