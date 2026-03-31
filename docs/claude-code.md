# Claude Code Guide

Memory Bank integrates with Claude Code in two ways:

- recall through a user-scoped HTTP MCP server
- capture through Claude hooks that call `memory-bank-hook`

The normal path is `mb setup`. You do not need repo-local Claude config for the supported setup.

## What `mb setup` Configures

`mb setup` will:

- register a user-scoped Claude MCP server named `memory-bank`
- update `~/.claude/settings.json`
- add Memory Bank hooks for:
  - `UserPromptSubmit`
  - `PreToolUse`
  - `PostToolUse`
  - `Stop`
- point those hooks at the managed binary under `~/.memory_bank/bin/memory-bank-hook`

If `~/.claude/settings.json` already exists, Memory Bank backs it up before rewriting it.

## Files And Settings It Touches

- Claude's user-scoped MCP registry entry for `memory-bank`
- `~/.claude/settings.json`

## Claude Code Quirks

- Claude's MCP registration and hook configuration are separate. Full Memory Bank behavior needs both recall and capture.
- `mb setup` expects the `memory-bank` MCP server to live at user scope. A conflicting project-scoped or local-scoped `memory-bank` entry can block setup until you remove or rename it.
- Memory Bank finalizes Claude turns on `Stop`. If Claude reports `stop_hook_active=true`, that stop event is treated as soft and the turn may not finalize yet.
- Restart Claude Code, or check `/mcp` and `/hooks`, after setup.

## Verify

Useful checks:

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

If you need general fixes, use [Troubleshooting](./troubleshooting.md). For platform support and provider notes, see [Requirements](./requirements.md).
