const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const DEFAULT_AGENT = "openclaw";
const APP_ROOT_DIR_NAME = ".memory_bank";
const APP_SETTINGS_FILE_NAME = "settings.toml";
const DEFAULT_SERVER_URL = "http://127.0.0.1:3737";
const DEDUPE_CACHE_LIMIT = 2048;
const DUPLICATE_WINDOW_MS = 5_000;
const MEMORY_BANK_PREFERENCE_SYSTEM_CONTEXT =
  "Memory Bank is the primary long-term memory system in this OpenClaw environment. " +
  "For prior-session facts, learned preferences, earlier decisions, project conventions, " +
  "constraints, and provenance questions like 'how do you know?' or 'what do you remember?', " +
  "call the retrieve_memory tool before answering. Prefer retrieve_memory over local workspace " +
  "profile files such as USER.md, MEMORY.md, BOOTSTRAP.md, or similar workspace notes when " +
  "reasoning about durable memory across sessions. Treat those files as auxiliary local notes, " +
  "not the canonical long-term memory source, unless the user is explicitly asking about those files.";

const plugin = {
  id: "memory-bank",
  name: "Memory Bank",
  register(api) {
    const runtime = createMemoryBankPluginRuntime(api, createPlatform());

    registerHook(api, "before_model_resolve", async (event, hookContext) => {
      await runtime.handleBeforeModelResolve(event, hookContext);
    });
    registerHook(api, "before_prompt_build", async (event, hookContext) => {
      return runtime.handleBeforePromptBuild(event, hookContext);
    });
    registerHook(api, "before_tool_call", async (event, hookContext) => {
      await runtime.handleBeforeToolCall(event, hookContext);
    });
    registerHook(api, "after_tool_call", async (event, hookContext) => {
      await runtime.handleAfterToolCall(event, hookContext);
    });
    registerHook(api, "agent_end", async (event, hookContext) => {
      await runtime.handleAgentEnd(event, hookContext);
    });
  },
};

function createMemoryBankPluginRuntime(api, platform = createPlatform()) {
  const config = resolvePluginConfig(api, platform);
  const logger = createLogger(config, platform);
  const state = createRuntimeState();
  const emitHook = createHookEmitter(config, logger, platform);
  const now = createNow(platform);

  void logger.debug("Initialized Memory Bank OpenClaw plugin", {
    debug: config.debug,
    debugFile: config.debugFile,
    hookBinary: config.hookBinary,
    serverUrl: config.serverUrl,
  });

  return {
    config,
    logger,
    state,
    hooks: {
      agent_end: (event, hookContext) =>
        handleAgentEnd({ emitHook, logger, now, state }, event, hookContext),
      after_tool_call: (event, hookContext) =>
        handleToolLifecycle(
          { emitHook, logger, now, state },
          "after_tool_call",
          event,
          hookContext,
        ),
      before_model_resolve: (event, hookContext) =>
        handleBeforeModelResolve(
          { emitHook, logger, now, state },
          event,
          hookContext,
        ),
      before_prompt_build: (event, hookContext) =>
        handleBeforePromptBuild(
          { emitHook, logger, now, state },
          event,
          hookContext,
        ),
      before_tool_call: (event, hookContext) =>
        handleToolLifecycle(
          { emitHook, logger, now, state },
          "before_tool_call",
          event,
          hookContext,
        ),
    },
    async handleBeforeModelResolve(event, hookContext) {
      await handleBeforeModelResolve(
        { emitHook, logger, now, state },
        event,
        hookContext,
      );
    },
    async handleBeforePromptBuild(event, hookContext) {
      return handleBeforePromptBuild(
        { emitHook, logger, now, state },
        event,
        hookContext,
      );
    },
    async handleBeforeToolCall(event, hookContext) {
      await handleToolLifecycle(
        { emitHook, logger, now, state },
        "before_tool_call",
        event,
        hookContext,
      );
    },
    async handleAfterToolCall(event, hookContext) {
      await handleToolLifecycle(
        { emitHook, logger, now, state },
        "after_tool_call",
        event,
        hookContext,
      );
    },
    async handleAgentEnd(event, hookContext) {
      await handleAgentEnd({ emitHook, logger, now, state }, event, hookContext);
    },
  };
}

function registerHook(api, eventName, handler) {
  if (typeof api?.on === "function") {
    api.on(eventName, handler);
    return;
  }
  if (typeof api?.registerHook === "function") {
    api.registerHook(eventName, handler);
    return;
  }
  throw new Error("OpenClaw plugin API does not expose on(...) or registerHook(...)");
}

