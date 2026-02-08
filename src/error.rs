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

use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("ACP connection error: {0}")]
    Acp(#[from] anyhow::Error),

    #[error("Node.js/npx not found in PATH. Install Node.js to use claude-rust.")]
    NodeNotFound,

    #[error("ACP adapter process exited unexpectedly: {0}")]
    AdapterCrashed(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Session error: {0}")]
    Session(String),

    #[error("Terminal error: {0}")]
    Terminal(String),
}
