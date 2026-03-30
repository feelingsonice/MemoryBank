# How Memory Bank Works

Memory Bank is a local memory service for coding agents.

At a high level, it separates memory into two paths:

- capture: collect conversation events from the agent
- recall: let the agent query long-term memory with `retrieve_memory`

## The Main Pieces

- `mb`
  - the user-facing CLI for setup, status, logs, namespace switching, and diagnostics
- the managed Memory Bank service
  - a local background process that exposes `/ingest`, `/mcp`, and `/healthz`
- `memory-bank-hook`
  - a lightweight binary that normalizes hook, plugin, and extension payloads into a shared ingest format
- the local data store
  - namespaced SQLite databases plus a local embedding-model cache under `~/.memory_bank/`

## Memory Lifecycle

1. Your agent emits events.

   Depending on the agent, those events come from hooks, a plugin, or an extension.

2. Memory Bank captures the turn.

   `memory-bank-hook` normalizes the raw event into an ingest payload and sends it to the local service.

3. The service assembles a finalized turn.

   User prompts, tool calls, tool results, and the final assistant reply are grouped into a single durable turn.

4. Memory Bank analyzes the turn.

   The configured provider turns that finalized conversation window into a structured memory note with context, keywords, and tags. When similar existing memories are found, Memory Bank may also run a second graph-evolution step to update links and refine tags.

5. Memory Bank stores the result locally.

   The memory note goes into the namespace's SQLite database, and a local embedding is stored for retrieval.

6. Future agent sessions retrieve memory over MCP.

   When the agent calls `retrieve_memory`, Memory Bank embeds the query, finds nearby notes, expands linked memories, and returns ranked memory results.

## Capture And Recall Are Different

The capture path depends on the agent:

- Claude Code: hooks
- Gemini CLI: hooks
- OpenCode: plugin
- OpenClaw: extension

The recall path is more uniform:

- most agents talk directly to Memory Bank over HTTP MCP
- OpenClaw uses a local stdio proxy that forwards to the same HTTP MCP endpoint

This is why an agent can have recall working while capture is broken, or vice versa.

## Namespaces

Namespaces let you keep separate memory stores for different projects, teams, or experiments.

Each namespace gets its own SQLite database under:

```text
~/.memory_bank/namespaces/<namespace>/memory.db
```

Use:

```bash
mb namespace list
mb namespace use <name>
```

When you switch namespaces, Memory Bank starts using a different local database immediately.

## What Memory Bank Is Not

Memory Bank is not a reverse proxy sitting in front of your coding agent's model traffic.

Instead:

- capture comes from agent hooks, plugins, or extensions
- recall comes from the `retrieve_memory` MCP tool
- the internal Memory Bank provider is separate from the coding agent you use directly

That separation is what makes cross-agent memory possible. You can use one tool as your coding agent and a different provider for Memory Bank's internal memory analysis.

## Related Docs

- [Requirements and Cost](./requirements-and-cost.md)
- [Troubleshooting](./troubleshooting.md)
- [Claude Code](./claude-code.md)
- [Gemini CLI](./gemini-cli.md)
- [OpenCode](./opencode.md)
- [OpenClaw](./openclaw.md)
