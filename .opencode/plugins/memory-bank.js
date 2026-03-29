import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";

const LOG_SERVICE = "memory-bank-opencode";
const DEFAULT_AGENT = "opencode";
const DEFAULT_SERVER_URL = "http://127.0.0.1:8080";
const DEDUPE_CACHE_LIMIT = 2048;
const IMMEDIATE_ASSISTANT_RETRY_ATTEMPTS = 4;
const IMMEDIATE_ASSISTANT_RETRY_DELAY_MS = 25;
const ASSISTANT_POLL_MAX_ATTEMPTS = 60;

export const MemoryBankPlugin = async (input, platform = createPlatform()) =>
  createMemoryBankPluginRuntime(input, platform).hooks;

function createMemoryBankPluginRuntime(
  { client, directory, worktree },
  platform = createPlatform(),
) {
  const config = resolvePluginConfig({ directory, worktree }, platform);
  const logger = createLogger(client, config, platform);
  const runtime = createRuntimeState();
  const emitHook = createHookEmitter(config, logger, platform);
  const context = {
    client,
    config,
    emitHook,
    logger,
    platform,
    runtime,
  };

  void logger.debug("Initialized Memory Bank OpenCode plugin", {
    hookBinary: config.hookBinary,
    projectRoot: config.projectRoot,
    serverUrl: config.serverUrl,
  });

  return {
    hooks: createHooks(context),
    state: runtime,
    config,
  };
}

function createHooks(context) {
  return {
    "chat.message": async (input, output) => {
      await withGuard(context.logger, "chat.message", () =>
        handleChatMessage(context, input, output),
      );
    },
    "tool.execute.before": async (input, output) => {
      await withGuard(context.logger, "tool.execute.before", () =>
        handleToolExecuteBefore(context, input, output),
      );
    },
    "tool.execute.after": async (input, output) => {
      await withGuard(context.logger, "tool.execute.after", () =>
        handleToolExecuteAfter(context, input, output),
      );
    },
    event: async ({ event }) => {
      await withGuard(context.logger, `event:${event?.type || "unknown"}`, () =>
        handleEvent(context, event),
      );
    },
  };
}

async function handleChatMessage(context, input, output) {
  const message = normalizeMessageRecord(output?.message, {
    messageID: input?.messageID,
    messageId: input?.messageID,
    parts: output?.parts,
    role: "user",
    sessionID: input?.sessionID,
    sessionId: input?.sessionID,
    session_id: input?.sessionID,
  });
  const payload = buildUserMessagePayload(message);
  if (!payload) {
    return;
  }

  if (context.runtime.emittedUserMessageIds.has(payload.message_id)) {
    return;
  }

  const emitted = await context.emitHook("message.updated", payload);
  if (emitted) {
    context.runtime.emittedUserMessageIds.add(payload.message_id);
  }
}

async function handleToolExecuteBefore(context, input, output) {
  const payload = buildToolPayload("tool.execute.before", input, output);
  await context.emitHook("tool.execute.before", payload);
}

async function handleToolExecuteAfter(context, input, output) {
  const payload = buildToolPayload("tool.execute.after", input, output);
  await context.emitHook("tool.execute.after", payload);
}

async function handleEvent(context, event) {
  if (!event || typeof event.type !== "string") {
    return;
  }

  if (event.type === "session.idle") {
    await handleSessionIdleEvent(context, event.properties ?? event);
    return;
  }

  if (event.type === "server.instance.disposed") {
    disposeRuntime(context.runtime, context.platform);
  }
}

async function handleSessionIdleEvent(context, input) {
  const sessionId = getSessionId(input);
  if (!sessionId) {
    await context.logger.warn("Received session.idle without a session id");
    return;
  }

  const emitted = await emitAssistantWithImmediateRetry(context, sessionId);
  if (!emitted) {
    scheduleAssistantPoll(context, sessionId);
  }
}

