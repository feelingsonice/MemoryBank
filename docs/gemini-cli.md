# Gemini CLI Guide

This guide covers the Gemini CLI specific wiring for Memory Bank.

For building the binaries, starting `memory-bank-server`, and the shared configuration surface, use the main [README](../README.md).

## How Gemini CLI Connects

Gemini CLI uses Memory Bank in two separate ways:

- Recall uses MCP. Gemini CLI connects to `http://127.0.0.1:8080/mcp` and can call `retrieve_memory`.
- Capture uses Gemini hooks. Those hooks shell out to `memory-bank-hook`, which forwards normalized fragments to `POST /ingest`.

## Hooks Used

| Gemini hook | Stored as | Notes |
| --- | --- | --- |
| `BeforeAgent` | user message | Captures the user's prompt text. |
| `BeforeTool` | tool call | Captures tool name plus tool input JSON. |
| `AfterTool` | tool result | Captures tool name plus tool output JSON. |
| `AfterAgent` | assistant message | Final assistant fragment. This is a hard terminal fragment only when `stop_hook_active=false`. |

## Project Setup

1. Make sure Memory Bank is already running.

Use the main [README Quick Start](../README.md#quick-start) or [Build From Source](../README.md#build-from-source) first.

2. Register the MCP server in Gemini CLI.

```bash
gemini mcp add --scope project --transport http memory-bank http://127.0.0.1:8080/mcp
```

3. Add the Memory Bank MCP entry and hooks to `.gemini/settings.json`.

Replace `/absolute/path/to/memory-bank-hook` with the absolute path to your `memory-bank-hook` binary.

```json
{
  "mcpServers": {
    "memory-bank": {
      "httpUrl": "http://127.0.0.1:8080/mcp"
    }
  },
  "hooks": {
    "BeforeAgent": [
      {
        "matcher": "*",
        "sequential": true,
        "hooks": [
          {
            "name": "memory-bank",
            "type": "command",
            "command": "/absolute/path/to/memory-bank-hook --agent gemini-cli --event BeforeAgent --server-url http://127.0.0.1:8080"
          }
        ]
      }
    ],
    "BeforeTool": [
      {
        "matcher": ".*",
        "sequential": true,
        "hooks": [
          {
            "name": "memory-bank",
            "type": "command",
            "command": "/absolute/path/to/memory-bank-hook --agent gemini-cli --event BeforeTool --server-url http://127.0.0.1:8080"
          }
        ]
      }
    ],
    "AfterTool": [
      {
        "matcher": ".*",
        "sequential": true,
        "hooks": [
          {
            "name": "memory-bank",
            "type": "command",
            "command": "/absolute/path/to/memory-bank-hook --agent gemini-cli --event AfterTool --server-url http://127.0.0.1:8080"
          }
        ]
      }
    ],
    "AfterAgent": [
      {
        "matcher": "*",
        "sequential": true,
        "hooks": [
          {
            "name": "memory-bank",
            "type": "command",
            "command": "/absolute/path/to/memory-bank-hook --agent gemini-cli --event AfterAgent --server-url http://127.0.0.1:8080"
          }
        ]
      }
    ]
  }
}
```

4. Restart Gemini CLI in this repo so it reloads the settings.

## Machine-Wide Setup

If you want the same Memory Bank setup in every Gemini CLI session on this machine:

- register MCP at user scope:

```bash
gemini mcp add --scope user --transport http memory-bank http://127.0.0.1:8080/mcp
```

- move the same `mcpServers` and `hooks` config into `~/.gemini/settings.json`

Repo-local `.gemini/settings.json` still overrides `~/.gemini/settings.json`.

## Gemini CLI Quirks

- Keep `sequential: true` on each Memory Bank hook group.
- If you already use hooks that can validate, rewrite, redact, deny, or retry the same event, put those hooks before Memory Bank.
- Keep the `memory-bank` hook last in each relevant hook group so Gemini's final version of the event is what gets captured.
- Use an absolute path for `memory-bank-hook`.
- A turn is only ready for memory analysis after a hard `AfterAgent`.
- If `stop_hook_active=true`, the `AfterAgent` fragment is treated as soft and the turn is not finalized yet.

## Verify

Useful checks:

- `gemini mcp list`
- inspect `.gemini/settings.json`

Simple smoke test:

```text
Remember that my favorite editor is Helix, then run pwd and summarize what you did.
```

Then ask:

```text
Before answering, call retrieve_memory for my editor preference and tell me what you find.
```

You can manually delete Gemini CLI's memory notes if you'd like.
