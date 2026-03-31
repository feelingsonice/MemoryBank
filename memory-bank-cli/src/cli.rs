use crate::cli_help::{
    CONFIG_AFTER_HELP, CONFIG_LONG_ABOUT, CONFIG_SET_AFTER_HELP, DOCTOR_AFTER_HELP,
    DOCTOR_LONG_ABOUT, LOGS_AFTER_HELP, LOGS_LONG_ABOUT, NAMESPACE_AFTER_HELP,
    NAMESPACE_LONG_ABOUT, ROOT_AFTER_HELP, SERVICE_AFTER_HELP, SERVICE_LONG_ABOUT,
    SETUP_AFTER_HELP, SETUP_LONG_ABOUT, STATUS_AFTER_HELP, STATUS_LONG_ABOUT,
};
use clap::{Parser, Subcommand};

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