async function emitAssistantWithImmediateRetry(context, sessionId) {
  for (let attempt = 0; attempt < IMMEDIATE_ASSISTANT_RETRY_ATTEMPTS; attempt += 1) {
    if (context.runtime.disposed) {
      return false;
    }

    const emitted = await tryEmitAssistantMessage(context, sessionId);
    if (emitted) {
      return true;
    }

    if (attempt + 1 < IMMEDIATE_ASSISTANT_RETRY_ATTEMPTS) {
      await context.platform.wait(IMMEDIATE_ASSISTANT_RETRY_DELAY_MS);
    }
  }

  return false;
}

async function tryEmitAssistantMessage(context, sessionId) {
  const assistantMessage = await resolveLatestAssistantMessage(context.client, sessionId);
  if (!assistantMessage) {
    return false;
  }

  const messageId =
    assistantMessage.messageId ||
    `session-idle:${sessionId}:${assistantMessage.timestamp}`;
  if (context.runtime.emittedAssistantMessageIds.has(messageId)) {
    clearActiveSession(context.runtime, context.platform, sessionId);
    return true;
  }

  const payload = {
    assistant_text: assistantMessage.text,
    hook_event_name: "session.idle",
    message_id: messageId,
    session_id: sessionId,
    timestamp: assistantMessage.timestamp,
  };

  const emitted = await context.emitHook("session.idle", payload);
  if (!emitted) {
    return false;
  }

  context.runtime.emittedAssistantMessageIds.add(messageId);
  clearActiveSession(context.runtime, context.platform, sessionId);
  return true;
}

function scheduleAssistantPoll(context, sessionId) {
  if (!sessionId || context.runtime.disposed) {
    return;
  }

  const state = ensureActiveSession(context.runtime, sessionId);
  if (state.pollHandle || state.polling) {
    return;
  }

  if (state.pollAttempts >= ASSISTANT_POLL_MAX_ATTEMPTS) {
    clearActiveSession(context.runtime, context.platform, sessionId);
    void context.logger.warn("Timed out waiting for the final assistant message", {
      attempts: state.pollAttempts,
      sessionId,
    });
    return;
  }

  const delayMs = pollDelayMs(state.pollAttempts);
  state.pollHandle = context.platform.setTimeout(() => {
    state.pollHandle = null;
    void pollForAssistantMessage(context, sessionId);
  }, delayMs);
}

async function pollForAssistantMessage(context, sessionId) {
  if (context.runtime.disposed) {
    return;
  }

  const state = ensureActiveSession(context.runtime, sessionId);
  if (state.polling) {
    return;
  }

  let shouldReschedule = false;
  state.polling = true;

  try {
    const emitted = await tryEmitAssistantMessage(context, sessionId);
    if (emitted) {
      return;
    }

    state.pollAttempts += 1;
    if (state.pollAttempts >= ASSISTANT_POLL_MAX_ATTEMPTS) {
      clearActiveSession(context.runtime, context.platform, sessionId);
      await context.logger.warn("Timed out waiting for the final assistant message", {
        attempts: state.pollAttempts,
        sessionId,
      });
      return;
    }

    shouldReschedule = true;
  } catch (error) {
    clearActiveSession(context.runtime, context.platform, sessionId);
    await context.logger.warn("Assistant polling failed", {
      error: describeError(error),
      sessionId,
    });
    return;
  } finally {
    const latest = context.runtime.activeSessions.get(sessionId);
    if (latest) {
      latest.polling = false;
    }
  }

  if (shouldReschedule) {
    scheduleAssistantPoll(context, sessionId);
  }
}

async function resolveLatestAssistantMessage(client, sessionId) {
  const messages = await listSessionMessages(client, sessionId);
  return findLatestAssistantMessage(client, messages, sessionId);
}

async function listSessionMessages(client, sessionId) {
  if (client?.session && typeof client.session.messages === "function") {
    const result = await client.session.messages({ path: { id: sessionId } });
    if (Array.isArray(result)) {
      return result;
    }
    if (Array.isArray(result?.data)) {
      return result.data;
    }
  }

  if (client?.session?.message && typeof client.session.message.list === "function") {
    const result = await client.session.message.list({ path: { id: sessionId } });
    if (Array.isArray(result)) {
      return result;
    }
    if (Array.isArray(result?.data)) {
      return result.data;
    }
  }

  throw new Error("OpenCode SDK client does not expose session message listing");
}

