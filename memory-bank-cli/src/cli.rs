use clap::{Parser, Subcommand};

const ROOT_AFTER_HELP: &str = "\
Quick start:
  mb setup               Run guided setup for provider, service, and agent integrations
  mb status              Check health, namespace, provider, and integration status
  mb doctor --fix        Repair common install and configuration problems

Common workflows:
  mb namespace list      See available memory namespaces
  mb service logs -f     Follow the managed service log
  mb config show         Review the saved settings file";

const SETUP_LONG_ABOUT: &str = "\
Run guided first-run setup for Memory Bank.

This interactive command helps you choose a namespace and provider, store any
required provider secret, configure the managed service, and wire supported
agents to Memory Bank.

`mb setup` requires an interactive terminal.";

const SETUP_AFTER_HELP: &str = "\
Examples:
  mb setup

Next steps:
  mb status              Confirm the service is healthy
  mb config show         Review the saved configuration";

const STATUS_LONG_ABOUT: &str = "\
Show the current Memory Bank status.

This reports the active namespace, resolved port, managed service state, saved
provider and model selection, best-effort server health, and whether supported
agent integrations appear to be configured.";

const STATUS_AFTER_HELP: &str = "\
Examples:
  mb status

Related commands:
  mb doctor              Diagnose problems when status looks wrong
  mb logs                Read the managed service log";

const DOCTOR_LONG_ABOUT: &str = "\
Diagnose common install and configuration problems.

Doctor checks CLI exposure, managed service setup, health, and required
provider configuration. Use `--fix` to attempt safe repairs when possible.";

const DOCTOR_AFTER_HELP: &str = "\
Examples:
  mb doctor
  mb doctor --fix

`--fix` may expose `mb`, install the managed service, and start it when the
current configuration is valid.";

const LOGS_LONG_ABOUT: &str = "\
Read the managed service log at `~/.memory_bank/logs/server.log`.

Use this when you want to inspect recent server activity or keep watching new
log lines as they are written.";

const LOGS_AFTER_HELP: &str = "\
Examples:
  mb logs
  mb logs --follow";

const NAMESPACE_LONG_ABOUT: &str = "\
Manage Memory Bank namespaces.

Namespaces isolate long-term memory state so different projects, teams, or
experiments can keep separate SQLite databases under the same install.";

const NAMESPACE_AFTER_HELP: &str = "\
Examples:
  mb namespace list
  mb namespace create work-project
  mb namespace use work-project
  mb namespace current

Notes:
  Namespace names are sanitized to letters, numbers, hyphens, and underscores.
  `mb namespace use` restarts or starts the managed service if it is installed.";

const SERVICE_LONG_ABOUT: &str = "\
Manage the user-scoped Memory Bank background service.

These commands control the managed server process that runs in the background:
launchd on macOS and systemd --user on Linux.";

const SERVICE_AFTER_HELP: &str = "\
Examples:
  mb service install
  mb service start
  mb service status
  mb service logs --follow

Notes:
  `mb service start` installs the service definition first if it is missing.";

const CONFIG_LONG_ABOUT: &str = "\
Inspect and edit saved Memory Bank settings.

Configuration is stored in `~/.memory_bank/settings.toml`. Use `show` to print
the saved file, `get` to read one value, and `set` to update a single key.";

const CONFIG_AFTER_HELP: &str = "\
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
  server.local_encoder_url
  server.remote_encoder_url
  integrations.claude_code.configured
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
  mb config set server.llm_provider gemini";

const CONFIG_SET_AFTER_HELP: &str = "\
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
  server.local_encoder_url
  server.remote_encoder_url
  integrations.claude_code.configured
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

Use an empty string to clear optional string overrides such as
`server.llm_model` or `server.ollama_url`.";

