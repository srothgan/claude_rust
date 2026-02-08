# claude-rs

A native Rust terminal interface for Claude Code. Drop-in replacement for Anthropic's stock Node.js/React Ink TUI, built for performance and a better user experience.

[![CI](https://github.com/srothgan/claude_rust/actions/workflows/ci.yml/badge.svg)](https://github.com/srothgan/claude_rust/actions/workflows/ci.yml)
[![License: AGPL-3.0-or-later](https://img.shields.io/badge/License-AGPL--3.0--or--later-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)
[![MSRV](https://img.shields.io/badge/MSRV-1.86.0-blue.svg)](https://blog.rust-lang.org/)

## About

claude-rs replaces the stock Claude Code terminal interface with a native Rust binary built on [Ratatui](https://ratatui.rs/). It connects to the same Claude API through the [ACP (Agent Client Protocol)](https://github.com/nicolo-ribaudo/agent-client-protocol) Rust SDK, spawning Zed's `@zed-industries/claude-code-acp` adapter as a child process. All existing Claude Code functionality — tool calls, file editing, terminal commands, permissions — works unchanged.

## Why

The stock Claude Code TUI runs on Node.js with React Ink. This causes real problems:

- **Memory**: 200-400MB baseline vs ~20-50MB for a native binary
- **Startup**: 2-5 seconds vs under 100ms
- **Scrollback**: Broken virtual scrolling that loses history
- **Input latency**: Event queue delays on keystroke handling
- **Copy/paste**: Custom implementation instead of native terminal support

claude-rs fixes all of these by compiling to a single native binary with direct terminal control via Crossterm.

## Architecture

Three-layer design:

**Presentation** (Rust/Ratatui) — Single binary with an async event loop (Tokio) handling keyboard input and ACP messages concurrently. Virtual-scrolled chat history with syntax-highlighted code blocks.

**Protocol** (ACP over stdio) — Spawns the Zed ACP adapter as a child process and communicates via JSON-RPC over stdin/stdout. Bidirectional streaming for user messages and response chunks. ACP futures are `!Send`, so all protocol code runs in a `tokio::task::LocalSet` with `mpsc` channels to the UI.

**Agent** (Zed ACP Adapter) — TypeScript/npm package by Zed Industries. Manages Claude API authentication, reads `~/.claude/config.json`, and handles tool execution.

## Prerequisites

- Rust 1.86.0+ (install via [rustup](https://rustup.rs))
- Node.js 18+ with npx (for the ACP adapter)
- Existing Claude Code authentication (`~/.claude/config.json`)

## Status

This project is in early development. See [CONTRIBUTING.md](CONTRIBUTING.md) for how to get involved.

## License

This project is licensed under the [GNU Affero General Public License v3.0 or later](LICENSE).

By using this software, you agree to the terms of the AGPL-3.0. If you modify this software and make it available over a network, you must offer the source code to users of that service.
