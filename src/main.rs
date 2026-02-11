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

use clap::Parser;
use claude_rust::Cli;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Write tracing logs to debug.log file so they don't corrupt the TUI.
    // Activate by setting RUST_LOG env var (e.g. RUST_LOG=debug).
    if std::env::var("RUST_LOG").is_ok()
        && let Ok(log_file) = std::fs::File::create("debug.log")
    {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(log_file)
            .with_ansi(false)
            .init();
    }

    let npx_path = which::which("npx")
        .map_err(|_| anyhow::anyhow!("Node.js/npx not found in PATH. Install Node.js first."))?;

    let rt = tokio::runtime::Runtime::new()?;
    let local_set = tokio::task::LocalSet::new();

    rt.block_on(local_set.run_until(async move {
        // Phase 1: connect (pre-TUI, errors to stderr)
        let (mut app, conn, _child, terminals) = claude_rust::app::connect(cli, npx_path).await?;

        // Phase 2: TUI event loop
        let result = claude_rust::app::run_tui(&mut app, conn).await;

        // Kill any spawned terminal child processes before exiting
        claude_rust::acp::client::kill_all_terminals(&terminals);

        result
    }))
}
