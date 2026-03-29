mod cli;

use chrono::Local;
use clap::Parser;
use cli::{Cli, Command, ConfigCommand, InternalCommand, NamespaceCommand, ServiceCommand};
use dialoguer::{Confirm, Input, MultiSelect, Password, Select};
use memory_bank_app::{
    AppConfigError, AppPaths, AppSettings, DEFAULT_ANTHROPIC_MODEL, DEFAULT_FASTEMBED_MODEL,
    DEFAULT_GEMINI_MODEL, DEFAULT_NAMESPACE_NAME, DEFAULT_OLLAMA_MODEL, DEFAULT_OPENAI_MODEL,
    DEFAULT_PORT, IntegrationState, IntegrationsSettings, Namespace, SETTINGS_SCHEMA_VERSION,
    SecretStore, default_server_url, env_key_for_provider, write_json_file,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use thiserror::Error;

const MB_BINARY_NAME: &str = "mb";
const SERVER_BINARY_NAME: &str = "memory-bank-server";
const HOOK_BINARY_NAME: &str = "memory-bank-hook";
const MCP_PROXY_BINARY_NAME: &str = "memory-bank-mcp-proxy";
const LAUNCHD_LABEL: &str = "com.memory-bank.mb";
const SYSTEMD_UNIT_NAME: &str = "memory-bank.service";

pub fn run() -> Result<(), AppError> {
    let cli = Cli::parse();
    match cli.command {
        Command::Setup => run_setup(),
        Command::Status => run_status(),
        Command::Doctor { fix } => run_doctor(fix),
        Command::Logs { follow } => run_logs(follow),
        Command::Namespace { command } => run_namespace(command),
        Command::Service { command } => run_service(command),
        Command::Config { command } => run_config(command),
        Command::Internal { command } => match command {
            InternalCommand::RunServer => run_internal_server(),
        },
    }
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    AppConfig(#[from] AppConfigError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("prompt failed: {0}")]
    Prompt(#[from] dialoguer::Error),
    #[error("unsupported platform '{0}'")]
    UnsupportedPlatform(String),
    #[error("command `{0}` failed: {1}")]
    CommandFailed(String, String),
    #[error("required binary `{0}` was not found")]
    MissingBinary(String),
    #[error("missing required provider secret `{0}` in ~/.memory_bank/secrets.env")]
    MissingProviderSecret(&'static str),
    #[error("health check failed: {0}")]
    Health(String),
    #[error("invalid config key `{0}`")]
    InvalidConfigKey(String),
    #[error("invalid config value for `{0}`: {1}")]
    InvalidConfigValue(String, String),
    #[error("{0}")]
    Message(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentKind {
    ClaudeCode,
    GeminiCli,
    OpenCode,
    OpenClaw,
}

impl AgentKind {
    fn all() -> [Self; 4] {
        [
            Self::ClaudeCode,
            Self::GeminiCli,
            Self::OpenCode,
            Self::OpenClaw,
        ]
    }

    fn command_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::GeminiCli => "gemini",
            Self::OpenCode => "opencode",
            Self::OpenClaw => "openclaw",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::GeminiCli => "Gemini CLI",
            Self::OpenCode => "OpenCode",
            Self::OpenClaw => "OpenClaw",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManagedPlatform {
    MacOs,
    Linux,
}

impl ManagedPlatform {
    fn detect() -> Result<Self, AppError> {
        match std::env::consts::OS {
            "macos" => Ok(Self::MacOs),
            "linux" => Ok(Self::Linux),
            other => Err(AppError::UnsupportedPlatform(other.to_string())),
        }
    }
}

#[derive(Debug, Deserialize)]
struct HealthCheck {
    ok: bool,
    namespace: String,
    port: u16,
    llm_provider: String,
    encoder_provider: String,
    version: String,
}

#[derive(Debug)]
struct ServiceStatus {
    installed: bool,
    active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServerLaunchSpec {
    program: PathBuf,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    remove_env: Vec<&'static str>,
}

fn run_setup() -> Result<(), AppError> {
    let paths = AppPaths::from_system()?;
    paths.ensure_base_dirs()?;
    materialize_install_artifacts(&paths)?;
    ensure_path_entry(&paths)?;

    let mut settings = AppSettings::load(&paths)?;
    let mut secrets = SecretStore::load(&paths)?;
    let detected_agents = detect_installed_agents();

    let namespace = prompt_namespace(settings.active_namespace())?;
    let provider = prompt_provider(
        settings
            .server
            .as_ref()
            .and_then(|server| server.llm_provider.clone())
            .as_deref(),
    )?;
    let model = prompt_model(
        &provider,
        settings
            .server
            .as_ref()
            .and_then(|server| server.llm_model.clone())
            .as_deref(),
    )?;
    let autostart = prompt_autostart(
        settings
            .service
            .as_ref()
            .and_then(|service| service.autostart),
    )?;
    let selected_agents = prompt_agents(&detected_agents)?;

    configure_provider_secret(&provider, &mut secrets)?;
    update_settings_for_setup(
        &mut settings,
        namespace.clone(),
        &provider,
        &model,
        autostart,
        &selected_agents,
    );
    settings.save(&paths)?;
    secrets.save(&paths)?;

    configure_selected_agents(&paths, &settings, &selected_agents)?;
    install_service(&paths, &settings)?;
    start_service(&paths)?;

    let health = fetch_health(&settings)?;
    println!(
        "Memory Bank is ready on {} using namespace `{}` and provider `{}`.",
        default_server_url(&settings),
        health.namespace,
        health.llm_provider
    );
    Ok(())
}

fn run_status() -> Result<(), AppError> {
    let paths = AppPaths::from_system()?;
    let settings = AppSettings::load(&paths)?;
    let service = service_status(&paths)?;

    println!("Memory Bank");
    println!("  Namespace: {}", settings.active_namespace());
    println!("  Port: {}", settings.resolved_port());
    println!("  Service installed: {}", yes_no(service.installed));
    println!("  Service active: {}", yes_no(service.active));

    if let Some(server) = settings.server.as_ref() {
        if let Some(provider) = server.llm_provider.as_deref() {
            println!("  Provider: {provider}");
        }
        if let Some(model) = server.llm_model.as_deref() {
            println!("  Model: {model}");
        }
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

fn run_doctor(fix: bool) -> Result<(), AppError> {
    let paths = AppPaths::from_system()?;
    let mut settings = AppSettings::load(&paths)?;
    let mut issues = Vec::new();

    if !paths.settings_file.exists() {
        issues.push("settings.json is missing".to_string());
    }
    if !paths.binary_path(MB_BINARY_NAME).exists() {
        issues.push("mb is not installed under ~/.memory_bank/bin".to_string());
    }
    for binary in [SERVER_BINARY_NAME, HOOK_BINARY_NAME, MCP_PROXY_BINARY_NAME] {
        if !paths.binary_path(binary).exists() {
            issues.push(format!("{binary} is missing from ~/.memory_bank/bin"));
        }
    }

    if let Some(provider) = settings
        .server
        .as_ref()
        .and_then(|server| server.llm_provider.as_deref())
        && let Some(env_key) = env_key_for_provider(provider)
    {
        let secrets = SecretStore::load(&paths)?;
        if secrets.get(env_key).is_none() {
            issues.push(format!("missing {env_key} in ~/.memory_bank/secrets.env"));
        }
    }

    let service = service_status(&paths)?;
    if !service.installed {
        issues.push("managed service is not installed".to_string());
    } else if !service.active {
        issues.push("managed service is not active".to_string());
    }

    if fetch_health(&settings).is_err() {
        issues.push("health check to /healthz failed".to_string());
    }

    if fix {
        paths.ensure_base_dirs()?;
        materialize_install_artifacts(&paths)?;
        ensure_path_entry(&paths)?;
        if !service.installed {
            install_service(&paths, &settings)?;
        }
        if service.installed && !service.active {
            start_service(&paths)?;
        }
        settings = AppSettings::load(&paths)?;
        issues.retain(|issue| !issue.contains("managed service is not installed"));
    }

    if issues.is_empty() {
        println!("Memory Bank doctor found no issues.");
    } else {
        println!("Memory Bank doctor found issues:");
        for issue in &issues {
            println!("  - {issue}");
        }
    }

    if fix {
        let health = fetch_health(&settings)?;
        println!(
            "Post-fix health is ok on {} for namespace `{}`.",
            default_server_url(&settings),
            health.namespace
        );
    }

    Ok(())
}

fn run_logs(follow: bool) -> Result<(), AppError> {
    let paths = AppPaths::from_system()?;
    tail_log_file(&paths.log_file, follow)
}

fn run_namespace(command: NamespaceCommand) -> Result<(), AppError> {
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
                restart_service(&paths)?;
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

fn run_service(command: ServiceCommand) -> Result<(), AppError> {
    let paths = AppPaths::from_system()?;
    let settings = AppSettings::load(&paths)?;

    match command {
        ServiceCommand::Install => install_service(&paths, &settings),
        ServiceCommand::Start => start_service(&paths),
        ServiceCommand::Stop => stop_service(&paths),
        ServiceCommand::Restart => restart_service(&paths),
        ServiceCommand::Status => {
            let status = service_status(&paths)?;
            println!("Installed: {}", yes_no(status.installed));
            println!("Active: {}", yes_no(status.active));
            Ok(())
        }
        ServiceCommand::Logs { follow } => tail_log_file(&paths.log_file, follow),
    }
}

fn run_config(command: ConfigCommand) -> Result<(), AppError> {
    let paths = AppPaths::from_system()?;
    let mut settings = AppSettings::load(&paths)?;

    match command {
        ConfigCommand::Show => {
            let rendered = serde_json::to_string_pretty(&settings)?;
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

fn run_internal_server() -> Result<(), AppError> {
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

fn prompt_namespace(current: Namespace) -> Result<Namespace, AppError> {
    let input: String = Input::new()
        .with_prompt("Active namespace")
        .default(current.to_string())
        .interact_text()?;
    Ok(Namespace::new(input))
}

fn prompt_provider(current: Option<&str>) -> Result<String, AppError> {
    let choices = ["anthropic", "gemini", "open-ai", "ollama"];
    let default_index = current
        .and_then(|value| choices.iter().position(|choice| choice == &value))
        .unwrap_or(0);
    let selection = Select::new()
        .with_prompt("LLM provider")
        .items(&choices)
        .default(default_index)
        .interact()?;
    Ok(choices[selection].to_string())
}

fn prompt_model(provider: &str, current: Option<&str>) -> Result<String, AppError> {
    let default_model = current
        .map(str::to_owned)
        .unwrap_or_else(|| default_model_for_provider(provider).to_string());
    let input: String = Input::new()
        .with_prompt(format!("Model for {provider}"))
        .default(default_model)
        .interact_text()?;
    Ok(input)
}

fn prompt_autostart(current: Option<bool>) -> Result<bool, AppError> {
    Confirm::new()
        .with_prompt("Start Memory Bank automatically on login")
        .default(current.unwrap_or(true))
        .interact()
        .map_err(AppError::from)
}

fn prompt_agents(detected: &[AgentKind]) -> Result<Vec<AgentKind>, AppError> {
    if detected.is_empty() {
        return Ok(Vec::new());
    }

    let labels: Vec<&str> = detected.iter().map(|agent| agent.display_name()).collect();
    let defaults = vec![true; labels.len()];
    let selected = MultiSelect::new()
        .with_prompt("Configure these detected agents")
        .items(&labels)
        .defaults(&defaults)
        .interact()?;

    Ok(selected.into_iter().map(|index| detected[index]).collect())
}

fn configure_provider_secret(provider: &str, secrets: &mut SecretStore) -> Result<(), AppError> {
    let Some(secret_key) = env_key_for_provider(provider) else {
        return Ok(());
    };

    let env_value = std::env::var(secret_key).ok();
    let stored_value = secrets.get(secret_key).map(str::to_owned);

    match (stored_value.as_deref(), env_value.as_deref()) {
        (None, Some(env_value)) => {
            let import = Confirm::new()
                .with_prompt(format!(
                    "Use the existing {secret_key} from your current environment for Memory Bank?"
                ))
                .default(true)
                .interact()?;
            if import {
                secrets.set(secret_key, env_value);
                return Ok(());
            }
        }
        (Some(stored_value), Some(env_value)) if stored_value != env_value => {
            let replace = Confirm::new()
                .with_prompt(format!(
                    "A different {secret_key} is already stored. Replace it with the current environment value?"
                ))
                .default(false)
                .interact()?;
            if replace {
                secrets.set(secret_key, env_value);
                return Ok(());
            }
        }
        (Some(_), _) => return Ok(()),
        (None, None) => {}
    }

    let entered = Password::new()
        .with_prompt(format!("Enter {secret_key} for Memory Bank"))
        .allow_empty_password(false)
        .interact()?;
    if entered.trim().is_empty() {
        return Err(AppError::MissingProviderSecret(secret_key));
    }
    secrets.set(secret_key, entered);
    Ok(())
}

fn update_settings_for_setup(
    settings: &mut AppSettings,
    namespace: Namespace,
    provider: &str,
    model: &str,
    autostart: bool,
    selected_agents: &[AgentKind],
) {
    settings.schema_version = SETTINGS_SCHEMA_VERSION;
    settings.active_namespace = if namespace.as_ref() == DEFAULT_NAMESPACE_NAME {
        None
    } else {
        Some(namespace.to_string())
    };

    let mut service = settings.service.clone().unwrap_or_default();
    service.autostart = autostart.then_some(true);
    if service.is_empty() {
        settings.service = None;
    } else {
        settings.service = Some(service);
    }

    let mut server = settings.server.clone().unwrap_or_default();
    server.llm_provider = if provider == "anthropic" {
        None
    } else {
        Some(provider.to_string())
    };
    let default_model = default_model_for_provider(provider);
    server.llm_model = if model == default_model {
        None
    } else {
        Some(model.to_string())
    };
    if server.is_empty() {
        settings.server = None;
    } else {
        settings.server = Some(server);
    }

    settings.integrations = Some(IntegrationsSettings {
        claude_code: Some(IntegrationState {
            configured: selected_agents.contains(&AgentKind::ClaudeCode),
        }),
        gemini_cli: Some(IntegrationState {
            configured: selected_agents.contains(&AgentKind::GeminiCli),
        }),
        opencode: Some(IntegrationState {
            configured: selected_agents.contains(&AgentKind::OpenCode),
        }),
        openclaw: Some(IntegrationState {
            configured: selected_agents.contains(&AgentKind::OpenClaw),
        }),
    });
}

fn detect_installed_agents() -> Vec<AgentKind> {
    AgentKind::all()
        .into_iter()
        .filter(|agent| find_on_path(agent.command_name()).is_some())
        .collect()
}

fn find_on_path(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for entry in std::env::split_paths(&path_var) {
        let candidate = entry.join(binary);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn materialize_install_artifacts(paths: &AppPaths) -> Result<(), AppError> {
    paths.ensure_base_dirs()?;
    let current_exe = std::env::current_exe()?;
    copy_if_needed(&current_exe, &paths.binary_path(MB_BINARY_NAME))?;

    let executable_dir = current_exe.parent().ok_or_else(|| {
        AppError::Message("failed to resolve current executable directory".to_string())
    })?;
    for binary in [SERVER_BINARY_NAME, HOOK_BINARY_NAME, MCP_PROXY_BINARY_NAME] {
        let source = executable_dir.join(binary);
        let target = paths.binary_path(binary);
        if source.exists() {
            copy_if_needed(&source, &target)?;
        } else if !target.exists() {
            return Err(AppError::MissingBinary(binary.to_string()));
        }
    }

    install_assets(paths)?;
    Ok(())
}

fn install_assets(paths: &AppPaths) -> Result<(), AppError> {
    let opencode_target = paths
        .integrations_dir
        .join("opencode")
        .join("memory-bank.js");
    let openclaw_target = paths.integrations_dir.join("openclaw").join("memory-bank");

    if opencode_target.exists() && openclaw_target.exists() {
        return Ok(());
    }

    let repo_root = find_repo_root().ok_or_else(|| {
        AppError::Message(
            "failed to locate repo assets for OpenCode/OpenClaw integration install".to_string(),
        )
    })?;
    let opencode_source = repo_root.join(".opencode/plugins/memory-bank.js");
    let openclaw_source = repo_root.join(".openclaw/extensions/memory-bank");

    if !opencode_source.exists() || !openclaw_source.exists() {
        return Err(AppError::Message(
            "repo asset sources for OpenCode/OpenClaw are missing".to_string(),
        ));
    }

    copy_if_needed(&opencode_source, &opencode_target)?;
    copy_dir_recursive(&openclaw_source, &openclaw_target)?;
    Ok(())
}

fn find_repo_root() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(current_dir) = std::env::current_dir() {
        candidates.push(current_dir);
    }

    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf);
    if let Some(root) = workspace_root {
        candidates.push(root);
    }

    for candidate in candidates {
        let mut current = Some(candidate.as_path());
        while let Some(path) = current {
            if path.join(".opencode/plugins/memory-bank.js").exists()
                && path.join(".openclaw/extensions/memory-bank").exists()
            {
                return Some(path.to_path_buf());
            }
            current = path.parent();
        }
    }

    None
}

fn copy_if_needed(source: &Path, target: &Path) -> Result<(), AppError> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }

    if source == target {
        return Ok(());
    }

    fs::copy(source, target)?;
    let permissions = fs::Permissions::from_mode(0o755);
    fs::set_permissions(target, permissions)?;

    Ok(())
}

fn copy_dir_recursive(source: &Path, target: &Path) -> Result<(), AppError> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir_recursive(&source_path, &target_path)?;
        } else {
            copy_if_needed(&source_path, &target_path)?;
        }
    }
    Ok(())
}

fn ensure_path_entry(paths: &AppPaths) -> Result<(), AppError> {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let target_rc = if shell.ends_with("zsh") {
        paths.home_dir.join(".zshrc")
    } else if shell.ends_with("bash") {
        paths.home_dir.join(".bashrc")
    } else {
        paths.home_dir.join(".profile")
    };
    let export_line = r#"export PATH="$HOME/.memory_bank/bin:$PATH""#;
    let existing = fs::read_to_string(&target_rc).unwrap_or_default();
    if existing.contains(".memory_bank/bin") {
        return Ok(());
    }

    let mut updated = existing;
    if !updated.ends_with('\n') && !updated.is_empty() {
        updated.push('\n');
    }
    updated.push_str("# Memory Bank\n");
    updated.push_str(export_line);
    updated.push('\n');
    fs::write(target_rc, updated)?;
    Ok(())
}

fn configure_selected_agents(
    paths: &AppPaths,
    settings: &AppSettings,
    selected_agents: &[AgentKind],
) -> Result<(), AppError> {
    let server_url = default_server_url(settings);
    for agent in selected_agents {
        match agent {
            AgentKind::ClaudeCode => configure_claude(paths, &server_url)?,
            AgentKind::GeminiCli => configure_gemini(paths, &server_url)?,
            AgentKind::OpenCode => configure_opencode(paths, &server_url)?,
            AgentKind::OpenClaw => configure_openclaw(paths, &server_url)?,
        }
    }
    Ok(())
}

fn configure_claude(paths: &AppPaths, server_url: &str) -> Result<(), AppError> {
    run_command(
        "claude",
        &[
            "mcp",
            "add",
            "--transport",
            "http",
            "--scope",
            "user",
            "memory-bank",
            &format!("{server_url}/mcp"),
        ],
    )?;

    let settings_path = paths.home_dir.join(".claude/settings.json");
    let mut root = load_json_config(&settings_path)?;
    let events = ["UserPromptSubmit", "PreToolUse", "PostToolUse", "Stop"];
    for event in events {
        let command = format!(
            "{} --agent claude-code --event {} --server-url {}",
            paths.binary_path(HOOK_BINARY_NAME).display(),
            event,
            server_url
        );
        upsert_claude_hook(&mut root, event, &command)?;
    }
    write_json_config_with_backups(paths, &settings_path, &root)
}

fn configure_gemini(paths: &AppPaths, server_url: &str) -> Result<(), AppError> {
    run_command(
        "gemini",
        &[
            "mcp",
            "add",
            "--scope",
            "user",
            "--transport",
            "http",
            "memory-bank",
            &format!("{server_url}/mcp"),
        ],
    )?;

    let settings_path = paths.home_dir.join(".gemini/settings.json");
    let mut root = load_json_config(&settings_path)?;
    ensure_object(&mut root);
    object_mut(&mut root)?
        .entry("mcpServers".to_string())
        .or_insert_with(|| json!({}));
    object_mut(
        object_mut(&mut root)?
            .get_mut("mcpServers")
            .expect("mcpServers"),
    )?
    .insert(
        "memory-bank".to_string(),
        json!({ "httpUrl": format!("{server_url}/mcp") }),
    );

    let hook_events = [
        ("BeforeAgent", "*"),
        ("BeforeTool", ".*"),
        ("AfterTool", ".*"),
        ("AfterAgent", "*"),
    ];
    for (event, matcher) in hook_events {
        let command = format!(
            "{} --agent gemini-cli --event {} --server-url {}",
            paths.binary_path(HOOK_BINARY_NAME).display(),
            event,
            server_url
        );
        upsert_gemini_hook(&mut root, event, matcher, &command)?;
    }
    write_json_config_with_backups(paths, &settings_path, &root)
}

fn configure_opencode(paths: &AppPaths, server_url: &str) -> Result<(), AppError> {
    let plugin_target = paths
        .home_dir
        .join(".config/opencode/plugins/memory-bank.js");
    copy_if_needed(
        &paths.integrations_dir.join("opencode/memory-bank.js"),
        &plugin_target,
    )?;

    let settings_path = paths.home_dir.join(".config/opencode/opencode.json");
    let mut root = load_json_config(&settings_path)?;
    ensure_object(&mut root);
    object_mut(&mut root)?
        .entry("mcp".to_string())
        .or_insert_with(|| json!({}));
    object_mut(object_mut(&mut root)?.get_mut("mcp").expect("mcp"))?.insert(
        "memory-bank".to_string(),
        json!({
            "type": "remote",
            "url": format!("{server_url}/mcp"),
            "enabled": true
        }),
    );
    write_json_config_with_backups(paths, &settings_path, &root)
}

fn configure_openclaw(paths: &AppPaths, server_url: &str) -> Result<(), AppError> {
    let extension_path = paths.integrations_dir.join("openclaw/memory-bank");
    run_command(
        "openclaw",
        &[
            "plugins",
            "install",
            "-l",
            extension_path.to_string_lossy().as_ref(),
        ],
    )?;
    let command_json = format!(
        "{{\"command\":\"{}\",\"args\":[\"--server-url\",\"{}\"]}}",
        paths.binary_path(MCP_PROXY_BINARY_NAME).display(),
        server_url
    );
    run_command("openclaw", &["mcp", "set", "memory-bank", &command_json])?;

    let settings_path = paths.home_dir.join(".openclaw/openclaw.json");
    let mut root = load_json_config(&settings_path)?;
    ensure_object(&mut root);
    {
        let root_map = object_mut(&mut root)?;
        root_map
            .entry("mcp".to_string())
            .or_insert_with(|| json!({}));
        root_map
            .entry("plugins".to_string())
            .or_insert_with(|| json!({}));
    }
    let mcp = object_mut(object_mut(&mut root)?.get_mut("mcp").expect("mcp"))?;
    mcp.entry("servers".to_string())
        .or_insert_with(|| json!({}));
    object_mut(mcp.get_mut("servers").expect("servers"))?.insert(
        "memory-bank".to_string(),
        json!({
            "command": paths.binary_path(MCP_PROXY_BINARY_NAME),
            "args": ["--server-url", server_url]
        }),
    );

    let plugins = object_mut(object_mut(&mut root)?.get_mut("plugins").expect("plugins"))?;
    plugins
        .entry("entries".to_string())
        .or_insert_with(|| json!({}));
    object_mut(plugins.get_mut("entries").expect("entries"))?.insert(
        "memory-bank".to_string(),
        json!({
            "enabled": true,
            "config": {
                "hookBinary": paths.binary_path(HOOK_BINARY_NAME),
                "serverUrl": server_url
            }
        }),
    );
    plugins
        .entry("slots".to_string())
        .or_insert_with(|| json!({}));
    object_mut(plugins.get_mut("slots").expect("slots"))?
        .insert("memory".to_string(), Value::String("none".to_string()));

    write_json_config_with_backups(paths, &settings_path, &root)
}

fn install_service(paths: &AppPaths, settings: &AppSettings) -> Result<(), AppError> {
    match ManagedPlatform::detect()? {
        ManagedPlatform::MacOs => install_launchd_service(paths),
        ManagedPlatform::Linux => install_systemd_service(paths, settings),
    }
}

fn install_launchd_service(paths: &AppPaths) -> Result<(), AppError> {
    let service_path = launchd_service_path(paths);
    if let Some(parent) = service_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&service_path, render_launchd_plist(paths))?;
    Ok(())
}

fn install_systemd_service(paths: &AppPaths, settings: &AppSettings) -> Result<(), AppError> {
    let service_path = systemd_service_path(paths);
    if let Some(parent) = service_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&service_path, render_systemd_unit(paths))?;
    run_command("systemctl", &["--user", "daemon-reload"])?;
    if settings.resolved_autostart() {
        run_command("systemctl", &["--user", "enable", SYSTEMD_UNIT_NAME])?;
    } else {
        let _ = run_command("systemctl", &["--user", "disable", SYSTEMD_UNIT_NAME]);
    }
    Ok(())
}

fn start_service(paths: &AppPaths) -> Result<(), AppError> {
    match ManagedPlatform::detect()? {
        ManagedPlatform::MacOs => {
            let uid = current_uid()?;
            let service_path = launchd_service_path(paths);
            if !service_path.exists() {
                install_launchd_service(paths)?;
            }
            let domain = format!("gui/{uid}");
            let status = ProcessCommand::new("launchctl")
                .args(["print", &format!("{domain}/{LAUNCHD_LABEL}")])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
            if status.success() {
                run_command(
                    "launchctl",
                    &["kickstart", "-k", &format!("{domain}/{LAUNCHD_LABEL}")],
                )
            } else {
                run_command(
                    "launchctl",
                    &[
                        "bootstrap",
                        &domain,
                        service_path.to_string_lossy().as_ref(),
                    ],
                )
            }
        }
        ManagedPlatform::Linux => run_command("systemctl", &["--user", "start", SYSTEMD_UNIT_NAME]),
    }
}

fn stop_service(_paths: &AppPaths) -> Result<(), AppError> {
    match ManagedPlatform::detect()? {
        ManagedPlatform::MacOs => {
            let uid = current_uid()?;
            let _ = run_command(
                "launchctl",
                &["bootout", &format!("gui/{uid}/{LAUNCHD_LABEL}")],
            );
            Ok(())
        }
        ManagedPlatform::Linux => {
            let _ = run_command("systemctl", &["--user", "stop", SYSTEMD_UNIT_NAME]);
            Ok(())
        }
    }
}

fn restart_service(_paths: &AppPaths) -> Result<(), AppError> {
    match ManagedPlatform::detect()? {
        ManagedPlatform::MacOs => {
            let uid = current_uid()?;
            run_command(
                "launchctl",
                &["kickstart", "-k", &format!("gui/{uid}/{LAUNCHD_LABEL}")],
            )
        }
        ManagedPlatform::Linux => {
            run_command("systemctl", &["--user", "restart", SYSTEMD_UNIT_NAME])
        }
    }
}

fn service_status(paths: &AppPaths) -> Result<ServiceStatus, AppError> {
    match ManagedPlatform::detect()? {
        ManagedPlatform::MacOs => {
            let installed = launchd_service_path(paths).exists();
            let uid = current_uid()?;
            let active = ProcessCommand::new("launchctl")
                .args(["print", &format!("gui/{uid}/{LAUNCHD_LABEL}")])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?
                .success();
            Ok(ServiceStatus { installed, active })
        }
        ManagedPlatform::Linux => {
            let installed = systemd_service_path(paths).exists();
            let active = ProcessCommand::new("systemctl")
                .args(["--user", "is-active", "--quiet", SYSTEMD_UNIT_NAME])
                .status()?
                .success();
            Ok(ServiceStatus { installed, active })
        }
    }
}

fn fetch_health(settings: &AppSettings) -> Result<HealthCheck, AppError> {
    let health_url = format!("{}/healthz", default_server_url(settings));
    let response = ureq::get(&health_url)
        .call()
        .map_err(|error| AppError::Health(error.to_string()))?;
    response
        .into_json::<HealthCheck>()
        .map_err(|error| AppError::Health(error.to_string()))
}

fn tail_log_file(path: &Path, follow: bool) -> Result<(), AppError> {
    let mut command = ProcessCommand::new("tail");
    if follow {
        command.args(["-n", "200", "-f"]);
    } else {
        command.args(["-n", "200"]);
    }
    command.arg(path);
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(AppError::CommandFailed(
            "tail".to_string(),
            format!("tail exited with status {status}"),
        ))
    }
}

fn build_server_launch_spec(
    paths: &AppPaths,
    settings: &AppSettings,
    secrets: &SecretStore,
) -> Result<ServerLaunchSpec, AppError> {
    let program = if paths.binary_path(SERVER_BINARY_NAME).exists() {
        paths.binary_path(SERVER_BINARY_NAME)
    } else {
        let current_exe = std::env::current_exe()?;
        let sibling = current_exe
            .parent()
            .ok_or_else(|| AppError::MissingBinary(SERVER_BINARY_NAME.to_string()))?
            .join(SERVER_BINARY_NAME);
        if sibling.exists() {
            sibling
        } else {
            return Err(AppError::MissingBinary(SERVER_BINARY_NAME.to_string()));
        }
    };

    let server_settings = settings.server.clone().unwrap_or_default();
    let provider = server_settings
        .llm_provider
        .clone()
        .unwrap_or_else(|| "anthropic".to_string());
    let encoder_provider = server_settings
        .encoder_provider
        .clone()
        .unwrap_or_else(|| "fast-embed".to_string());
    let mut env = BTreeMap::new();
    if let Some(secret_key) = env_key_for_provider(&provider) {
        let secret = secrets
            .get(secret_key)
            .ok_or(AppError::MissingProviderSecret(secret_key))?;
        env.insert(secret_key.to_string(), secret.to_string());
    }

    match provider.as_str() {
        "ollama" => {
            if let Some(model) = server_settings.llm_model {
                env.insert("MEMORY_BANK_OLLAMA_MODEL".to_string(), model);
            }
        }
        _ => {
            if let Some(model) = server_settings.llm_model {
                env.insert("MEMORY_BANK_LLM_MODEL".to_string(), model);
            }
        }
    }
    if let Some(model) = server_settings.fastembed_model {
        env.insert("MEMORY_BANK_FASTEMBED_MODEL".to_string(), model);
    }
    if let Some(url) = server_settings.local_encoder_url {
        env.insert("MEMORY_BANK_LOCAL_ENCODER_URL".to_string(), url);
    }
    if let Some(url) = server_settings.remote_encoder_url {
        env.insert("MEMORY_BANK_REMOTE_ENCODER_URL".to_string(), url);
    }

    Ok(ServerLaunchSpec {
        program,
        args: vec![
            "--port".to_string(),
            settings.resolved_port().to_string(),
            "--namespace".to_string(),
            settings.active_namespace().to_string(),
            "--llm-provider".to_string(),
            provider,
            "--encoder-provider".to_string(),
            encoder_provider,
            "--history-window-size".to_string(),
            server_settings.history_window_size.unwrap_or(0).to_string(),
            "--nearest-neighbor-count".to_string(),
            server_settings
                .nearest_neighbor_count
                .unwrap_or(10)
                .to_string(),
        ],
        env,
        remove_env: vec![
            "ANTHROPIC_API_KEY",
            "GEMINI_API_KEY",
            "OPENAI_API_KEY",
            "MEMORY_BANK_LLM_MODEL",
            "MEMORY_BANK_FASTEMBED_MODEL",
            "MEMORY_BANK_LOCAL_ENCODER_URL",
            "MEMORY_BANK_REMOTE_ENCODER_URL",
            "MEMORY_BANK_OLLAMA_MODEL",
            "MEMORY_BANK_OLLAMA_URL",
        ],
    })
}

fn integration_configured(settings: &AppSettings, agent: AgentKind) -> bool {
    settings
        .integrations
        .as_ref()
        .and_then(|integrations| match agent {
            AgentKind::ClaudeCode => integrations.claude_code.as_ref(),
            AgentKind::GeminiCli => integrations.gemini_cli.as_ref(),
            AgentKind::OpenCode => integrations.opencode.as_ref(),
            AgentKind::OpenClaw => integrations.openclaw.as_ref(),
        })
        .map(|state| state.configured)
        .unwrap_or(false)
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

fn get_config_value(settings: &AppSettings, key: &str) -> Result<String, AppError> {
    match key {
        "schema_version" => Ok(settings.schema_version.to_string()),
        "active_namespace" => Ok(settings.active_namespace().to_string()),
        "service.port" => Ok(settings.resolved_port().to_string()),
        "service.autostart" => Ok(settings.resolved_autostart().to_string()),
        "server.llm_provider" => Ok(settings
            .server
            .as_ref()
            .and_then(|server| server.llm_provider.clone())
            .unwrap_or_else(|| "anthropic".to_string())),
        "server.llm_model" => Ok(settings
            .server
            .as_ref()
            .and_then(|server| server.llm_model.clone())
            .unwrap_or_else(|| DEFAULT_ANTHROPIC_MODEL.to_string())),
        "server.encoder_provider" => Ok(settings
            .server
            .as_ref()
            .and_then(|server| server.encoder_provider.clone())
            .unwrap_or_else(|| "fast-embed".to_string())),
        "server.fastembed_model" => Ok(settings
            .server
            .as_ref()
            .and_then(|server| server.fastembed_model.clone())
            .unwrap_or_else(|| DEFAULT_FASTEMBED_MODEL.to_string())),
        "server.history_window_size" => Ok(settings
            .server
            .as_ref()
            .and_then(|server| server.history_window_size)
            .unwrap_or(0)
            .to_string()),
        "server.nearest_neighbor_count" => Ok(settings
            .server
            .as_ref()
            .and_then(|server| server.nearest_neighbor_count)
            .unwrap_or(10)
            .to_string()),
        "server.local_encoder_url" => Ok(settings
            .server
            .as_ref()
            .and_then(|server| server.local_encoder_url.clone())
            .unwrap_or_default()),
        "server.remote_encoder_url" => Ok(settings
            .server
            .as_ref()
            .and_then(|server| server.remote_encoder_url.clone())
            .unwrap_or_default()),
        key if key.starts_with("integrations.") && key.ends_with(".configured") => {
            let agent = config_key_to_agent(key)?;
            Ok(integration_configured(settings, agent).to_string())
        }
        _ => Err(AppError::InvalidConfigKey(key.to_string())),
    }
}

fn set_config_value(settings: &mut AppSettings, key: &str, value: &str) -> Result<(), AppError> {
    match key {
        "active_namespace" => {
            let namespace = Namespace::new(value);
            settings.active_namespace = if namespace.as_ref() == DEFAULT_NAMESPACE_NAME {
                None
            } else {
                Some(namespace.to_string())
            };
        }
        "service.port" => {
            let port: u16 = value.parse::<u16>().map_err(|error| {
                AppError::InvalidConfigValue(key.to_string(), error.to_string())
            })?;
            let mut service = settings.service.clone().unwrap_or_default();
            service.port = if port == DEFAULT_PORT {
                None
            } else {
                Some(port)
            };
            settings.service = if service.is_empty() {
                None
            } else {
                Some(service)
            };
        }
        "service.autostart" => {
            let autostart = parse_bool(value, key)?;
            let mut service = settings.service.clone().unwrap_or_default();
            service.autostart = autostart.then_some(true);
            settings.service = if service.is_empty() {
                None
            } else {
                Some(service)
            };
        }
        "server.llm_provider" => {
            let mut server = settings.server.clone().unwrap_or_default();
            server.llm_provider = if value == "anthropic" {
                None
            } else {
                Some(value.to_string())
            };
            settings.server = if server.is_empty() {
                None
            } else {
                Some(server)
            };
        }
        "server.llm_model" => {
            let mut server = settings.server.clone().unwrap_or_default();
            server.llm_model = Some(value.to_string());
            settings.server = Some(server);
        }
        "server.encoder_provider" => {
            let mut server = settings.server.clone().unwrap_or_default();
            server.encoder_provider = if value == "fast-embed" {
                None
            } else {
                Some(value.to_string())
            };
            settings.server = if server.is_empty() {
                None
            } else {
                Some(server)
            };
        }
        "server.fastembed_model" => {
            let mut server = settings.server.clone().unwrap_or_default();
            server.fastembed_model = Some(value.to_string());
            settings.server = Some(server);
        }
        "server.history_window_size" => {
            let parsed: u32 = value.parse::<u32>().map_err(|error| {
                AppError::InvalidConfigValue(key.to_string(), error.to_string())
            })?;
            let mut server = settings.server.clone().unwrap_or_default();
            server.history_window_size = if parsed == 0 { None } else { Some(parsed) };
            settings.server = if server.is_empty() {
                None
            } else {
                Some(server)
            };
        }
        "server.nearest_neighbor_count" => {
            let parsed: i32 = value.parse::<i32>().map_err(|error| {
                AppError::InvalidConfigValue(key.to_string(), error.to_string())
            })?;
            if parsed < 1 {
                return Err(AppError::InvalidConfigValue(
                    key.to_string(),
                    "must be at least 1".to_string(),
                ));
            }
            let mut server = settings.server.clone().unwrap_or_default();
            server.nearest_neighbor_count = if parsed == 10 { None } else { Some(parsed) };
            settings.server = if server.is_empty() {
                None
            } else {
                Some(server)
            };
        }
        "server.local_encoder_url" => {
            let mut server = settings.server.clone().unwrap_or_default();
            server.local_encoder_url = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
            settings.server = if server.is_empty() {
                None
            } else {
                Some(server)
            };
        }
        "server.remote_encoder_url" => {
            let mut server = settings.server.clone().unwrap_or_default();
            server.remote_encoder_url = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
            settings.server = if server.is_empty() {
                None
            } else {
                Some(server)
            };
        }
        key if key.starts_with("integrations.") && key.ends_with(".configured") => {
            let configured = parse_bool(value, key)?;
            let agent = config_key_to_agent(key)?;
            let integrations = settings
                .integrations
                .get_or_insert_with(IntegrationsSettings::default);
            let state = Some(IntegrationState { configured });
            match agent {
                AgentKind::ClaudeCode => integrations.claude_code = state,
                AgentKind::GeminiCli => integrations.gemini_cli = state,
                AgentKind::OpenCode => integrations.opencode = state,
                AgentKind::OpenClaw => integrations.openclaw = state,
            }
            if integrations.is_empty() {
                settings.integrations = None;
            }
        }
        _ => return Err(AppError::InvalidConfigKey(key.to_string())),
    }

    Ok(())
}

fn parse_bool(value: &str, key: &str) -> Result<bool, AppError> {
    value
        .parse::<bool>()
        .map_err(|error| AppError::InvalidConfigValue(key.to_string(), error.to_string()))
}

fn config_key_to_agent(key: &str) -> Result<AgentKind, AppError> {
    match key {
        "integrations.claude_code.configured" => Ok(AgentKind::ClaudeCode),
        "integrations.gemini_cli.configured" => Ok(AgentKind::GeminiCli),
        "integrations.opencode.configured" => Ok(AgentKind::OpenCode),
        "integrations.openclaw.configured" => Ok(AgentKind::OpenClaw),
        _ => Err(AppError::InvalidConfigKey(key.to_string())),
    }
}

fn upsert_claude_hook(root: &mut Value, event: &str, command: &str) -> Result<(), AppError> {
    ensure_object(root);
    let root_map = object_mut(root)?;
    let hooks = root_map
        .entry("hooks".to_string())
        .or_insert_with(|| json!({}));
    let hooks_map = object_mut(hooks)?;
    let groups = hooks_map
        .entry(event.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    let groups_array = array_mut(groups)?;
    let marker = format!("--agent claude-code --event {event}");
    let desired = json!({
        "type": "command",
        "command": command,
    });
    if let Some(existing) = groups_array.iter_mut().find_map(|group| {
        group
            .get_mut("hooks")
            .and_then(Value::as_array_mut)
            .and_then(|hooks| {
                hooks.iter_mut().find(|hook| {
                    hook.get("command")
                        .and_then(Value::as_str)
                        .map(|value| value.contains(&marker))
                        .unwrap_or(false)
                })
            })
    }) {
        *existing = desired;
    } else {
        groups_array.push(json!({ "hooks": [desired] }));
    }
    Ok(())
}

fn upsert_gemini_hook(
    root: &mut Value,
    event: &str,
    matcher: &str,
    command: &str,
) -> Result<(), AppError> {
    ensure_object(root);
    let root_map = object_mut(root)?;
    let hooks = root_map
        .entry("hooks".to_string())
        .or_insert_with(|| json!({}));
    let hooks_map = object_mut(hooks)?;
    let groups = hooks_map
        .entry(event.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    let groups_array = array_mut(groups)?;
    let desired_hook = json!({
        "name": "memory-bank",
        "type": "command",
        "command": command,
    });
    if let Some(existing_group) = groups_array.iter_mut().find(|group| {
        group
            .get("hooks")
            .and_then(Value::as_array)
            .map(|hooks| {
                hooks.iter().any(|hook| {
                    hook.get("name")
                        .and_then(Value::as_str)
                        .map(|value| value == "memory-bank")
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    }) {
        if let Some(hooks) = existing_group
            .get_mut("hooks")
            .and_then(Value::as_array_mut)
        {
            if let Some(existing_hook) = hooks.iter_mut().find(|hook| {
                hook.get("name")
                    .and_then(Value::as_str)
                    .map(|value| value == "memory-bank")
                    .unwrap_or(false)
            }) {
                *existing_hook = desired_hook;
            }
        }
    } else {
        groups_array.push(json!({
            "matcher": matcher,
            "sequential": true,
            "hooks": [desired_hook],
        }));
    }
    Ok(())
}

fn load_json_config(path: &Path) -> Result<Value, AppError> {
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let contents = fs::read_to_string(path)?;
    let value = serde_json::from_str(&contents)?;
    Ok(value)
}

fn write_json_config_with_backups(
    paths: &AppPaths,
    original_path: &Path,
    value: &Value,
) -> Result<(), AppError> {
    if original_path.exists() {
        backup_existing_file(paths, original_path)?;
    } else if let Some(parent) = original_path.parent() {
        fs::create_dir_all(parent)?;
    }
    write_json_file(original_path, value)?;
    Ok(())
}

fn backup_existing_file(paths: &AppPaths, original_path: &Path) -> Result<(), AppError> {
    let timestamp = Local::now().format("%Y%m%d%H%M%S").to_string();
    let relative = original_path
        .strip_prefix(Path::new("/"))
        .unwrap_or(original_path);
    let central_backup = paths.backups_dir.join(timestamp).join(relative);
    if let Some(parent) = central_backup.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(original_path, &central_backup)?;
    let sibling_backup = PathBuf::from(format!("{}.mb_backup", original_path.display()));
    if let Some(parent) = sibling_backup.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(original_path, sibling_backup)?;
    Ok(())
}

fn render_launchd_plist(paths: &AppPaths) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
  <dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
      <string>{program}</string>
      <string>internal</string>
      <string>run-server</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log_file}</string>
    <key>StandardErrorPath</key>
    <string>{log_file}</string>
  </dict>
</plist>
"#,
        label = LAUNCHD_LABEL,
        program = paths.binary_path(MB_BINARY_NAME).display(),
        log_file = paths.log_file.display(),
    )
}

fn render_systemd_unit(paths: &AppPaths) -> String {
    let escaped_mb = shell_escape(paths.binary_path(MB_BINARY_NAME).to_string_lossy().as_ref());
    let escaped_log = shell_escape(paths.log_file.to_string_lossy().as_ref());
    format!(
        "[Unit]\nDescription=Memory Bank\nAfter=network.target\n\n[Service]\nType=simple\nExecStart=/bin/sh -lc 'exec {escaped_mb} internal run-server >> {escaped_log} 2>&1'\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n"
    )
}

fn current_uid() -> Result<String, AppError> {
    let output = ProcessCommand::new("id").arg("-u").output()?;
    if !output.status.success() {
        return Err(AppError::CommandFailed(
            "id -u".to_string(),
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn launchd_service_path(paths: &AppPaths) -> PathBuf {
    paths
        .home_dir
        .join("Library/LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist"))
}

fn systemd_service_path(paths: &AppPaths) -> PathBuf {
    paths
        .home_dir
        .join(".config/systemd/user")
        .join(SYSTEMD_UNIT_NAME)
}

fn run_command(program: &str, args: &[&str]) -> Result<(), AppError> {
    let output = ProcessCommand::new(program).args(args).output()?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let details = if stderr.is_empty() { stdout } else { stderr };
    Err(AppError::CommandFailed(
        format!("{program} {}", args.join(" ")),
        details,
    ))
}

fn shell_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', r#"'"'"'"#))
}

fn ensure_object(value: &mut Value) {
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
}

fn object_mut(value: &mut Value) -> Result<&mut Map<String, Value>, AppError> {
    value
        .as_object_mut()
        .ok_or_else(|| AppError::Message("expected JSON object".to_string()))
}

fn array_mut(value: &mut Value) -> Result<&mut Vec<Value>, AppError> {
    value
        .as_array_mut()
        .ok_or_else(|| AppError::Message("expected JSON array".to_string()))
}

fn default_model_for_provider(provider: &str) -> &'static str {
    match provider {
        "anthropic" => DEFAULT_ANTHROPIC_MODEL,
        "gemini" => DEFAULT_GEMINI_MODEL,
        "open-ai" => DEFAULT_OPENAI_MODEL,
        "ollama" => DEFAULT_OLLAMA_MODEL,
        _ => DEFAULT_ANTHROPIC_MODEL,
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_bank_app::{ServerSettings, ServiceSettings};
    use tempfile::TempDir;

    #[test]
    fn launch_spec_uses_secrets_env_and_strips_ambient_keys() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings = AppSettings {
            schema_version: SETTINGS_SCHEMA_VERSION,
            active_namespace: Some("work".to_string()),
            service: Some(ServiceSettings {
                port: Some(4545),
                autostart: Some(true),
            }),
            server: Some(ServerSettings {
                llm_provider: Some("anthropic".to_string()),
                llm_model: Some("claude-custom".to_string()),
                ..ServerSettings::default()
            }),
            integrations: None,
        };
        let mut secrets = SecretStore::default();
        secrets.set("ANTHROPIC_API_KEY", "from-secrets");

        fs::create_dir_all(&paths.bin_dir).expect("bin dir");
        fs::write(paths.binary_path(SERVER_BINARY_NAME), "").expect("server placeholder");
        let spec = build_server_launch_spec(&paths, &settings, &secrets).expect("spec");

        assert_eq!(spec.program, paths.binary_path(SERVER_BINARY_NAME));
        assert!(spec.args.contains(&"--port".to_string()));
        assert_eq!(
            spec.env.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some("from-secrets")
        );
        assert!(spec.remove_env.contains(&"ANTHROPIC_API_KEY"));
        assert_eq!(
            spec.env.get("MEMORY_BANK_LLM_MODEL").map(String::as_str),
            Some("claude-custom")
        );
    }

    #[test]
    fn launchd_plist_points_to_mb_binary_and_log_file() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let rendered = render_launchd_plist(&paths);
        assert!(rendered.contains("internal"));
        assert!(rendered.contains("run-server"));
        assert!(rendered.contains(paths.binary_path(MB_BINARY_NAME).to_string_lossy().as_ref()));
        assert!(rendered.contains(paths.log_file.to_string_lossy().as_ref()));
    }

    #[test]
    fn systemd_unit_redirects_to_app_log_file() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let rendered = render_systemd_unit(&paths);
        assert!(rendered.contains("ExecStart=/bin/sh -lc"));
        assert!(rendered.contains("internal run-server"));
        assert!(rendered.contains("server.log"));
    }
}
