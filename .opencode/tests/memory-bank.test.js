import { describe, expect, it } from "bun:test";

import { MemoryBankPlugin } from "../plugins/memory-bank.js";

describe("Memory Bank OpenCode plugin", () => {
  it("emits a user fragment from chat.message", async () => {
    const harness = await createHarness();

    await harness.hooks["chat.message"](
      { messageID: "msg_user", sessionID: "ses_demo" },
      {
        message: { id: "msg_user", role: "user" },
        parts: [{ text: "hi there", type: "text" }],
      },
    );

    expect(harness.emitted).toHaveLength(1);
    expect(harness.emitted[0]).toMatchObject({
      event: "message.updated",
      payload: {
        hook_event_name: "message.updated",
        message_id: "msg_user",
        role: "user",
        session_id: "ses_demo",
      },
    });
    expect(harness.emitted[0].payload.parts).toEqual([
      { reverted: false, text: "hi there", type: "text" },
    ]);
  });

  it("deduplicates duplicate user chat.message events", async () => {
    const harness = await createHarness();
    const input = { messageID: "msg_user", sessionID: "ses_demo" };
    const output = {
      message: { id: "msg_user", role: "user" },
      parts: [{ text: "hi there", type: "text" }],
    };

    await harness.hooks["chat.message"](input, output);
    await harness.hooks["chat.message"](input, output);

    expect(harness.emitted).toHaveLength(1);
  });

  it("emits the final assistant fragment on session.idle when available", async () => {
    const harness = await createHarness({
      sessionMessages: [
        [
          assistantMessage("ses_demo", "msg_assistant", "final assistant reply", {
            completed: 20,
            created: 10,
          }),
        ],
      ],
    });

    await harness.hooks.event({
      event: {
        properties: { sessionID: "ses_demo" },
        type: "session.idle",
      },
    });

    expect(harness.emitted).toHaveLength(1);
    expect(harness.emitted[0]).toMatchObject({
      event: "session.idle",
      payload: {
        assistant_text: "final assistant reply",
        hook_event_name: "session.idle",
        message_id: "msg_assistant",
        session_id: "ses_demo",
      },
    });
    expect(harness.scheduler.pendingCount()).toBe(0);
  });

  it("falls back to polling when the assistant reply is delayed", async () => {
    const harness = await createHarness({
      sessionMessages: [
        [],
        [],
        [],
        [],
        [
          assistantMessage("ses_demo", "msg_assistant", "delayed assistant reply", {
            completed: 20,
            created: 10,
          }),
        ],
      ],
    });

    await harness.hooks.event({
      event: {
        properties: { sessionID: "ses_demo" },
        type: "session.idle",
      },
    });

    expect(harness.emitted).toHaveLength(0);
    await harness.scheduler.flushNext();

    expect(harness.emitted).toHaveLength(1);
    expect(harness.emitted[0]).toMatchObject({
      event: "session.idle",
      payload: {
        assistant_text: "delayed assistant reply",
        message_id: "msg_assistant",
      },
    });
    expect(harness.scheduler.pendingCount()).toBe(0);
  });

  it("preserves tool before and after payload shapes", async () => {
    const harness = await createHarness();

    await harness.hooks["tool.execute.before"](
      { callID: "call_tool", sessionID: "ses_demo", tool: "read" },
      { args: { file: "README.md" } },
    );
    await harness.hooks["tool.execute.after"](
      { callID: "call_tool", sessionID: "ses_demo", tool: "read" },
      { metadata: { bytes: 8 }, output: "contents", title: "Read result" },
    );

    expect(harness.emitted).toEqual([
      {
        event: "tool.execute.before",
        payload: {
          hook_event_name: "tool.execute.before",
          part_id: "call_tool",
          session_id: "ses_demo",
          timestamp: harness.emitted[0].payload.timestamp,
          tool_arguments: { file: "README.md" },
          tool_name: "read",
        },
      },
      {
        event: "tool.execute.after",
        payload: {
          hook_event_name: "tool.execute.after",
          part_id: "call_tool",
          session_id: "ses_demo",
          timestamp: harness.emitted[1].payload.timestamp,
          tool_name: "read",
          tool_output: {
            metadata: { bytes: 8 },
            output: "contents",
            title: "Read result",
          },
        },
      },
    ]);
  });

  it("falls back to legacy partID fields for tool payload correlation", async () => {
    const harness = await createHarness();

    await harness.hooks["tool.execute.before"](
      { partID: "legacy_part", sessionID: "ses_demo", tool: "read" },
      { args: { file: "README.md" } },
    );

    expect(harness.emitted).toHaveLength(1);
    expect(harness.emitted[0].payload.part_id).toBe("legacy_part");
  });

  it("does not create a debug log file by default", async () => {
    const harness = await createHarness();

    await harness.hooks["chat.message"](
      { messageID: "msg_user", sessionID: "ses_demo" },
      {
        message: { id: "msg_user", role: "user" },
        parts: [{ text: "hi there", type: "text" }],
      },
    );

    expect(harness.files.size).toBe(0);
  });

  it("writes a debug log file only when MEMORY_BANK_OPENCODE_DEBUG_FILE is set", async () => {
    const harness = await createHarness({
      env: {
        MEMORY_BANK_OPENCODE_DEBUG_FILE: "/tmp/memory-bank-plugin-debug.log",
      },
      fileExists: () => false,
    });

    await harness.hooks["chat.message"](
      { messageID: "msg_user", sessionID: "ses_demo" },
      {
        message: { id: "msg_user", role: "user" },
        parts: [{ text: "hi there", type: "text" }],
      },
    );

    expect(harness.files.get("/tmp/memory-bank-plugin-debug.log")).toContain(
      "memory-bank-hook binary not found",
    );
  });

  it("skips summary user messages exposed as summary objects by the SDK", async () => {
    const harness = await createHarness();

    await harness.hooks["chat.message"](
      { messageID: "msg_summary", sessionID: "ses_demo" },
      {
        message: {
          id: "msg_summary",
          role: "user",
          summary: {
            body: "summarized user text",
            diffs: [],
            title: "Compaction",
          },
        },
        parts: [{ text: "summarized user text", type: "text" }],
      },
    );

    expect(harness.emitted).toHaveLength(0);
  });

  it("cleans up per-session state after polling times out", async () => {
    const harness = await createHarness({
      sessionMessages: Array.from({ length: 100 }, () => []),
    });

    await harness.hooks.event({
      event: {
        properties: { sessionID: "ses_demo" },
        type: "session.idle",
      },
    });
    await harness.scheduler.flushAll();

    expect(harness.appLogs.some((entry) => entry.message === "Timed out waiting for the final assistant message")).toBe(
      true,
    );
    expect(harness.scheduler.pendingCount()).toBe(0);
  });

  it("clears outstanding polling state on instance disposal", async () => {
    const harness = await createHarness({
      sessionMessages: Array.from({ length: 10 }, () => []),
    });

    await harness.hooks.event({
      event: {
        properties: { sessionID: "ses_demo" },
        type: "session.idle",
      },
    });

    expect(harness.scheduler.pendingCount()).toBe(1);

    await harness.hooks.event({
      event: {
        properties: { directory: "/workspace" },
        type: "server.instance.disposed",
      },
    });

    expect(harness.scheduler.pendingCount()).toBe(0);
  });
});

