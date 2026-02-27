import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import {
  CACHE_SPLIT_POLICY,
  buildToolResultFields,
  buildUsageUpdateFromResult,
  createToolCall,
  extractSessionHistoryUpdatesFromJsonl,
  looksLikeAuthRequired,
  normalizeToolResultText,
  normalizeToolKind,
  parseCommandEnvelope,
  permissionOptionsFromSuggestions,
  permissionResultFromOutcome,
  previewKilobyteLabel,
  unwrapToolUseResult,
} from "./bridge.js";

test("parseCommandEnvelope validates initialize command", () => {
  const parsed = parseCommandEnvelope(
    JSON.stringify({
      request_id: "req-1",
      command: "initialize",
      cwd: "C:/work",
    }),
  );
  assert.equal(parsed.requestId, "req-1");
  assert.equal(parsed.command.command, "initialize");
  if (parsed.command.command !== "initialize") {
    throw new Error("unexpected command variant");
  }
  assert.equal(parsed.command.cwd, "C:/work");
});

test("parseCommandEnvelope validates load_session command without cwd", () => {
  const parsed = parseCommandEnvelope(
    JSON.stringify({
      request_id: "req-2",
      command: "load_session",
      session_id: "session-123",
    }),
  );
  assert.equal(parsed.requestId, "req-2");
  assert.equal(parsed.command.command, "load_session");
  if (parsed.command.command !== "load_session") {
    throw new Error("unexpected command variant");
  }
  assert.equal(parsed.command.session_id, "session-123");
});

test("parseCommandEnvelope rejects missing required fields", () => {
  assert.throws(
    () => parseCommandEnvelope(JSON.stringify({ command: "set_model", session_id: "s1" })),
    /set_model\.model must be a string/,
  );
});

test("normalizeToolKind maps known tool names", () => {
  assert.equal(normalizeToolKind("Bash"), "execute");
  assert.equal(normalizeToolKind("Delete"), "delete");
  assert.equal(normalizeToolKind("Move"), "move");
  assert.equal(normalizeToolKind("ExitPlanMode"), "switch_mode");
  assert.equal(normalizeToolKind("TodoWrite"), "other");
});

test("createToolCall builds edit diff content", () => {
  const toolCall = createToolCall("tc-1", "Edit", {
    file_path: "src/main.rs",
    old_string: "old",
    new_string: "new",
  });
  assert.equal(toolCall.kind, "edit");
  assert.equal(toolCall.content.length, 1);
  assert.deepEqual(toolCall.content[0], {
    type: "diff",
    old_path: "src/main.rs",
    new_path: "src/main.rs",
    old: "old",
    new: "new",
  });
  assert.deepEqual(toolCall.meta, { claudeCode: { toolName: "Edit" } });
});

test("createToolCall builds write preview diff content", () => {
  const toolCall = createToolCall("tc-w", "Write", {
    file_path: "src/new-file.ts",
    content: "export const x = 1;\n",
  });
  assert.equal(toolCall.kind, "edit");
  assert.deepEqual(toolCall.content, [
    {
      type: "diff",
      old_path: "src/new-file.ts",
      new_path: "src/new-file.ts",
      old: "",
      new: "export const x = 1;\n",
    },
  ]);
});

test("createToolCall includes glob and webfetch context in title", () => {
  const glob = createToolCall("tc-g", "Glob", { pattern: "**/*.md", path: "notes" });
  assert.equal(glob.title, "Glob **/*.md in notes");

  const fetch = createToolCall("tc-f", "WebFetch", { url: "https://example.com" });
  assert.equal(fetch.title, "WebFetch https://example.com");
});

test("buildToolResultFields extracts plain-text output", () => {
  const fields = buildToolResultFields(false, [{ text: "line 1" }, { text: "line 2" }]);
  assert.equal(fields.status, "completed");
  assert.equal(fields.raw_output, "line 1\nline 2");
  assert.deepEqual(fields.content, [
    { type: "content", content: { type: "text", text: "line 1\nline 2" } },
  ]);
});

