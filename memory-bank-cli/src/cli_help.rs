pub(crate) const ROOT_AFTER_HELP: &str = "\
Quick start:
  mb setup               Run guided setup for provider, service, and agent integrations
  mb status              Check health, namespace, provider, and integration status
  mb doctor --fix        Repair common install and configuration problems

Common workflows:
  mb namespace list      See available memory namespaces
  mb service logs -f     Follow the managed service log
  mb config show         Review the saved settings file";

pub(crate) const SETUP_LONG_ABOUT: &str = "\
Run guided first-run setup for Memory Bank.

This interactive command helps you choose a namespace and provider, store any
required provider secret, configure the managed service, and wire supported
agents to Memory Bank.

`mb setup` requires an interactive terminal.";

pub(crate) const SETUP_AFTER_HELP: &str = "\
Examples:
  mb setup

Next steps:
  mb status              Confirm the service is healthy
  mb config show         Review the saved configuration";

pub(crate) const STATUS_LONG_ABOUT: &str = "\
Show the current Memory Bank status.

This reports the active namespace, resolved port, managed service state, saved
provider and model selection, best-effort server health, and whether supported
agent integrations appear to be configured.";

pub(crate) const STATUS_AFTER_HELP: &str = "\
Examples:
  mb status

Related commands:
  mb doctor              Diagnose problems when status looks wrong
  mb logs                Read the managed service log";

pub(crate) const DOCTOR_LONG_ABOUT: &str = "\
Diagnose common install and configuration problems.

Doctor checks CLI exposure, managed service setup, health, and required
provider configuration. Use `--fix` to attempt safe repairs when possible.";

pub(crate) const DOCTOR_AFTER_HELP: &str = "\
Examples:
  mb doctor
  mb doctor --fix

`--fix` may expose `mb`, install the managed service, and start it when the
current configuration is valid.";

pub(crate) const LOGS_LONG_ABOUT: &str = "\
Read the managed service log at `~/.memory_bank/logs/server.log`.

Use this when you want to inspect recent server activity or keep watching new
log lines as they are written.";

pub(crate) const LOGS_AFTER_HELP: &str = "\
Examples:
  mb logs
  mb logs --follow";

pub(crate) const NAMESPACE_LONG_ABOUT: &str = "\
Manage Memory Bank namespaces.

Namespaces isolate long-term memory state so different projects, teams, or
experiments can keep separate SQLite databases under the same install.";

pub(crate) const NAMESPACE_AFTER_HELP: &str = "\
Examples:
  mb namespace list
  mb namespace create work-project
  mb namespace use work-project
  mb namespace current

Notes:
  Namespace names are sanitized to letters, numbers, hyphens, and underscores.
  `mb namespace use` restarts or starts the managed service if it is installed.";

pub(crate) const SERVICE_LONG_ABOUT: &str = "\
Manage the user-scoped Memory Bank background service.

These commands control the managed server process that runs in the background:
launchd on macOS and systemd --user on Linux.";

pub(crate) const SERVICE_AFTER_HELP: &str = "\
Examples:
  mb service install
  mb service start
  mb service status
  mb service logs --follow

Notes:
  `mb service start` installs the service definition first if it is missing.";

pub(crate) const CONFIG_LONG_ABOUT: &str = "\
Inspect and edit saved Memory Bank settings.

Configuration is stored in `~/.memory_bank/settings.toml`. Use `show` to print
the saved file, `get` to read one value, and `set` to update a single key.";

pub(crate) const CONFIG_AFTER_HELP: &str = "\
Supported keys:
  active_namespace
  service.port
  service.autostart
  server.llm_provider
  server.llm_model
  server.ollama_url
  server.encoder_provider
  server.fastembed_model
  server.history_window_size
  server.nearest_neighbor_count
  server.max_processing_attempts
  server.local_encoder_url
  server.remote_encoder_url
  integrations.claude_code.configured
  integrations.codex.configured
  integrations.gemini_cli.configured
  integrations.opencode.configured
  integrations.openclaw.configured

Important values and defaults:
  Default namespace: default
  Default service.port: 3737
  server.llm_provider: anthropic | gemini | open-ai | ollama
  server.encoder_provider: fast-embed | local-api | remote-api

Examples:
  mb config show
  mb config get server.llm_provider
  mb config set service.port 4545
  mb config set server.llm_provider gemini
  mb config set --yes server.fastembed_model custom/embed-model";

pub(crate) const CONFIG_SET_AFTER_HELP: &str = "\
Supported keys:
  active_namespace
  service.port
  service.autostart
  server.llm_provider
  server.llm_model
  server.ollama_url
  server.encoder_provider
  server.fastembed_model
  server.history_window_size
  server.nearest_neighbor_count
  server.max_processing_attempts
  server.local_encoder_url
  server.remote_encoder_url
  integrations.claude_code.configured
  integrations.codex.configured
  integrations.gemini_cli.configured
  integrations.opencode.configured
  integrations.openclaw.configured

Important values:
  server.llm_provider: anthropic | gemini | open-ai | ollama
  server.encoder_provider: fast-embed | local-api | remote-api

Examples:
  mb config set service.port 4545
  mb config set server.llm_provider gemini
  mb config set active_namespace work-project
  mb config set server.llm_model \"\"
  mb config set --yes server.fastembed_model custom/embed-model

Use an empty string to clear optional string overrides such as
`server.llm_model` or `server.ollama_url`.

Changing `server.fastembed_model` requires confirmation because the next server
start will rebuild the vector index and re-encode existing memories for that
namespace. Use `--yes` in automation.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_help_catalog_mentions_supported_keys_and_examples() {
        assert!(CONFIG_AFTER_HELP.contains("server.llm_provider"));
        assert!(CONFIG_AFTER_HELP.contains("server.fastembed_model"));
        assert!(CONFIG_AFTER_HELP.contains("integrations.openclaw.configured"));
        assert!(CONFIG_AFTER_HELP.contains("integrations.codex.configured"));
        assert!(CONFIG_SET_AFTER_HELP.contains("mb config set service.port 4545"));
        assert!(CONFIG_SET_AFTER_HELP.contains("Use `--yes` in automation."));
    }
}
