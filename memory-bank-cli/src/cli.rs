use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(author, version, about = "Memory Bank control plane")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Setup,
    Status,
    Doctor {
        #[arg(long)]
        fix: bool,
    },
    Logs {
        #[arg(short = 'f', long)]
        follow: bool,
    },
    Namespace {
        #[command(subcommand)]
        command: NamespaceCommand,
    },
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
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
    List,
    Create { name: String },
    Use { name: String },
    Current,
}

#[derive(Debug, Subcommand)]
pub enum ServiceCommand {
    Install,
    Start,
    Stop,
    Restart,
    Status,
    Logs {
        #[arg(short = 'f', long)]
        follow: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    Show,
    Get { key: String },
    Set { key: String, value: String },
}

#[derive(Debug, Subcommand)]
pub enum InternalCommand {
    RunServer,
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
}
