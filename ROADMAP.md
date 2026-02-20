# Roadmap

## Done

- [x] Help overlay (?) with keybinding grid
- [x] Input field line wrapping with horizontal padding and cursor tracking
- [x] Detect actual model name from ACP session/turn response instead of hardcoding
- [x] Unified edit/permission block (merged diff view and permission prompt into single block)
- [x] Todo list panel (Plan + TodoWrite updates, auto-hide when all completed)
- [x] Markdown table rendering (native Table widget with inline markdown in cells)
- [x] Smooth scrolling animation -- animate scroll position changes over 2-3 frames instead of instant jump
- [x] Selection and clipboard copy (mouse drag + Ctrl+C)
- [x] Thinking indicator between tool calls (restore AppStatus::Thinking mid-turn, render spinner at bottom of last assistant message)
- [x] `@` file/folder references in input
- [x] Run ACP connection and TUI in parallel (show TUI immediately, connect in background)
    - `Connecting` state with pulsing spinner in input area
    - Auth errors displayed in TUI with login hint banner
    - Connection failures shown as inline chat messages
- [x] Login hint: when not authenticated, show auth context text above the input field
- [x] Resize scroll fix (ground-truth height measurement via `Paragraph::line_count`)
- [x] Multi line input via ctrl v
- [x] Help commands view shipped (Keys/Slash tabs, Left/Right switching, focus tracking, loading state, single-column slash rows with wrapping)
- [x] Slash command system shipped (`/cancel`, `/compact`, `/mode`, `/model`, `/new-session`, ACP-supported filtering, unsupported system message, shared dialog logic, light-magenta slash token styling)
- [x] Input textarea replacement with `tui-textarea-2` fork (better maintained than custom implementation)
- [x] Scroll position indicator (minimal scrollbar on right edge of chat viewport)
- [x] Release checklist (v0.1.0)
    - [x] README with install instructions (build from source, usage)
    - [x] Release binary builds (GitHub Actions for Windows/macOS/Linux)
    - [x] LICENSE file (AGPL-3.0-or-later declared in Cargo.toml but needs actual file)
    - [x] CHANGELOG for the initial release

## In Progress

## Next

- [ ] Advanced commands roadmap
    - `/context`: add local command to show context remaining (%) in the footer bottom-right
    - `/context`: handle adapter usage absence gracefully (status text/fallback until usage events are available)
    - Auth command rollout: evaluate `/login` and `/logout` discoverability policy when enabled
    - Auth command rollout: investigate whether `/logout` needs `ext_method` or custom bridge logic
    - Slash UX polish: show argument/usage hints in help/autocomplete from ACP metadata where available
    - Slash reliability: add explicit parity tests for `/compact` forwarding + local history clear behavior

## Near

- [ ] User configuration system (`~/.claude-rs/config.toml` or similar)
    - Color/theme settings (currently hardcoded -- allow customizing accent, dim, error colors etc.)
    - Default model selection (avoid needing `/model` every session)
    - Edit diff display mode: always-show (default) vs collapsible with Ctrl+O
    - Tool call default state: collapsed vs expanded on arrival
    - Keybinding overrides (remap Ctrl+O, etc.)
    - Terminal behavior: scroll speed, max scrollback lines
    - Terminal box max output lines (currently hardcoded to 12, total box height 15)
    - Persist across sessions, reload on change
    
- [ ] Typed error enum (`AppError` via `thiserror`) -- currently all errors use `anyhow::Result`. A typed enum would enable distinct exit codes (e.g. 2 for `NodeNotFound`, 3 for `AdapterCrashed`), programmatic error recovery, and clearer error messages at the CLI boundary. Low priority while this is a single binary.

- [ ] Add test suite (unit tests, integration tests, CI test coverage)
    - [x] Unit tests for core modules (acp types, message parsing, state transitions)
    - [x] Integration tests for ACP connection flow
    - [ ] UI snapshot/regression tests for TUI rendering
    - [x] CI pipeline with `cargo test` and `cargo clippy`
    
- [ ] Trusted folder tracking (remember approved directories across sessions)
    - use claude native logic if possible?
  
- [ ] Populate "Recent activity" panel from session persistence

## Future

- [ ] Subagent thinking indicator
    - Show trailing `{spinner} Thinking...` inside in-progress Task tool calls
    - Mirrors existing mid-turn thinking indicator but scoped to the Agent block content area
    - Render with `│` pipe prefix (expanded) or below summary (collapsed)
    - No caching needed -- in-progress tool calls already skip cache

- [ ] Subagent (Task) tool call grouping
    - Currently subagent children render as flat top-level tool calls (no grouping)
    - Group child tool calls visually under their parent Task block
    - Collapsible Task group: expand/collapse to show/hide children
    - Track parent-child relationship via `active_task_ids` (already maintained)
    - Children must always remain interactive (permissions, content) even when visually grouped
    - Never hide children -- hiding broke permissions and tool execution

- [ ] Interactive PTY sessions (vim, less, etc.) -- requires `tui-term` + `portable-pty`

- [ ] Separate terminal panel -- currently output displays inline in chat tool call blocks

- [ ] Stdin to running processes -- not needed since ACP runs commands non-interactively, but could enable interactive workflows

