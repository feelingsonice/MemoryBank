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
