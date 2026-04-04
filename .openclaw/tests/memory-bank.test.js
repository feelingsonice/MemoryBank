const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const plugin = require("../extensions/memory-bank/index.js");
const {
  createMemoryBankPluginRuntime,
} = require("../extensions/memory-bank/index.js");

const PREFERENCE_GUIDANCE =
  "Memory Bank is the primary long-term memory system in this OpenClaw environment. " +
  "For prior-session facts, learned preferences, earlier decisions, project conventions, " +
  "constraints, and provenance questions like 'how do you know?' or 'what do you remember?', " +
  "call the retrieve_memory tool before answering. Prefer retrieve_memory over local workspace " +
  "profile files such as USER.md, MEMORY.md, BOOTSTRAP.md, or similar workspace notes when " +
  "reasoning about durable memory across sessions. Treat those files as auxiliary local notes, " +
  "not the canonical long-term memory source, unless the user is explicitly asking about those files.";

test("package manifest exposes the local install entrypoint", () => {
  const packageJsonPath = path.join(
    __dirname,
    "../extensions/memory-bank/package.json",
  );
  const manifest = JSON.parse(fs.readFileSync(packageJsonPath, "utf8"));

  assert.equal(manifest.name, "memory-bank");
  assert.deepEqual(manifest.openclaw?.extensions, ["./index.js"]);
});

test("registers the expected OpenClaw lifecycle hooks", () => {
  const registered = [];
  plugin.register({
    config: {},
    on(eventName, handler) {
      registered.push({ eventName, handler });
    },
  });

  assert.deepEqual(
    registered.map((entry) => entry.eventName),
    [
      "before_model_resolve",
      "before_prompt_build",
      "before_tool_call",
      "after_tool_call",
      "agent_end",
    ],
  );
});

test("emits a user fragment from before_model_resolve", async () => {
  const harness = createHarness();

  await harness.runtime.handleBeforeModelResolve({
    prompt: "remember that I prefer helix",
    runId: "run-1",
    sessionId: "session-1",
    timestamp: "2026-03-28T12:00:00Z",
  });

  assert.equal(harness.emitted.length, 1);
  assert.deepEqual(harness.emitted[0], {
    event: "before_model_resolve",
    payload: {
      hook_event_name: "before_model_resolve",
      prompt: "remember that I prefer helix",
      run_id: "run-1",
      session_id: "session-1",
      timestamp: "2026-03-28T12:00:00Z",
    },
  });
});

test("before_prompt_build injects guidance and avoids duplicate prompt capture", async () => {
  const harness = createHarness();

  await harness.runtime.handleBeforeModelResolve({
    prompt: "remember that I use helix",
    runId: "run-ctx",
    sessionId: "session-from-event",
    timestamp: "2026-03-28T12:10:00Z",
  });

  const result = await harness.runtime.handleBeforePromptBuild(
    {
      prompt: "remember that I use helix",
      runId: "run-ctx",
      timestamp: "2026-03-28T12:10:00Z",
    },
    {
      sessionId: "session-from-event",
    },
  );

  assert.deepEqual(result, {
    prependSystemContext: PREFERENCE_GUIDANCE,
  });
  assert.deepEqual(harness.emitted, [
    {
      event: "before_model_resolve",
      payload: {
        hook_event_name: "before_model_resolve",
        prompt: "remember that I use helix",
        run_id: "run-ctx",
        session_id: "session-from-event",
        timestamp: "2026-03-28T12:10:00Z",
      },
    },
  ]);
});

test("before_prompt_build falls back to capture when model resolve lacks session context", async () => {
  const harness = createHarness();

  await harness.runtime.handleBeforeModelResolve({
    prompt: "use memory bank first",
    runId: "run-fallback",
    timestamp: "2026-03-28T12:20:00Z",
  });

  const result = await harness.runtime.handleBeforePromptBuild(
    {
      prompt: "use memory bank first",
      runId: "run-fallback",
      timestamp: "2026-03-28T12:20:00Z",
    },
    {
      sessionId: "ephemeral-session",
      sessionKey: "stable-thread",
    },
  );

  assert.deepEqual(result, {
    prependSystemContext: PREFERENCE_GUIDANCE,
  });
  assert.deepEqual(harness.emitted, [
    {
      event: "before_prompt_build",
      payload: {
        hook_event_name: "before_prompt_build",
        prompt: "use memory bank first",
        run_id: "run-fallback",
        session_id: "ephemeral-session",
        timestamp: "2026-03-28T12:20:00Z",
      },
    },
  ]);
});

