import { randomUUID } from "node:crypto";
import { spawn as spawnChild } from "node:child_process";
import fs from "node:fs";
import readline from "node:readline";
import { pathToFileURL } from "node:url";
import {
  query,
  type CanUseTool,
  type PermissionMode,
  type PermissionResult,
  type PermissionUpdate,
  type Query,
  type SDKMessage,
  type SDKUserMessage,
} from "@anthropic-ai/claude-agent-sdk";
import type {
  AvailableCommand,
  BridgeCommand,
  BridgeEvent,
  BridgeEventEnvelope,
  Json,
  PermissionOutcome,
  PermissionOption,
  PermissionRequest,
  PlanEntry,
  SessionUpdate,
  ToolCall,
  ToolCallUpdate,
  ToolCallUpdateFields,
} from "./types.js";
import { parseCommandEnvelope, toPermissionMode, buildModeState } from "./bridge/commands.js";
import { asRecordOrNull } from "./bridge/shared.js";
import { looksLikeAuthRequired } from "./bridge/auth.js";
import {
  TOOL_RESULT_TYPES,
  buildToolResultFields,
  createToolCall,
  isToolUseBlockType,
  normalizeToolKind,
  normalizeToolResultText,
  unwrapToolUseResult,
} from "./bridge/tooling.js";
import { CACHE_SPLIT_POLICY, previewKilobyteLabel } from "./bridge/cache_policy.js";
import { buildUsageUpdateFromResult, buildUsageUpdateFromResultForSession } from "./bridge/usage.js";
import {
  formatPermissionUpdates,
  permissionOptionsFromSuggestions,
  permissionResultFromOutcome,
} from "./bridge/permissions.js";
import {
  extractSessionHistoryUpdatesFromJsonl,
  listRecentPersistedSessions,
  resolvePersistedSessionEntry,
} from "./bridge/history.js";

export {
  CACHE_SPLIT_POLICY,
  buildToolResultFields,
  buildUsageUpdateFromResult,
  createToolCall,
  extractSessionHistoryUpdatesFromJsonl,
  looksLikeAuthRequired,
  normalizeToolKind,
  normalizeToolResultText,
  parseCommandEnvelope,
  permissionOptionsFromSuggestions,
  permissionResultFromOutcome,
  previewKilobyteLabel,
  unwrapToolUseResult,
};

type ConnectEventKind = "connected" | "session_replaced";

