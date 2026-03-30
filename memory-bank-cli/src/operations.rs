use crate::AppError;
use crate::agents::{AgentKind, integration_configured};
use crate::assets::{ensure_path_entry, materialize_install_artifacts};
use crate::cli::{ConfigCommand, NamespaceCommand, ServiceCommand};
use crate::command_utils::yes_no;
use crate::config::{
    get_config_value, llm_provider_value, resolved_llm_model, resolved_ollama_url, set_config_value,
};
use crate::constants::{HEALTH_POLL_INTERVAL, HEALTH_STARTUP_TIMEOUT};
use crate::service::{
    build_server_launch_spec, collect_doctor_issues, fetch_health, install_service,
    restart_service, service_status, start_service, stop_service, tail_log_file, wait_for_health,
};
use memory_bank_app::{AppPaths, AppSettings, DEFAULT_NAMESPACE_NAME, Namespace, SecretStore};
use std::fs;
use std::os::unix::process::CommandExt;
use std::process::Command as ProcessCommand;

pub(crate) fn run_status() -> Result<(), AppError> {
    let paths = AppPaths::from_system()?;
    let settings = AppSettings::load(&paths)?;
    let service = service_status(&paths)?;
    let provider = llm_provider_value(&settings);
    let model = resolved_llm_model(&settings);

    println!("Memory Bank");
    println!("  Namespace: {}", settings.active_namespace());
    println!("  Port: {}", settings.resolved_port());
    println!("  Service installed: {}", yes_no(service.installed));
    println!("  Service active: {}", yes_no(service.active));
    println!("  Provider: {provider}");
    println!("  Model: {model}");

    if provider == "ollama" {
        println!(
            "  Ollama URL: {}",
            resolved_ollama_url(
                settings
                    .server
                    .as_ref()
                    .and_then(|server| server.ollama_url.as_deref()),
            )
        );
    }

    if let Ok(health) = fetch_health(&settings) {
        println!("  Health: {}", yes_no(health.ok));
        println!("  Health namespace: {}", health.namespace);
        println!("  Health port: {}", health.port);
        println!("  Health encoder: {}", health.encoder_provider);
        println!("  Health version: {}", health.version);
    } else {
        println!("  Health: unavailable");
    }

    for agent in AgentKind::all() {
        let configured = integration_configured(&settings, agent);
        println!("  {}: {}", agent.display_name(), yes_no(configured));
    }

    Ok(())
}

pub(crate) fn run_doctor(fix: bool) -> Result<(), AppError> {
    let paths = AppPaths::from_system()?;
    let mut settings = AppSettings::load(&paths)?;
    let mut issues = collect_doctor_issues(&paths, &settings)?;

    if fix {
        paths.ensure_base_dirs()?;
        materialize_install_artifacts(&paths)?;
        ensure_path_entry(&paths)?;
        let secrets = SecretStore::load(&paths)?;
        let service = service_status(&paths)?;
        if !service.installed {
            install_service(&paths, &settings)?;
        }
        let service = service_status(&paths)?;
        if !service.active && build_server_launch_spec(&paths, &settings, &secrets).is_ok() {
            start_service(&paths)?;
        }
        settings = AppSettings::load(&paths)?;
        issues = collect_doctor_issues(&paths, &settings)?;
    }

    if issues.is_empty() {
        println!("Memory Bank doctor found no issues.");
    } else {
        println!("Memory Bank doctor found issues:");
        for issue in &issues {
            println!("  - {issue}");
        }
    }

    if fix && issues.is_empty() {
        let health = wait_for_health(&settings, HEALTH_STARTUP_TIMEOUT, HEALTH_POLL_INTERVAL)?;
        println!(
            "Post-fix health is ok on {} for namespace `{}`.",
            memory_bank_app::default_server_url(&settings),
            health.namespace
        );
    }

    Ok(())
}

pub(crate) fn run_logs(follow: bool) -> Result<(), AppError> {
    let paths = AppPaths::from_system()?;
    tail_log_file(&paths.log_file, follow)
}