#[derive(Debug, Parser)]
#[command(
    author,
    version,
    about = "Memory Bank control plane",
    after_help = ROOT_AFTER_HELP
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(
        about = "Run guided setup for provider, service, and agent integrations",
        long_about = SETUP_LONG_ABOUT,
        after_help = SETUP_AFTER_HELP
    )]
    Setup,
    #[command(
        about = "Check current Memory Bank health and configuration",
        long_about = STATUS_LONG_ABOUT,
        after_help = STATUS_AFTER_HELP
    )]
    Status,
    #[command(
        about = "Diagnose common install and configuration problems",
        long_about = DOCTOR_LONG_ABOUT,
        after_help = DOCTOR_AFTER_HELP
    )]
    Doctor {
        #[arg(
            long,
            help = "Attempt safe repairs such as exposing `mb`, installing the service, and starting it when configuration is valid"
        )]
        fix: bool,
    },
    #[command(
        about = "Read the managed service log",
        long_about = LOGS_LONG_ABOUT,
        after_help = LOGS_AFTER_HELP
    )]
    Logs {
        #[arg(
            short = 'f',
            long,
            help = "Keep streaming `~/.memory_bank/logs/server.log` as new lines arrive"
        )]
        follow: bool,
    },
    #[command(
        about = "Manage memory namespaces",
        long_about = NAMESPACE_LONG_ABOUT,
        after_help = NAMESPACE_AFTER_HELP
    )]
    Namespace {
        #[command(subcommand)]
        command: NamespaceCommand,
    },
    #[command(
        about = "Manage the user-scoped background service",
        long_about = SERVICE_LONG_ABOUT,
        after_help = SERVICE_AFTER_HELP
    )]
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    #[command(
        about = "Inspect and edit saved configuration",
        long_about = CONFIG_LONG_ABOUT,
        after_help = CONFIG_AFTER_HELP
    )]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    #[command(hide = true)]
    Internal {
        #[command(subcommand)]
        command: InternalCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum NamespaceCommand {
    #[command(about = "List known namespaces and mark the active one")]
    List,
    #[command(
        about = "Create a namespace directory without switching to it",
        long_about = "Create a namespace directory without switching to it.\n\nNamespace names are sanitized to letters, numbers, hyphens, and underscores."
    )]
    Create {
        #[arg(help = "Namespace name to create. Invalid characters are replaced with underscores")]
        name: String,
    },
    #[command(
        about = "Switch the active namespace",
        long_about = "Switch the active namespace.\n\nIf the managed service is installed, `mb namespace use` restarts it when already running or starts it when stopped so the new namespace takes effect immediately."
    )]
    Use {
        #[arg(
            help = "Namespace name to activate. Invalid characters are replaced with underscores"
        )]
        name: String,
    },
    #[command(about = "Print the active namespace")]
    Current,
}

#[derive(Debug, Subcommand)]
pub enum ServiceCommand {
    #[command(
        about = "Install the managed service definition",
        long_about = "Install the user-scoped Memory Bank service definition for the current platform.\n\nThis writes a launchd agent on macOS or a systemd --user unit on Linux."
    )]
    Install,
    #[command(
        about = "Start the managed Memory Bank service",
        long_about = "Start the managed Memory Bank service.\n\nIf the service definition is missing, `mb service start` installs it first."
    )]
    Start,
    #[command(about = "Stop the managed Memory Bank service")]
    Stop,
    #[command(about = "Restart the managed Memory Bank service")]
    Restart,
    #[command(about = "Show whether the managed service is installed and running")]
    Status,
    #[command(
        about = "Read the managed service log",
        long_about = "Read the managed service log at `~/.memory_bank/logs/server.log`."
    )]
    Logs {
        #[arg(
            short = 'f',
            long,
            help = "Keep streaming `~/.memory_bank/logs/server.log` as new lines arrive"
        )]
        follow: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    #[command(about = "Print the saved settings file")]
    Show,
    #[command(
        about = "Read a single saved config value",
        long_about = "Read a single saved config value from `~/.memory_bank/settings.toml`.\n\nRun `mb config --help` to see the supported key catalog."
    )]
    Get {
        #[arg(help = "Config key to read. Run `mb config --help` to see supported keys")]
        key: String,
    },
    #[command(
        about = "Update a single saved config value",
        long_about = "Update a single saved config value in `~/.memory_bank/settings.toml`.",
        after_help = CONFIG_SET_AFTER_HELP
    )]
    Set {
        #[arg(help = "Config key to update. Run `mb config --help` to see supported keys")]
        key: String,
        #[arg(
            help = "New value for the selected key. Use an empty string to clear optional string overrides"
        )]
        value: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum InternalCommand {
    RunServer,
    BootstrapInstall,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_service_logs_follow_flag() {
        let cli = Cli::try_parse_from(["mb", "service", "logs", "--follow"]).expect("parse cli");

        assert!(matches!(
            cli.command,
            Command::Service {
                command: ServiceCommand::Logs { follow: true }
            }
        ));
    }

    #[test]
    fn parses_config_set_command() {
        let cli = Cli::try_parse_from(["mb", "config", "set", "service.port", "4545"])
            .expect("parse cli");

        assert!(matches!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Set { key, value }
            } if key == "service.port" && value == "4545"
        ));
    }

    #[test]
    fn parses_hidden_internal_command() {
        let cli = Cli::try_parse_from(["mb", "internal", "run-server"]).expect("parse cli");

        assert!(matches!(
            cli.command,
            Command::Internal {
                command: InternalCommand::RunServer
            }
        ));
    }

    #[test]
    fn parses_hidden_bootstrap_install_command() {
        let cli = Cli::try_parse_from(["mb", "internal", "bootstrap-install"]).expect("parse cli");

        assert!(matches!(
            cli.command,
            Command::Internal {
                command: InternalCommand::BootstrapInstall
            }
        ));
    }
}
