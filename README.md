# Memory Bank

Memory Bank gives coding agents shared, long-term memory.

It combines a local MCP server for recall, a lightweight hook binary for capture, and a SQLite-backed memory store that turns prior conversations into reusable notes. Today it supports Claude Code, Gemini CLI, OpenCode, and OpenClaw, so memories captured in one agent can be recalled from another instead of staying trapped inside a single tool's memory system.

This repository is an implementation inspired by the original [A-MEM](https://arxiv.org/abs/2502.12110) paper, adapted for practical MCP-based agent workflows.

Currently supports:

- Claude Code
- Gemini CLI
- OpenCode
- OpenClaw

## Why Memory Bank

- Cross-agent continuity. A fact learned in Claude Code, Gemini CLI, OpenCode, or OpenClaw can be recalled later in another supported agent, and vice versa.
- Memory ownership. Your memory store, embeddings, namespaces, and internal analysis model stay under your control instead of being tied to one agent's plugin ecosystem.
- Consistent behavior. You get one memory policy, one retrieval surface, one place to debug capture quality, and one backup or migration story across agents.
- Vendor hedge. If your team changes agents later, the memory corpus stays useful.
- Better recall quality. Memory Bank stores structured turns with user messages, assistant replies, and tool activity so retrieval has richer context than a simple append-only note.
- OpenCode support. OpenCode can use long-term memory through the bundled plugin and the same Memory Bank server.
- OpenClaw support. OpenClaw can capture through the repo-owned extension and recall through the stdio MCP proxy while keeping the same shared Memory Bank backend.

## Quick Start

The intended default install path is the repository's GitHub Releases page. The user-facing install flow is the `mb` CLI plus the bootstrap installer at [`./install.sh`](./install.sh). The installer tracks the latest published release and downloads the correct tarball for the current supported macOS or Linux architecture.

If you are developing locally and want the installer to build from this checkout instead of downloading a release artifact, use:

```bash
./install.sh --from-source
```

1. Run the installer.

```bash
./install.sh
```

2. If you are developing locally and want to start the server without the managed service, run:

```bash
export ANTHROPIC_API_KEY=your-key
./target/release/memory-bank-server --llm-provider anthropic
```

By default the managed service binds to `127.0.0.1:3737` and exposes:

- MCP: `http://127.0.0.1:3737/mcp`
- Ingest: `http://127.0.0.1:3737/ingest`

The first startup may take a little longer because the embedding model is downloaded and cached locally.

2. Connect your agent to the MCP endpoint and configure hook-based capture.

Use the agent-specific guides in [`/docs`](./docs):

- [Claude Code](./docs/claude-code.md)
- [Gemini CLI](./docs/gemini-cli.md)
- [OpenCode](./docs/opencode.md)
- [OpenClaw](./docs/openclaw.md)

The agent you choose here is separate from the `--llm-provider` you chose when starting the server.

3. Run a smoke test.

In a fresh agent session, ask it to remember something memorable and do at least one tool call:

```text
Remember that my favorite editor is Helix, then run pwd and summarize what you did.
```

Then, in the next prompt, ask the agent to retrieve memory before answering:

```text
Before answering, call retrieve_memory for my editor preference and tell me what you find.
```

If everything is wired correctly, the agent should call `retrieve_memory` and answer using the stored note.

## Release Assets

Each GitHub Release currently publishes three platform-specific tarballs plus `SHA256SUMS`:

- `memory-bank-aarch64-apple-darwin.tar.gz`
- `memory-bank-x86_64-unknown-linux-gnu.tar.gz`
- `memory-bank-aarch64-unknown-linux-gnu.tar.gz`
- `SHA256SUMS`

These tarballs extract directly into the `~/.memory_bank/` app-root layout expected by `mb` and `install.sh`, including the local fallback copy of the setup model catalog under `config/setup-model-catalog.json`.

Linux note: the release pipeline currently ships native `gnu` builds for modern glibc-based Linux distributions.

Intel macOS note: release binaries are temporarily unavailable on `x86_64-apple-darwin` because the current FastEmbed and ONNX Runtime dependency chain does not publish a compatible prebuilt package for that target. Intel macOS users can still build from source.

## Features

- Cross-agent continuity. Memories captured from Claude Code, Gemini CLI, OpenCode, or OpenClaw can be recalled from the same Memory Bank namespace.
- Memory ownership. Storage, namespaces, embeddings, and internal analysis stay in your environment.
- Consistent behavior. Every supported agent recalls through the same MCP tool: `retrieve_memory`.
- Vendor hedge. Your memory corpus is portable across supported agents instead of being locked to one runtime.
- Better recall and response quality. Retrieved context helps assistants answer with stronger continuity, consistency, and personalization.
- OpenCode memory support. OpenCode can use long-term memory through the bundled plugin and the same Memory Bank server.
- OpenClaw memory support. OpenClaw can use long-term memory through the repo-owned extension for capture and the stdio MCP proxy for recall.
- Local, namespaced storage. Each namespace gets its own SQLite database so you can separate projects, teams, or experiments.
- Hook and plugin based capture. Conversation events are captured through agent hooks or repo-owned plugins/extensions and normalized by `memory-bank-hook`.
- Flexible internal providers. The server can analyze and write memories using Anthropic, Gemini, OpenAI, or Ollama.
- Flexible embedding models. FastEmbed supports built-in models and compatible Hugging Face ONNX repos through `MEMORY_BANK_FASTEMBED_MODEL`.

## How It Works

1. Your agent emits conversation events.
2. `memory-bank-hook` normalizes those events and sends them to `POST /ingest`.
3. `memory-bank-server` groups fragments into turns, analyzes them with your configured LLM, and stores memory notes in SQLite.
4. Your agent calls `retrieve_memory` from the MCP endpoint at `http://127.0.0.1:3737/mcp` whenever earlier context could improve the answer.

Important: MCP handles recall only. To actually capture memories, you also need the hook side configured for your agent.

Important: the front-facing coding agent does not need to match the server's internal LLM provider.

## Agent Vs Server Provider

These are two separate choices, and it is easy to confuse them:

- The coding agent is the tool you use directly, such as Claude Code, Gemini CLI, OpenCode, or OpenClaw.
- The server LLM provider is what `memory-bank-server` uses internally to analyze turns and write memories, such as Anthropic, Gemini, OpenAI, or Ollama.

They are independent. For example, you can use Claude Code as your coding agent while running `memory-bank-server --llm-provider gemini`, or use Gemini CLI while the server runs with `--llm-provider anthropic`.

## Agent Guides

Agent-specific documentation lives in [`/docs`](./docs). The main README covers the shared setup and configuration surface; the pages below focus on each agent's own wiring, hooks, and quirks.

All supported agents can read from the same Memory Bank namespace if you point them at the same server, which is what enables cross-agent continuity.

That agent choice is separate from the internal model/provider used by the server to create memories.

| Agent | Capture path | Recall path | Guide |
| --- | --- | --- | --- |
| Claude Code | Hooks -> `memory-bank-hook` | MCP | [docs/claude-code.md](./docs/claude-code.md) |
| Gemini CLI | Hooks -> `memory-bank-hook` | MCP | [docs/gemini-cli.md](./docs/gemini-cli.md) |
| OpenCode | Bundled plugin -> `memory-bank-hook` | MCP | [docs/opencode.md](./docs/opencode.md) |
| OpenClaw | Native extension -> `memory-bank-hook` | stdio MCP proxy -> Memory Bank MCP | [docs/openclaw.md](./docs/openclaw.md) |

If you are choosing an agent today, all four are supported. The main difference is how capture is wired:

- Claude Code uses hooks configured in Claude settings.
- Gemini CLI uses hooks configured in Gemini settings and should keep the Memory Bank hook last in each sequential hook group.
- OpenCode uses the repo-owned plugin at `.opencode/plugins/memory-bank.js`.
- OpenClaw uses the repo-owned extension at `.openclaw/extensions/memory-bank/` and should disable OpenClaw's built-in memory slot when Memory Bank is the primary memory system.

## What This Repo Builds

This workspace currently contains five Rust crates:

| Crate | Type | Purpose |
| --- | --- | --- |
| `memory-bank-cli` | Binary (`mb`) | User-facing control plane for setup, service management, namespace switching, logs, and diagnostics. |
| `memory-bank-server` | Binary | Runs the local HTTP server, hosts `/mcp` and `/ingest`, stores memory, and serves `retrieve_memory`. |
| `memory-bank-hook` | Binary | Reads hook/plugin payloads from stdin, normalizes them, and posts them to the server. |
| `memory-bank-mcp-proxy` | Binary | Exposes `retrieve_memory` as a stdio MCP server and forwards calls to the upstream Memory Bank HTTP MCP server. |
| `memory-bank-protocol` | Library | Shared typed ingest schema and shared retrieve-memory MCP contract used by the binaries. |

For most users, the only artifacts you need are:

- `mb`
- `memory-bank-server`
- `memory-bank-hook`
- `memory-bank-mcp-proxy` if you are using OpenClaw

## Build From Source

Prebuilt binaries from GitHub Releases should be the easiest install path. If you want to build locally instead, you have two good options:

- `./install.sh --from-source` to build this checkout and install it into `~/.memory_bank` using the same layout as a release install
- `cargo build --release --workspace` if you want the binaries only and plan to run them manually

### Requirements

- A recent Rust toolchain with `cargo`
- A working native build toolchain
- An API key for the LLM provider you plan to use, unless you run with Ollama
- Enough disk space for the SQLite database and the embedding model cache

Linux users may also need their distro's SQLite development package and `pkg-config` if linking SQLite fails during compilation.

### Build

```bash
cargo build --release --workspace
```

If you want the full managed install experience from a local checkout, including copying binaries and bundled assets into `~/.memory_bank`, run:

```bash
./install.sh --from-source
```

Release binaries will be written to:

- `target/release/memory-bank-server`
- `target/release/memory-bank-hook`
- `target/release/memory-bank-mcp-proxy`
- `target/release/mb`

Useful verification commands:

```bash
./target/release/mb --help
./target/release/memory-bank-server --help
./target/release/memory-bank-hook --help
./target/release/memory-bank-mcp-proxy --help
make test
```

Useful Make targets:

```bash
make help
make build
make build-release
make test
make test-ci
make test-cli-blackbox
make test-cli-real
```

Notes:

- `make test` is the default local suite and includes the `mb` black-box CLI tests.
- `make test-ci` matches the CI and release validation suite and intentionally skips the heavier `mb` black-box integration test target.
- `make test-cli-real` opt-ins to the installed-tool integration checks for Claude Code, Gemini CLI, OpenCode, and OpenClaw by setting `MEMORY_BANK_REAL_BIN_TESTS=1`.

### What To Run After Building

Start the server:

```bash
export ANTHROPIC_API_KEY=your-key
./target/release/memory-bank-server --llm-provider anthropic --encoder-provider fast-embed
```

Then follow the guide for your agent in [`/docs`](./docs) to wire MCP plus hook-based capture.

## Release Process

GitHub Actions manages CI and releases:

- CI runs on pull requests targeting `main` and uses `make fmt-check`, `make check`, and `make test-ci`
- Releases are built from semver tags like `v0.1.0`
- The release workflow re-runs `make fmt-check`, `make check`, and `make test-ci` before any release build or publish step
- The release workflow builds native tarballs for Apple Silicon macOS plus x86_64 and ARM64 Linux
- Releases are created as drafts first, assets are uploaded, and the release is published only after every asset and checksum has been attached

The heavier local-only test layers are intentionally excluded from CI and release validation:

- `mb` black-box CLI integration tests live behind `make test` or `make test-cli-blackbox`
- Real installed-tool checks live behind `make test-cli-real`

## Configuration And Environment Variables

There are four places users typically configure Memory Bank:

1. Server CLI flags
2. Hook CLI flags
3. Environment variables
4. Agent-specific config files in [`/docs`](./docs)

Before changing settings, it helps to keep the split clear:

- Agent configuration controls how Claude Code, Gemini CLI, OpenCode, or OpenClaw sends events to Memory Bank and calls the MCP tool.
- Server LLM configuration controls how Memory Bank analyzes captured turns internally.
- These settings do not need to match.

### Server CLI Flags

`memory-bank-server` accepts the following flags:

| Flag | Default | Description |
| --- | --- | --- |
| `--port` | `3737` | Local HTTP port for the server. MCP is exposed at `/mcp` and ingest is exposed at `/ingest`. |
| `--namespace` | `default` | Storage namespace. Each namespace gets its own SQLite database. |
| `--llm-provider` | `anthropic` | Memory-analysis provider. Supported values: `gemini`, `anthropic`, `open-ai`, `ollama`. |
| `--encoder-provider` | `fast-embed` | Embedding provider. Supported values: `fast-embed`, `local-api`, `remote-api`. Only `fast-embed` is implemented today. |
| `--history-window-size` | `0` | Number of prior stored turns replayed during memory analysis. `0` means unlimited. |
| `--nearest-neighbor-count` | `10` | Number of nearest-neighbor matches loaded during MCP recall and graph evolution. Must be at least `1`. |

Notes:

- The server binds to `127.0.0.1` by default.
- Namespace names are sanitized before being used on disk.
- `--llm-provider` configures Memory Bank's internal memory-analysis model, not the coding agent you use in the UI or CLI.
- `--nearest-neighbor-count` lets you tune retrieval breadth at runtime without rebuilding the server.
- `local-api` and `remote-api` are exposed in the config surface, but they are not implemented yet.

### Hook CLI Flags

`memory-bank-hook` accepts the following flags:

| Flag | Default | Description |
| --- | --- | --- |
| `--agent` | required | Source identifier. Supported today: `claude-code`, `gemini-cli`, `openclaw`, `opencode`. |
| `--event` | required | Event name for the incoming hook or plugin payload. |
| `--server-url` | `http://127.0.0.1:3737` | Base URL for the running Memory Bank server. |

### MCP Proxy CLI Flags

`memory-bank-mcp-proxy` accepts the following flags:

| Flag | Default | Description |
| --- | --- | --- |
| `--server-url` | `http://127.0.0.1:3737` | Base URL for the running Memory Bank server. The proxy forwards stdio MCP calls to the server's `/mcp` endpoint and also accepts an explicit `/mcp` URL. |

### LLM Provider Environment Variables

These variables configure the server's internal memory-analysis model. They do not configure Claude Code, Gemini CLI, OpenCode, or OpenClaw themselves.

| Provider | Required | Optional | Default model/value |
| --- | --- | --- | --- |
| Anthropic | `ANTHROPIC_API_KEY` | `MEMORY_BANK_LLM_MODEL` | `claude-sonnet-4-6` |
| Gemini | `GEMINI_API_KEY` | `MEMORY_BANK_LLM_MODEL` | `gemini-3-flash-preview` |
| OpenAI | `OPENAI_API_KEY` | `MEMORY_BANK_LLM_MODEL` | `gpt-5-mini` |
| Ollama | none | `MEMORY_BANK_OLLAMA_URL`, `MEMORY_BANK_OLLAMA_MODEL` | `http://localhost:11434`, `qwen3` |

Ollama note: on startup the server verifies that the configured URL points at the native Ollama API root and that the configured model already exists locally. `mb setup` will try to read the installed model list from that Ollama daemon and let you pick from those local models first.

### Encoder Environment Variables

| Variable | Used by | Default | Description |
| --- | --- | --- | --- |
| `MEMORY_BANK_FASTEMBED_MODEL` | `--encoder-provider fast-embed` | `jinaai/jina-embeddings-v2-base-code` | Embedding model ID used by FastEmbed. |
| `MEMORY_BANK_LOCAL_ENCODER_URL` | `--encoder-provider local-api` | none | Reserved config for a local encoder API. Not implemented yet. |
| `MEMORY_BANK_REMOTE_ENCODER_API_KEY` | `--encoder-provider remote-api` | none | Reserved config for a remote encoder API. Not implemented yet. |
| `MEMORY_BANK_REMOTE_ENCODER_URL` | `--encoder-provider remote-api` | none | Reserved config for a remote encoder API. Not implemented yet. |

When you use `--encoder-provider fast-embed`, Memory Bank resolves `MEMORY_BANK_FASTEMBED_MODEL` in two stages:

- First it tries to interpret the value as a FastEmbed-native model name.
- If that does not match a built-in FastEmbed model, it treats the value as a Hugging Face repo ID and tries to load a compatible ONNX model from that repository.

In practice, this means you can use either:

- a model name that FastEmbed already supports natively
- a compatible Hugging Face ONNX embedding repo, as long as `MEMORY_BANK_FASTEMBED_MODEL` is set to the correct repo string

For Hugging Face ONNX repos, Memory Bank currently expects the repo to expose:

- `onnx/model.onnx` or `model.onnx`
- `tokenizer.json`
- `config.json`
- `special_tokens_map.json`
- `tokenizer_config.json`

If the model string is correct and the repo has that layout, Memory Bank will download and cache it automatically under the local models directory.

### OpenCode Plugin Environment Variables

These are only relevant if you are using the bundled OpenCode plugin:

| Variable | Default | Description |
| --- | --- | --- |
| `MEMORY_BANK_HOOK_BIN` | `~/.memory_bank/bin/memory-bank-hook` | Overrides which hook binary the plugin executes. |
| `MEMORY_BANK_SERVER_URL` | `http://127.0.0.1:3737` | Overrides where the plugin sends hook fragments. |
| `MEMORY_BANK_OPENCODE_DEBUG` | unset | Enables verbose plugin diagnostics when set to `1`. |
| `MEMORY_BANK_OPENCODE_DEBUG_FILE` | unset | If set, also mirrors plugin logs to a file. |

### OpenClaw Extension Environment Variables

These are only relevant if you are using the repo-owned OpenClaw extension:

| Variable | Default | Description |
| --- | --- | --- |
| `MEMORY_BANK_HOOK_BIN` | `~/.memory_bank/bin/memory-bank-hook` | Fallback override for which hook binary the extension executes. OpenClaw plugin config is the primary config surface. |
| `MEMORY_BANK_SERVER_URL` | `http://127.0.0.1:3737` | Fallback override for where the extension sends hook fragments and where `memory-bank-mcp-proxy` forwards recall calls. |
| `MEMORY_BANK_OPENCLAW_DEBUG` | unset | Enables verbose OpenClaw extension diagnostics when set to `1`. |
| `MEMORY_BANK_OPENCLAW_DEBUG_FILE` | unset | If set, also mirrors OpenClaw extension logs to a file. |

### Logging

Both binaries respect `RUST_LOG`.

| Variable | Default | Description |
| --- | --- | --- |
| `RUST_LOG` | `info` | Controls log verbosity for `memory-bank-server` and `memory-bank-hook`. |

### Agent Config Files

| Agent | Project-scoped config | User-scoped config | Notes |
| --- | --- | --- | --- |
| Claude Code | `.claude/settings.local.json` | `~/.claude/settings.json` | MCP registration and hook configuration are separate concerns. |
| Gemini CLI | `.gemini/settings.json` | `~/.gemini/settings.json` | Keep the Memory Bank hook last in each relevant sequential hook group. |
| OpenCode | `opencode.json` plus `.opencode/plugins/memory-bank.js` | `~/.config/opencode/opencode.json` plus `~/.config/opencode/plugins/` | MCP is configured in `opencode.json`; capture is handled by the plugin. |
| OpenClaw | `.openclaw/extensions/memory-bank/` plus workspace plugin config | `~/.openclaw/openclaw.json` | MCP is configured through OpenClaw's saved MCP server definitions; capture is handled by the repo-owned extension. |

The exact setup steps and examples for those files live in [`/docs`](./docs).

## Storage Layout

Memory Bank stores data under your home directory in a top-level `.memory_bank` folder.

- App root: `{home_dir}/.memory_bank/`
- App settings: `{home_dir}/.memory_bank/settings.toml`
- Namespaces: `{home_dir}/.memory_bank/namespaces/<namespace>/`
- Database: `{home_dir}/.memory_bank/namespaces/<namespace>/memory.db`
- Model cache: `{home_dir}/.memory_bank/models/`
- Setup model catalog fallback: `{home_dir}/.memory_bank/config/setup-model-catalog.json`

Examples:

- macOS: `~/.memory_bank/`
- Linux: `~/.memory_bank/`
- Windows: `%USERPROFILE%/.memory_bank/`

This means you can isolate experiments, teams, or projects by running separate namespaces.

## Current Scope

The current implementation is intentionally focused:

- One recall tool is exposed over MCP: `retrieve_memory`
- Claude Code, Gemini CLI, OpenCode, and OpenClaw are the supported agent integrations today
- `fast-embed` is the only implemented encoder provider today
- OpenCode support currently relies on the bundled plugin in this repository
- OpenClaw support currently relies on the bundled workspace extension plus the stdio MCP proxy in this repository

## License

This project is licensed under `MIT`. See [LICENSE](./LICENSE) for the full text.

## Citation

If this project is useful in your work, please cite the original A-MEM paper:

```bibtex
@article{xu2025mem,
  title={{A-MEM}: Agentic Memory for {LLM} Agents},
  author={Xu, Wujiang and Liang, Zujie and Mei, Kai and Gao, Hang and Tan, Juntao and Zhang, Yongfeng},
  journal={arXiv preprint arXiv:2502.12110},
  year={2025}
}
```
