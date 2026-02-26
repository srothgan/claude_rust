import { randomUUID } from "node:crypto";
import { spawn as spawnChild } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import readline from "node:readline";
import { pathToFileURL } from "node:url";
import {
  query,
  type CanUseTool,
  type PermissionMode,
  type PermissionRuleValue,
  type PermissionResult,
  type PermissionUpdate,
  type Query,
  type SDKMessage,
  type SDKUserMessage,
} from "@anthropic-ai/claude-agent-sdk";
import type {
  AvailableCommand,
  BridgeCommand,
  BridgeCommandEnvelope,
  BridgeEvent,
  BridgeEventEnvelope,
  Json,
  ModeInfo,
  ModeState,
  PermissionOutcome,
  PermissionOption,
  PermissionRequest,
  PlanEntry,
  SessionUpdate,
  ToolCall,
  ToolCallUpdate,
  ToolCallUpdateFields,
} from "./types.js";

type ConnectEventKind = "connected" | "session_replaced";

type PendingPermission = {
  resolve: (result: PermissionResult) => void;
  toolName: string;
  inputData: Record<string, unknown>;
  suggestions?: PermissionUpdate[];
};

type PermissionSuggestionsByScope = {
  session: PermissionUpdate[];
  persistent: PermissionUpdate[];
};

type SessionState = {
  sessionId: string;
  cwd: string;
  model: string;
  mode: PermissionMode;
  yolo: boolean;
  query: Query;
  input: AsyncQueue<SDKUserMessage>;
  connected: boolean;
  connectEvent: ConnectEventKind;
  connectRequestId?: string;
  toolCalls: Map<string, ToolCall>;
  taskToolUseIds: Map<string, string>;
  pendingPermissions: Map<string, PendingPermission>;
  authHintSent: boolean;
  lastTotalCostUsd?: number;
  sessionsToCloseAfterConnect?: SessionState[];
  resumeUpdates?: SessionUpdate[];
};

type PersistedSessionEntry = {
  session_id: string;
  cwd: string;
  file_path: string;
  title?: string;
  updated_at?: string;
  sort_ms: number;
};

const MODE_NAMES: Record<PermissionMode, string> = {
  default: "Default",
  acceptEdits: "Accept Edits",
  bypassPermissions: "Bypass Permissions",
  plan: "Plan",
  dontAsk: "Don't Ask",
};

const MODE_OPTIONS: ModeInfo[] = [
  { id: "default", name: "Default", description: "Standard permission flow" },
  { id: "acceptEdits", name: "Accept Edits", description: "Auto-approve edit operations" },
  { id: "plan", name: "Plan", description: "No tool execution" },
  { id: "dontAsk", name: "Don't Ask", description: "Reject non-approved tools" },
  { id: "bypassPermissions", name: "Bypass Permissions", description: "Auto-approve all tools" },
];

const TOOL_RESULT_TYPES = new Set([
  "tool_result",
  "tool_search_tool_result",
  "web_fetch_tool_result",
  "web_search_tool_result",
  "code_execution_tool_result",
  "bash_code_execution_tool_result",
  "text_editor_code_execution_tool_result",
  "mcp_tool_result",
]);

const sessions = new Map<string, SessionState>();
const permissionDebugEnabled =
  process.env.CLAUDE_RS_SDK_PERMISSION_DEBUG === "1" || process.env.CLAUDE_RS_SDK_DEBUG === "1";
const SESSION_PERMISSION_DESTINATIONS = new Set(["session", "cliArg"]);
const PERSISTENT_PERMISSION_DESTINATIONS = new Set(["userSettings", "projectSettings", "localSettings"]);

function formatPermissionRule(rule: PermissionRuleValue): string {
  return rule.ruleContent === undefined ? rule.toolName : `${rule.toolName}(${rule.ruleContent})`;
}

function formatPermissionUpdates(updates: PermissionUpdate[] | undefined): string {
  if (!updates || updates.length === 0) {
    return "<none>";
  }
  return updates
    .map((update) => {
      if (update.type === "addRules" || update.type === "replaceRules" || update.type === "removeRules") {
        const rules = update.rules.map((rule) => formatPermissionRule(rule)).join(", ");
        return `${update.type}:${update.behavior}:${update.destination}=[${rules}]`;
      }
      if (update.type === "setMode") {
        return `${update.type}:${update.mode}:${update.destination}`;
      }
      return `${update.type}:${update.destination}=[${update.directories.join(", ")}]`;
    })
    .join(" | ");
}

function logPermissionDebug(message: string): void {
  if (!permissionDebugEnabled) {
    return;
  }
  console.error(`[perm debug] ${message}`);
}

function splitPermissionSuggestionsByScope(
  suggestions: PermissionUpdate[] | undefined,
): PermissionSuggestionsByScope {
  if (!suggestions || suggestions.length === 0) {
    return { session: [], persistent: [] };
  }

  const session: PermissionUpdate[] = [];
  const persistent: PermissionUpdate[] = [];
  for (const suggestion of suggestions) {
    if (SESSION_PERMISSION_DESTINATIONS.has(suggestion.destination)) {
      session.push(suggestion);
      continue;
    }
    if (PERSISTENT_PERMISSION_DESTINATIONS.has(suggestion.destination)) {
      persistent.push(suggestion);
      continue;
    }
    // Keep forward-compatible behavior: unknown destinations behave as session-scoped.
    session.push(suggestion);
  }
  return { session, persistent };
}

function asRecordOrNull(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return null;
  }
  return value as Record<string, unknown>;
}

function normalizeUserPromptText(raw: string): string {
  // Keep only user-readable prompt text; strip heavy inline context blobs.
  let text = raw.replace(/<context[\s\S]*/gi, " ");
  text = text.replace(/\[([^\]]+)\]\([^)]+\)/g, "$1");
  text = text.replace(/\s+/g, " ").trim();
  return text;
}

function truncateTextByChars(text: string, maxChars: number): string {
  const chars = Array.from(text);
  if (chars.length <= maxChars) {
    return text;
  }
  return chars.slice(0, maxChars).join("");
}

function firstUserMessageTitleFromRecord(record: Record<string, unknown>): string | undefined {
  if (record.type !== "user") {
    return undefined;
  }
  const message = asRecordOrNull(record.message);
  if (!message || message.role !== "user" || !Array.isArray(message.content)) {
    return undefined;
  }

  const parts: string[] = [];
  for (const item of message.content) {
    const block = asRecordOrNull(item);
    if (!block || block.type !== "text" || typeof block.text !== "string") {
      continue;
    }
    const cleaned = normalizeUserPromptText(block.text);
    if (!cleaned) {
      continue;
    }
    parts.push(cleaned);
    const combined = parts.join(" ");
    if (Array.from(combined).length >= 180) {
      return truncateTextByChars(combined, 180);
    }
  }

  if (parts.length === 0) {
    return undefined;
  }
  return truncateTextByChars(parts.join(" "), 180);
}

function extractSessionPreviewFromJsonl(filePath: string): { cwd?: string; title?: string } {
  let text: string;
  try {
    text = fs.readFileSync(filePath, "utf8");
  } catch {
    return {};
  }

  let cwd: string | undefined;
  let title: string | undefined;
  const lines = text.split(/\r?\n/);
  for (const rawLine of lines) {
    const line = rawLine.trim();
    if (line.length === 0) {
      continue;
    }
    let parsed: unknown;
    try {
      parsed = JSON.parse(line);
    } catch {
      continue;
    }
    const record = asRecordOrNull(parsed);
    if (!record) {
      continue;
    }

    if (!cwd && typeof record.cwd === "string" && record.cwd.trim().length > 0) {
      cwd = record.cwd;
    }
    if (!title) {
      title = firstUserMessageTitleFromRecord(record);
    }
    if (cwd && title) {
      break;
    }
  }

  return {
    ...(cwd ? { cwd } : {}),
    ...(title ? { title } : {}),
  };
}

function listRecentPersistedSessions(limit = 8): PersistedSessionEntry[] {
  const root = path.join(os.homedir(), ".claude", "projects");
  if (!fs.existsSync(root)) {
    return [];
  }

  const candidates: PersistedSessionEntry[] = [];
  let projectDirs: fs.Dirent[];
  try {
    projectDirs = fs.readdirSync(root, { withFileTypes: true }).filter((dirent) => dirent.isDirectory());
  } catch {
    return [];
  }

  for (const dirent of projectDirs) {
    const projectDir = path.join(root, dirent.name);

    let sessionFiles: fs.Dirent[];
    try {
      sessionFiles = fs
        .readdirSync(projectDir, { withFileTypes: true })
        .filter((entry) => entry.isFile() && entry.name.endsWith(".jsonl"));
    } catch {
      sessionFiles = [];
    }

    for (const sessionFile of sessionFiles) {
      const sessionId = sessionFile.name.slice(0, -".jsonl".length);
      if (!sessionId) {
        continue;
      }
      let mtimeMs = 0;
      try {
        mtimeMs = fs.statSync(path.join(projectDir, sessionFile.name)).mtimeMs;
      } catch {
        continue;
      }
      if (!Number.isFinite(mtimeMs) || mtimeMs <= 0) {
        continue;
      }
      candidates.push({
        session_id: sessionId,
        cwd: "",
        file_path: path.join(projectDir, sessionFile.name),
        updated_at: new Date(mtimeMs).toISOString(),
        sort_ms: mtimeMs,
      });
    }
  }

  candidates.sort((a, b) => b.sort_ms - a.sort_ms);
  const deduped: PersistedSessionEntry[] = [];
  const seen = new Set<string>();
  for (const candidate of candidates) {
    if (seen.has(candidate.session_id)) {
      continue;
    }
    seen.add(candidate.session_id);

    const preview = extractSessionPreviewFromJsonl(candidate.file_path);
    const cwd = preview.cwd?.trim();
    if (!cwd) {
      continue;
    }

    deduped.push({
      session_id: candidate.session_id,
      cwd,
      file_path: candidate.file_path,
      ...(preview.title ? { title: preview.title } : {}),
      ...(candidate.updated_at ? { updated_at: candidate.updated_at } : {}),
      sort_ms: candidate.sort_ms,
    });
    if (deduped.length >= limit) {
      break;
    }
  }
  return deduped;
}

