# Gemini CLI Guide

Memory Bank integrates with Gemini CLI in two ways:

- recall through HTTP MCP
- capture through Gemini hooks that call `memory-bank-hook`

The supported setup is user-scoped and driven by `mb setup`.

## What `mb setup` Configures

`mb setup` will update `~/.gemini/settings.json` and add:

- a `memory-bank` MCP server pointing at the local Memory Bank service
- hook entries for:
  - `BeforeAgent`
  - `BeforeTool`
  - `AfterTool`
  - `AfterAgent`

It also writes the managed hook binary path under `~/.memory_bank/bin/memory-bank-hook`.

If `~/.gemini/settings.json` already exists, Memory Bank backs it up before rewriting it.

## Files And Settings It Touches

- `~/.gemini/settings.json`

## Gemini CLI Quirks

- Gemini capture depends on hook ordering. If you already use other hooks that transform, redact, deny, or retry events, keep those before the Memory Bank hook so Memory Bank sees the final version.
- Memory Bank expects its hook groups to stay `sequential: true`.
- Memory Bank finalizes Gemini turns on `AfterAgent`. If Gemini reports `stop_hook_active=true`, that event is treated as soft and the turn may not finalize yet.
- Restart Gemini CLI after setup so it reloads the updated settings file.

## Verify

Useful checks:

- `gemini mcp list`
- inspect `~/.gemini/settings.json`

Simple smoke test:

```text
Remember that my favorite editor is Helix, then run pwd and summarize what you did.
```

Then ask:

```text
Before answering, call retrieve_memory for my editor preference and tell me what you find.
```

If you need general fixes, use [Troubleshooting](./troubleshooting.md). For platform support and provider notes, see [Requirements](./requirements.md).
