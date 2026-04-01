# Codex Guide

Memory Bank integrates with Codex in two ways:

- recall through a user-scoped HTTP MCP server
- capture through Codex hooks that call `memory-bank-hook`

The supported Memory Bank setup is user-scoped and handled by `mb setup`.

## What `mb setup` Configures

`mb setup` updates Codex's user config under `~/.codex/` and:

- updates `~/.codex/config.toml`
- ensures `[features]` contains `codex_hooks = true`
- ensures `[mcp_servers.memory-bank]` points at the local Memory Bank MCP endpoint
- sets `enabled = true` for that MCP entry
- updates `~/.codex/hooks.json`
- adds Memory Bank hook handlers for:
  - `UserPromptSubmit`
  - `PreToolUse`
  - `PostToolUse`
  - `Stop`
- uses `matcher = "Bash"` for `PreToolUse` and `PostToolUse`
- points every Memory Bank hook at the managed binary under `~/.memory_bank/bin/memory-bank-hook`
- sets `timeout = 10` for each Memory Bank Codex hook

If these files already exist, Memory Bank backs them up first and only rewrites the Memory Bank-owned Codex MCP entry and Memory Bank-owned Codex hook handlers. Unrelated Codex settings, MCP servers, and hooks are preserved.

## Files And Settings It Touches

- `~/.codex/config.toml`
- `~/.codex/hooks.json`

## What Memory Bank Captures From Codex

Memory Bank currently captures these Codex hook events:

- `UserPromptSubmit` as the user message
- `PreToolUse` as a tool call
- `PostToolUse` as a tool result
- `Stop` as the final assistant message and turn finalization

Memory Bank uses Codex `session_id` and `turn_id` to assemble a turn. If Codex reaches `Stop` without `last_assistant_message`, Memory Bank still finalizes the turn, but the assistant text may be missing from that stored turn.

## Codex Quirks

- Memory Bank configures Codex at user scope only. Codex also supports project-scoped `.codex/config.toml` and `.codex/hooks.json`, but `mb setup` does not manage those files in the current integration.
- Codex hooks are still experimental and under active development, and they require `features.codex_hooks = true`.
- Codex currently disables hooks on Windows. Memory Bank's managed install is also macOS/Linux-only today.
- Codex loads hooks from every active config layer. A repo-local `.codex/hooks.json` does not replace `~/.codex/hooks.json`; both can run.
- Multiple matching Codex command hooks for the same event run concurrently. One hook cannot prevent another matching hook from starting.
- `PreToolUse` and `PostToolUse` currently only emit for the `Bash` tool. That means Memory Bank currently captures user prompts and final assistant replies reliably, but separate tool-call and tool-result capture is limited to Bash commands.
- `UserPromptSubmit` and `Stop` do not currently use `matcher`.
- `Stop` is special in Codex: JSON on `stdout` controls continuation behavior. Memory Bank's hook does not emit its own continuation or block responses, so it stays out of that control path and only captures the event.
- Codex stores MCP config in `config.toml`, and the Codex CLI plus the IDE extension share that same MCP configuration. A user-scoped Memory Bank setup therefore applies to both Codex clients.

## Verify

Useful checks:

- use `/mcp` in the Codex TUI to confirm the `memory-bank` MCP server is loaded
- inspect `~/.codex/config.toml`
- inspect `~/.codex/hooks.json`
- restart Codex after setup so it reloads the updated user config

Simple smoke test:

```text
Remember that my favorite editor is Helix, then run pwd and summarize what you did.
```

Then ask:

```text
Before answering, call retrieve_memory for my editor preference and tell me what you find.
```

If you also use repo-local `.codex/` config, verify that those local hooks are not blocking or shadowing the user-level Memory Bank setup. Because Codex loads matching hooks from both layers, conflicting custom hooks can make capture behavior harder to reason about.

## Official Codex References

- [Hooks](https://developers.openai.com/codex/hooks)
- [Model Context Protocol](https://developers.openai.com/codex/mcp)
- [Config basics](https://developers.openai.com/codex/config-basic)
- [Config reference](https://developers.openai.com/codex/config-reference)

If you need general fixes, use [Troubleshooting](./troubleshooting.md). For platform support and provider notes, see [Requirements](./requirements.md).
