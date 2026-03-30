# Requirements and Cost

## Supported Platforms

Release installs are currently aimed at:

- macOS on Apple Silicon
- Linux on `x86_64`
- Linux on `aarch64` / `arm64`

Current caveats:

- Intel macOS is source-build only for now: use `./install.sh --from-source`
- Windows is not currently a supported managed install target

## What You Need

- A supported agent installed if you want `mb setup` to wire it automatically: Claude Code, Gemini CLI, OpenCode, or OpenClaw
- One internal LLM provider for Memory Bank: Anthropic, Gemini, OpenAI, or Ollama
- Internet access for the default installer and for the first embedding-model download
- Enough local disk for `~/.memory_bank/`, the embedding model cache, and your namespace databases as they grow

Memory Bank's default managed setup also expects:

- `launchd` on macOS
- `systemd --user` on Linux

## Local Resource Expectations

Memory Bank is designed to run as a local background service on `127.0.0.1`.

The default setup keeps the main moving parts on your machine:

- the managed service
- the SQLite databases
- the FastEmbed model cache
- the agent integration assets copied into `~/.memory_bank/integrations/`

The first startup can be slower than later ones because the default embedding model may need to download and initialize locally.

There is no separate external database to provision for the default path.

## How Cost Works

Memory Bank's cost is separate from the cost of the coding agent you use directly.

For example:

- Claude Code plus Memory Bank on Anthropic means both Claude Code usage and Memory Bank's internal memory-analysis calls can matter
- OpenCode plus Memory Bank on Ollama avoids hosted LLM charges for Memory Bank's internal analysis, but OpenCode itself may still have its own costs depending on how you use it

In the current design, each finalized turn usually triggers:

- one memory-analysis LLM call
- sometimes a second graph-evolution LLM call when nearby existing memories are found
- local embeddings by default

The default embedding path is local FastEmbed, so the default setup does not add a separate embedding API bill.

## Caching And Cost Control

Memory Bank already benefits from provider-side caching where supported:

- Anthropic prompt caching is enabled
- OpenAI prompt caching is automatic on supported models
- Gemini relies on the provider's implicit caching on supported models

If you want to keep costs down:

- choose a smaller or cheaper hosted model during `mb setup`
- use Ollama if you want Memory Bank's internal analysis to stay local
- leave advanced settings alone unless you have a specific reason to tune them

If you need to review the currently saved provider and model:

```bash
mb status
mb config show
```

## Practical Expectations

For most users, the main questions are:

- Can my machine run the local service? Usually yes if it can run the supported agent and a small local background process.
- Will the first run take longer? Yes, because of local model download and cache warm-up.
- Do I need a GPU? Not for the normal managed setup.
- Will Memory Bank add cost on top of my agent? Yes, unless you use a local provider like Ollama for Memory Bank's internal analysis.

## Related Docs

- [Troubleshooting](./troubleshooting.md)
- [How Memory Bank Works](./how-it-works.md)