### Performance (deferred from Phase 1-4 audit)

- [ ] Progressive height computation on width change -- currently a terminal resize invalidates all N message heights and recomputes them synchronously in one frame (O(n) spike). Instead:
    - Compute heights in priority order: visible range first, then expand outward each frame
    - Cap work per frame (e.g. 20-50 messages) so resize never causes a visible stall
    - Use stale (last-known) height as estimate for not-yet-recomputed messages; scroll position drifts slightly until all heights converge but stabilizes within a few frames
    - `BlockCache` already stores height per width -- stale value is immediately available as estimate, no extra storage needed
    - `update_visual_heights` already breaks early on cache hits; the fix is to also break when per-frame budget is exhausted and resume next frame

- [ ] Full virtual scrolling (Option C) -- bypass `Paragraph` entirely, render lines directly to buffer
    - Requires pre-wrapping all lines to viewport width before caching
    - Currently blocked: `tui_markdown::from_str()` has no width parameter, lines can be arbitrarily long
    - Would upgrade from O(visible_messages) to O(visible_lines) rendering

- [ ] Incremental markdown parsing -- `tui_markdown` re-parses full block on every invalidation
    - Rate-limiting invalidation is sufficient for now; true incremental requires upstream support

- [ ] Unbounded message history -- `Vec<ChatMessage>` grows without limit
    - Needs eviction/pagination design (LRU ring buffer, or offload old messages to disk)

- [ ] Synchronized terminal output (CSI ?2026h) -- low priority, ratatui diff-based rendering mitigates flicker

- [ ] `imara-diff` over `similar` -- diffs are cached so compute cost is one-time; marginal gain

- [ ] Spinner timing via `Instant::elapsed()` -- cosmetic only, decouples animation from frame rate

- [ ] Execute tool call border `repeat()` allocation -- only affects in-progress calls (skip cache)

- [ ] Table `lines().collect::<Vec>()` -- replace with `.enumerate().peekable()` iterator

- [ ] Autocomplete highlight substring clones -- only 5 visible candidates, requires `'static` lifetime relaxation

- [ ] Tokio `"full"` feature -- replace with minimal feature set to reduce compile time and binary size

## Blocked

### Token Usage & Cost Tracking

Blocked by the ACP adapter (`@zed-industries/claude-code-acp` v0.16.0) not populating usage data.

**What the ACP spec provides:**
- `UsageUpdate` streaming event -- context window usage (`used`/`size` tokens) and cumulative cost (`amount`/`currency`). Sent as a `SessionUpdate` variant during a turn.
- `PromptResponse.usage` -- per-turn token breakdown (`input_tokens`, `output_tokens`, `total_tokens`, optional `thought_tokens`, `cached_read_tokens`, `cached_write_tokens`). Returned at turn completion.

Both require the `unstable_session_usage` feature flag (already enabled in our `Cargo.toml`).

**What we verified (debug.log):**
- Zero `UsageUpdate` events received across entire sessions
- `PromptResponse.usage` is always `None`
- The adapter only sends: `AvailableCommandsUpdate`, `AgentMessageChunk`, `ToolCall`, `ToolCallUpdate`, `CurrentModeUpdate`

**Planned UI (ready to implement once adapter supports it):**
- **Header bar**: show `Xk / Yk tokens` (context window) and `$X.XX` (cumulative cost) from `UsageUpdate`
- **Per-turn inline**: show `Xk in / Xk out` in italic next to "Claude" role label from `PromptResponse.usage`

**Code still in place:**
- `UsageUpdate` and `Cost` types are re-exported in `src/acp/types.rs`
- `handle_session_update` logs `UsageUpdate` if received (debug level)
- `PromptResponse` stop_reason is logged at turn completion

**To re-enable:** restore `UsageInfo` struct + `App.usage` field, `TurnUsage` + `ChatMessage.turn_usage`, header token/cost display, and inline turn usage on the "Claude" label. See git history for the removed implementation.

### Session Resume (`--resume`) -- Blocked on adapter release

The `--resume` flag and `load_session()` code path are implemented but fail with "Session not found" on Windows due to a path encoding mismatch in the adapter's `encodeProjectPath`.

**The fix is merged upstream but not yet released:**
- [ea00a40](https://github.com/zed-industries/claude-code-acp/commit/ea00a4014af1a2e10e02f43fb391fc975e042473) adds fallback scanning across all project directories when the cwd-encoded path doesn't match
- Current npm release is 0.16.0 (does NOT include the fix)
- Check for new releases: `npm view @zed-industries/claude-code-acp version`

**What's implemented on our side:**
- `--resume <session_id>` CLI flag calls `conn.load_session()`
- Auth retry flow for `load_session()` (same pattern as `new_session()`)
- Model/mode extraction from `LoadSessionResponse`
- Session ID print on exit (so users can copy it for `--resume`) -- to be added once resume works

**How it will work once the adapter ships the fix:**
- The adapter persists sessions as JSONL at `~/.claude/projects/<encoded-cwd>/<sessionId>.jsonl`
- `loadSession` reads the file and replays history as `session/update` notifications
- Our existing `handle_session_update` handlers rebuild the conversation in the TUI
