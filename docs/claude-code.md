# Claude Code Guide

This guide covers the Claude Code specific wiring for Memory Bank.

For building the binaries, starting `memory-bank-server`, and the shared configuration surface, use the main [README](../README.md).

## How Claude Code Connects

Claude Code uses Memory Bank in two separate ways:

- Recall uses MCP. Claude Code connects to `http://127.0.0.1:8080/mcp` and can call `retrieve_memory`.
- Capture uses Claude hooks. Those hooks shell out to `memory-bank-hook`, which forwards normalized fragments to `POST /ingest`.

## Hooks Used

| Claude hook | Stored as | Notes |
| --- | --- | --- |
| `UserPromptSubmit` | user message | Captures the user's prompt text. |
| `PreToolUse` | tool call | Captures tool name plus tool input JSON. |
| `PostToolUse` | tool result | Captures tool name plus tool output JSON. |
| `Stop` | assistant message | Final assistant fragment. This is a hard terminal fragment only when `stop_hook_active=false`. |

## Project Setup

1. Make sure Memory Bank is already running.

Use the main [README Quick Start](../README.md#quick-start) or [Build From Source](../README.md#build-from-source) first.

2. Register the MCP server in Claude Code.

```bash
claude mcp add --transport http --scope local memory-bank http://127.0.0.1:8080/mcp
```

3. Add the Memory Bank hooks to `.claude/settings.local.json`.

Replace `/absolute/path/to/memory-bank-hook` with the absolute path to your `memory-bank-hook` binary.

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "/absolute/path/to/memory-bank-hook --agent claude-code --event UserPromptSubmit --server-url http://127.0.0.1:8080"
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "/absolute/path/to/memory-bank-hook --agent claude-code --event PreToolUse --server-url http://127.0.0.1:8080"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "/absolute/path/to/memory-bank-hook --agent claude-code --event PostToolUse --server-url http://127.0.0.1:8080"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "/absolute/path/to/memory-bank-hook --agent claude-code --event Stop --server-url http://127.0.0.1:8080"
          }
        ]
      }
    ]
  }
}
```

If the file already contains other top-level keys such as `permissions`, keep them and add `hooks` alongside them.

4. Restart Claude Code in this repo, or use `/hooks` to confirm the hooks loaded.

## Machine-Wide Setup

If you want the same Memory Bank setup in every Claude Code session on this machine:

- register MCP at user scope:

```bash
claude mcp add --transport http --scope user memory-bank http://127.0.0.1:8080/mcp
```

- put the same `hooks` object in `~/.claude/settings.json`

Claude Code quirk: user-scoped MCP servers and user-scoped hooks are configured separately. `claude mcp add --scope user` manages the global MCP entry, while hook commands still come from `~/.claude/settings.json`.

## Claude Code Quirks

- Use an absolute path for `memory-bank-hook`. Hooks may run from contexts where relative paths are unreliable.
- A turn is only ready for memory analysis after a hard `Stop`.
- If `stop_hook_active=true`, the `Stop` fragment is treated as soft and the turn is not finalized yet.
- Settings precedence still applies. Project-local settings override broader Claude settings.

## Verify

Useful checks:

- `claude mcp list`
- `claude mcp get memory-bank`
- `/mcp`
- `/hooks`

Simple smoke test:

```text
Remember that my favorite editor is Helix, then run pwd and summarize what you did.
```

Then ask:

```text
Before answering, call retrieve_memory for my editor preference and tell me what you find.
```

You can manually delete Claude Code's memory notes if you'd like.
