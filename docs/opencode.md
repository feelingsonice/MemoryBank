# OpenCode Guide

OpenCode uses Memory Bank a little differently from Claude Code and Gemini CLI:

- recall still uses HTTP MCP
- capture is plugin-based instead of hook-block based

The normal path is `mb setup`. Manual per-project plugin copying is no longer the default user flow.

## What `mb setup` Configures

`mb setup` will:

- copy the bundled Memory Bank plugin to `~/.config/opencode/plugins/memory-bank.js`
- update `~/.config/opencode/opencode.json`
- add a `memory-bank` MCP entry pointing at the local Memory Bank service

The plugin then shells out to the managed hook binary under `~/.memory_bank/bin/memory-bank-hook`.

If `~/.config/opencode/opencode.json` already exists, Memory Bank backs it up before rewriting it.

## Files And Settings It Touches

- `~/.config/opencode/opencode.json`
- `~/.config/opencode/plugins/memory-bank.js`

## OpenCode Quirks

- Capture is plugin-based. MCP recall can work even if the plugin is not loading, so capture and recall can fail independently.
- The plugin emits the final assistant fragment on `session.idle`, not from streaming assistant message updates.
- If the final assistant message is not durable yet when `session.idle` fires, the plugin retries briefly before giving up.
- If `memory-bank-hook` is missing, the plugin skips capture instead of crashing OpenCode.
- Optional debug logging is available through `MEMORY_BANK_OPENCODE_DEBUG=1` and `MEMORY_BANK_OPENCODE_DEBUG_FILE=/path/to/log`.

## Verify

Useful checks:

- `opencode mcp list`
- confirm `~/.config/opencode/opencode.json` contains a `memory-bank` MCP entry
- confirm `~/.config/opencode/plugins/memory-bank.js` exists

Then restart OpenCode and run the same smoke test used in the main README.

If you need general fixes, use [Troubleshooting](./troubleshooting.md). For platform support and provider notes, see [Requirements](./requirements.md).
