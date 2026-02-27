import type { Json, ToolCall, ToolCallUpdateFields } from "../types.js";
import { asRecordOrNull } from "./shared.js";
import { CACHE_SPLIT_POLICY, previewKilobyteLabel } from "./cache_policy.js";

export const TOOL_RESULT_TYPES = new Set([
  "tool_result",
  "tool_search_tool_result",
  "web_fetch_tool_result",
  "web_search_tool_result",
  "code_execution_tool_result",
  "bash_code_execution_tool_result",
  "text_editor_code_execution_tool_result",
  "mcp_tool_result",
]);

export function isToolUseBlockType(blockType: string): boolean {
  return blockType === "tool_use" || blockType === "server_tool_use" || blockType === "mcp_tool_use";
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
const EXPECTED_PREVIEW_LINE = `preview (first ${previewKilobyteLabel(CACHE_SPLIT_POLICY).toLowerCase()}):`;

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
      if (cleaned.toLowerCase() === EXPECTED_PREVIEW_LINE) {
        continue;
      }
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
    if (base?.content.some((entry) => entry.type === "diff")) {
      return fields;
    }
  }

  if (rawOutput) {
    fields.content = [{ type: "content", content: { type: "text", text: rawOutput } }];
  }
  return fields;
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