function isToolUseBlockType(blockType: string): boolean {
  return blockType === "tool_use" || blockType === "server_tool_use" || blockType === "mcp_tool_use";
}

function persistedMessageCandidates(record: Record<string, unknown>): Record<string, unknown>[] {
  const candidates: Record<string, unknown>[] = [];

  const topLevel = asRecordOrNull(record.message);
  if (topLevel) {
    candidates.push(topLevel);
  }

  const nested = asRecordOrNull(asRecordOrNull(asRecordOrNull(record.data)?.message)?.message);
  if (nested) {
    candidates.push(nested);
  }

  return candidates;
}

function pushResumeTextChunk(updates: SessionUpdate[], role: "user" | "assistant", text: string): void {
  if (!text.trim()) {
    return;
  }
  if (role === "assistant") {
    updates.push({ type: "agent_message_chunk", content: { type: "text", text } });
    return;
  }
  updates.push({ type: "user_message_chunk", content: { type: "text", text } });
}

function pushResumeToolUse(
  updates: SessionUpdate[],
  toolCalls: Map<string, ToolCall>,
  block: Record<string, unknown>,
): void {
  const toolUseId = typeof block.id === "string" ? block.id : "";
  if (!toolUseId) {
    return;
  }
  const name = typeof block.name === "string" ? block.name : "Tool";
  const input = asRecordOrNull(block.input) ?? {};

  const toolCall = createToolCall(toolUseId, name, input);
  toolCall.status = "in_progress";
  toolCalls.set(toolUseId, toolCall);
  updates.push({ type: "tool_call", tool_call: toolCall });
}

function pushResumeToolResult(
  updates: SessionUpdate[],
  toolCalls: Map<string, ToolCall>,
  block: Record<string, unknown>,
): void {
  const toolUseId = typeof block.tool_use_id === "string" ? block.tool_use_id : "";
  if (!toolUseId) {
    return;
  }
  const isError = Boolean(block.is_error);
  const base = toolCalls.get(toolUseId);
  const fields = buildToolResultFields(isError, block.content, base);
  updates.push({ type: "tool_call_update", tool_call_update: { tool_call_id: toolUseId, fields } });

  if (!base) {
    return;
  }
  base.status = fields.status ?? base.status;
  if (fields.raw_output) {
    base.raw_output = fields.raw_output;
  }
  if (fields.content) {
    base.content = fields.content;
  }
}

function pushResumeUsageUpdate(
  updates: SessionUpdate[],
  message: Record<string, unknown>,
  emittedUsageMessageIds: Set<string>,
): void {
  const messageId = typeof message.id === "string" ? message.id : "";
  if (messageId && emittedUsageMessageIds.has(messageId)) {
    return;
  }

  const usageUpdate = buildUsageUpdateFromResult(message);
  if (!usageUpdate) {
    return;
  }

  updates.push(usageUpdate);
  if (messageId) {
    emittedUsageMessageIds.add(messageId);
  }
}

export function extractSessionHistoryUpdatesFromJsonl(filePath: string): SessionUpdate[] {
  let text: string;
  try {
    text = fs.readFileSync(filePath, "utf8");
  } catch {
    return [];
  }

  const updates: SessionUpdate[] = [];
  const toolCalls = new Map<string, ToolCall>();
  const emittedUsageMessageIds = new Set<string>();
  const lines = text.split(/\r?\n/);
  for (const rawLine of lines) {
    const line = rawLine.trim();
    if (line.length === 0) {
      continue;
    }
    let parsed: unknown;
    try {
      parsed = JSON.parse(line);
    } catch {
      continue;
    }
    const record = asRecordOrNull(parsed);
    if (!record) {
      continue;
    }
    for (const message of persistedMessageCandidates(record)) {
      const role = message.role;
      if (role !== "user" && role !== "assistant") {
        continue;
      }
      const content = Array.isArray(message.content) ? message.content : [];
      for (const item of content) {
        const block = asRecordOrNull(item);
        if (!block) {
          continue;
        }
        const blockType = typeof block.type === "string" ? block.type : "";
        if (blockType === "thinking") {
          continue;
        }
        if (blockType === "text" && typeof block.text === "string") {
          pushResumeTextChunk(updates, role, block.text);
          continue;
        }
        if (isToolUseBlockType(blockType) && role === "assistant") {
          pushResumeToolUse(updates, toolCalls, block);
          continue;
        }
        if (TOOL_RESULT_TYPES.has(blockType)) {
          pushResumeToolResult(updates, toolCalls, block);
          continue;
        }
        if (blockType === "image") {
          pushResumeTextChunk(updates, role, "[image]");
        }
      }
      pushResumeUsageUpdate(updates, message, emittedUsageMessageIds);
    }
  }
  return updates;
}

function resolvePersistedSessionEntry(sessionId: string): PersistedSessionEntry | null {
  if (
    sessionId.trim().length === 0 ||
    sessionId.includes("/") ||
    sessionId.includes("\\") ||
    sessionId.includes("..")
  ) {
    return null;
  }
  const root = path.join(os.homedir(), ".claude", "projects");
  if (!fs.existsSync(root)) {
    return null;
  }

  let projectDirs: fs.Dirent[];
  try {
    projectDirs = fs.readdirSync(root, { withFileTypes: true }).filter((dirent) => dirent.isDirectory());
  } catch {
    return null;
  }

  let best: PersistedSessionEntry | null = null;
  for (const dirent of projectDirs) {
    const filePath = path.join(root, dirent.name, `${sessionId}.jsonl`);
    if (!fs.existsSync(filePath)) {
      continue;
    }
    const preview = extractSessionPreviewFromJsonl(filePath);
    const cwd = preview.cwd?.trim();
    if (!cwd) {
      continue;
    }

    let mtimeMs = 0;
    try {
      mtimeMs = fs.statSync(filePath).mtimeMs;
    } catch {
      mtimeMs = 0;
    }
    if (!best || mtimeMs >= best.sort_ms) {
      best = {
        session_id: sessionId,
        cwd,
        file_path: filePath,
        sort_ms: mtimeMs,
      };
    }
  }
  return best;
}

class AsyncQueue<T> implements AsyncIterable<T> {
  private readonly items: T[] = [];
  private readonly waiters: Array<(result: IteratorResult<T>) => void> = [];
  private closed = false;

  enqueue(item: T): void {
    if (this.closed) {
      return;
    }
    const waiter = this.waiters.shift();
    if (waiter) {
      waiter({ value: item, done: false });
      return;
    }
    this.items.push(item);
  }

  close(): void {
    if (this.closed) {
      return;
    }
    this.closed = true;
    while (this.waiters.length > 0) {
      const waiter = this.waiters.shift();
      waiter?.({ value: undefined, done: true });
    }
  }

  [Symbol.asyncIterator](): AsyncIterator<T> {
    return {
      next: async (): Promise<IteratorResult<T>> => {
        if (this.items.length > 0) {
          const value = this.items.shift();
          return { value: value as T, done: false };
        }
        if (this.closed) {
          return { value: undefined, done: true };
        }
        return await new Promise<IteratorResult<T>>((resolve) => {
          this.waiters.push(resolve);
        });
      },
    };
  }
}

function writeEvent(event: BridgeEvent, requestId?: string): void {
  const envelope: BridgeEventEnvelope = {
    ...(requestId ? { request_id: requestId } : {}),
    ...event,
  };
  process.stdout.write(`${JSON.stringify(envelope)}\n`);
}

function failConnection(message: string, requestId?: string): void {
  writeEvent({ event: "connection_failed", message }, requestId);
}

function slashError(sessionId: string, message: string, requestId?: string): void {
  writeEvent({ event: "slash_error", session_id: sessionId, message }, requestId);
}

