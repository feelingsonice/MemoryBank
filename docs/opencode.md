# OpenCode Guide

This guide covers the OpenCode specific wiring for Memory Bank.

OpenCode is different from Claude Code and Gemini CLI. Recall still uses MCP, but capture is handled by a plugin rather than a hook block in a settings file.

For building the binaries, starting `memory-bank-server`, and the shared configuration surface, use the main [README](../README.md).

## How OpenCode Connects

OpenCode uses Memory Bank in two separate ways:

- Recall uses MCP. OpenCode connects to `http://127.0.0.1:3737/mcp` and can call `retrieve_memory`.
- Capture uses the Memory Bank plugin at `.opencode/plugins/memory-bank.js`. The plugin listens to OpenCode events, shells out to `memory-bank-hook`, and forwards normalized fragments to `POST /ingest`.

Without the plugin, OpenCode can still retrieve from Memory Bank through MCP, but it will not capture new memories.

## Events Used

| OpenCode source | Sent through hook as | Stored as | Notes |
| --- | --- | --- | --- |
| `chat.message` | `message.updated` | user message | Only non-summary, non-reverted user messages with text are captured. |
| `tool.execute.before` | `tool.execute.before` | tool call | Captures tool name plus tool arguments JSON. |
| `tool.execute.after` | `tool.execute.after` | tool result | Captures tool name plus tool output JSON. |
| `session.idle` | `session.idle` | assistant message | The plugin resolves the latest assistant reply and emits a hard terminal fragment. |

The plugin intentionally does not capture:

- assistant `message.updated` streaming events
- reasoning
- `file.edited`
- `command.executed`
- deleted or reverted message retractions
- undocumented or experimental hooks

## Project Setup

1. Make sure Memory Bank is already running.

Use the main [README Quick Start](../README.md#quick-start) or [Build From Source](../README.md#build-from-source) first.

2. Add the MCP server to `opencode.json`.

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "memory-bank": {
      "type": "remote",
      "url": "http://127.0.0.1:3737/mcp",
      "enabled": true
    }
  }
}
```

3. Make sure the plugin is available.

If you are running OpenCode inside this repository, the plugin already exists at `.opencode/plugins/memory-bank.js` and OpenCode will load it automatically.

If you want to use Memory Bank from another OpenCode project, copy or symlink this plugin into that project's `.opencode/plugins/` directory:

```bash
mkdir -p /path/to/your-project/.opencode/plugins
ln -sf /absolute/path/to/a-mem-mcp/.opencode/plugins/memory-bank.js /path/to/your-project/.opencode/plugins/memory-bank.js
```

4. Make sure the plugin can find `memory-bank-hook`.

By default, the plugin looks for:

- `./target/debug/memory-bank-hook`
- then `./target/release/memory-bank-hook`

If your binary lives somewhere else, set:

```bash
export MEMORY_BANK_HOOK_BIN=/absolute/path/to/memory-bank-hook
export MEMORY_BANK_SERVER_URL=http://127.0.0.1:3737
```

`MEMORY_BANK_SERVER_URL` is only needed if you are not using the default server address.

## Machine-Wide Setup

If you want the same Memory Bank setup in every OpenCode project on this machine:

1. Put the MCP server in `~/.config/opencode/opencode.json`.
2. Copy or symlink the plugin into `~/.config/opencode/plugins/memory-bank.js`.
3. Export `MEMORY_BANK_HOOK_BIN` from your shell profile if the binary is not in the plugin's default project-relative location.

Project-local `opencode.json` still overrides the global config when both exist.

## OpenCode Quirks

- Capture is plugin-based, not settings-hook based.
- The plugin emits the final assistant fragment on `session.idle`, not from streaming assistant message updates.
- When `session.idle` fires, the plugin fetches the latest assistant message through the OpenCode session API. If that message is not durable yet, it retries briefly before giving up.
- If `memory-bank-hook` is missing, the plugin logs a warning and skips capture instead of crashing OpenCode.
- Debugging is available through:

```bash
export MEMORY_BANK_OPENCODE_DEBUG=1
export MEMORY_BANK_OPENCODE_DEBUG_FILE=/absolute/path/to/memory-bank-opencode.log
```

## Verify

Useful checks:

- `opencode mcp list`
- confirm `opencode.json` contains the `memory-bank` MCP entry
- confirm `.opencode/plugins/memory-bank.js` exists in the active project or global plugin directory

Simple smoke test:

```text
Remember that my favorite editor is Helix, then run pwd and summarize what you did.
```

Then ask:

```text
Before answering, use the memory-bank retrieve_memory tool to look up my editor preference and tell me what you find.
```
