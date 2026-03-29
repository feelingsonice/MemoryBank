# OpenClaw Guide

This guide covers the OpenClaw-specific wiring for Memory Bank.

OpenClaw is different from Claude Code, Gemini CLI, and OpenCode in one important way: its native runtime currently expects stdio MCP servers, not a remote HTTP MCP endpoint. That means OpenClaw uses Memory Bank in two separate ways:

- Recall uses the `memory-bank-mcp-proxy` stdio MCP bridge.
- Capture uses the repo-owned OpenClaw extension at `.openclaw/extensions/memory-bank/`, which shells out to `memory-bank-hook`.

For building the binaries, starting `memory-bank-server`, and the shared configuration surface, use the main [README](../README.md).

## How OpenClaw Connects

OpenClaw uses Memory Bank in two separate ways:

- Recall uses the stdio `memory-bank-mcp-proxy`, which forwards `retrieve_memory` calls to `http://127.0.0.1:3737/mcp`.
- Capture uses the repo-owned OpenClaw extension. The extension listens to OpenClaw lifecycle hooks, shells out to `memory-bank-hook`, and forwards normalized fragments to `POST /ingest`.
- The extension also injects a short system reminder telling OpenClaw to prefer `retrieve_memory` for durable memory and provenance questions such as "how do you know?".

Without the extension, OpenClaw can still retrieve from Memory Bank through the proxy, but it will not capture new memories.

## Events Used

| OpenClaw source | Sent through hook as | Stored as | Notes |
| --- | --- | --- | --- |
| `before_model_resolve` | `before_model_resolve` | user message | Primary prompt-capture hook. Runs before model resolution and is not gated by prompt-injection policy. |
| `before_prompt_build` | `before_prompt_build` | user message | Fallback prompt capture when the pre-session phase lacks enough context to emit safely. Also injects the Memory Bank preference guidance. |
| `before_tool_call` | `before_tool_call` | tool call | Captures tool name plus tool arguments JSON. |
| `after_tool_call` | `after_tool_call` | tool result | Captures tool name plus tool output JSON, including tool error metadata when OpenClaw surfaces it. |
| `agent_end` | `agent_end` | assistant message | Captures the final assistant reply as a hard terminal fragment. |

The extension intentionally does not capture:

- raw message transit hooks
- reasoning
- compaction
- OpenClaw's own memory internals
- empty prompts or empty final assistant messages

## Recommended Setup

1. Make sure Memory Bank is already running.

