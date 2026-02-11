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

pub mod acp;
pub mod app;
pub mod ui;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "claude-rust", about = "Native Rust terminal for Claude Code")]
pub struct Cli {
    /// Override the model (sonnet, opus, haiku)
    #[arg(long, short)]
    pub model: Option<String>,

    /// Resume a previous session by ID
    #[arg(long)]
    pub resume: Option<String>,

    /// Auto-approve all tool calls (dangerous)
    #[arg(long)]
    pub yolo: bool,

    /// Working directory (defaults to cwd)
    #[arg(long, short = 'C')]
    pub dir: Option<std::path::PathBuf>,
}