pub(crate) fn run_namespace(command: NamespaceCommand) -> Result<(), AppError> {
    let paths = AppPaths::from_system()?;
    let mut settings = AppSettings::load(&paths)?;

    match command {
        NamespaceCommand::List => {
            paths.ensure_base_dirs()?;
            let mut namespaces = list_namespaces(&paths)?;
            let active = settings.active_namespace();
            if !namespaces.contains(&active) {
                namespaces.push(active);
                namespaces.sort();
                namespaces.dedup();
            }

            for namespace in namespaces {
                let suffix = if namespace == settings.active_namespace() {
                    " (active)"
                } else {
                    ""
                };
                println!("{}{}", namespace, suffix);
            }
            Ok(())
        }
        NamespaceCommand::Create { name } => {
            let namespace = Namespace::new(name);
            paths.ensure_namespace_dir(&namespace)?;
            println!("Created namespace `{namespace}`.");
            Ok(())
        }
        NamespaceCommand::Use { name } => {
            let namespace = Namespace::new(name);
            paths.ensure_namespace_dir(&namespace)?;
            settings.active_namespace = if namespace.as_ref() == DEFAULT_NAMESPACE_NAME {
                None
            } else {
                Some(namespace.to_string())
            };
            settings.save(&paths)?;

            let status = service_status(&paths)?;
            if status.installed {
                if status.active {
                    restart_service(&paths)?;
                } else {
                    start_service(&paths)?;
                }
            }

            println!("Active namespace is now `{namespace}`.");
            Ok(())
        }
        NamespaceCommand::Current => {
            println!("{}", settings.active_namespace());
            Ok(())
        }
    }
}

pub(crate) fn run_service(command: ServiceCommand) -> Result<(), AppError> {
    let paths = AppPaths::from_system()?;
    let settings = AppSettings::load(&paths)?;

    match command {
        ServiceCommand::Install => {
            materialize_install_artifacts(&paths)?;
            install_service(&paths, &settings)
        }
        ServiceCommand::Start => {
            materialize_install_artifacts(&paths)?;
            start_service(&paths)
        }
        ServiceCommand::Stop => stop_service(&paths),
        ServiceCommand::Restart => {
            materialize_install_artifacts(&paths)?;
            restart_service(&paths)
        }
        ServiceCommand::Status => {
            let status = service_status(&paths)?;
            println!("Installed: {}", yes_no(status.installed));
            println!("Active: {}", yes_no(status.active));
            Ok(())
        }
        ServiceCommand::Logs { follow } => tail_log_file(&paths.log_file, follow),
    }
}

pub(crate) fn run_config(command: ConfigCommand) -> Result<(), AppError> {
    let paths = AppPaths::from_system()?;
    let mut settings = AppSettings::load(&paths)?;

    match command {
        ConfigCommand::Show => {
            let rendered = settings.to_toml_string()?;
            println!("{rendered}");
            Ok(())
        }
        ConfigCommand::Get { key } => {
            let value = get_config_value(&settings, &key)?;
            println!("{value}");
            Ok(())
        }
        ConfigCommand::Set { key, value } => {
            set_config_value(&mut settings, &key, &value)?;
            settings.save(&paths)?;
            println!("Updated `{key}`.");
            Ok(())
        }
    }
}

pub(crate) fn run_internal_server() -> Result<(), AppError> {
    let paths = AppPaths::from_system()?;
    let settings = AppSettings::load(&paths)?;
    let secrets = SecretStore::load(&paths)?;
    let spec = build_server_launch_spec(&paths, &settings, &secrets)?;

    let mut command = ProcessCommand::new(&spec.program);
    command.args(&spec.args);
    for key in &spec.remove_env {
        command.env_remove(key);
    }
    for (key, value) in &spec.env {
        command.env(key, value);
    }

    Err(AppError::Io(command.exec()))
}

fn list_namespaces(paths: &AppPaths) -> Result<Vec<Namespace>, AppError> {
    if !paths.namespaces_dir.exists() {
        return Ok(Vec::new());
    }

    let mut namespaces = Vec::new();
    for entry in fs::read_dir(&paths.namespaces_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            namespaces.push(Namespace::new(entry.file_name().to_string_lossy()));
        }
    }
    namespaces.sort();
    Ok(namespaces)
}