Use the main [README Quick Start](../README.md#quick-start) or [Build From Source](../README.md#build-from-source) first.

2. Register the Memory Bank MCP proxy with OpenClaw.

```bash
openclaw mcp set memory-bank '{"command":"/absolute/path/to/memory-bank-mcp-proxy","args":["--server-url","http://127.0.0.1:3737"]}'
```

This stores a stdio MCP server definition under OpenClaw's MCP registry. The proxy also advertises Memory Bank as the preferred long-term memory source for prior-session recall inside OpenClaw.

`memory-bank-mcp-proxy --server-url` accepts either:

- the base server URL, such as `http://127.0.0.1:3737`
- or the explicit MCP URL, such as `http://127.0.0.1:3737/mcp`

This guide uses the base server URL form because the proxy appends `/mcp` automatically when needed.

3. Install the repo-owned OpenClaw extension.

From this repository root:

```bash
openclaw plugins install -l ./.openclaw/extensions/memory-bank
```

This native plugin keeps a minimal local-install:

- `package.json`
- `openclaw.plugin.json`
- `index.js`

The `package.json` is required for `openclaw plugins install` on current OpenClaw releases because the installer looks for `openclaw.extensions` when validating local plugin paths.

4. Enable the extension and make Memory Bank the primary memory system.

Update `~/.openclaw/openclaw.json` to include:

```json
{
  "mcp": {
    "servers": {
      "memory-bank": {
        "command": "/absolute/path/to/memory-bank-mcp-proxy",
        "args": ["--server-url", "http://127.0.0.1:3737"]
      }
    }
  },
  "plugins": {
    "entries": {
      "memory-bank": {
        "enabled": true,
        "config": {
          "hookBinary": "/absolute/path/to/memory-bank-hook",
          "serverUrl": "http://127.0.0.1:3737"
        }
      }
    },
    "slots": {
      "memory": "none"
    }
  }
}
```

Important: setting `plugins.slots.memory` to `"none"` is the recommended v1 setup. It avoids split-brain behavior between OpenClaw's built-in memory plugins and Memory Bank.

5. Restart OpenClaw so it reloads the saved MCP server definitions and the extension config.

If you are running the managed gateway service:

```bash
openclaw gateway restart
```

If you are running OpenClaw in the foreground instead, stop that process and start it again with:

```bash
openclaw gateway run
```

## Workspace-Local Development

The supported v1 path is workspace-local development from this repository.

The extension lives at:

```text
.openclaw/extensions/memory-bank/
```

OpenClaw can also read the same `memory-bank` extension after installation from another workspace, as long as the extension path and binary paths resolve correctly.

## Extension Config

The OpenClaw extension uses a hybrid config model:

- Primary: OpenClaw plugin config in `~/.openclaw/openclaw.json`
- Fallback: environment variables

Supported config keys:

| Key | Description |
| --- | --- |
| `serverUrl` | Base URL for the running Memory Bank server. |
| `hookBinary` | Absolute path to `memory-bank-hook`. |
| `debug` | Enables verbose extension diagnostics. |
| `debugFile` | Optional log file path for extension diagnostics. |

Supported fallback environment variables:

| Variable | Description |
| --- | --- |
| `MEMORY_BANK_SERVER_URL` | Fallback base URL for the running Memory Bank server. |
| `MEMORY_BANK_HOOK_BIN` | Fallback path to `memory-bank-hook`. |
| `MEMORY_BANK_OPENCLAW_DEBUG` | Enables verbose extension diagnostics when set to `1`. |
| `MEMORY_BANK_OPENCLAW_DEBUG_FILE` | Mirrors extension logs to a file. |

## OpenClaw Quirks

- Recall goes through a stdio proxy, not directly to the HTTP `/mcp` endpoint.
- Capture is extension-based, not hook-block based.
- Prompt capture uses the modern OpenClaw hook split: `before_model_resolve` for prompt capture and `before_prompt_build` for prompt shaping plus fallback capture.
- The recommended setup disables OpenClaw's built-in memory slot so Memory Bank is the only long-term memory system in play.
- Even with the built-in memory slot disabled, OpenClaw workspace files like `USER.md` and `MEMORY.md` can still exist and be loaded by the host. The proxy instructions and plugin prompt injection are there to bias OpenClaw toward `retrieve_memory` as the primary durable memory source.
- If `plugins.entries.memory-bank.hooks.allowPromptInjection=false`, OpenClaw blocks the prompt-shaping hook but still allows prompt capture through `before_model_resolve`.
- The extension is written to be defensive about hook payload shape and skips incomplete events instead of crashing the runtime.
- If `memory-bank-hook` is missing, the extension logs a warning and skips capture instead of crashing OpenClaw.

## Verify

Useful checks:

- `openclaw mcp list`
- confirm `~/.openclaw/openclaw.json` contains the `memory-bank` MCP server definition
- confirm the `memory-bank` plugin entry is enabled
- confirm `plugins.slots.memory` is set to `"none"` in the recommended setup

Simple smoke test:

```text
Remember that my favorite editor is Helix, then run pwd and summarize what you did.
```

Then ask:

```text
Before answering, use the memory-bank retrieve_memory tool to look up my editor preference and tell me what you find.
```

If everything is wired correctly, OpenClaw should call `retrieve_memory` through the stdio proxy and answer using the stored note.