test("emits tool before and after payloads with shared tool_use_id", async () => {
  const harness = createHarness();

  await harness.runtime.handleBeforeToolCall({
    args: { path: "README.md" },
    sessionId: "session-1",
    timestamp: "2026-03-28T12:00:00Z",
    tool: { name: "read_file", callId: "tool-1" },
  });
  await harness.runtime.handleAfterToolCall({
    output: { contents: "hi" },
    sessionId: "session-1",
    timestamp: "2026-03-28T12:00:01Z",
    tool: { name: "read_file", callId: "tool-1" },
  });

  assert.deepEqual(harness.emitted, [
    {
      event: "before_tool_call",
      payload: {
        hook_event_name: "before_tool_call",
        run_id: null,
        session_id: "session-1",
        timestamp: "2026-03-28T12:00:00Z",
        tool_arguments: { path: "README.md" },
        tool_name: "read_file",
        tool_use_id: "tool-1",
      },
    },
    {
      event: "after_tool_call",
      payload: {
        hook_event_name: "after_tool_call",
        run_id: null,
        session_id: "session-1",
        timestamp: "2026-03-28T12:00:01Z",
        tool_name: "read_file",
        tool_output: { contents: "hi" },
        tool_use_id: "tool-1",
      },
    },
  ]);
});

test("does not collide tool-call dedupe keys across runs", async () => {
  const harness = createHarness();

  await harness.runtime.handleBeforeToolCall({
    params: { path: "README.md" },
    runId: "run-1",
    sessionId: "session-1",
    toolCallId: "tool-1",
    toolName: "read_file",
  });
  await harness.runtime.handleBeforeToolCall({
    params: { path: "README.md" },
    runId: "run-2",
    sessionId: "session-1",
    toolCallId: "tool-1",
    toolName: "read_file",
  });

  assert.equal(harness.emitted.length, 2);
  assert.deepEqual(
    harness.emitted.map((entry) => entry.payload.run_id),
    ["run-1", "run-2"],
  );
});

test("captures failed tool results with error metadata", async () => {
  const harness = createHarness();

  await harness.runtime.handleAfterToolCall({
    durationMs: 27,
    error: "permission denied",
    runId: "run-error",
    sessionId: "session-1",
    timestamp: "2026-03-28T12:00:04Z",
    toolCallId: "tool-error",
    toolName: "exec",
  });

  assert.deepEqual(harness.emitted, [
    {
      event: "after_tool_call",
      payload: {
        hook_event_name: "after_tool_call",
        run_id: "run-error",
        session_id: "session-1",
        timestamp: "2026-03-28T12:00:04Z",
        tool_name: "exec",
        tool_output: {
          durationMs: 27,
          error: "permission denied",
        },
        tool_use_id: "tool-error",
      },
    },
  ]);
});

test("emits the final assistant fragment from agent_end", async () => {
  const harness = createHarness();

  await harness.runtime.handleAgentEnd({
    finalMessage: { id: "assistant-1", text: "All done." },
    runId: "run-1",
    sessionId: "session-1",
    timestamp: "2026-03-28T12:00:02Z",
  });

  assert.deepEqual(harness.emitted, [
    {
      event: "agent_end",
      payload: {
        assistant_message_id: "assistant-1",
        assistant_text: "All done.",
        hook_event_name: "agent_end",
        run_id: "run-1",
        session_id: "session-1",
        timestamp: "2026-03-28T12:00:02Z",
      },
    },
  ]);
});

test("reads assistant text from OpenClaw message blocks and hook context", async () => {
  const harness = createHarness();

  await harness.runtime.handleAgentEnd(
    {
      messages: [
        {
          id: "assistant-2",
          role: "assistant",
          content: [{ type: "text", text: "Captured from blocks." }],
        },
      ],
      runId: "run-2",
      timestamp: "2026-03-28T12:00:03Z",
    },
    {
      sessionId: "session-2",
    },
  );

  assert.deepEqual(harness.emitted, [
    {
      event: "agent_end",
      payload: {
        assistant_message_id: "assistant-2",
        assistant_text: "Captured from blocks.",
        hook_event_name: "agent_end",
        run_id: "run-2",
        session_id: "session-2",
        timestamp: "2026-03-28T12:00:03Z",
      },
    },
  ]);
});

