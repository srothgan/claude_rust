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

mod acp;
mod app;
mod error;
mod ui;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "claude-rust", about = "Native Rust terminal for Claude Code")]
pub struct Cli {
    /// Override the model (sonnet, opus, haiku)
    #[arg(long, short)]
    model: Option<String>,

    /// Resume the last session
    #[arg(long)]
    resume: bool,

    /// Auto-approve all tool calls (dangerous)
    #[arg(long)]
    yolo: bool,

    /// Working directory (defaults to cwd)
    #[arg(long, short = 'C')]
    dir: Option<std::path::PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let npx_path = which::which("npx")
        .map_err(|_| anyhow::anyhow!("Node.js/npx not found in PATH. Install Node.js first."))?;

    let rt = tokio::runtime::Runtime::new()?;
    let local_set = tokio::task::LocalSet::new();

    rt.block_on(local_set.run_until(async move {
        // Phase 1: connect (pre-TUI, errors to stderr)
        let (mut app, conn, _child) = app::connect(cli, npx_path).await?;

        // Phase 2: TUI event loop
        app::run_tui(&mut app, conn).await
    }))
}