function asRecord(value: unknown, context: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${context} must be an object`);
  }
  return value as Record<string, unknown>;
}

function expectString(
  record: Record<string, unknown>,
  key: string,
  context: string,
): string {
  const value = record[key];
  if (typeof value !== "string") {
    throw new Error(`${context}.${key} must be a string`);
  }
  return value;
}

function expectBoolean(
  record: Record<string, unknown>,
  key: string,
  context: string,
): boolean {
  const value = record[key];
  if (typeof value !== "boolean") {
    throw new Error(`${context}.${key} must be a boolean`);
  }
  return value;
}

function optionalString(
  record: Record<string, unknown>,
  key: string,
  context: string,
): string | undefined {
  const value = record[key];
  if (value === undefined || value === null) {
    return undefined;
  }
  if (typeof value !== "string") {
    throw new Error(`${context}.${key} must be a string when provided`);
  }
  return value;
}

function optionalMetadata(record: Record<string, unknown>, key: string): Record<string, Json> {
  const value = record[key];
  if (value === undefined || value === null) {
    return {};
  }
  return asRecord(value, `${key} metadata`) as Record<string, Json>;
}

function parsePromptChunks(
  record: Record<string, unknown>,
  context: string,
): Array<{ kind: string; value: Json }> {
  const rawChunks = record.chunks;
  if (!Array.isArray(rawChunks)) {
    throw new Error(`${context}.chunks must be an array`);
  }
  return rawChunks.map((chunk, index) => {
    const parsed = asRecord(chunk, `${context}.chunks[${index}]`);
    const kind = expectString(parsed, "kind", `${context}.chunks[${index}]`);
    return { kind, value: (parsed.value ?? null) as Json };
  });
}

export function parseCommandEnvelope(line: string): { requestId?: string; command: BridgeCommand } {
  const raw = asRecord(JSON.parse(line) as BridgeCommandEnvelope, "command envelope");
  const requestId = typeof raw.request_id === "string" ? raw.request_id : undefined;
  const commandName = expectString(raw, "command", "command envelope");

  const command: BridgeCommand = (() => {
    switch (commandName) {
      case "initialize":
        return {
          command: "initialize",
          cwd: expectString(raw, "cwd", "initialize"),
          metadata: optionalMetadata(raw, "metadata"),
        };
      case "create_session":
        return {
          command: "create_session",
          cwd: expectString(raw, "cwd", "create_session"),
          yolo: expectBoolean(raw, "yolo", "create_session"),
          model: optionalString(raw, "model", "create_session"),
          resume: optionalString(raw, "resume", "create_session"),
          metadata: optionalMetadata(raw, "metadata"),
        };
      case "load_session":
        return {
          command: "load_session",
          session_id: expectString(raw, "session_id", "load_session"),
          metadata: optionalMetadata(raw, "metadata"),
        };
      case "new_session":
        return {
          command: "new_session",
          cwd: expectString(raw, "cwd", "new_session"),
          yolo: expectBoolean(raw, "yolo", "new_session"),
          model: optionalString(raw, "model", "new_session"),
        };
      case "prompt":
        return {
          command: "prompt",
          session_id: expectString(raw, "session_id", "prompt"),
          chunks: parsePromptChunks(raw, "prompt"),
        };
      case "cancel_turn":
        return {
          command: "cancel_turn",
          session_id: expectString(raw, "session_id", "cancel_turn"),
        };
      case "set_model":
        return {
          command: "set_model",
          session_id: expectString(raw, "session_id", "set_model"),
          model: expectString(raw, "model", "set_model"),
        };
      case "set_mode":
        return {
          command: "set_mode",
          session_id: expectString(raw, "session_id", "set_mode"),
          mode: expectString(raw, "mode", "set_mode"),
        };
      case "permission_response": {
        const outcome = asRecord(raw.outcome, "permission_response.outcome");
        const outcomeType = expectString(outcome, "outcome", "permission_response.outcome");
        if (outcomeType !== "selected" && outcomeType !== "cancelled") {
          throw new Error("permission_response.outcome.outcome must be 'selected' or 'cancelled'");
        }
        const parsedOutcome: PermissionOutcome =
          outcomeType === "selected"
            ? {
                outcome: "selected",
                option_id: expectString(outcome, "option_id", "permission_response.outcome"),
              }
            : { outcome: "cancelled" };
        return {
          command: "permission_response",
          session_id: expectString(raw, "session_id", "permission_response"),
          tool_call_id: expectString(raw, "tool_call_id", "permission_response"),
          outcome: parsedOutcome,
        };
      }
      case "shutdown":
        return { command: "shutdown" };
      default:
        throw new Error(`unsupported command: ${commandName}`);
    }
  })();

  return { requestId, command };
}

export function toPermissionMode(mode: string): PermissionMode | null {
  if (
    mode === "default" ||
    mode === "acceptEdits" ||
    mode === "bypassPermissions" ||
    mode === "plan" ||
    mode === "dontAsk"
  ) {
    return mode;
  }
  return null;
}

function modeState(mode: PermissionMode): ModeState {
  return {
    current_mode_id: mode,
    current_mode_name: MODE_NAMES[mode],
    available_modes: MODE_OPTIONS,
  };
}

function emitConnectEvent(session: SessionState): void {
  const historyUpdates = session.resumeUpdates;
  const connectEvent: BridgeEvent =
    session.connectEvent === "session_replaced"
      ? {
          event: "session_replaced",
          session_id: session.sessionId,
          cwd: session.cwd,
          model_name: session.model,
          mode: modeState(session.mode),
          ...(historyUpdates && historyUpdates.length > 0 ? { history_updates: historyUpdates } : {}),
        }
      : {
          event: "connected",
          session_id: session.sessionId,
          cwd: session.cwd,
          model_name: session.model,
          mode: modeState(session.mode),
          ...(historyUpdates && historyUpdates.length > 0 ? { history_updates: historyUpdates } : {}),
        };
  writeEvent(connectEvent, session.connectRequestId);
  session.connectRequestId = undefined;
  session.connected = true;
  session.authHintSent = false;
  session.resumeUpdates = undefined;

  const staleSessions = session.sessionsToCloseAfterConnect;
  session.sessionsToCloseAfterConnect = undefined;
  if (!staleSessions || staleSessions.length === 0) {
    return;
  }
  void (async () => {
    for (const stale of staleSessions) {
      if (stale === session) {
        continue;
      }
      if (sessions.get(stale.sessionId) === stale) {
        sessions.delete(stale.sessionId);
      }
      await closeSession(stale);
    }
  })();
}

function textFromPrompt(command: Extract<BridgeCommand, { command: "prompt" }>): string {
  const chunks = command.chunks ?? [];
  return chunks
    .map((chunk) => {
      if (chunk.kind !== "text") {
        return "";
      }
      return typeof chunk.value === "string" ? chunk.value : "";
    })
    .filter((part) => part.length > 0)
    .join("");
}

function sessionById(sessionId: string): SessionState | null {
  return sessions.get(sessionId) ?? null;
}

function updateSessionId(session: SessionState, newSessionId: string): void {
  if (session.sessionId === newSessionId) {
    return;
  }
  sessions.delete(session.sessionId);
  session.sessionId = newSessionId;
  sessions.set(newSessionId, session);
}

export function normalizeToolKind(name: string): string {
  switch (name) {
    case "Bash":
      return "execute";
    case "Read":
      return "read";
    case "Write":
    case "Edit":
      return "edit";
    case "Delete":
      return "delete";
    case "Move":
      return "move";
    case "Glob":
    case "Grep":
      return "search";
    case "WebFetch":
      return "fetch";
    case "TodoWrite":
      return "other";
    case "Task":
      return "think";
    case "ExitPlanMode":
      return "switch_mode";
    default:
      return "think";
  }
}

export function toolTitle(name: string, input: Record<string, unknown>): string {
  if (name === "Bash") {
    const command = typeof input.command === "string" ? input.command : "";
    return command || "Terminal";
  }
  if (name === "Glob") {
    const pattern = typeof input.pattern === "string" ? input.pattern : "";
    const path = typeof input.path === "string" ? input.path : "";
    if (pattern && path) {
      return `Glob ${pattern} in ${path}`;
    }
    if (pattern) {
      return `Glob ${pattern}`;
    }
    if (path) {
      return `Glob ${path}`;
    }
  }
  if (name === "WebFetch") {
    const url = typeof input.url === "string" ? input.url : "";
    if (url) {
      return `WebFetch ${url}`;
    }
  }
  if (name === "WebSearch") {
    const query = typeof input.query === "string" ? input.query : "";
    if (query) {
      return `WebSearch ${query}`;
    }
  }
  if ((name === "Read" || name === "Write" || name === "Edit") && typeof input.file_path === "string") {
    return `${name} ${input.file_path}`;
  }
  return name;
}

function editDiffContent(name: string, input: Record<string, unknown>): ToolCall["content"] {
  const filePath = typeof input.file_path === "string" ? input.file_path : "";
  if (!filePath) {
    return [];
  }

  if (name === "Edit") {
    const oldText = typeof input.old_string === "string" ? input.old_string : "";
    const newText = typeof input.new_string === "string" ? input.new_string : "";
    if (!oldText && !newText) {
      return [];
    }
    return [{ type: "diff", old_path: filePath, new_path: filePath, old: oldText, new: newText }];
  }

  if (name === "Write") {
    const newText = typeof input.content === "string" ? input.content : "";
    if (!newText) {
      return [];
    }
    return [{ type: "diff", old_path: filePath, new_path: filePath, old: "", new: newText }];
  }

  return [];
}

export function createToolCall(toolUseId: string, name: string, input: Record<string, unknown>): ToolCall {
  return {
    tool_call_id: toolUseId,
    title: toolTitle(name, input),
    kind: normalizeToolKind(name),
    status: "pending",
    content: editDiffContent(name, input),
    raw_input: input as unknown as Json,
    locations: typeof input.file_path === "string" ? [{ path: input.file_path }] : [],
    meta: {
      claudeCode: {
        toolName: name,
      },
    },
  };
}

function emitSessionUpdate(sessionId: string, update: SessionUpdate): void {
  writeEvent({ event: "session_update", session_id: sessionId, update });
}

export function extractText(value: unknown): string {
  if (typeof value === "string") {
    return value;
  }
  if (Array.isArray(value)) {
    return value
      .map((entry) => {
        if (typeof entry === "string") {
          return entry;
        }
        if (entry && typeof entry === "object" && "text" in entry && typeof entry.text === "string") {
          return entry.text;
        }
        return "";
      })
      .filter((part) => part.length > 0)
      .join("\n");
  }
  if (value && typeof value === "object" && "text" in value && typeof value.text === "string") {
    return value.text;
  }
  return "";
}

const PERSISTED_OUTPUT_OPEN_TAG = "<persisted-output>";
const PERSISTED_OUTPUT_CLOSE_TAG = "</persisted-output>";

function extractPersistedOutputInnerText(text: string): string | null {
  const lower = text.toLowerCase();
  const openIdx = lower.indexOf(PERSISTED_OUTPUT_OPEN_TAG);
  if (openIdx < 0) {
    return null;
  }
  const bodyStart = openIdx + PERSISTED_OUTPUT_OPEN_TAG.length;
  const closeIdx = lower.indexOf(PERSISTED_OUTPUT_CLOSE_TAG, bodyStart);
  if (closeIdx < 0) {
    return null;
  }
  return text.slice(bodyStart, closeIdx);
}

function persistedOutputFirstLine(text: string): string | null {
  const inner = extractPersistedOutputInnerText(text);
  if (inner === null) {
    return null;
  }

  for (const line of inner.split(/\r?\n/)) {
    const cleaned = line.replace(/^[\s|│┃║]+/u, "").trim();
    if (cleaned.length > 0) {
      return cleaned;
    }
  }
  return null;
}

export function normalizeToolResultText(value: unknown): string {
  const text = extractText(value);
  if (!text) {
    return "";
  }
  const persistedLine = persistedOutputFirstLine(text);
  if (persistedLine) {
    return persistedLine;
  }
  return text;
}

function emitToolCall(session: SessionState, toolUseId: string, name: string, input: Record<string, unknown>): void {
  const toolCall = createToolCall(toolUseId, name, input);
  const status: ToolCall["status"] = "in_progress";
  toolCall.status = status;

  const existing = session.toolCalls.get(toolUseId);
  if (!existing) {
    session.toolCalls.set(toolUseId, toolCall);
    emitSessionUpdate(session.sessionId, { type: "tool_call", tool_call: toolCall });
    return;
  }

  const fields: ToolCallUpdateFields = {
    title: toolCall.title,
    kind: toolCall.kind,
    status,
    raw_input: toolCall.raw_input,
    locations: toolCall.locations,
    meta: toolCall.meta,
  };
  if (toolCall.content.length > 0) {
    fields.content = toolCall.content;
  }
  emitSessionUpdate(session.sessionId, {
    type: "tool_call_update",
    tool_call_update: { tool_call_id: toolUseId, fields },
  });

  existing.title = toolCall.title;
  existing.kind = toolCall.kind;
  existing.status = status;
  existing.raw_input = toolCall.raw_input;
  existing.locations = toolCall.locations;
  existing.meta = toolCall.meta;
  if (toolCall.content.length > 0) {
    existing.content = toolCall.content;
  }
}

function ensureToolCallVisible(
  session: SessionState,
  toolUseId: string,
  toolName: string,
  input: Record<string, unknown>,
): ToolCall {
  const existing = session.toolCalls.get(toolUseId);
  if (existing) {
    return existing;
  }
  const toolCall = createToolCall(toolUseId, toolName, input);
  session.toolCalls.set(toolUseId, toolCall);
  emitSessionUpdate(session.sessionId, { type: "tool_call", tool_call: toolCall });
  return toolCall;
}

function emitPlanIfTodoWrite(session: SessionState, name: string, input: Record<string, unknown>): void {
  if (name !== "TodoWrite" || !Array.isArray(input.todos)) {
    return;
  }
  const entries: PlanEntry[] = input.todos
    .map((todo) => {
      if (!todo || typeof todo !== "object") {
        return null;
      }
      const todoObj = todo as Record<string, unknown>;
      const content = typeof todoObj.content === "string" ? todoObj.content : "";
      const status = typeof todoObj.status === "string" ? todoObj.status : "pending";
      if (!content) {
        return null;
      }
      return { content, status, active_form: status };
    })
    .filter((entry): entry is PlanEntry => entry !== null);

  if (entries.length > 0) {
    emitSessionUpdate(session.sessionId, { type: "plan", entries });
  }
}

function resolveToolName(toolCall: ToolCall | undefined): string {
  const meta = asRecordOrNull(toolCall?.meta);
  const claudeCode = asRecordOrNull(meta?.claudeCode);
  const toolName = claudeCode?.toolName;
  return typeof toolName === "string" ? toolName : "";
}

function writeDiffFromInput(rawInput: Json | undefined): ToolCall["content"] {
  const input = asRecordOrNull(rawInput);
  if (!input) {
    return [];
  }
  const filePath = typeof input.file_path === "string" ? input.file_path : "";
  const content = typeof input.content === "string" ? input.content : "";
  if (!filePath || !content) {
    return [];
  }
  return [{ type: "diff", old_path: filePath, new_path: filePath, old: "", new: content }];
}

function editDiffFromInput(rawInput: Json | undefined): ToolCall["content"] {
  const input = asRecordOrNull(rawInput);
  if (!input) {
    return [];
  }
  const filePath = typeof input.file_path === "string" ? input.file_path : "";
  const oldText =
    typeof input.old_string === "string"
      ? input.old_string
      : typeof input.oldString === "string"
        ? input.oldString
        : "";
  const newText =
    typeof input.new_string === "string"
      ? input.new_string
      : typeof input.newString === "string"
        ? input.newString
        : "";
  if (!filePath || (!oldText && !newText)) {
    return [];
  }
  return [{ type: "diff", old_path: filePath, new_path: filePath, old: oldText, new: newText }];
}

function writeDiffFromResult(rawContent: unknown): ToolCall["content"] {
  const candidates = Array.isArray(rawContent) ? rawContent : [rawContent];
  for (const candidate of candidates) {
    const record = asRecordOrNull(candidate);
    if (!record) {
      continue;
    }
    const filePath =
      typeof record.filePath === "string"
        ? record.filePath
        : typeof record.file_path === "string"
          ? record.file_path
          : "";
    const content = typeof record.content === "string" ? record.content : "";
    const originalRaw =
      "originalFile" in record ? record.originalFile : "original_file" in record ? record.original_file : undefined;
    if (!filePath || !content || originalRaw === undefined) {
      continue;
    }
    const original = typeof originalRaw === "string" ? originalRaw : originalRaw === null ? "" : "";
    return [{ type: "diff", old_path: filePath, new_path: filePath, old: original, new: content }];
  }
  return [];
}

export function buildToolResultFields(
  isError: boolean,
  rawContent: unknown,
  base?: ToolCall,
): ToolCallUpdateFields {
  const rawOutput = normalizeToolResultText(rawContent);
  const toolName = resolveToolName(base);
  const fields: ToolCallUpdateFields = {
    status: isError ? "failed" : "completed",
    raw_output: rawOutput || JSON.stringify(rawContent),
  };

  if (!isError && toolName === "Write") {
    const structuredDiff = writeDiffFromResult(rawContent);
    if (structuredDiff.length > 0) {
      fields.content = structuredDiff;
      return fields;
    }
    const inputDiff = writeDiffFromInput(base?.raw_input);
    if (inputDiff.length > 0) {
      fields.content = inputDiff;
      return fields;
    }
  }

  if (!isError && toolName === "Edit") {
    const inputDiff = editDiffFromInput(base?.raw_input);
    if (inputDiff.length > 0) {
      fields.content = inputDiff;
      return fields;
    }
    // Preserve initial edit diff content when result payload is plain text.
    if (base?.content.some((entry) => entry.type === "diff")) {
      return fields;
    }
  }

  if (rawOutput) {
    fields.content = [{ type: "content", content: { type: "text", text: rawOutput } }];
  }
  return fields;
}

function emitToolResultUpdate(session: SessionState, toolUseId: string, isError: boolean, rawContent: unknown): void {
  const base = session.toolCalls.get(toolUseId);
  const fields = buildToolResultFields(isError, rawContent, base);
  const update: ToolCallUpdate = { tool_call_id: toolUseId, fields };
  emitSessionUpdate(session.sessionId, { type: "tool_call_update", tool_call_update: update });

  if (base) {
    base.status = fields.status ?? base.status;
    if (fields.raw_output) {
      base.raw_output = fields.raw_output;
    }
    if (fields.content) {
      base.content = fields.content;
    }
  }
}

function finalizeOpenToolCalls(session: SessionState, status: "completed" | "failed"): void {
  for (const [toolUseId, toolCall] of session.toolCalls) {
    if (toolCall.status !== "pending" && toolCall.status !== "in_progress") {
      continue;
    }
    const fields: ToolCallUpdateFields = { status };
    emitSessionUpdate(session.sessionId, {
      type: "tool_call_update",
      tool_call_update: { tool_call_id: toolUseId, fields },
    });
    toolCall.status = status;
  }
}

function emitToolProgressUpdate(session: SessionState, toolUseId: string, toolName: string): void {
  const existing = session.toolCalls.get(toolUseId);
  if (!existing) {
    emitToolCall(session, toolUseId, toolName, {});
    return;
  }
  if (existing.status === "in_progress") {
    return;
  }

  const fields: ToolCallUpdateFields = { status: "in_progress" };
  emitSessionUpdate(session.sessionId, {
    type: "tool_call_update",
    tool_call_update: { tool_call_id: toolUseId, fields },
  });
  existing.status = "in_progress";
}

function emitToolSummaryUpdate(session: SessionState, toolUseId: string, summary: string): void {
  const base = session.toolCalls.get(toolUseId);
  if (!base) {
    return;
  }
  const fields: ToolCallUpdateFields = {
    status: base.status === "failed" ? "failed" : "completed",
    raw_output: summary,
    content: [{ type: "content", content: { type: "text", text: summary } }],
  };
  emitSessionUpdate(session.sessionId, {
    type: "tool_call_update",
    tool_call_update: { tool_call_id: toolUseId, fields },
  });
  base.status = fields.status ?? base.status;
  base.raw_output = summary;
}

function setToolCallStatus(
  session: SessionState,
  toolUseId: string,
  status: "pending" | "in_progress" | "completed" | "failed",
  message?: string,
): void {
  const base = session.toolCalls.get(toolUseId);
  if (!base) {
    return;
  }

  const fields: ToolCallUpdateFields = { status };
  if (message && message.length > 0) {
    fields.raw_output = message;
    fields.content = [{ type: "content", content: { type: "text", text: message } }];
  }
  emitSessionUpdate(session.sessionId, {
    type: "tool_call_update",
    tool_call_update: { tool_call_id: toolUseId, fields },
  });
  base.status = status;
  if (fields.raw_output) {
    base.raw_output = fields.raw_output;
  }
}

function resolveTaskToolUseId(session: SessionState, msg: Record<string, unknown>): string {
  const direct = typeof msg.tool_use_id === "string" ? msg.tool_use_id : "";
  if (direct) {
    return direct;
  }
  const taskId = typeof msg.task_id === "string" ? msg.task_id : "";
  if (!taskId) {
    return "";
  }
  return session.taskToolUseIds.get(taskId) ?? "";
}

function taskProgressText(msg: Record<string, unknown>): string {
  const description = typeof msg.description === "string" ? msg.description : "";
  const lastTool = typeof msg.last_tool_name === "string" ? msg.last_tool_name : "";
  if (description && lastTool) {
    return `${description} (last tool: ${lastTool})`;
  }
  return description || lastTool;
}

function handleTaskSystemMessage(
  session: SessionState,
  subtype: string,
  msg: Record<string, unknown>,
): void {
  if (subtype !== "task_started" && subtype !== "task_progress" && subtype !== "task_notification") {
    return;
  }

  const taskId = typeof msg.task_id === "string" ? msg.task_id : "";
  const explicitToolUseId = typeof msg.tool_use_id === "string" ? msg.tool_use_id : "";
  if (taskId && explicitToolUseId) {
    session.taskToolUseIds.set(taskId, explicitToolUseId);
  }
  const toolUseId = resolveTaskToolUseId(session, msg);
  if (!toolUseId) {
    return;
  }

  const toolCall = ensureToolCallVisible(session, toolUseId, "Task", {});
  if (toolCall.status === "pending") {
    toolCall.status = "in_progress";
    emitSessionUpdate(session.sessionId, {
      type: "tool_call_update",
      tool_call_update: { tool_call_id: toolUseId, fields: { status: "in_progress" } },
    });
  }

  if (subtype === "task_started") {
    const description = typeof msg.description === "string" ? msg.description : "";
    if (!description) {
      return;
    }
    emitSessionUpdate(session.sessionId, {
      type: "tool_call_update",
      tool_call_update: {
        tool_call_id: toolUseId,
        fields: {
          status: "in_progress",
          raw_output: description,
          content: [{ type: "content", content: { type: "text", text: description } }],
        },
      },
    });
    return;
  }

  if (subtype === "task_progress") {
    const progress = taskProgressText(msg);
    if (!progress) {
      return;
    }
    emitSessionUpdate(session.sessionId, {
      type: "tool_call_update",
      tool_call_update: {
        tool_call_id: toolUseId,
        fields: {
          status: "in_progress",
          raw_output: progress,
          content: [{ type: "content", content: { type: "text", text: progress } }],
        },
      },
    });
    return;
  }

  const status = typeof msg.status === "string" ? msg.status : "";
  const summary = typeof msg.summary === "string" ? msg.summary : "";
  const finalStatus = status === "completed" ? "completed" : "failed";
  const fields: ToolCallUpdateFields = { status: finalStatus };
  if (summary) {
    fields.raw_output = summary;
    fields.content = [{ type: "content", content: { type: "text", text: summary } }];
  }
  emitSessionUpdate(session.sessionId, {
    type: "tool_call_update",
    tool_call_update: { tool_call_id: toolUseId, fields },
  });
  toolCall.status = finalStatus;
  if (taskId) {
    session.taskToolUseIds.delete(taskId);
  }
}

export function unwrapToolUseResult(rawResult: unknown): { isError: boolean; content: unknown } {
  if (!rawResult || typeof rawResult !== "object") {
    return { isError: false, content: rawResult };
  }
  const record = rawResult as Record<string, unknown>;
  const isError =
    (typeof record.is_error === "boolean" && record.is_error) ||
    (typeof record.error === "boolean" && record.error);
  if ("content" in record) {
    return { isError: Boolean(isError), content: record.content };
  }
  if ("result" in record) {
    return { isError: Boolean(isError), content: record.result };
  }
  if ("text" in record) {
    return { isError: Boolean(isError), content: record.text };
  }
  return { isError: Boolean(isError), content: rawResult };
}

function handleContentBlock(session: SessionState, block: Record<string, unknown>): void {
  const blockType = typeof block.type === "string" ? block.type : "";

  if (blockType === "text") {
    const text = typeof block.text === "string" ? block.text : "";
    if (text) {
      emitSessionUpdate(session.sessionId, { type: "agent_message_chunk", content: { type: "text", text } });
    }
    return;
  }

  if (blockType === "thinking") {
    const text = typeof block.thinking === "string" ? block.thinking : "";
    if (text) {
      emitSessionUpdate(session.sessionId, { type: "agent_thought_chunk", content: { type: "text", text } });
    }
    return;
  }

  if (blockType === "tool_use" || blockType === "server_tool_use" || blockType === "mcp_tool_use") {
    const toolUseId = typeof block.id === "string" ? block.id : "";
    const name = typeof block.name === "string" ? block.name : "Tool";
    const input =
      block.input && typeof block.input === "object" ? (block.input as Record<string, unknown>) : {};
    if (!toolUseId) {
      return;
    }
    emitPlanIfTodoWrite(session, name, input);
    emitToolCall(session, toolUseId, name, input);
    return;
  }

  if (TOOL_RESULT_TYPES.has(blockType)) {
    const toolUseId = typeof block.tool_use_id === "string" ? block.tool_use_id : "";
    if (!toolUseId) {
      return;
    }
    const isError = Boolean(block.is_error);
    emitToolResultUpdate(session, toolUseId, isError, block.content);
  }
}

function handleStreamEvent(session: SessionState, event: Record<string, unknown>): void {
  const eventType = typeof event.type === "string" ? event.type : "";

  if (eventType === "content_block_start") {
    if (event.content_block && typeof event.content_block === "object") {
      handleContentBlock(session, event.content_block as Record<string, unknown>);
    }
    return;
  }

  if (eventType === "content_block_delta") {
    if (!event.delta || typeof event.delta !== "object") {
      return;
    }
    const delta = event.delta as Record<string, unknown>;
    const deltaType = typeof delta.type === "string" ? delta.type : "";
    if (deltaType === "text_delta") {
      const text = typeof delta.text === "string" ? delta.text : "";
      if (text) {
        emitSessionUpdate(session.sessionId, { type: "agent_message_chunk", content: { type: "text", text } });
      }
    } else if (deltaType === "thinking_delta") {
      const text = typeof delta.thinking === "string" ? delta.thinking : "";
      if (text) {
        emitSessionUpdate(session.sessionId, { type: "agent_thought_chunk", content: { type: "text", text } });
      }
    }
  }
}

function handleAssistantMessage(session: SessionState, message: Record<string, unknown>): void {
  const messageObject =
    message.message && typeof message.message === "object"
      ? (message.message as Record<string, unknown>)
      : null;
  if (!messageObject) {
    return;
  }
  const content = Array.isArray(messageObject.content) ? messageObject.content : [];
  for (const block of content) {
    if (!block || typeof block !== "object") {
      continue;
    }
    const blockRecord = block as Record<string, unknown>;
    const blockType = typeof blockRecord.type === "string" ? blockRecord.type : "";
    if (
      blockType === "tool_use" ||
      blockType === "server_tool_use" ||
      blockType === "mcp_tool_use" ||
      TOOL_RESULT_TYPES.has(blockType)
    ) {
      handleContentBlock(session, blockRecord);
    }
  }
}

function handleUserToolResultBlocks(session: SessionState, message: Record<string, unknown>): void {
  const messageObject =
    message.message && typeof message.message === "object"
      ? (message.message as Record<string, unknown>)
      : null;
  if (!messageObject) {
    return;
  }
  const content = Array.isArray(messageObject.content) ? messageObject.content : [];
  for (const block of content) {
    if (!block || typeof block !== "object") {
      continue;
    }
    const blockRecord = block as Record<string, unknown>;
    const blockType = typeof blockRecord.type === "string" ? blockRecord.type : "";
    if (TOOL_RESULT_TYPES.has(blockType)) {
      handleContentBlock(session, blockRecord);
    }
  }
}

export function looksLikeAuthRequired(input: string): boolean {
  const normalized = input.toLowerCase();
  return (
    normalized.includes("/login") ||
    normalized.includes("auth required") ||
    normalized.includes("authentication failed") ||
    normalized.includes("please log in")
  );
}

function emitAuthRequired(session: SessionState, detail?: string): void {
  if (session.authHintSent) {
    return;
  }
  session.authHintSent = true;
  writeEvent({
    event: "auth_required",
    method_name: "Claude Login",
    method_description:
      detail && detail.trim().length > 0
        ? detail
        : "Run `claude /login` in a terminal, then retry.",
  });
}

function numberField(record: Record<string, unknown>, ...keys: string[]): number | undefined {
  for (const key of keys) {
    const value = record[key];
    if (typeof value === "number" && Number.isFinite(value)) {
      return value;
    }
  }
  return undefined;
}

export function buildUsageUpdateFromResult(message: Record<string, unknown>): SessionUpdate | null {
  return buildUsageUpdateFromResultForSession(undefined, message);
}

function selectModelUsageRecord(
  session: SessionState | undefined,
  message: Record<string, unknown>,
): Record<string, unknown> | null {
  const modelUsageRaw = asRecordOrNull(message.modelUsage);
  if (!modelUsageRaw) {
    return null;
  }
  const sortedKeys = Object.keys(modelUsageRaw).sort();
  if (sortedKeys.length === 0) {
    return null;
  }

  const preferredKeys = new Set<string>();
  if (session?.model) {
    preferredKeys.add(session.model);
  }
  if (typeof message.model === "string") {
    preferredKeys.add(message.model);
  }

  for (const key of preferredKeys) {
    const value = asRecordOrNull(modelUsageRaw[key]);
    if (value) {
      return value;
    }
  }
  for (const key of sortedKeys) {
    const value = asRecordOrNull(modelUsageRaw[key]);
    if (value) {
      return value;
    }
  }
  return null;
}

function buildUsageUpdateFromResultForSession(
  session: SessionState | undefined,
  message: Record<string, unknown>,
): SessionUpdate | null {
  const usage = asRecordOrNull(message.usage);
  const inputTokens = usage ? numberField(usage, "inputTokens", "input_tokens") : undefined;
  const outputTokens = usage ? numberField(usage, "outputTokens", "output_tokens") : undefined;
  const cacheReadTokens = usage
    ? numberField(
        usage,
        "cacheReadInputTokens",
        "cache_read_input_tokens",
        "cache_read_tokens",
      )
    : undefined;
  const cacheWriteTokens = usage
    ? numberField(
        usage,
        "cacheCreationInputTokens",
        "cache_creation_input_tokens",
        "cache_write_tokens",
      )
    : undefined;

  const totalCostUsd = numberField(message, "total_cost_usd", "totalCostUsd");
  let turnCostUsd: number | undefined;
  if (totalCostUsd !== undefined && session) {
    if (session.lastTotalCostUsd === undefined) {
      turnCostUsd = totalCostUsd;
    } else {
      turnCostUsd = Math.max(0, totalCostUsd - session.lastTotalCostUsd);
    }
    session.lastTotalCostUsd = totalCostUsd;
  }

  const modelUsage = selectModelUsageRecord(session, message);
  const contextWindow = modelUsage
    ? numberField(modelUsage, "contextWindow", "context_window")
    : undefined;
  const maxOutputTokens = modelUsage
    ? numberField(modelUsage, "maxOutputTokens", "max_output_tokens")
    : undefined;

  if (
    inputTokens === undefined &&
    outputTokens === undefined &&
    cacheReadTokens === undefined &&
    cacheWriteTokens === undefined &&
    totalCostUsd === undefined &&
    turnCostUsd === undefined &&
    contextWindow === undefined &&
    maxOutputTokens === undefined
  ) {
    return null;
  }

  return {
    type: "usage_update",
    usage: {
      ...(inputTokens !== undefined ? { input_tokens: inputTokens } : {}),
      ...(outputTokens !== undefined ? { output_tokens: outputTokens } : {}),
      ...(cacheReadTokens !== undefined ? { cache_read_tokens: cacheReadTokens } : {}),
      ...(cacheWriteTokens !== undefined ? { cache_write_tokens: cacheWriteTokens } : {}),
      ...(totalCostUsd !== undefined ? { total_cost_usd: totalCostUsd } : {}),
      ...(turnCostUsd !== undefined ? { turn_cost_usd: turnCostUsd } : {}),
      ...(contextWindow !== undefined ? { context_window: contextWindow } : {}),
      ...(maxOutputTokens !== undefined ? { max_output_tokens: maxOutputTokens } : {}),
    },
  };
}

function handleResultMessage(session: SessionState, message: Record<string, unknown>): void {
  const usageUpdate = buildUsageUpdateFromResultForSession(session, message);
  if (usageUpdate) {
    emitSessionUpdate(session.sessionId, usageUpdate);
  }

  const subtype = typeof message.subtype === "string" ? message.subtype : "";
  if (subtype === "success") {
    finalizeOpenToolCalls(session, "completed");
    writeEvent({ event: "turn_complete", session_id: session.sessionId });
    return;
  }

  const errors =
    Array.isArray(message.errors) && message.errors.every((entry) => typeof entry === "string")
      ? (message.errors as string[])
      : [];
  const authHint = errors.find((entry) => looksLikeAuthRequired(entry));
  if (authHint) {
    emitAuthRequired(session, authHint);
  }
  finalizeOpenToolCalls(session, "failed");
  const fallback = subtype ? `turn failed: ${subtype}` : "turn failed";
  writeEvent({
    event: "turn_error",
    session_id: session.sessionId,
    message: errors.length > 0 ? errors.join("\n") : fallback,
  });
}

function handleSdkMessage(session: SessionState, message: SDKMessage): void {
  const msg = message as unknown as Record<string, unknown>;
  const type = typeof msg.type === "string" ? msg.type : "";

  if (type === "system") {
    const subtype = typeof msg.subtype === "string" ? msg.subtype : "";
    if (subtype === "init") {
      const previousSessionId = session.sessionId;
      const incomingSessionId = typeof msg.session_id === "string" ? msg.session_id : session.sessionId;
      updateSessionId(session, incomingSessionId);
      const modelName = typeof msg.model === "string" ? msg.model : session.model;
      session.model = modelName;

      const incomingMode = typeof msg.permissionMode === "string" ? toPermissionMode(msg.permissionMode) : null;
      if (incomingMode) {
        session.mode = incomingMode;
      }

      if (!session.connected) {
        emitConnectEvent(session);
      } else if (previousSessionId !== session.sessionId) {
        const historyUpdates = session.resumeUpdates;
        writeEvent({
          event: "session_replaced",
          session_id: session.sessionId,
          cwd: session.cwd,
          model_name: session.model,
          mode: modeState(session.mode),
          ...(historyUpdates && historyUpdates.length > 0
            ? { history_updates: historyUpdates }
            : {}),
        });
        session.resumeUpdates = undefined;
      }

      if (Array.isArray(msg.slash_commands)) {
        const commands: AvailableCommand[] = msg.slash_commands
          .filter((entry): entry is string => typeof entry === "string")
          .map((name) => ({ name, description: "", input_hint: undefined }));
        if (commands.length > 0) {
          emitSessionUpdate(session.sessionId, { type: "available_commands_update", commands });
        }
      }

      void session.query
        .supportedCommands()
        .then((commands) => {
          const mapped: AvailableCommand[] = commands.map((command) => ({
            name: command.name,
            description: command.description ?? "",
            input_hint: command.argumentHint ?? undefined,
          }));
          emitSessionUpdate(session.sessionId, { type: "available_commands_update", commands: mapped });
        })
        .catch(() => {
          // Best-effort only; slash commands from init were already emitted.
        });
      return;
    }

    if (subtype === "status") {
      const mode =
        typeof msg.permissionMode === "string" ? toPermissionMode(msg.permissionMode) : null;
      if (mode) {
        session.mode = mode;
        emitSessionUpdate(session.sessionId, { type: "current_mode_update", current_mode_id: mode });
      }
      if (msg.status === "compacting") {
        emitSessionUpdate(session.sessionId, { type: "session_status_update", status: "compacting" });
      } else if (msg.status === null) {
        emitSessionUpdate(session.sessionId, { type: "session_status_update", status: "idle" });
      }
      return;
    }

    if (subtype === "compact_boundary") {
      const compactMetadata = asRecordOrNull(msg.compact_metadata);
      if (!compactMetadata) {
        return;
      }
      const trigger = compactMetadata.trigger;
      const preTokens = numberField(compactMetadata, "pre_tokens", "preTokens");
      if ((trigger === "manual" || trigger === "auto") && preTokens !== undefined) {
        emitSessionUpdate(session.sessionId, {
          type: "compaction_boundary",
          trigger,
          pre_tokens: preTokens,
        });
      }
      return;
    }
    handleTaskSystemMessage(session, subtype, msg);
    return;
  }

  if (type === "auth_status") {
    const output = Array.isArray(msg.output)
      ? msg.output.filter((entry): entry is string => typeof entry === "string").join("\n")
      : "";
    const errorText = typeof msg.error === "string" ? msg.error : "";
    const combined = [errorText, output].filter((entry) => entry.length > 0).join("\n");
    if (combined && looksLikeAuthRequired(combined)) {
      emitAuthRequired(session, combined);
    }
    return;
  }

  if (type === "stream_event") {
    if (msg.event && typeof msg.event === "object") {
      handleStreamEvent(session, msg.event as Record<string, unknown>);
    }
    return;
  }

  if (type === "tool_progress") {
    const toolUseId = typeof msg.tool_use_id === "string" ? msg.tool_use_id : "";
    const toolName = typeof msg.tool_name === "string" ? msg.tool_name : "Tool";
    if (toolUseId) {
      emitToolProgressUpdate(session, toolUseId, toolName);
    }
    return;
  }

  if (type === "tool_use_summary") {
    const summary = typeof msg.summary === "string" ? msg.summary : "";
    const toolIds = Array.isArray(msg.preceding_tool_use_ids)
      ? msg.preceding_tool_use_ids.filter((id): id is string => typeof id === "string")
      : [];
    if (summary && toolIds.length > 0) {
      for (const toolUseId of toolIds) {
        emitToolSummaryUpdate(session, toolUseId, summary);
      }
    }
    return;
  }

  if (type === "user") {
    handleUserToolResultBlocks(session, msg);

    const toolUseId = typeof msg.parent_tool_use_id === "string" ? msg.parent_tool_use_id : "";
    if (toolUseId && "tool_use_result" in msg) {
      const parsed = unwrapToolUseResult(msg.tool_use_result);
      emitToolResultUpdate(session, toolUseId, parsed.isError, parsed.content);
    }
    return;
  }

  if (type === "assistant") {
    if (msg.error === "authentication_failed") {
      emitAuthRequired(session);
    }
    handleAssistantMessage(session, msg);
    return;
  }

  if (type === "result") {
    handleResultMessage(session, msg);
  }
}

export function permissionOptionsFromSuggestions(
  suggestions: PermissionUpdate[] | undefined,
): PermissionOption[] {
  const scoped = splitPermissionSuggestionsByScope(suggestions);
  const hasSessionScoped = scoped.session.length > 0;
  const hasPersistentScoped = scoped.persistent.length > 0;
  const sessionOnly = hasSessionScoped && !hasPersistentScoped;

  const options: PermissionOption[] = [{ option_id: "allow_once", name: "Allow once", kind: "allow_once" }];
  options.push({
    option_id: sessionOnly ? "allow_session" : "allow_always",
    name: sessionOnly ? "Allow for session" : "Always allow",
    kind: sessionOnly ? "allow_session" : "allow_always",
  });
  options.push({ option_id: "reject_once", name: "Deny", kind: "reject_once" });
  return options;
}

async function closeSession(session: SessionState): Promise<void> {
  session.input.close();
  session.query.close();
  for (const pending of session.pendingPermissions.values()) {
    pending.resolve({ behavior: "deny", message: "Session closed" });
  }
  session.pendingPermissions.clear();
}

async function closeAllSessions(): Promise<void> {
  const active = Array.from(sessions.values());
  sessions.clear();
  await Promise.all(active.map((session) => closeSession(session)));
}

async function createSession(params: {
  cwd: string;
  yolo: boolean;
  model?: string;
  resume?: string;
  connectEvent: ConnectEventKind;
  requestId?: string;
  sessionsToCloseAfterConnect?: SessionState[];
  resumeUpdates?: SessionUpdate[];
}): Promise<void> {
  const input = new AsyncQueue<SDKUserMessage>();
  const startMode: PermissionMode = params.yolo ? "bypassPermissions" : "default";
  const provisionalSessionId = params.resume ?? randomUUID();

  let session!: SessionState;
  const canUseTool: CanUseTool = async (toolName, inputData, options) => {
    const toolUseId = options.toolUseID;
    if (toolName === "ExitPlanMode") {
      return { behavior: "allow", toolUseID: toolUseId };
    }
    logPermissionDebug(
      `request tool_use_id=${toolUseId} tool=${toolName} blocked_path=${options.blockedPath ?? "<none>"} ` +
        `decision_reason=${options.decisionReason ?? "<none>"} suggestions=${formatPermissionUpdates(options.suggestions)}`,
    );
    const existing = ensureToolCallVisible(session, toolUseId, toolName, inputData);

    const request: PermissionRequest = {
      tool_call: existing,
      options: permissionOptionsFromSuggestions(options.suggestions),
    };
    writeEvent({ event: "permission_request", session_id: session.sessionId, request });

    return await new Promise<PermissionResult>((resolve) => {
      session.pendingPermissions.set(toolUseId, {
        resolve,
        toolName,
        inputData: inputData,
        suggestions: options.suggestions,
      });
    });
  };

  const claudeCodeExecutable = process.env.CLAUDE_CODE_EXECUTABLE;
  const sdkDebugFile = process.env.CLAUDE_RS_SDK_DEBUG_FILE;
  const enableSdkDebug = process.env.CLAUDE_RS_SDK_DEBUG === "1" || Boolean(sdkDebugFile);
  const enableSpawnDebug = process.env.CLAUDE_RS_SDK_SPAWN_DEBUG === "1";
  if (claudeCodeExecutable && !fs.existsSync(claudeCodeExecutable)) {
    throw new Error(`CLAUDE_CODE_EXECUTABLE does not exist: ${claudeCodeExecutable}`);
  }

  let queryHandle: Query;
  try {
    queryHandle = query({
      prompt: input,
      options: {
        cwd: params.cwd,
        includePartialMessages: true,
        executable: "node",
        ...(params.resume ? {} : { sessionId: provisionalSessionId }),
        ...(claudeCodeExecutable
          ? { pathToClaudeCodeExecutable: claudeCodeExecutable }
          : {}),
        ...(enableSdkDebug ? { debug: true } : {}),
        ...(sdkDebugFile ? { debugFile: sdkDebugFile } : {}),
        stderr: (line: string) => {
          if (line.trim().length > 0) {
            console.error(`[sdk stderr] ${line}`);
          }
        },
        ...(enableSpawnDebug
          ? {
              spawnClaudeCodeProcess: (options: {
                command: string;
                args: string[];
                cwd?: string;
                env: Record<string, string | undefined>;
                signal: AbortSignal;
              }) => {
                console.error(
                  `[sdk spawn] command=${options.command} args=${JSON.stringify(options.args)} cwd=${options.cwd ?? "<none>"}`,
                );
                const child = spawnChild(options.command, options.args, {
                  cwd: options.cwd,
                  env: options.env,
                  signal: options.signal,
                  stdio: ["pipe", "pipe", "pipe"],
                  windowsHide: true,
                });
                child.on("error", (error) => {
                  console.error(
                    `[sdk spawn error] code=${(error as NodeJS.ErrnoException).code ?? "<none>"} message=${error.message}`,
                  );
                });
                return child;
              },
            }
          : {}),
        // Match claude-agent-acp defaults to avoid emitting an empty
        // --setting-sources argument.
        settingSources: ["user", "project", "local"],
        permissionMode: startMode,
        allowDangerouslySkipPermissions: params.yolo,
        resume: params.resume,
        model: params.model,
        canUseTool,
      },
    });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    throw new Error(
      `query() failed: node_executable=${process.execPath}; cwd=${params.cwd}; ` +
        `resume=${params.resume ?? "<none>"}; model=${params.model ?? "<none>"}; ` +
        `CLAUDE_CODE_EXECUTABLE=${claudeCodeExecutable ?? "<unset>"}; error=${message}`,
    );
  }

  session = {
    sessionId: provisionalSessionId,
    cwd: params.cwd,
    model: params.model ?? "default",
    mode: startMode,
    yolo: params.yolo,
    query: queryHandle,
    input,
    connected: false,
    connectEvent: params.connectEvent,
    connectRequestId: params.requestId,
    toolCalls: new Map<string, ToolCall>(),
    taskToolUseIds: new Map<string, string>(),
    pendingPermissions: new Map<string, PendingPermission>(),
    authHintSent: false,
    ...(params.resumeUpdates && params.resumeUpdates.length > 0
      ? { resumeUpdates: params.resumeUpdates }
      : {}),
    ...(params.sessionsToCloseAfterConnect
      ? { sessionsToCloseAfterConnect: params.sessionsToCloseAfterConnect }
      : {}),
  };
  sessions.set(provisionalSessionId, session);

  // In stream-input mode the SDK may defer init until input arrives.
  // Trigger initialization explicitly so the Rust UI can receive `connected`
  // before the first user prompt.
  void session.query
    .initializationResult()
    .then((result) => {
      if (!session.connected) {
        emitConnectEvent(session);
      }

      const commands = Array.isArray(result.commands)
        ? result.commands.map((command) => ({
            name: command.name,
            description: command.description ?? "",
            input_hint: command.argumentHint ?? undefined,
          }))
        : [];
      if (commands.length > 0) {
        emitSessionUpdate(session.sessionId, { type: "available_commands_update", commands });
      }
    })
    .catch((error) => {
      if (session.connected) {
        return;
      }
      const message = error instanceof Error ? error.message : String(error);
      failConnection(`agent initialization failed: ${message}`, session.connectRequestId);
      session.connectRequestId = undefined;
    });

  void (async () => {
    try {
      for await (const message of session.query) {
        handleSdkMessage(session, message);
      }
      if (!session.connected) {
        failConnection("agent stream ended before session initialization", params.requestId);
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      failConnection(`agent stream failed: ${message}`, params.requestId);
    }
  })();
}

export function permissionResultFromOutcome(
  outcome: PermissionOutcome,
  toolCallId: string,
  inputData: Record<string, unknown>,
  suggestions?: PermissionUpdate[],
  toolName?: string,
): PermissionResult {
  const scopedSuggestions = splitPermissionSuggestionsByScope(suggestions);

  if (outcome.outcome === "selected") {
    if (outcome.option_id === "allow_once") {
      return { behavior: "allow", updatedInput: inputData, toolUseID: toolCallId };
    }
    if (outcome.option_id === "allow_session") {
      const sessionSuggestions = scopedSuggestions.session;
      const fallbackSuggestions: PermissionUpdate[] | undefined =
        sessionSuggestions.length > 0
          ? sessionSuggestions
          : toolName
            ? [
                {
                  type: "addRules",
                  rules: [{ toolName }],
                  behavior: "allow",
                  destination: "session",
                },
              ]
            : undefined;
      return {
        behavior: "allow",
        updatedInput: inputData,
        ...(fallbackSuggestions && fallbackSuggestions.length > 0
          ? { updatedPermissions: fallbackSuggestions }
          : {}),
        toolUseID: toolCallId,
      };
    }
    if (outcome.option_id === "allow_always") {
      const suggestionsForAlways = scopedSuggestions.persistent;
      return {
        behavior: "allow",
        updatedInput: inputData,
        ...(suggestionsForAlways && suggestionsForAlways.length > 0
          ? { updatedPermissions: suggestionsForAlways }
          : {}),
        toolUseID: toolCallId,
      };
    }
    return { behavior: "deny", message: "Permission denied", toolUseID: toolCallId };
  }
  return { behavior: "deny", message: "Permission cancelled", toolUseID: toolCallId };
}

function handlePermissionResponse(command: Extract<BridgeCommand, { command: "permission_response" }>): void {
  const session = sessionById(command.session_id);
  if (!session) {
    logPermissionDebug(
      `response dropped: unknown session session_id=${command.session_id} tool_call_id=${command.tool_call_id}`,
    );
    return;
  }
  const resolver = session.pendingPermissions.get(command.tool_call_id);
  if (!resolver) {
    logPermissionDebug(
      `response dropped: no pending resolver session_id=${command.session_id} tool_call_id=${command.tool_call_id}`,
    );
    return;
  }
  session.pendingPermissions.delete(command.tool_call_id);

  const outcome = command.outcome as PermissionOutcome;
  const selectedOption = outcome.outcome === "selected" ? outcome.option_id : "cancelled";
  logPermissionDebug(
    `response session_id=${command.session_id} tool_call_id=${command.tool_call_id} tool=${resolver.toolName} ` +
      `selected=${selectedOption} suggestions=${formatPermissionUpdates(resolver.suggestions)}`,
  );
  if (
    outcome.outcome === "selected" &&
    (outcome.option_id === "allow_once" ||
      outcome.option_id === "allow_session" ||
      outcome.option_id === "allow_always")
  ) {
    setToolCallStatus(session, command.tool_call_id, "in_progress");
  } else if (outcome.outcome === "selected") {
    setToolCallStatus(session, command.tool_call_id, "failed", "Permission denied");
  } else {
    setToolCallStatus(session, command.tool_call_id, "failed", "Permission cancelled");
  }

  const permissionResult = permissionResultFromOutcome(
    outcome,
    command.tool_call_id,
    resolver.inputData,
    resolver.suggestions,
    resolver.toolName,
  );
  if (permissionResult.behavior === "allow") {
    logPermissionDebug(
      `result tool_call_id=${command.tool_call_id} behavior=allow updated_permissions=` +
        `${formatPermissionUpdates(permissionResult.updatedPermissions)}`,
    );
  } else {
    logPermissionDebug(
      `result tool_call_id=${command.tool_call_id} behavior=deny message=${permissionResult.message}`,
    );
  }
  resolver.resolve(permissionResult);
}

async function handleCommand(command: BridgeCommand, requestId?: string): Promise<void> {
  switch (command.command) {
    case "initialize":
      writeEvent(
        {
          event: "initialized",
          result: {
            agent_name: "claude-rs-agent-bridge",
            agent_version: "0.1.0",
            auth_methods: [
              {
                id: "claude-login",
                name: "Log in with Claude",
                description: "Run `claude /login` in a terminal",
              },
            ],
            capabilities: {
              prompt_image: false,
              prompt_embedded_context: true,
              load_session: true,
              supports_list_sessions: true,
              supports_resume: true,
            },
          },
        },
        requestId,
      );
      writeEvent({
        event: "sessions_listed",
        sessions: listRecentPersistedSessions().map((entry) => ({
          session_id: entry.session_id,
          cwd: entry.cwd,
          ...(entry.title ? { title: entry.title } : {}),
          ...(entry.updated_at ? { updated_at: entry.updated_at } : {}),
        })),
      });
      return;

    case "create_session":
      await createSession({
        cwd: command.cwd,
        yolo: command.yolo,
        model: command.model,
        resume: command.resume,
        connectEvent: "connected",
        requestId,
      });
      return;

    case "load_session": {
      const persisted = resolvePersistedSessionEntry(command.session_id);
      if (!persisted) {
        slashError(command.session_id, `unknown session: ${command.session_id}`, requestId);
        return;
      }
      const resumeUpdates = extractSessionHistoryUpdatesFromJsonl(persisted.file_path);
      const staleSessions = Array.from(sessions.values());
      const hadActiveSession = staleSessions.length > 0;
      try {
        await createSession({
          cwd: persisted.cwd,
          yolo: false,
          resume: command.session_id,
          ...(resumeUpdates.length > 0 ? { resumeUpdates } : {}),
          connectEvent: hadActiveSession ? "session_replaced" : "connected",
          requestId,
          ...(hadActiveSession ? { sessionsToCloseAfterConnect: staleSessions } : {}),
        });
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        slashError(command.session_id, `failed to resume session: ${message}`, requestId);
      }
      return;
    }

    case "new_session":
      await closeAllSessions();
      await createSession({
        cwd: command.cwd,
        yolo: command.yolo,
        model: command.model,
        connectEvent: "session_replaced",
        requestId,
      });
      return;

    case "prompt": {
      const session = sessionById(command.session_id);
      if (!session) {
        slashError(command.session_id, `unknown session: ${command.session_id}`, requestId);
        return;
      }
      const text = textFromPrompt(command);
      if (!text.trim()) {
        return;
      }
      session.input.enqueue({
        type: "user",
        session_id: session.sessionId,
        parent_tool_use_id: null,
        message: {
          role: "user",
          content: [{ type: "text", text }],
        },
      } as SDKUserMessage);
      return;
    }

    case "cancel_turn": {
      const session = sessionById(command.session_id);
      if (!session) {
        slashError(command.session_id, `unknown session: ${command.session_id}`, requestId);
        return;
      }
      await session.query.interrupt();
      return;
    }

    case "set_model": {
      const session = sessionById(command.session_id);
      if (!session) {
        slashError(command.session_id, `unknown session: ${command.session_id}`, requestId);
        return;
      }
      await session.query.setModel(command.model);
      session.model = command.model;
      emitSessionUpdate(session.sessionId, {
        type: "config_option_update",
        option_id: "model",
        value: command.model,
      });
      return;
    }

    case "set_mode": {
      const session = sessionById(command.session_id);
      if (!session) {
        slashError(command.session_id, `unknown session: ${command.session_id}`, requestId);
        return;
      }
      const mode = toPermissionMode(command.mode);
      if (!mode) {
        slashError(command.session_id, `unsupported mode: ${command.mode}`, requestId);
        return;
      }
      await session.query.setPermissionMode(mode);
      session.mode = mode;
      emitSessionUpdate(session.sessionId, {
        type: "current_mode_update",
        current_mode_id: mode,
      });
      return;
    }

    case "permission_response":
      handlePermissionResponse(command);
      return;

    case "shutdown":
      await closeAllSessions();
      process.exit(0);

    default:
      failConnection(`unhandled command: ${(command as { command?: string }).command ?? "unknown"}`, requestId);
  }
}

function main(): void {
  const rl = readline.createInterface({
    input: process.stdin,
    crlfDelay: Number.POSITIVE_INFINITY,
  });

  rl.on("line", (line) => {
    if (line.trim().length === 0) {
      return;
    }
    void (async () => {
      let parsed: { requestId?: string; command: BridgeCommand };
      try {
        parsed = parseCommandEnvelope(line);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        failConnection(`invalid command envelope: ${message}`);
        return;
      }

      try {
        await handleCommand(parsed.command, parsed.requestId);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        failConnection(
          `bridge command failed (${parsed.command.command}): ${message}`,
          parsed.requestId,
        );
      }
    })();
  });

  rl.on("close", () => {
    void closeAllSessions().finally(() => process.exit(0));
  });
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main();
}