async function findLatestAssistantMessage(client, messages, sessionId) {
  const normalizedMessages = Array.isArray(messages)
    ? messages.map((message) =>
        normalizeMessageRecord(message, {
          sessionID: sessionId,
          sessionId,
          session_id: sessionId,
        }),
      )
    : [];

  for (let index = normalizedMessages.length - 1; index >= 0; index -= 1) {
    const candidate = normalizedMessages[index];
    if (candidate.sessionId !== sessionId) {
      continue;
    }
    if (candidate.role !== "assistant" || candidate.summary || candidate.reverted) {
      continue;
    }

    let resolved = candidate;
    if (candidate.messageId && (!candidate.completed || !extractMessageText(candidate.parts))) {
      const detailed = await fetchSessionMessageDetail(client, sessionId, candidate.messageId);
      if (detailed) {
        const hydrated = normalizeMessageRecord(detailed, {
          messageID: candidate.messageId,
          messageId: candidate.messageId,
          role: candidate.role,
          sessionID: sessionId,
          sessionId,
          session_id: sessionId,
        });
        resolved = mergeNormalizedMessages(candidate, hydrated);
      }
    }

    if (!resolved.completed) {
      continue;
    }

    const text = extractMessageText(resolved.parts);
    if (!text) {
      continue;
    }

    return {
      messageId: resolved.messageId,
      text,
      timestamp: resolved.completedAt || resolved.timestamp,
    };
  }

  return null;
}

async function fetchSessionMessageDetail(client, sessionId, messageId) {
  const getMessage =
    typeof client?.session?.message === "function"
      ? client.session.message.bind(client.session)
      : typeof client?.session?.message?.get === "function"
        ? client.session.message.get.bind(client.session.message)
        : null;

  if (!getMessage || !messageId) {
    return null;
  }

  const candidates = [
    { id: sessionId, messageID: messageId },
    { id: sessionId, messageId },
  ];

  for (const requestPath of candidates) {
    try {
      const result = await getMessage({ path: requestPath });
      if (asObject(result)) {
        return result;
      }
    } catch {
      continue;
    }
  }

  return null;
}

function buildUserMessagePayload(message) {
  if (!message.sessionId || !message.messageId) {
    return null;
  }

  if (
    message.role !== "user" ||
    message.summary ||
    message.reverted ||
    !extractMessageText(message.parts)
  ) {
    return null;
  }

  return {
    hook_event_name: "message.updated",
    message_id: message.messageId,
    parts: message.parts,
    reverted: message.reverted,
    role: message.role,
    session_id: message.sessionId,
    summary: message.summary,
    timestamp: message.timestamp,
  };
}

function buildToolPayload(eventName, input, output) {
  const sessionId = getSessionId(input) || getSessionId(output);
  const toolName = firstString(
    input?.tool,
    input?.toolName,
    input?.tool_name,
    output?.tool,
    output?.toolName,
    output?.tool_name,
  );
  if (!sessionId || !toolName) {
    return null;
  }

  const payload = {
    hook_event_name: eventName,
    part_id: firstString(
      input?.partID,
      input?.partId,
      input?.part_id,
      output?.partID,
      output?.partId,
      output?.part_id,
    ),
    session_id: sessionId,
    timestamp: toRfc3339(
      input?.timestamp ??
        output?.timestamp ??
        input?.createdAt ??
        output?.createdAt ??
        Date.now(),
    ),
    tool_name: toolName,
  };

  if (eventName === "tool.execute.before") {
    payload.tool_arguments = sanitizeForJson(
      output?.args ?? input?.args ?? input?.arguments ?? input?.input ?? {},
    );
    return payload;
  }

  payload.tool_output = sanitizeForJson(
    output?.result ?? output?.output ?? output?.response ?? output?.data ?? output?.value ?? output,
  );
  return payload;
}