test("normalizeToolResultText collapses persisted-output payload to first meaningful line", () => {
  const normalized = normalizeToolResultText(`
<persisted-output>
  │ Output too large (132.5KB). Full output saved to: C:\\tmp\\tool-results\\bbf63b9.txt
  │
  │ Preview (first 2KB):
  │
  │ {"huge":"payload"}
  │ ...
  │ </persisted-output>
`);
  assert.equal(normalized, "Output too large (132.5KB). Full output saved to: C:\\tmp\\tool-results\\bbf63b9.txt");
});

test("cache split policy defaults stay aligned with UI thresholds", () => {
  assert.equal(CACHE_SPLIT_POLICY.softLimitBytes, 1536);
  assert.equal(CACHE_SPLIT_POLICY.hardLimitBytes, 4096);
  assert.equal(CACHE_SPLIT_POLICY.previewLimitBytes, 2048);
  assert.equal(previewKilobyteLabel(CACHE_SPLIT_POLICY), "2KB");
});

test("buildToolResultFields uses normalized persisted-output text", () => {
  const fields = buildToolResultFields(
    false,
    `<persisted-output>
      │ Output too large (14KB). Full output saved to: C:\\tmp\\tool-results\\x.txt
      │
      │ Preview (first 2KB):
      │ {"k":"v"}
      │ </persisted-output>`,
  );
  assert.equal(fields.raw_output, "Output too large (14KB). Full output saved to: C:\\tmp\\tool-results\\x.txt");
  assert.deepEqual(fields.content, [
    {
      type: "content",
      content: {
        type: "text",
        text: "Output too large (14KB). Full output saved to: C:\\tmp\\tool-results\\x.txt",
      },
    },
  ]);
});

test("buildToolResultFields maps structured Write output to diff content", () => {
  const base = createToolCall("tc-w", "Write", {
    file_path: "src/main.ts",
    content: "new",
  });
  const fields = buildToolResultFields(
    false,
    {
      type: "update",
      filePath: "src/main.ts",
      content: "new",
      originalFile: "old",
      structuredPatch: [],
    },
    base,
  );
  assert.equal(fields.status, "completed");
  assert.deepEqual(fields.content, [
    {
      type: "diff",
      old_path: "src/main.ts",
      new_path: "src/main.ts",
      old: "old",
      new: "new",
    },
  ]);
});

test("buildToolResultFields preserves Edit diff content from input", () => {
  const base = createToolCall("tc-e", "Edit", {
    file_path: "src/main.ts",
    old_string: "old",
    new_string: "new",
  });
  const fields = buildToolResultFields(false, [{ text: "Updated successfully" }], base);
  assert.equal(fields.status, "completed");
  assert.deepEqual(fields.content, [
    {
      type: "diff",
      old_path: "src/main.ts",
      new_path: "src/main.ts",
      old: "old",
      new: "new",
    },
  ]);
});

test("unwrapToolUseResult extracts error/content payload", () => {
  const parsed = unwrapToolUseResult({
    is_error: true,
    content: [{ text: "failure output" }],
  });
  assert.equal(parsed.isError, true);
  assert.deepEqual(parsed.content, [{ text: "failure output" }]);
});

test("permissionResultFromOutcome maps selected and cancelled outcomes", () => {
  const allow = permissionResultFromOutcome(
    { outcome: "selected", option_id: "allow_always" },
    "tool-1",
    { command: "echo test" },
    [],
  );
  assert.equal(allow.behavior, "allow");
  if (allow.behavior === "allow") {
    assert.deepEqual(allow.updatedInput, { command: "echo test" });
  }

  const deny = permissionResultFromOutcome(
    { outcome: "selected", option_id: "reject_once" },
    "tool-1",
    { command: "echo test" },
  );
  assert.equal(deny.behavior, "deny");
  assert.match(String(deny.message), /Permission denied/);

  const cancelled = permissionResultFromOutcome(
    { outcome: "cancelled" },
    "tool-1",
    { command: "echo test" },
  );
  assert.equal(cancelled.behavior, "deny");
  assert.match(String(cancelled.message), /cancelled/i);
});

test("permissionOptionsFromSuggestions uses session label when only session scope is suggested", () => {
  const options = permissionOptionsFromSuggestions([
    {
      type: "setMode",
      mode: "acceptEdits",
      destination: "session",
    },
  ]);
  assert.deepEqual(options, [
    { option_id: "allow_once", name: "Allow once", kind: "allow_once" },
    { option_id: "allow_session", name: "Allow for session", kind: "allow_session" },
    { option_id: "reject_once", name: "Deny", kind: "reject_once" },
  ]);
});