async function createHarness(options = {}) {
  const appLogs = [];
  const emitted = [];
  const files = new Map();
  const scheduler = createScheduler();
  const sessionMessages = [...(options.sessionMessages || [])];
  const messageDetails = options.messageDetails || new Map();
  const platform = {
    appendFile(filePath, text) {
      files.set(filePath, `${files.get(filePath) || ""}${text}`);
    },
    clearTimeout: scheduler.clearTimeout,
    cwd() {
      return "/workspace";
    },
    env: options.env || {},
    fileExists(filePath) {
      if (typeof options.fileExists === "function") {
        return options.fileExists(filePath);
      }
      return true;
    },
    setTimeout: scheduler.setTimeout,
    spawnSync(binary, args, spawnOptions) {
      const event = args[args.indexOf("--event") + 1];
      emitted.push({
        event,
        payload: JSON.parse(spawnOptions.input),
      });
      return options.spawnResult || { status: 0, stderr: "", stdout: "" };
    },
    wait: async () => {},
  };
  const client = {
    app: {
      async log({ body }) {
        appLogs.push(body);
      },
    },
    session: {
      async messages() {
        if (sessionMessages.length === 0) {
          return [];
        }
        if (sessionMessages.length === 1) {
          return sessionMessages[0];
        }
        return sessionMessages.shift();
      },
      message: {
        async get({ path }) {
          return messageDetails.get(path.messageID || path.messageId) || null;
        },
      },
    },
  };

  const hooks = await MemoryBankPlugin(
    {
      client,
      directory: "/workspace",
      worktree: "/workspace",
    },
    platform,
  );

  return {
    appLogs,
    emitted,
    files,
    hooks,
    scheduler,
  };
}

function createScheduler() {
  const queue = [];

  return {
    clearTimeout(handle) {
      if (handle) {
        handle.cancelled = true;
      }
    },
    async flushAll(limit = 200) {
      for (let index = 0; index < limit; index += 1) {
        const flushed = await this.flushNext();
        if (!flushed) {
          return;
        }
      }
      throw new Error("Timer queue did not drain");
    },
    async flushNext() {
      while (queue.length > 0) {
        const handle = queue.shift();
        if (!handle || handle.cancelled) {
          continue;
        }
        await handle.fn();
        return true;
      }
      return false;
    },
    pendingCount() {
      return queue.filter((handle) => !handle.cancelled).length;
    },
    setTimeout(fn, delayMs) {
      const handle = {
        cancelled: false,
        delayMs,
        fn,
      };
      queue.push(handle);
      return handle;
    },
  };
}

function assistantMessage(sessionId, messageId, text, { completed, created }) {
  return {
    info: {
      id: messageId,
      role: "assistant",
      sessionID: sessionId,
      time: {
        completed,
        created,
      },
    },
    parts: [{ text, type: "text" }],
  };
}
