<h1>
  <img src="./docs/logo.svg" alt="Memory Bank logo" width="88" align="absmiddle" />
  Memory Bank
</h1>

Memory Bank gives coding agents shared, long-term memory across sessions and across tools.

It runs locally, stores memory in your own namespaced SQLite databases, and works with Claude Code, Codex, Gemini CLI, OpenCode, and OpenClaw.

## Why Memory Bank

- Shared memory across supported agents instead of siloed memory per tool.
- Local ownership of storage, namespaces, and internal model choice.
- Better continuity from captured user prompts, tool calls, tool results, and assistant replies.
- A simple day-to-day control surface through `mb` instead of manual server management.

## Quick Start

The normal install path is the release installer plus `mb setup`.

Install from GitHub Releases:

```bash
sh -c "$(curl -fsSL https://raw.githubusercontent.com/feelingsonice/MemoryBank/main/install.sh)"
```

If you already cloned this repo and just want to use the local installer script:

```bash
./install.sh
```

Then finish setup:

```bash
mb setup
```

`mb setup` walks you through:

- choosing a namespace
- picking the internal LLM provider and model
- storing the provider secret if needed
- installing and starting the managed background service
- wiring any supported agents it detects on your `PATH`

Verify that everything is healthy:

```bash
mb status
```

If the installer finished in a non-interactive shell and skipped setup, just run `mb setup` afterward. If `mb` is not available in the current shell yet, use `~/.memory_bank/bin/mb setup` or open a new shell.

If you later change `server.fastembed_model` with `mb config set`, the CLI will ask you to confirm it. The next service start will rebuild the vector index for the active namespace and re-encode any existing memories with the new embedding model. While that runs, `mb status` and `mb service status` will report that Memory Bank is not up yet because it is reindexing.

### Smoke Test

In a fresh agent session, ask it to remember something memorable and do at least one tool call:

```text
Remember that my favorite editor is Helix, then run pwd and summarize what you did.
```

Then ask it to retrieve memory before answering:

```text
Before answering, call retrieve_memory for my editor preference and tell me what you find.
```

If the setup is working, the agent should call `retrieve_memory` and answer using the stored note.

## Supported Agents

| Agent | Recall path | Capture path | Guide |
| --- | --- | --- | --- |
| Claude Code | HTTP MCP | Claude hooks -> `memory-bank-hook` | [Claude Code](./docs/claude-code.md) |
| Codex | HTTP MCP | Codex hooks -> `memory-bank-hook` | [Codex](./docs/codex.md) |
| Gemini CLI | HTTP MCP | Gemini hooks -> `memory-bank-hook` | [Gemini CLI](./docs/gemini-cli.md) |
| OpenCode | HTTP MCP | OpenCode plugin -> `memory-bank-hook` | [OpenCode](./docs/opencode.md) |
| OpenClaw | stdio MCP proxy -> HTTP MCP | OpenClaw extension -> `memory-bank-hook` | [OpenClaw](./docs/openclaw.md) |

## More Docs

- [Troubleshooting](./docs/troubleshooting.md)
- [Architecture](./docs/architecture.md)
- [Requirements](./docs/requirements.md)
- [Claude Code Guide](./docs/claude-code.md)
- [Codex Guide](./docs/codex.md)
- [Gemini CLI Guide](./docs/gemini-cli.md)
- [OpenCode Guide](./docs/opencode.md)
- [OpenClaw Guide](./docs/openclaw.md)

## How It Works

1. Your agent emits hook, plugin, or extension events.
2. `memory-bank-hook` normalizes those events and sends them to the local Memory Bank service.
3. The service assembles finalized turns, analyzes them with your configured provider, and stores memory notes plus local embeddings.
4. Agents call `retrieve_memory` over MCP when prior context could improve the answer.

Important: the coding agent you use directly is separate from the internal provider Memory Bank uses for memory analysis. For example, you can use Claude Code while Memory Bank runs on Gemini, OpenAI, Anthropic, or Ollama.

## Advanced

If you want to build from source instead of downloading a release, use:

```bash
./install.sh --from-source
```

That is the advanced path. The lower-level binaries still exist, but `mb` and the built-in `--help` pages are the intended user interface.

## License

Memory Bank is licensed under `MIT`. See [LICENSE](./LICENSE).