async function handleBeforeModelResolve(context, event, hookContext) {
  await capturePromptEvent(context, "before_model_resolve", event, hookContext);
}

async function handleBeforePromptBuild(context, event, hookContext) {
  await capturePromptEvent(context, "before_prompt_build", event, hookContext);
  return {
    prependSystemContext: MEMORY_BANK_PREFERENCE_SYSTEM_CONTEXT,
  };
}

async function capturePromptEvent(context, eventName, event, hookContext) {
  const sessionId = resolveSessionId(event, hookContext);
  const prompt = resolvePrompt(event);
  if (!sessionId || !prompt) {
    await context.logger.debug(`Skipping ${eventName} without session or prompt`, {
      eventKeys: Object.keys(event || {}),
      hookContextKeys: Object.keys(hookContext || {}),
    });
    return false;
  }

  const payload = {
    hook_event_name: eventName,
    prompt,
    run_id: resolveRunId(event, hookContext),
    session_id: sessionId,
    timestamp: resolveTimestamp(event),
  };
  const key = buildPromptCaptureKey(sessionId, payload.run_id, prompt);
  return emitOnce(context, eventName, key, payload);
}

async function handleToolLifecycle(context, eventName, event, hookContext) {
  const sessionId = resolveSessionId(event, hookContext);
  const toolName = resolveToolName(event);
  if (!sessionId || !toolName) {
    await context.logger.debug(`Skipping ${eventName} without session or tool name`, {
      eventKeys: Object.keys(event || {}),
      hookContextKeys: Object.keys(hookContext || {}),
    });
    return;
  }

  const toolUseId = resolveToolUseId(event);
  const payload = {
    hook_event_name: eventName,
    run_id: resolveRunId(event, hookContext),
    session_id: sessionId,
    timestamp: resolveTimestamp(event),
    tool_name: toolName,
    tool_use_id: toolUseId,
  };

  if (eventName === "before_tool_call") {
    payload.tool_arguments = sanitizeForJson(resolveToolArguments(event));
  } else {
    payload.tool_output = sanitizeForJson(resolveToolOutput(event));
  }

  const runIdPart = payload.run_id || "no-run";
  const fallbackSource =
    toolUseId ||
    `${toolName}:${stableJson(
      eventName === "before_tool_call" ? payload.tool_arguments : payload.tool_output,
    )}`;
  const key = `${eventName}:${sessionId}:${runIdPart}:${fallbackSource}`;
  await emitOnce(context, eventName, key, payload);
}

async function handleAgentEnd(context, event, hookContext) {
  const sessionId = resolveSessionId(event, hookContext);
  const assistantText = resolveAssistantText(event);
  if (!sessionId || !assistantText) {
    await context.logger.debug("Skipping agent_end without session or assistant text", {
      eventKeys: Object.keys(event || {}),
      hookContextKeys: Object.keys(hookContext || {}),
    });
    return;
  }

  const assistantMessageId = resolveAssistantMessageId(event);
  const payload = {
    assistant_message_id: assistantMessageId,
    assistant_text: assistantText,
    hook_event_name: "agent_end",
    run_id: resolveRunId(event, hookContext),
    session_id: sessionId,
    timestamp: resolveTimestamp(event),
  };
  const key = `agent_end:${sessionId}:${assistantMessageId || hashString(assistantText)}`;
  await emitOnce(context, "agent_end", key, payload);
}

async function emitOnce(context, eventName, key, payload) {
  const now = context.now();
  const previous = context.state.emitted.get(key);
  if (previous !== undefined && now - previous <= DUPLICATE_WINDOW_MS) {
    await context.logger.debug(`Skipping recent duplicate ${eventName}`, { key });
    return false;
  }

  const emitted = await context.emitHook(eventName, payload);
  if (emitted) {
    rememberKey(context.state.emitted, key, now);
  }
  return emitted;
}

function createRuntimeState() {
  return {
    emitted: new Map(),
  };
}

function rememberKey(cache, key, now) {
  cache.delete(key);
  cache.set(key, now);
  if (cache.size <= DEDUPE_CACHE_LIMIT) {
    return;
  }

  const oldest = cache.keys().next().value;
  if (oldest !== undefined) {
    cache.delete(oldest);
  }
}

