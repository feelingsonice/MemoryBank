# OpenClaw Guide

OpenClaw is the most different integration because recall goes through a local stdio proxy instead of talking to the HTTP MCP endpoint directly.

In practice that means:

- recall uses `memory-bank-mcp-proxy`
- capture uses the bundled OpenClaw Memory Bank extension

The supported setup is user-scoped and handled by `mb setup`.

## What `mb setup` Configures

`mb setup` will update `~/.openclaw/openclaw.json` and:

- add a `memory-bank` MCP server that launches `memory-bank-mcp-proxy`
- point that proxy at the local Memory Bank service
- add the bundled Memory Bank extension load path under `~/.memory_bank/integrations/openclaw/memory-bank`
- enable the `memory-bank` plugin entry
- point the plugin at the managed `memory-bank-hook` binary
- set `plugins.slots.memory` to `none`

If `~/.openclaw/openclaw.json` already exists, Memory Bank backs it up before rewriting it.

## Files And Settings It Touches

- `~/.openclaw/openclaw.json`
- the extension assets under `~/.memory_bank/integrations/openclaw/memory-bank`

## OpenClaw Quirks

- Recall goes through the local stdio proxy, not directly to `http://127.0.0.1:3737/mcp`.
- Capture is extension-based, not hook-block based.
- The extension captures prompt information before model resolution and captures the final assistant reply on `agent_end`.
- The supported setup disables OpenClaw's built-in memory slot with `plugins.slots.memory = "none"` to avoid split-brain behavior between OpenClaw memory plugins and Memory Bank.
- After setup, restart OpenClaw or its gateway so the new MCP and extension settings are picked up.

## Verify

Useful checks:

- `openclaw mcp list`
- inspect `~/.openclaw/openclaw.json`
- confirm the `memory-bank` plugin entry is enabled
- confirm `plugins.slots.memory` is set to `none`

Then restart OpenClaw and run the same smoke test used in the main README.

If you need general fixes, use [Troubleshooting](./troubleshooting.md). For platform support and provider notes, see [Requirements](./requirements.md).
