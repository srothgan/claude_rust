import test from "node:test";
import assert from "node:assert/strict";
import {
  buildToolResultFields,
  buildUsageUpdateFromResult,
  createToolCall,
  looksLikeAuthRequired,
  normalizeToolKind,
  parseCommandEnvelope,
  permissionOptionsFromSuggestions,
  permissionResultFromOutcome,
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