function normalizeMessageRecord(record, fallbackInput = {}) {
  const candidate = asObject(record) ? record : {};
  const info = asObject(candidate.info) ? candidate.info : candidate;

  return {
    completed: hasTruthyValue(
      info.time?.completed,
      candidate.time?.completed,
      fallbackInput?.time?.completed,
      fallbackInput?.completedAt,
      fallbackInput?.completed,
    ),
    completedAt: toOptionalRfc3339(
      info.time?.completed ??
        candidate.time?.completed ??
        fallbackInput?.time?.completed ??
        fallbackInput?.completedAt,
    ),
    messageId: firstString(
      info.id,
      candidate.id,
      candidate.messageID,
      candidate.messageId,
      candidate.message_id,
      fallbackInput?.messageID,
      fallbackInput?.messageId,
      fallbackInput?.message_id,
    ),
    parts: normalizeMessageParts(resolveMessageParts(candidate, info, fallbackInput)),
    reverted: hasTruthyValue(
      info.reverted,
      info.isReverted,
      info.revertedAt,
      candidate.reverted,
      candidate.isReverted,
      candidate.revertedAt,
      fallbackInput?.reverted,
    ),
    role: firstString(info.role, candidate.role, fallbackInput?.role),
    sessionId: getSessionId(candidate) || getSessionId(info) || getSessionId(fallbackInput),
    summary: strictFlagValue(
      info.summary,
      info.isSummary,
      candidate.summary,
      candidate.isSummary,
      fallbackInput?.summary,
    ),
    timestamp: toRfc3339(
      info.updatedAt ??
        info.time?.completed ??
        info.time?.updated ??
        info.createdAt ??
        info.time?.created ??
        info.timestamp ??
        candidate.updatedAt ??
        candidate.createdAt ??
        candidate.timestamp ??
        fallbackInput?.timestamp ??
        Date.now(),
    ),
  };
}

function resolveMessageParts(candidate, info, fallbackInput) {
  const sources = [
    candidate?.parts,
    info?.parts,
    candidate?.message?.parts,
    info?.message?.parts,
    candidate?.data?.parts,
    info?.data?.parts,
    fallbackInput?.parts,
  ];

  for (const source of sources) {
    if (Array.isArray(source)) {
      return source;
    }
  }

  return [];
}

function mergeNormalizedMessages(base, overlay) {
  return {
    completed: overlay.completed || base.completed,
    completedAt: overlay.completedAt || base.completedAt,
    messageId: overlay.messageId || base.messageId,
    parts: chooseMessageParts(base.parts, overlay.parts),
    reverted: overlay.reverted || base.reverted,
    role: overlay.role || base.role,
    sessionId: overlay.sessionId || base.sessionId,
    summary: overlay.summary || base.summary,
    timestamp: overlay.timestamp || base.timestamp,
  };
}

function chooseMessageParts(baseParts, overlayParts) {
  if (extractMessageText(overlayParts)) {
    return overlayParts;
  }
  if (extractMessageText(baseParts)) {
    return baseParts;
  }
  if (Array.isArray(overlayParts) && overlayParts.length > 0) {
    return overlayParts;
  }
  return baseParts;
}

function normalizeMessageParts(parts) {
  if (!Array.isArray(parts)) {
    return [];
  }

  return parts.map((part) => ({
    reverted: hasTruthyValue(part?.reverted, part?.isReverted, part?.revertedAt),
    text: extractPartText(part),
    type: firstString(part?.type, part?.kind),
  }));
}

function extractMessageText(parts) {
  if (!Array.isArray(parts)) {
    return "";
  }

  return parts
    .filter((part) => !part?.reverted)
    .filter((part) => {
      const type = part?.type || "text";
      return type === "text" || type === "input_text";
    })
    .map((part) => (typeof part?.text === "string" ? part.text : ""))
    .join("")
    .trim();
}