function resolvePluginConfig(api, platform) {
  const pluginConfig = getPluginConfig(api);
  const cwd = platform.cwd();
  const env = platform.env || {};
  const resolvePath = createPathResolver(api, platform);
  const appSettings = loadAppSettings(platform);
  const appConfigRoot = resolveAppConfigRoot(platform);

  return {
    debug: Boolean(pluginConfig.debug) || env.MEMORY_BANK_OPENCLAW_DEBUG === "1",
    debugFile: resolveOptionalPath(
      resolvePath,
      firstNonEmpty(
        pluginConfig.debugFile,
        env.MEMORY_BANK_OPENCLAW_DEBUG_FILE,
      ),
    ),
    hookBinary: firstExistingPath(
      platform,
      resolveOptionalPath(
        resolvePath,
        firstNonEmpty(pluginConfig.hookBinary, env.MEMORY_BANK_HOOK_BIN),
      ),
      appConfigRoot && path.join(appConfigRoot, "bin", "memory-bank-hook"),
      path.join(cwd, "target/debug/memory-bank-hook"),
      path.join(cwd, "target/release/memory-bank-hook"),
    ),
    serverUrl: firstNonEmpty(
      pluginConfig.serverUrl,
      env.MEMORY_BANK_SERVER_URL,
      resolveServerUrlFromSettings(appSettings),
      DEFAULT_SERVER_URL,
    ),
  };
}

function resolveAppConfigRoot(platform) {
  const homeDir = firstNonEmpty(platform.env?.HOME, platform.homeDir?.());
  if (!homeDir) {
    return null;
  }
  return path.join(homeDir, APP_ROOT_DIR_NAME);
}

function loadAppSettings(platform) {
  const appRoot = resolveAppConfigRoot(platform);
  if (!appRoot) {
    return null;
  }

  const settingsPath = path.join(appRoot, APP_SETTINGS_FILE_NAME);
  if (!platform.fileExists(settingsPath)) {
    return null;
  }

  try {
    return parseAppSettingsToml(platform.readFile(settingsPath));
  } catch {
    return null;
  }
}

function parseAppSettingsToml(contents) {
  let currentSection = null;
  let port = null;

  for (const rawLine of String(contents).replace(/^\uFEFF/, "").split(/\r?\n/u)) {
    const line = stripTomlLineComment(rawLine).trim();
    if (!line) {
      continue;
    }

    const sectionMatch = line.match(/^\[(.+)\]$/u);
    if (sectionMatch) {
      currentSection = sectionMatch[1].trim();
      continue;
    }

    if (currentSection !== "service") {
      continue;
    }

    const portMatch = line.match(/^port\s*=\s*(\d+)\s*$/u);
    if (!portMatch) {
      continue;
    }

    const parsed = Number.parseInt(portMatch[1], 10);
    if (Number.isInteger(parsed) && parsed >= 1 && parsed <= 65535) {
      port = parsed;
    }
  }

  return port === null ? null : { service: { port } };
}

function stripTomlLineComment(line) {
  let inDoubleQuote = false;
  let escaped = false;
  let result = "";

  for (const char of line) {
    if (inDoubleQuote) {
      result += char;
      if (escaped) {
        escaped = false;
      } else if (char === "\\") {
        escaped = true;
      } else if (char === '"') {
        inDoubleQuote = false;
      }
      continue;
    }

    if (char === '"') {
      inDoubleQuote = true;
      result += char;
      continue;
    }

    if (char === "#") {
      break;
    }

    result += char;
  }

  return result;
}

function resolveServerUrlFromSettings(appSettings) {
  const port = appSettings?.service?.port;
  if (typeof port !== "number" || !Number.isFinite(port)) {
    return null;
  }

  return `http://127.0.0.1:${port}`;
}

function getPluginConfig(api) {
  if (api?.pluginConfig && typeof api.pluginConfig === "object") {
    return api.pluginConfig;
  }
  if (typeof api?.getConfig === "function") {
    return api.getConfig() || {};
  }
  return api?.config || api?.state?.config || {};
}

function createPathResolver(api, platform) {
  if (typeof api?.resolvePath === "function") {
    return (value) => api.resolvePath(value);
  }
  return (value) => {
    if (!value) {
      return value;
    }
    return path.isAbsolute(value) ? value : path.resolve(platform.cwd(), value);
  };
}

function resolveOptionalPath(resolvePath, value) {
  if (!value) {
    return undefined;
  }
  return resolvePath(value);
}