test("permissionOptionsFromSuggestions uses persistent label when settings scope is suggested", () => {
  const options = permissionOptionsFromSuggestions([
    {
      type: "addRules",
      behavior: "allow",
      destination: "localSettings",
      rules: [{ toolName: "Bash", ruleContent: "npm install" }],
    },
  ]);
  assert.deepEqual(options, [
    { option_id: "allow_once", name: "Allow once", kind: "allow_once" },
    { option_id: "allow_always", name: "Always allow", kind: "allow_always" },
    { option_id: "reject_once", name: "Deny", kind: "reject_once" },
  ]);
});

test("permissionResultFromOutcome keeps Bash allow_always suggestions unchanged", () => {
  const allow = permissionResultFromOutcome(
    { outcome: "selected", option_id: "allow_always" },
    "tool-1",
    { command: "npm install" },
    [
      {
        type: "addRules",
        behavior: "allow",
        destination: "projectSettings",
        rules: [
          { toolName: "Bash", ruleContent: "npm install" },
          { toolName: "WebFetch", ruleContent: "https://example.com" },
          { toolName: "Bash", ruleContent: "dir /B" },
        ],
      },
    ],
    "Bash",
  );

  assert.equal(allow.behavior, "allow");
  if (allow.behavior !== "allow") {
    throw new Error("expected allow permission result");
  }
  assert.deepEqual(allow.updatedPermissions, [
    {
      type: "addRules",
      behavior: "allow",
      destination: "projectSettings",
      rules: [
        { toolName: "Bash", ruleContent: "npm install" },
        { toolName: "WebFetch", ruleContent: "https://example.com" },
        { toolName: "Bash", ruleContent: "dir /B" },
      ],
    },
  ]);
});

test("permissionResultFromOutcome keeps Write allow_session suggestions unchanged", () => {
  const suggestions = [
    {
      type: "addRules" as const,
      behavior: "allow" as const,
      destination: "session" as const,
      rules: [{ toolName: "Write", ruleContent: "C:\\work\\foo.txt" }],
    },
  ];
  const allow = permissionResultFromOutcome(
    { outcome: "selected", option_id: "allow_session" },
    "tool-2",
    { file_path: "C:\\work\\foo.txt" },
    suggestions,
    "Write",
  );

  assert.equal(allow.behavior, "allow");
  if (allow.behavior !== "allow") {
    throw new Error("expected allow permission result");
  }
  assert.deepEqual(allow.updatedPermissions, suggestions);
});

test("permissionResultFromOutcome falls back to session tool rule for allow_session when suggestions are missing", () => {
  const allow = permissionResultFromOutcome(
    { outcome: "selected", option_id: "allow_session" },
    "tool-3",
    { file_path: "C:\\work\\bar.txt" },
    undefined,
    "Write",
  );

  assert.equal(allow.behavior, "allow");
  if (allow.behavior !== "allow") {
    throw new Error("expected allow permission result");
  }
  assert.deepEqual(allow.updatedPermissions, [
    {
      type: "addRules",
      behavior: "allow",
      destination: "session",
      rules: [{ toolName: "Write" }],
    },
  ]);
});

test("permissionResultFromOutcome does not apply session suggestions to allow_always", () => {
  const allow = permissionResultFromOutcome(
    { outcome: "selected", option_id: "allow_always" },
    "tool-4",
    { file_path: "C:\\work\\baz.txt" },
    [
      {
        type: "addRules",
        behavior: "allow",
        destination: "session",
        rules: [{ toolName: "Write", ruleContent: "C:\\work\\baz.txt" }],
      },
    ],
    "Write",
  );

  assert.equal(allow.behavior, "allow");
  if (allow.behavior !== "allow") {
    throw new Error("expected allow permission result");
  }
  assert.equal(allow.updatedPermissions, undefined);
});