function extractPartText(part) {
  if (typeof part === "string") {
    return part;
  }

  if (!asObject(part)) {
    return undefined;
  }

  if (typeof part.text === "string") {
    return part.text;
  }

  if (typeof part.content === "string") {
    return part.content;
  }

  if (Array.isArray(part.content)) {
    return part.content.map(extractPartText).filter(Boolean).join("");
  }

  if (asObject(part.content) && typeof part.content.text === "string") {
    return part.content.text;
  }

  if (asObject(part.delta) && typeof part.delta.text === "string") {
    return part.delta.text;
  }

  if (typeof part.value === "string") {
    return part.value;
  }

  return undefined;
}

function createHookEmitter(config, logger, platform) {
  let warnedMissingBinary = false;

  return async function emitHook(eventName, payload) {
    if (!payload) {
      return false;
    }

    if (!platform.fileExists(config.hookBinary)) {
      if (!warnedMissingBinary) {
        warnedMissingBinary = true;
        await logger.warn("memory-bank-hook binary not found; skipping OpenCode capture", {
          eventName,
          hookBinary: config.hookBinary,
        });
      }
      return false;
    }

    const child = platform.spawnSync(
      config.hookBinary,
      [
        "--agent",
        DEFAULT_AGENT,
        "--event",
        eventName,
        "--server-url",
        config.serverUrl,
      ],
      {
        encoding: "utf8",
        input: `${JSON.stringify(payload)}\n`,
      },
    );

    if (child?.error) {
      await logger.warn("Failed to invoke memory-bank-hook", {
        error: describeError(child.error),
        eventName,
        hookBinary: config.hookBinary,
      });
      return false;
    }

    if (child?.status !== 0) {
      await logger.warn("memory-bank-hook exited with a non-zero status", {
        eventName,
        hookBinary: config.hookBinary,
        status: child?.status,
        stderr: normalizeLogText(child?.stderr),
        stdout: normalizeLogText(child?.stdout),
      });
      return false;
    }

    return true;
  };
}

function createLogger(client, config, platform) {
  return {
    debug(message, extra = undefined) {
      return writeLog(client, config, platform, "debug", message, extra, { force: false });
    },
    info(message, extra = undefined) {
      return writeLog(client, config, platform, "info", message, extra, { force: false });
    },
    warn(message, extra = undefined) {
      return writeLog(client, config, platform, "warn", message, extra, { force: true });
    },
    error(message, extra = undefined) {
      return writeLog(client, config, platform, "error", message, extra, { force: true });
    },
  };
}

async function writeLog(client, config, platform, level, message, extra, options) {
  if (!options.force && !config.debugEnabled) {
    return;
  }

  const record = {
    extra,
    level,
    message,
    service: LOG_SERVICE,
  };
  writeDebugFile(config.debugFilePath, platform, record);

  if (client?.app && typeof client.app.log === "function") {
    try {
      await client.app.log({ body: record });
      return;
    } catch (error) {
      if (options.force || config.debugEnabled) {
        writeConsoleRecord("warn", "Failed to write structured Memory Bank log", {
          error: describeError(error),
          original: record,
        });
      }
    }
  }

  if (options.force || config.debugEnabled) {
    writeConsoleRecord(level, message, extra);
  }
}

function writeDebugFile(debugFilePath, platform, record) {
  if (!debugFilePath) {
    return;
  }

  try {
    platform.appendFile(
      debugFilePath,
      `${JSON.stringify({ ts: new Date().toISOString(), ...record })}\n`,
    );
  } catch {
    // Best effort only.
  }
}

function writeConsoleRecord(level, message, extra) {
  if (extra !== undefined) {
    console.warn(`[${LOG_SERVICE}] ${level}: ${message}`, extra);
    return;
  }

  console.warn(`[${LOG_SERVICE}] ${level}: ${message}`);
}

function createRuntimeState() {
  return {
    activeSessions: new Map(),
    disposed: false,
    emittedAssistantMessageIds: createBoundedIdCache(DEDUPE_CACHE_LIMIT),
    emittedUserMessageIds: createBoundedIdCache(DEDUPE_CACHE_LIMIT),
  };
}

