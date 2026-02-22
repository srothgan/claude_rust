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

pub mod acp;
pub mod app;
pub mod perf;
pub mod ui;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "claude-rs", about = "Native Rust terminal for Claude Code")]
#[allow(clippy::struct_excessive_bools)]
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

    /// Disable startup update checks.
    #[arg(long)]
    pub no_update_check: bool,

    /// Working directory (defaults to cwd)
    #[arg(long, short = 'C')]
    pub dir: Option<std::path::PathBuf>,

    /// Path to an ACP adapter binary (highest startup priority).
    #[arg(long)]
    pub adapter_bin: Option<std::path::PathBuf>,

    /// Write tracing diagnostics to a file (disabled unless explicitly set).
    #[arg(long, value_name = "PATH")]
    pub log_file: Option<std::path::PathBuf>,

    /// Tracing filter directives (example: `info,claude_code_rust::ui=trace`).
    /// Falls back to `RUST_LOG` when omitted.
    #[arg(long, value_name = "FILTER")]
    pub log_filter: Option<String>,

    /// Append to `--log-file` instead of truncating on startup.
    #[arg(long)]
    pub log_append: bool,

    /// Write frame performance events to a file (requires `--features perf` build).
    #[arg(long, value_name = "PATH")]
    pub perf_log: Option<std::path::PathBuf>,

    /// Append to `--perf-log` instead of truncating on startup.
    #[arg(long)]
    pub perf_append: bool,
}
