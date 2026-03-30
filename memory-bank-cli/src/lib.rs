mod agents;
mod assets;
mod cli;
mod command_utils;
mod config;
mod constants;
mod error;
mod json_config;
mod models;
mod operations;
mod service;
mod setup;

use clap::Parser;
use cli::{Cli, Command, InternalCommand};

pub use error::AppError;

pub fn run() -> Result<(), AppError> {
    let cli = Cli::parse();
    match cli.command {
        Command::Setup => setup::run_setup(),
        Command::Status => operations::run_status(),
        Command::Doctor { fix } => operations::run_doctor(fix),
        Command::Logs { follow } => operations::run_logs(follow),
        Command::Namespace { command } => operations::run_namespace(command),
        Command::Service { command } => operations::run_service(command),
        Command::Config { command } => operations::run_config(command),
        Command::Internal { command } => match command {
            InternalCommand::RunServer => operations::run_internal_server(),
        },
    }
}