function ensureActiveSession(runtime, sessionId) {
  let state = runtime.activeSessions.get(sessionId);
  if (!state) {
    state = {
      pollAttempts: 0,
      pollHandle: null,
      polling: false,
    };
    runtime.activeSessions.set(sessionId, state);
  }
  return state;
}

function clearActiveSession(runtime, platform, sessionId) {
  const state = runtime.activeSessions.get(sessionId);
  if (!state) {
    return;
  }

  if (state.pollHandle) {
    platform.clearTimeout(state.pollHandle);
  }
  runtime.activeSessions.delete(sessionId);
}

function disposeRuntime(runtime, platform) {
  runtime.disposed = true;

  for (const [sessionId] of runtime.activeSessions) {
    clearActiveSession(runtime, platform, sessionId);
  }
}

function createBoundedIdCache(limit) {
  const ids = new Set();
  const queue = [];

  return {
    add(id) {
      if (typeof id !== "string" || !id || ids.has(id)) {
        return;
      }

      ids.add(id);
      queue.push(id);
      while (queue.length > limit) {
        const oldest = queue.shift();
        if (oldest) {
          ids.delete(oldest);
        }
      }
    },
    has(id) {
      return typeof id === "string" && ids.has(id);
    },
  };
}

function resolvePluginConfig({ directory, worktree }, platform) {
  const projectRoot = worktree || directory || platform.cwd();
  const env = platform.env || {};

  return {
    debugEnabled: envFlag(
      env.MEMORY_BANK_OPENCODE_DEBUG,
      env.MEMORY_BANK_DEBUG_OPENCODE,
    ),
    debugFilePath: resolveOptionalDebugFilePath(
      projectRoot,
      env.MEMORY_BANK_OPENCODE_DEBUG_FILE,
    ),
    hookBinary: resolveHookBinary(projectRoot, env, platform),
    projectRoot,
    serverUrl: firstString(env.MEMORY_BANK_SERVER_URL) || DEFAULT_SERVER_URL,
  };
}

function resolveOptionalDebugFilePath(projectRoot, configuredPath) {
  const filePath = firstString(configuredPath);
  if (!filePath) {
    return null;
  }

  return path.isAbsolute(filePath) ? filePath : path.resolve(projectRoot, filePath);
}

function resolveHookBinary(projectRoot, env, platform) {
  const configured = firstString(
    env.MEMORY_BANK_HOOK_BIN,
    env.MEMORY_BANK_OPENCODE_HOOK_BIN,
  );
  if (configured) {
    return configured;
  }

  const binaryName =
    process.platform === "win32" ? "memory-bank-hook.exe" : "memory-bank-hook";
  const candidates = [
    path.join(projectRoot, "target", "debug", binaryName),
    path.join(projectRoot, "target", "release", binaryName),
  ];

  return candidates.find((candidate) => platform.fileExists(candidate)) || candidates[0];
}

function createPlatform(overrides = {}) {
  return {
    appendFile(filePath, text) {
      fs.mkdirSync(path.dirname(filePath), { recursive: true });
      fs.appendFileSync(filePath, text, "utf8");
    },
    clearTimeout(handle) {
      clearTimeout(handle);
    },
    cwd() {
      return process.cwd();
    },
    env: process.env,
    fileExists(filePath) {
      return fs.existsSync(filePath);
    },
    setTimeout(fn, delayMs) {
      return setTimeout(fn, delayMs);
    },
    spawnSync,
    wait(delayMs) {
      return new Promise((resolve) => setTimeout(resolve, delayMs));
    },
    ...overrides,
  };
}

async function withGuard(logger, hookName, fn) {
  try {
    await fn();
  } catch (error) {
    await logger.warn("Memory Bank OpenCode hook failed", {
      error: describeError(error),
      hookName,
    });
  }
}

function getSessionId(value) {
  return firstString(
    value?.sessionID,
    value?.sessionId,
    value?.session_id,
    value?.properties?.sessionID,
    value?.properties?.sessionId,
    value?.properties?.session_id,
    sessionScopedId(value?.id),
    value?.session?.id,
    value?.info?.sessionID,
    value?.info?.sessionId,
    value?.info?.session_id,
    sessionScopedId(value?.info?.id),
  );
}

