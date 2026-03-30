# Troubleshooting

Start with the three commands below before changing anything else:

```bash
mb status
mb doctor
mb logs -f
```

If you want Memory Bank to try safe repairs on its own, run:

```bash
mb doctor --fix
```

## Setup Never Ran After Install

This usually happens when the installer ran in a non-interactive shell.

Run:

```bash
mb setup
```

If `mb` is not available in the current shell yet, use:

```bash
~/.memory_bank/bin/mb setup
```

Then open a new shell later so plain `mb` works there too.

## Managed Service Is Not Active

If `mb status` or `mb doctor` says the managed service is not active:

1. Run `mb service status`.
2. Run `mb doctor --fix`.
3. If needed, run `mb service start`.
4. Follow the log with `mb logs -f`.

On macOS the managed service uses `launchd`. On Linux it uses `systemd --user`.

## Health Check To `/healthz` Failed

This usually means the service is not fully up yet or the provider configuration is invalid.

Check:

- `mb logs -f`
- `mb status`
- `mb config show`

Also remember that the first startup can take longer than later runs because the embedding model may need to download and warm its local cache under `~/.memory_bank/models/`.

## Missing Provider Secret Or Invalid Provider Config

Hosted providers need a saved secret so the background service can start on its own.

The fastest fix is to rerun:

```bash
mb setup
```

If you want to inspect the current saved settings first:

```bash
mb config show
```

Memory Bank stores provider secrets in:

```text
~/.memory_bank/secrets.env
```

## Ollama Is Not Working

If you use Ollama and startup fails:

- Make sure the Ollama daemon is actually running.
- Use the base Ollama URL, such as `http://localhost:11434`.
- Do not point Memory Bank at `/v1` or another path suffix.
- Make sure the selected model already exists locally.

If the model is missing, pull it first:

```bash
ollama pull <model>
```

Then rerun `mb setup` or restart the service.

## Recall Works But New Memories Are Not Showing Up

That usually means MCP recall is configured, but the capture side is not loading correctly.

Check:

- `mb status` to see whether the integration is marked configured
- that you restarted the agent after running `mb setup`
- that you completed a full turn with a final assistant answer

Agent-specific capture notes:

- Claude Code and Gemini CLI need their Memory Bank hooks loaded.
- OpenCode capture is plugin-based.
- OpenClaw capture is extension-based and recall goes through the stdio proxy.

If recall works but nothing new is being stored, the capture side is the first thing to inspect.

## The Agent Never Calls `retrieve_memory`

First confirm the MCP side is loaded:

- Claude Code: use `/mcp`
- Gemini CLI: check `gemini mcp list`
- OpenCode: check `opencode mcp list`
- OpenClaw: check `openclaw mcp list`

Then ask the agent explicitly to call `retrieve_memory` as part of a smoke test. Once the MCP integration is confirmed, you can decide how strongly you want to prompt the agent to use memory by default.

## I Need To Inspect Or Undo Config Changes

When `mb setup` rewrites JSON-based agent config files, it makes backups first.

Look for:

- sibling `*.mb_backup` files next to the edited config
- centralized backups under `~/.memory_bank/backups/`

If something looks stale or inconsistent, rerun `mb setup` before hand-editing multiple files.

## Still Stuck?

Use the agent-specific guide for the integration you are using:

- [Claude Code](./claude-code.md)
- [Gemini CLI](./gemini-cli.md)
- [OpenCode](./opencode.md)
- [OpenClaw](./openclaw.md)

For a high-level overview of the moving parts, see [Architecture](./architecture.md). For platform support and provider notes, see [Requirements](./requirements.md).