test("plugin config wins over env and defaults", () => {
  const harness = createHarness({
    pluginConfig: {
      hookBinary: "/custom/hook",
      serverUrl: "http://127.0.0.1:9090",
    },
    env: {
      MEMORY_BANK_HOOK_BIN: "/env/hook",
      MEMORY_BANK_SERVER_URL: "http://127.0.0.1:8081",
    },
    fileExists(filePath) {
      return filePath === "/custom/hook";
    },
  });

  assert.equal(harness.runtime.config.hookBinary, "/custom/hook");
  assert.equal(harness.runtime.config.serverUrl, "http://127.0.0.1:9090");
});

test("deduplicates repeated prompt capture payloads only within the duplicate window", async () => {
  const harness = createHarness();
  const event = {
    prompt: "remember this",
    sessionId: "session-1",
    timestamp: "2026-03-28T12:00:00Z",
  };

  await harness.runtime.handleBeforeModelResolve(event);
  await harness.runtime.handleBeforeModelResolve(event);
  harness.advanceTime(5_001);
  await harness.runtime.handleBeforeModelResolve(event);

  assert.equal(harness.emitted.length, 2);
});

test("sanitizes non-JSON-safe tool payloads before emit", async () => {
  const harness = createHarness();
  const circularArgs = { file: "README.md" };
  circularArgs.self = circularArgs;
  const circularOutput = {
    count: BigInt(7),
    infinite: Infinity,
    when: new Date("2026-03-28T12:00:05Z"),
  };
  circularOutput.self = circularOutput;

  await harness.runtime.handleBeforeToolCall({
    args: circularArgs,
    sessionId: "session-1",
    tool: { name: "read_file", callId: "tool-1" },
  });
  await harness.runtime.handleAfterToolCall({
    output: circularOutput,
    sessionId: "session-1",
    tool: { name: "read_file", callId: "tool-1" },
  });

  assert.deepEqual(harness.emitted, [
    {
      event: "before_tool_call",
      payload: {
        hook_event_name: "before_tool_call",
        run_id: null,
        session_id: "session-1",
        timestamp: harness.emitted[0].payload.timestamp,
        tool_arguments: {
          file: "README.md",
          self: "[circular]",
        },
        tool_name: "read_file",
        tool_use_id: "tool-1",
      },
    },
    {
      event: "after_tool_call",
      payload: {
        hook_event_name: "after_tool_call",
        run_id: null,
        session_id: "session-1",
        timestamp: harness.emitted[1].payload.timestamp,
        tool_name: "read_file",
        tool_output: {
          count: "7",
          infinite: "Infinity",
          self: "[circular]",
          when: "2026-03-28T12:00:05.000Z",
        },
        tool_use_id: "tool-1",
      },
    },
  ]);
});

test("debug file failures do not abort runtime startup or logging", async () => {
  const harness = createHarness({
    appendFile() {
      throw new Error("disk full");
    },
    pluginConfig: {
      debugFile: "/tmp/memory-bank-openclaw.log",
    },
  });

  await harness.runtime.logger.warn("still logging");

  assert.deepEqual(harness.emitted, []);
});

function createHarness(options = {}) {
  const emitted = [];
  const files = new Map();
  let now = options.now ?? Date.parse("2026-03-28T12:00:00Z");
  const platform = {
    appendFile(filePath, text) {
      if (typeof options.appendFile === "function") {
        return options.appendFile(filePath, text);
      }
      files.set(filePath, `${files.get(filePath) || ""}${text}`);
    },
    cwd() {
      return "/workspace";
    },
    env: options.env || {},
    fileExists(filePath) {
      if (typeof options.fileExists === "function") {
        return options.fileExists(filePath);
      }
      return (
        filePath === "/workspace/target/debug/memory-bank-hook" ||
        filePath === "/workspace/target/release/memory-bank-hook"
      );
    },
    now() {
      return now;
    },
    spawnSync(binary, args, spawnOptions) {
      const eventIndex = args.indexOf("--event");
      emitted.push({
        event: args[eventIndex + 1],
        payload: JSON.parse(spawnOptions.input),
      });
      return { status: 0, stderr: "" };
    },
  };

  const api = {
    config: options.config || {},
    pluginConfig: options.pluginConfig || {},
    on() {},
    resolvePath(filePath) {
      return filePath;
    },
  };

  return {
    advanceTime(ms) {
      now += ms;
    },
    emitted,
    files,
    runtime: createMemoryBankPluginRuntime(api, platform),
  };
}