function sessionScopedId(value) {
  return typeof value === "string" && value.startsWith("ses_") ? value : undefined;
}

function pollDelayMs(attempt) {
  if (attempt < 8) {
    return 250;
  }
  if (attempt < 24) {
    return 500;
  }
  return 1000;
}

function sanitizeForJson(value, seen = new WeakSet()) {
  if (value === null) {
    return null;
  }

  if (typeof value === "string" || typeof value === "boolean") {
    return value;
  }

  if (typeof value === "number") {
    return Number.isFinite(value) ? value : String(value);
  }

  if (typeof value === "bigint") {
    return value.toString();
  }

  if (value instanceof Date) {
    return value.toISOString();
  }

  if (Array.isArray(value)) {
    return value.map((entry) => sanitizeForJson(entry, seen));
  }

  if (typeof value !== "object") {
    return value == null ? null : String(value);
  }

  if (seen.has(value)) {
    return "[circular]";
  }

  seen.add(value);
  const result = {};
  for (const [key, entry] of Object.entries(value)) {
    if (entry === undefined || typeof entry === "function" || typeof entry === "symbol") {
      continue;
    }
    result[key] = sanitizeForJson(entry, seen);
  }
  seen.delete(value);
  return result;
}

function normalizeLogText(value) {
  if (typeof value !== "string") {
    return undefined;
  }

  const trimmed = value.trim();
  return trimmed || undefined;
}

function firstString(...values) {
  for (const value of values) {
    if (typeof value === "string" && value.trim()) {
      return value;
    }
  }
  return undefined;
}

function hasTruthyValue(...values) {
  for (const value of values) {
    if (typeof value === "boolean") {
      return value;
    }
    if (typeof value === "string") {
      if (value === "true" || value === "1") {
        return true;
      }
      if (value === "false" || value === "0") {
        return false;
      }
    }
    if (typeof value === "number") {
      if (value === 1) {
        return true;
      }
      if (value === 0) {
        return false;
      }
    }
    if (value) {
      return true;
    }
  }
  return false;
}

function strictFlagValue(...values) {
  for (const value of values) {
    if (typeof value === "boolean") {
      return value;
    }
    if (typeof value === "string") {
      if (value === "true" || value === "1") {
        return true;
      }
      if (value === "false" || value === "0") {
        return false;
      }
    }
    if (typeof value === "number") {
      if (value === 1) {
        return true;
      }
      if (value === 0) {
        return false;
      }
    }
  }

  return false;
}

function toRfc3339(value) {
  if (value instanceof Date) {
    return value.toISOString();
  }

  if (typeof value === "number" && Number.isFinite(value)) {
    return new Date(value).toISOString();
  }

  if (typeof value === "string" && value.trim()) {
    const parsed = Date.parse(value);
    if (!Number.isNaN(parsed)) {
      return new Date(parsed).toISOString();
    }
  }

  return new Date().toISOString();
}

function toOptionalRfc3339(value) {
  if (value === undefined || value === null || value === "") {
    return undefined;
  }

  if (value instanceof Date) {
    return value.toISOString();
  }

  if (typeof value === "number" && Number.isFinite(value)) {
    return new Date(value).toISOString();
  }

  if (typeof value === "string" && value.trim()) {
    const parsed = Date.parse(value);
    if (!Number.isNaN(parsed)) {
      return new Date(parsed).toISOString();
    }
  }

  return undefined;
}

function envFlag(...values) {
  for (const value of values) {
    if (typeof value === "string") {
      const normalized = value.trim().toLowerCase();
      if (!normalized) {
        continue;
      }
      if (["1", "true", "yes", "on"].includes(normalized)) {
        return true;
      }
      if (["0", "false", "no", "off"].includes(normalized)) {
        return false;
      }
    }

    if (typeof value === "boolean") {
      return value;
    }
  }

  return false;
}

function asObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function describeError(error) {
  return error instanceof Error ? error.message : String(error);
}