function createHookEmitter(config, logger, platform) {
  return async function emitHook(eventName, payload) {
    if (!config.hookBinary) {
      await logger.warn("memory-bank-hook binary not found", {
        eventName,
      });
      return false;
    }

    let serializedPayload;
    try {
      serializedPayload = JSON.stringify(sanitizeForJson(payload));
    } catch (error) {
      await logger.warn("failed to serialize Memory Bank payload", {
        error: describeError(error),
        eventName,
        sessionId: payload?.session_id,
      });
      return false;
    }

    const result = platform.spawnSync(
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
        input: serializedPayload,
      },
    );

    if (result?.status === 0) {
      await logger.debug(`Delivered ${eventName} to Memory Bank`, {
        eventName,
        sessionId: payload.session_id,
      });
      return true;
    }

    await logger.warn("memory-bank-hook invocation failed", {
      eventName,
      error: result?.error ? String(result.error) : undefined,
      stderr: result?.stderr || undefined,
      status: result?.status,
    });
    return false;
  };
}

function createLogger(config, platform) {
  const shouldWriteFile = Boolean(config.debugFile);
  const shouldDebug = Boolean(config.debug) || shouldWriteFile;
  let warnedDebugFileFailure = false;

  async function write(level, message, meta) {
    const line = JSON.stringify(sanitizeForJson({
      level,
      message,
      meta: meta || undefined,
      timestamp: new Date().toISOString(),
    }));

    if (shouldWriteFile) {
      try {
        platform.appendFile(config.debugFile, `${line}\n`);
      } catch (error) {
        if (!warnedDebugFileFailure) {
          warnedDebugFileFailure = true;
          console.warn("[memory-bank-openclaw] Failed to write debug log file", {
            debugFile: config.debugFile,
            error: describeError(error),
          });
        }
      }
    }

    if (!shouldDebug && level === "debug") {
      return;
    }

    const sink = level === "warn" ? console.warn : console.error;
    sink(`[memory-bank-openclaw] ${message}`, meta || "");
  }

  return {
    debug(message, meta) {
      return write("debug", message, meta);
    },
    warn(message, meta) {
      return write("warn", message, meta);
    },
  };
}

function resolveSessionId(event, hookContext) {
  return firstNonEmpty(
    event?.session_id,
    event?.sessionId,
    event?.sessionID,
    hookContext?.session_id,
    hookContext?.sessionId,
    hookContext?.sessionID,
    event?.session_key,
    event?.sessionKey,
    hookContext?.sessionKey,
    event?.conversation_id,
    event?.conversationId,
    event?.session?.id,
    event?.context?.sessionId,
    event?.run?.sessionId,
    event?.run?.session?.id,
  );
}

function resolveRunId(event, hookContext) {
  return firstNonEmpty(
    event?.run_id,
    event?.runId,
    event?.run?.id,
    event?.context?.runId,
    hookContext?.run_id,
    hookContext?.runId,
  );
}

function resolveTimestamp(event) {
  return (
    firstNonEmpty(event?.timestamp, event?.completedAt, event?.createdAt) ||
    new Date().toISOString()
  );
}

function resolvePrompt(event) {
  return firstNonEmpty(
    event?.prompt,
    event?.userPrompt,
    event?.input?.prompt,
    event?.input?.text,
    event?.request?.prompt,
    event?.request?.text,
    lastMessageText(event?.messages, "user"),
    lastMessageText(event?.inputMessages, "user"),
    lastMessageText(event?.conversation, "user"),
    lastMessageText(event?.transcript, "user"),
  );
}

function resolveToolName(event) {
  return firstNonEmpty(
    event?.tool_name,
    event?.toolName,
    event?.tool?.name,
    event?.name,
  );
}

function resolveToolUseId(event) {
  return firstNonEmpty(
    event?.tool_use_id,
    event?.toolUseId,
    event?.tool_call_id,
    event?.toolCallId,
    event?.invocation_id,
    event?.invocationId,
    event?.tool?.callId,
    event?.tool?.id,
  );
}

function resolveToolArguments(event) {
  return (
    event?.tool_arguments ??
    event?.toolArguments ??
    event?.params ??
    event?.arguments ??
    event?.args ??
    event?.input ??
    {}
  );
}

function resolveToolOutput(event) {
  const result =
    event?.tool_output ??
    event?.toolOutput ??
    event?.result ??
    event?.output;
  const error = firstNonEmpty(
    event?.error,
    event?.errorMessage,
    event?.toolError,
  );
  const durationMs =
    typeof event?.durationMs === "number" ? event.durationMs : undefined;

  if (!error && durationMs === undefined) {
    return result ?? null;
  }

  const output = {};
  if (result !== undefined) {
    output.result = result;
  }
  if (error) {
    output.error = error;
  }
  if (durationMs !== undefined) {
    output.durationMs = durationMs;
  }
  return Object.keys(output).length > 0 ? output : null;
}

