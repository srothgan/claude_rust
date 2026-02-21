# Changelog

All notable changes to this project will be documented in this file.

## [0.1.2] - 2026-02-21

### UX and Interaction

- Add OS-level shutdown signal handling (`Ctrl+C`/`SIGTERM`) so external interrupts also trigger graceful TUI teardown
- Keep in-app `Ctrl+C` key behavior for selection copy versus quit, while unifying shutdown through the existing cleanup path
- Make chat scrollbar draggable with proportional thumb-to-content mapping
- Ensure scrollbar dragging can reach absolute top and bottom of chat history

## [0.1.1] - 2026-02-21

### CI and Release

- Replace release-plz with direct cargo and npm publish workflows
- `release-cargo.yml`: publishes to crates.io on Cargo.toml version bump
- `release-npm.yml`: builds cross-platform binaries, creates verified GitHub Release, publishes to npm with provenance
- Triggers based on Cargo.toml version changes instead of tag chaining
- Tags created by github-actions[bot] for verified provenance
- Remove release-plz.toml and cliff.toml

## [0.1.0] - 2026-02-20

### Release Summary

`claude-rust` reaches a strong pre-1.0 baseline with near feature parity for core Claude Code terminal workflows:

- Native Rust TUI built with Ratatui and Crossterm
- ACP protocol integration via `@zed-industries/claude-code-acp`
- Streaming chat, tool calls, permissions, diffs, and terminal command output
- Modern input UX (multiline, paste burst handling, mentions, slash commands)
- Substantial rendering and scrolling performance work for long sessions
- Broad unit and integration test coverage across app state, events, permissions, and UI paths

The only major parity gap intentionally excluded from this release is token/cost usage display because the upstream ACP adapter currently does not emit usage data.

### Architecture And Tooling

- Three-layer runtime design:
  - Presentation: Rust + Ratatui
  - Protocol: ACP over stdio
  - Agent: Zed ACP adapter process
- Async runtime and event handling:
  - Tokio runtime with ACP work kept on `LocalSet` (`!Send` futures)
  - `mpsc` channels between ACP client events and UI state machine
- CLI and platform support:
  - Clap-based CLI (`--model`, `--resume`, `--yolo`, `-C`, adapter/log/perf flags)
  - Cross-platform adapter launcher fallback (explicit path, env path, global bin, npx)
  - Windows-safe process resolution via `which`

### Core Features

- Chat and rendering:
  - Native markdown rendering including tables
  - Inline code/diff presentation and tool-call block rendering
  - Welcome/system/tool content unified in normal chat flow
- Input and commands:
  - `tui-textarea-2` powered editor path
  - Multiline paste placeholder pipeline and burst detection
  - `@` file/folder mention autocomplete with resource embedding
  - Slash command workflow with ACP-backed filtering and help integration
- Tool execution UX:
  - Unified inline permission controls inside tool-call blocks
  - Focus-aware keyboard routing for mention, todo, and permission contexts
  - Better interruption semantics and stale spinner cleanup
  - Internal ACP/adapter failures rendered distinctly from normal command failures
- Session and app UX:
  - Parallel startup (TUI appears immediately while ACP connects in background)
  - In-TUI connecting/auth failure messaging and login hinting
  - Header model/location/branch context
  - Help overlay and shortcut discoverability improvements
  - Mouse selection and clipboard copy support
  - Smooth chat scroll and minimal scroll position indicator

### Performance Work

Performance optimization was a major release theme across recent commits:

- Block-level render caching and deduplicated markdown parsing
- Incremental markdown handling in streaming scenarios
- Prefix sums + binary search for first visible message
- Viewport culling for long-chat scaling
- Ground-truth height measurement and improved resize correctness
- Conditional redraw paths and optional perf diagnostics logging
- Additional targeted UI smoothing for scroll and scrollbar transitions

### Reliability, Quality, And Tests

- Significant test investment across both unit and integration layers
- Current codebase includes over 400 Rust `#[test]` cases
- Dedicated integration suites for ACP events, tool lifecycle, permissions, state transitions, and internal failure rendering
- CI includes test, clippy (`-D warnings`), fmt, MSRV, and lockfile checks

### Release And Distribution Setup

- Rust crate is now publish-ready for crates.io as `claude-rust`
- CLI executable name is `claude-rust`
- npm global package added as `claude-rust`:
  - installs `claude-rust` command
  - downloads matching GitHub release binary during `postinstall`
- Tag-based GitHub Actions release workflow added for:
  - cross-platform binary builds (Windows/macOS/Linux)
  - GitHub release asset publishing
  - npm publishing (when `NPM_TOKEN` is configured)
- `release-plz` remains in place for release PR automation and changelog/version workflows

### Known Limitations

- Slash command availability is intentionally conservative for this release:
  - `/login` and `/logout` are not offered
  - they remain excluded until ACP/Zed support is reliable enough for production use
- Token usage and cost tracking is blocked by current ACP adapter behavior:
  - `UsageUpdate` events are not emitted
  - `PromptResponse.usage` is `None`
- Session resume (`--resume`) is blocked on an upstream adapter release that contains a Windows path encoding fix
