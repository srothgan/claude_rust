# Changelog

All notable changes to this project will be documented in this file.

## [0.5.1] - 2026-02-27 [Changes][v0.5.1]

### Fixes

- **Input smoothness during rapid keys**: Restore frame rendering during non-paste active key bursts by narrowing suppression to confirmed paste bursts only; preserves paste placeholder anti-flicker behavior

## [0.5.0] - 2026-02-27 [Changes][v0.5.0]

### Features

- **Paste handling overhaul** (#53): Character-count threshold (1000 chars) replaces line-count; placeholder label updated; session identity tracking prevents append across separate pastes; burst finalization scoped to newly pasted range only
- **Turn error classification** (#54): `TurnError` strings matched against known patterns (rate limit, plan limit, max turns, quota, 429); actionable recovery hint pushed as a system message in chat; unclassified errors preserve existing behavior

### Fixes

- **Typed `AppError` enum** (#54): `NodeNotFound`, `AdapterCrashed`, `AuthRequired`, `ConnectionFailed`, `SessionNotFound` variants with per-variant exit codes and user-facing messages

### Performance

- **Unified cache budgeting + LRU history retention** (#52): Single cache budget across all message blocks; LRU eviction for long sessions; reduces memory growth on extended conversations

### UI

- **Footer three-column layout**: Update hint and context percentage now render in separate right-aligned columns simultaneously instead of either-or

## [0.4.1] - 2026-02-27 [Changes][v0.4.1]

### Fixes

- **Dynamic bridge log levels** (`client.rs`): Bridge stderr lines are now routed to the correct tracing level -- `[sdk error]`/panic lines go to `error!`, `[sdk warn]` lines to `warn!`, and ordinary SDK chatter to `debug!` -- instead of unconditionally emitting `error!` for every line
- **Height cache invalidated on interruption** (`events.rs`): `TurnComplete` and `TurnError` now call `mark_message_layout_dirty` on the tail assistant message so the height cache is re-measured after a cancelled or failed turn, fixing stale layout after interruption

## [0.4.0] - 2026-02-27 [Changes][v0.4.0]

### Features

- **Agent SDK migration** (#45, closes #23): Replace `@zed-industries/claude-code-acp` with the in-repo Agent SDK bridge; align permission suggestions with SDK session/always-allow scope
- **Session resume** (#46, closes #22): `--resume` is cwd-aware and restores full transcript state; input locked while resuming; recent sessions shown in welcome context
- **Token and cost tracking** (#47, closes #21): Footer shows live `Context: XX%`; assistant turns show per-turn `(Xk tok / $X.XX)`; compaction spinner during SDK-reported compaction
- **Slash command popovers and AskUserQuestion** (#48): Variable-input slash commands show dynamic argument popovers; full `AskUserQuestion` flow with option rendering and answer propagation

### Fixes

- **TodoWrite flicker** (#45): Ignore transient payloads without a todos array so the list no longer clears and reappears mid-turn
- **Failed Bash rendering** (#45): Compress failed tool output to a single exit-code summary line instead of the full stderr dump
- **Ctrl+C determinism** (#46): Copy only when selection is non-empty and clear it after; otherwise quit
- **Submission pipeline** (#47): Single queue gate for submissions; cancel active turn before dispatching queued action; wait for turn-settle before ready
- **Persisted tool-result normalization** (#48): Strip leading box-drawing prefixes from tool result summaries

### Performance

- **Streaming frame cost** (#49): Generation-keyed tool call measurement cache with O(1) fast path; terminal output delta-append; skip invalidation for no-op updates

### Internal

- Agent SDK bridge modularized into focused modules (`commands.ts`, `tooling.ts`, `permissions.ts`, `usage.ts`, `history.ts`, `auth.ts`, `shared.ts`) (#48)
- Perf instrumentation markers for key invalidation, measurement, and snapshot paths (#49)

## [0.3.0] - 2026-02-25 [Changes][v0.3.0]

### Features

- **Startup update check** (#30): Non-blocking check via GitHub Releases API with 24h cache, footer hint, `Ctrl+U` dismiss, `--no-update-check` / `CLAUDE_RUST_NO_UPDATE_CHECK=1` opt-out
- **Shortcuts during connecting** (#38): Navigation and help shortcuts work while ACP adapter connects; input keys remain blocked
- **Global Ctrl+Q quit** (#38): Safe quit available in all states including connecting and error
- **Input height API and word wrapping** (#40): Adopt tui-textarea-2 v0.10 `TextArea::measure()` for input sizing, switch to `WrapMode::WordOrGlyph`, remove custom `InputWrapCache` plumbing

### Fixes

- **Height cache recalculation** (#39): Track dirty message index and re-measure non-tail messages when content or tool blocks change
- **Error state and input locking** (#39): Connection and turn failures surface immediately with quit hint; input blocked during connecting/error
- **Scroll clamp after permission collapse** (#39): Clamp overscroll when content shrinks; ease scroll position for smooth settling; consume Up/Down with single pending permission
- **Permission shortcut reliability** (#29): `Ctrl+Y/A/N` work globally while prompts are pending with fallback option matching
- **Tool-call error rendering** (#29): Improved error handling with raw_output fallback and cleaner failed-call display

### CI and Dependencies

- Bump `actions/upload-artifact` 4 to 6, `actions/setup-node` 4 to 6, `actions/download-artifact` 5 to 7 (#31, #32, #33)
- Bump `pulldown-cmark` from 0.13.0 to 0.13.1 (#34)
- Unify cargo publish, binary build, GitHub release, and npm publish into one workflow (#30)
- Add `revert` to allowed semantic PR title types (#37)

### Internal

- Attempted migration to `claude-agent-acp` (#29), reverted to `claude-code-acp` (#37) due to feature parity gaps
- Regression tests for height remeasurement, scroll clamp, permission keys, connecting shortcuts, and update check

## [0.2.0] - 2026-02-22 [Changes][v0.2.0]

### Rename and Distribution

- Rename crate/package to `claude-code-rust`
- Rename command to `claude-rs`
- Update release workflows and artifacts to publish/build under the new names

## [0.1.3] - 2026-02-21 [Changes][v0.1.3]

### Fixes

- Rescan files on each `@` mention activation so new/deleted files are reflected during a session
- Add keywords to npm package.json for better discoverability

## [0.1.2] - 2026-02-21 [Changes][v0.1.2]

### UX and Interaction

- Add OS-level shutdown signal handling (`Ctrl+C`/`SIGTERM`) so external interrupts also trigger graceful TUI teardown
- Keep in-app `Ctrl+C` key behavior for selection copy versus quit, while unifying shutdown through the existing cleanup path
- Make chat scrollbar draggable with proportional thumb-to-content mapping
- Ensure scrollbar dragging can reach absolute top and bottom of chat history

## [0.1.1] - 2026-02-21 [Changes][v0.1.1]

### CI and Release

- Replace release-plz with direct cargo and npm publish workflows
- `release-cargo.yml`: publishes to crates.io on Cargo.toml version bump
- `release-npm.yml`: builds cross-platform binaries, creates verified GitHub Release, publishes to npm with provenance
- Triggers based on Cargo.toml version changes instead of tag chaining
- Tags created by github-actions[bot] for verified provenance
- Remove release-plz.toml and cliff.toml

## [0.1.0] - 2026-02-20 [Changes][v0.1.0]

### Release Summary

`Claude Code Rust` reaches a strong pre-1.0 baseline with near feature parity for core Claude Code terminal workflows:

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

- Rust crate is now publish-ready for crates.io as `claude-code-rust`
- CLI executable name is `claude-rs`
- npm global package added as `claude-code-rust`:
  - installs `claude-rs` command
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

[v0.5.1]: https://github.com/srothgan/claude-code-rust/compare/v0.5.0...v0.5.1
[v0.5.0]: https://github.com/srothgan/claude-code-rust/compare/v0.4.1...v0.5.0
[v0.4.1]: https://github.com/srothgan/claude-code-rust/compare/v0.4.0...v0.4.1
[v0.4.0]: https://github.com/srothgan/claude-code-rust/compare/v0.3.0...v0.4.0
[v0.3.0]: https://github.com/srothgan/claude-code-rust/compare/v0.2.0...v0.3.0
[v0.2.0]: https://github.com/srothgan/claude-code-rust/compare/v0.1.3...v0.2.0
[v0.1.3]: https://github.com/srothgan/claude-code-rust/compare/v0.1.2...v0.1.3
[v0.1.2]: https://github.com/srothgan/claude-code-rust/compare/v0.1.1...v0.1.2
[v0.1.1]: https://github.com/srothgan/claude-code-rust/compare/v0.1.0...v0.1.1
[v0.1.0]: https://github.com/srothgan/claude-code-rust/releases/tag/v0.1.0
