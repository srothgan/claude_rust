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

use agent_client_protocol::{self as acp};
use std::path::Path;
use std::process::Stdio;
use tokio::process::{Child, Command};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

pub struct AdapterProcess {
    pub child: Child,
    pub connection: acp::ClientSideConnection,
}

/// Spawn the ACP adapter as a child process and establish the connection.
///
/// `npx_path` must be the resolved absolute path to the `npx` executable
/// (use `which::which("npx")` to resolve it cross-platform).
///
/// Must be called from within a `tokio::task::LocalSet` context because
/// ACP futures are `!Send`.
pub async fn spawn_adapter(
    client: impl acp::Client + 'static,
    npx_path: &Path,
) -> anyhow::Result<AdapterProcess> {
    let mut child = Command::new(npx_path)
        .arg("@zed-industries/claude-code-acp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    let child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture adapter stdin"))?;
    let child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture adapter stdout"))?;

    let stdin_compat = child_stdin.compat_write();
    let stdout_compat = child_stdout.compat();

    let (connection, io_future) =
        acp::ClientSideConnection::new(client, stdin_compat, stdout_compat, |fut| {
            tokio::task::spawn_local(fut);
        });

    // Spawn the I/O handler on the LocalSet
    tokio::task::spawn_local(async move {
        if let Err(e) = io_future.await {
            tracing::error!("ACP I/O error: {e}");
        }
    });

    Ok(AdapterProcess { child, connection })
}