type PendingPermission = {
  resolve?: (result: PermissionResult) => void;
  onOutcome?: (outcome: PermissionOutcome) => void;
  toolName: string;
  inputData: Record<string, unknown>;
  suggestions?: PermissionUpdate[];
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

const sessions = new Map<string, SessionState>();
const permissionDebugEnabled =
  process.env.CLAUDE_RS_SDK_PERMISSION_DEBUG === "1" || process.env.CLAUDE_RS_SDK_DEBUG === "1";

function logPermissionDebug(message: string): void {
  if (!permissionDebugEnabled) {
    return;
  }
  console.error(`[perm debug] ${message}`);
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

function emitSessionUpdate(sessionId: string, update: SessionUpdate): void {
  writeEvent({ event: "session_update", session_id: sessionId, update });
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
          mode: buildModeState(session.mode),
          ...(historyUpdates && historyUpdates.length > 0 ? { history_updates: historyUpdates } : {}),
        }
      : {
          event: "connected",
          session_id: session.sessionId,
          cwd: session.cwd,
          model_name: session.model,
          mode: buildModeState(session.mode),
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
          mode: buildModeState(session.mode),
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

type AskUserQuestionOption = {
  label: string;
  description: string;
};

type AskUserQuestionPrompt = {
  question: string;
  header: string;
  multiSelect: boolean;
  options: AskUserQuestionOption[];
};

const ASK_USER_QUESTION_TOOL_NAME = "AskUserQuestion";
const QUESTION_CHOICE_KIND = "question_choice";

function parseAskUserQuestionPrompts(inputData: Record<string, unknown>): AskUserQuestionPrompt[] {
  const rawQuestions = Array.isArray(inputData.questions) ? inputData.questions : [];
  const prompts: AskUserQuestionPrompt[] = [];

  for (const rawQuestion of rawQuestions) {
    const questionRecord = asRecordOrNull(rawQuestion);
    if (!questionRecord) {
      continue;
    }
    const question = typeof questionRecord.question === "string" ? questionRecord.question.trim() : "";
    if (!question) {
      continue;
    }
    const headerRaw = typeof questionRecord.header === "string" ? questionRecord.header.trim() : "";
    const header = headerRaw || `Q${prompts.length + 1}`;
    const multiSelect = Boolean(questionRecord.multiSelect);
    const rawOptions = Array.isArray(questionRecord.options) ? questionRecord.options : [];
    const options: AskUserQuestionOption[] = [];
    for (const rawOption of rawOptions) {
      const optionRecord = asRecordOrNull(rawOption);
      if (!optionRecord) {
        continue;
      }
      const label = typeof optionRecord.label === "string" ? optionRecord.label.trim() : "";
      const description =
        typeof optionRecord.description === "string" ? optionRecord.description.trim() : "";
      if (!label) {
        continue;
      }
      options.push({ label, description });
    }
    if (options.length < 2) {
      continue;
    }
    prompts.push({ question, header, multiSelect, options });
  }

  return prompts;
}

function askUserQuestionOptions(prompt: AskUserQuestionPrompt): PermissionOption[] {
  return prompt.options.map((option, index) => ({
    option_id: `question_${index}`,
    name: option.label,
    description: option.description,
    kind: QUESTION_CHOICE_KIND,
  }));
}

function askUserQuestionPromptToolCall(
  base: ToolCall,
  prompt: AskUserQuestionPrompt,
  index: number,
  total: number,
): ToolCall {
  return {
    ...base,
    title: prompt.question,
    raw_input: {
      questions: [
        {
          question: prompt.question,
          header: prompt.header,
          multiSelect: prompt.multiSelect,
          options: prompt.options,
        },
      ],
      question_index: index,
      total_questions: total,
    },
  };
}

function askUserQuestionTranscript(
  answers: Array<{ header: string; question: string; answer: string }>,
): string {
  return answers.map((entry) => `${entry.header}: ${entry.answer}\n  ${entry.question}`).join("\n");
}

async function requestAskUserQuestionAnswers(
  session: SessionState,
  toolUseId: string,
  toolName: string,
  inputData: Record<string, unknown>,
  baseToolCall: ToolCall,
): Promise<PermissionResult> {
  const prompts = parseAskUserQuestionPrompts(inputData);
  if (prompts.length === 0) {
    return { behavior: "allow", updatedInput: inputData, toolUseID: toolUseId };
  }

  const answers: Record<string, string> = {};
  const transcript: Array<{ header: string; question: string; answer: string }> = [];

  for (const [index, prompt] of prompts.entries()) {
    const promptToolCall = askUserQuestionPromptToolCall(baseToolCall, prompt, index, prompts.length);
    const fields: ToolCallUpdateFields = {
      title: promptToolCall.title,
      status: "in_progress",
      raw_input: promptToolCall.raw_input,
    };
    emitSessionUpdate(session.sessionId, {
      type: "tool_call_update",
      tool_call_update: { tool_call_id: toolUseId, fields },
    });
    const tracked = session.toolCalls.get(toolUseId);
    if (tracked) {
      tracked.title = promptToolCall.title;
      tracked.status = "in_progress";
      tracked.raw_input = promptToolCall.raw_input;
    }

    const request: PermissionRequest = {
      tool_call: promptToolCall,
      options: askUserQuestionOptions(prompt),
    };

    const outcome = await new Promise<PermissionOutcome>((resolve) => {
      session.pendingPermissions.set(toolUseId, {
        onOutcome: resolve,
        toolName,
        inputData,
      });
      writeEvent({ event: "permission_request", session_id: session.sessionId, request });
    });

    if (outcome.outcome !== "selected") {
      setToolCallStatus(session, toolUseId, "failed", "Question cancelled");
      return { behavior: "deny", message: "Question cancelled", toolUseID: toolUseId };
    }

    const selected = request.options.find((option) => option.option_id === outcome.option_id);
    if (!selected) {
      setToolCallStatus(session, toolUseId, "failed", "Question answer was invalid");
      return { behavior: "deny", message: "Question answer was invalid", toolUseID: toolUseId };
    }

    answers[prompt.question] = selected.name;
    transcript.push({ header: prompt.header, question: prompt.question, answer: selected.name });

    const summary = askUserQuestionTranscript(transcript);
    const progressFields: ToolCallUpdateFields = {
      status: index + 1 >= prompts.length ? "completed" : "in_progress",
      raw_output: summary,
      content: [{ type: "content", content: { type: "text", text: summary } }],
    };
    emitSessionUpdate(session.sessionId, {
      type: "tool_call_update",
      tool_call_update: { tool_call_id: toolUseId, fields: progressFields },
    });
    if (tracked) {
      tracked.status = progressFields.status ?? tracked.status;
      tracked.raw_output = summary;
      tracked.content = progressFields.content ?? tracked.content;
    }
  }

  return {
    behavior: "allow",
    updatedInput: { ...inputData, answers },
    toolUseID: toolUseId,
  };
}

async function closeSession(session: SessionState): Promise<void> {
  session.input.close();
  session.query.close();
  for (const pending of session.pendingPermissions.values()) {
    pending.resolve?.({ behavior: "deny", message: "Session closed" });
    pending.onOutcome?.({ outcome: "cancelled" });
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

    if (toolName === ASK_USER_QUESTION_TOOL_NAME) {
      return await requestAskUserQuestionAnswers(
        session,
        toolUseId,
        toolName,
        inputData,
        existing,
      );
    }

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
  if (resolver.onOutcome) {
    resolver.onOutcome(outcome);
    return;
  }
  if (!resolver.resolve) {
    logPermissionDebug(
      `response dropped: resolver missing callback session_id=${command.session_id} tool_call_id=${command.tool_call_id}`,
    );
    return;
  }
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