function resolveAssistantText(event) {
  return firstNonEmpty(
    event?.assistant_text,
    event?.assistantText,
    event?.finalText,
    event?.outputText,
    event?.responseText,
    event?.result?.text,
    messageText(event?.finalMessage),
    messageText(event?.assistantMessage),
    lastMessageText(event?.messages, "assistant"),
    lastMessageText(event?.finalMessages, "assistant"),
    lastMessageText(event?.transcript, "assistant"),
  );
}

function resolveAssistantMessageId(event) {
  return firstNonEmpty(
    event?.assistant_message_id,
    event?.assistantMessageId,
    event?.finalMessage?.id,
    event?.assistantMessage?.id,
    lastMessageId(event?.messages, "assistant"),
    lastMessageId(event?.finalMessages, "assistant"),
  );
}

function messageText(message) {
  if (!message || typeof message !== "object") {
    return null;
  }
  return firstNonEmpty(
    message.text,
    message.content,
    contentBlocksText(message.content),
    Array.isArray(message.parts) ? message.parts.map((part) => part?.text || "").join("") : null,
  );
}

function contentBlocksText(content) {
  if (!Array.isArray(content)) {
    return null;
  }

  const text = content
    .map((block) => {
      if (!block || typeof block !== "object") {
        return "";
      }
      if (typeof block.text === "string") {
        return block.text;
      }
      if (typeof block.content === "string") {
        return block.content;
      }
      return "";
    })
    .join("")
    .trim();

  return text || null;
}

function lastMessageText(messages, role) {
  if (!Array.isArray(messages)) {
    return null;
  }

  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (!message || typeof message !== "object") {
      continue;
    }
    if (role && normalizeRole(message.role) !== role) {
      continue;
    }

    const text = messageText(message);
    if (text) {
      return text;
    }
  }

  return null;
}

function lastMessageId(messages, role) {
  if (!Array.isArray(messages)) {
    return null;
  }

  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (!message || typeof message !== "object") {
      continue;
    }
    if (role && normalizeRole(message.role) !== role) {
      continue;
    }

    const id = firstNonEmpty(message.id, message.messageId, message.message_id);
    if (id) {
      return id;
    }
  }

  return null;
}

function normalizeRole(value) {
  if (typeof value !== "string") {
    return null;
  }
  return value.trim().toLowerCase();
}

function hashString(value) {
  return crypto.createHash("sha1").update(value).digest("hex");
}

function buildPromptCaptureKey(sessionId, runId, prompt) {
  return `prompt_capture:${sessionId}:${runId || hashString(prompt)}`;
}

function stableJson(value) {
  try {
    return JSON.stringify(value ?? null);
  } catch {
    return "[unserializable]";
  }
}

function firstExistingPath(platform, ...values) {
  for (const value of values) {
    if (!value || typeof value !== "string") {
      continue;
    }
    if (platform.fileExists(value)) {
      return value;
    }
  }
  return null;
}

function firstNonEmpty(...values) {
  for (const value of values) {
    if (typeof value === "string" && value.trim()) {
      return value.trim();
    }
  }
  return null;
}

function createNow(platform) {
  if (typeof platform.now === "function") {
    return () => platform.now();
  }
  return () => Date.now();
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

function createPlatform() {
  return {
    appendFile(filePath, text) {
      fs.mkdirSync(path.dirname(filePath), { recursive: true });
      fs.appendFileSync(filePath, text, "utf8");
    },
    cwd() {
      return process.cwd();
    },
    env: process.env,
    fileExists(filePath) {
      return fs.existsSync(filePath);
    },
    homeDir() {
      return process.env.HOME || null;
    },
    now() {
      return Date.now();
    },
    readFile(filePath) {
      return fs.readFileSync(filePath, "utf8");
    },
    spawnSync(binary, args, options) {
      return spawnSync(binary, args, {
        ...options,
        stdio: ["pipe", "ignore", "pipe"],
      });
    },
  };
}

function describeError(error) {
  return error instanceof Error ? error.message : String(error);
}

module.exports = plugin;
module.exports.default = plugin;
module.exports.createMemoryBankPluginRuntime = createMemoryBankPluginRuntime;
module.exports.createPlatform = createPlatform;