test("buildUsageUpdateFromResult maps SDK camelCase usage keys", () => {
  const update = buildUsageUpdateFromResult({
    usage: {
      inputTokens: 12,
      outputTokens: 34,
      cacheReadInputTokens: 5,
      cacheCreationInputTokens: 6,
    },
  });
  assert.deepEqual(update, {
    type: "usage_update",
    usage: {
      input_tokens: 12,
      output_tokens: 34,
      cache_read_tokens: 5,
      cache_write_tokens: 6,
    },
  });
});

test("buildUsageUpdateFromResult includes cost and context window fields", () => {
  const update = buildUsageUpdateFromResult({
    total_cost_usd: 1.25,
    modelUsage: {
      "claude-sonnet-4-5": {
        contextWindow: 200000,
        maxOutputTokens: 64000,
      },
    },
  });
  assert.deepEqual(update, {
    type: "usage_update",
    usage: {
      total_cost_usd: 1.25,
      context_window: 200000,
      max_output_tokens: 64000,
    },
  });
});

test("looksLikeAuthRequired detects login hints", () => {
  assert.equal(looksLikeAuthRequired("Please run /login to continue"), true);
  assert.equal(looksLikeAuthRequired("normal tool output"), false);
});

function withTempJsonl(lines: unknown[], run: (filePath: string) => void): void {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "claude-rs-resume-test-"));
  const filePath = path.join(dir, "session.jsonl");
  fs.writeFileSync(filePath, `${lines.map((line) => JSON.stringify(line)).join("\n")}\n`, "utf8");
  try {
    run(filePath);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

test("extractSessionHistoryUpdatesFromJsonl parses nested progress message records", () => {
  const lines = [
    {
      type: "user",
      message: {
        role: "user",
        content: [{ type: "text", text: "Top-level user prompt" }],
      },
    },
    {
      type: "progress",
      data: {
        message: {
          type: "assistant",
          message: {
            id: "msg-nested-1",
            role: "assistant",
            content: [
              {
                type: "tool_use",
                id: "tool-nested-1",
                name: "Bash",
                input: { command: "echo hello" },
              },
            ],
            usage: {
              input_tokens: 11,
              output_tokens: 7,
              cache_read_input_tokens: 5,
              cache_creation_input_tokens: 3,
            },
          },
        },
      },
    },
    {
      type: "progress",
      data: {
        message: {
          type: "user",
          message: {
            role: "user",
            content: [
              {
                type: "tool_result",
                tool_use_id: "tool-nested-1",
                content: "ok",
                is_error: false,
              },
            ],
          },
        },
      },
    },
    {
      type: "progress",
      data: {
        message: {
          type: "assistant",
          message: {
            id: "msg-nested-1",
            role: "assistant",
            content: [{ type: "text", text: "Nested assistant final" }],
            usage: {
              input_tokens: 11,
              output_tokens: 7,
              cache_read_input_tokens: 5,
              cache_creation_input_tokens: 3,
            },
          },
        },
      },
    },
  ];

  withTempJsonl(lines, (filePath) => {
    const updates = extractSessionHistoryUpdatesFromJsonl(filePath);
    const variantCounts = new Map<string, number>();
    for (const update of updates) {
      variantCounts.set(update.type, (variantCounts.get(update.type) ?? 0) + 1);
    }

    assert.equal(variantCounts.get("user_message_chunk"), 1);
    assert.equal(variantCounts.get("agent_message_chunk"), 1);
    assert.equal(variantCounts.get("tool_call"), 1);
    assert.equal(variantCounts.get("tool_call_update"), 1);
    assert.equal(variantCounts.get("usage_update"), 1);

    const usage = updates.find((update) => update.type === "usage_update");
    assert.ok(usage && usage.type === "usage_update");
    assert.deepEqual(usage.usage, {
      input_tokens: 11,
      output_tokens: 7,
      cache_read_tokens: 5,
      cache_write_tokens: 3,
    });
  });
});

test("extractSessionHistoryUpdatesFromJsonl ignores invalid records", () => {
  withTempJsonl(
    [
      { type: "queue-operation", operation: "enqueue" },
      { type: "progress", data: { not_message: true } },
      { type: "user", message: { role: "assistant", content: [{ type: "thinking", thinking: "h" }] } },
    ],
    (filePath) => {
      const updates = extractSessionHistoryUpdatesFromJsonl(filePath);
      assert.equal(updates.length, 0);
    },
  );
});
