mod cli;

use chrono::Local;
use clap::Parser;
use cli::{Cli, Command, ConfigCommand, InternalCommand, NamespaceCommand, ServiceCommand};
use inquire::ui::{Color, RenderConfig, StyleSheet, Styled};
use inquire::validator::Validation;
use inquire::{
    Confirm, CustomType, InquireError, MultiSelect, Password, Select, Text,
    set_global_render_config,
};
use jsonc_parser::{ParseOptions, parse_to_serde_value};
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
use std::io::{self, IsTerminal, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;

const MB_BINARY_NAME: &str = "mb";
const SERVER_BINARY_NAME: &str = "memory-bank-server";
const HOOK_BINARY_NAME: &str = "memory-bank-hook";
const MCP_PROXY_BINARY_NAME: &str = "memory-bank-mcp-proxy";
const LAUNCHD_LABEL: &str = "com.memory-bank.mb";
const SYSTEMD_UNIT_NAME: &str = "memory-bank.service";
const REMOTE_MODEL_CATALOG_URL: &str =
    "https://raw.githubusercontent.com/feelingsonice/MemoryBank/main/config/setup-model-catalog.json";
const HEALTH_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_secs(2);

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
    Prompt(#[from] InquireError),
    #[error("setup was canceled")]
    SetupCanceled,
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

impl std::fmt::Display for AgentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.display_name())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProviderChoice {
    Anthropic,
    Gemini,
    OpenAi,
    Ollama,
}

impl ProviderChoice {
    fn all() -> Vec<Self> {
        vec![Self::Anthropic, Self::Gemini, Self::OpenAi, Self::Ollama]
    }

    fn from_config_value(value: Option<&str>) -> Self {
        match value {
            Some("gemini") => Self::Gemini,
            Some("open-ai") => Self::OpenAi,
            Some("ollama") => Self::Ollama,
            _ => Self::Anthropic,
        }
    }

    fn as_config_value(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::OpenAi => "open-ai",
            Self::Ollama => "ollama",
        }
    }
}

impl std::fmt::Display for ProviderChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::Anthropic => "Anthropic",
            Self::Gemini => "Gemini",
            Self::OpenAi => "OpenAI",
            Self::Ollama => "Ollama (local)",
        };
        f.write_str(label)
    }
}

impl std::fmt::Display for ModelChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Preset(model) => f.write_str(model),
            Self::Current(model) => write!(f, "Current saved model ({model})"),
            Self::Custom => f.write_str("Enter a custom model..."),
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

#[derive(Debug)]
struct AgentSetupOutcome {
    configured: Vec<AgentKind>,
    warnings: Vec<String>,
}

#[derive(Debug)]
struct CommandOutcome {
    program: String,
    args: Vec<String>,
    success: bool,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServerLaunchSpec {
    program: PathBuf,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    remove_env: Vec<&'static str>,
}

#[derive(Debug, Clone)]
struct SetupAnswers {
    namespace: Namespace,
    provider: String,
    model: String,
    ollama_url: Option<String>,
    autostart: bool,
    selected_agents: Vec<AgentKind>,
    secret_choice: SecretChoice,
    advanced: AdvancedAnswers,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
struct ModelCatalog {
    #[serde(default)]
    providers: BTreeMap<String, ProviderModelCatalog>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
struct ProviderModelCatalog {
    #[serde(default)]
    models: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    #[serde(default)]
    models: Vec<OllamaTagModel>,
}

#[derive(Debug, Deserialize)]
struct OllamaTagModel {
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AdvancedAnswers {
    port: u16,
    fastembed_model: String,
    history_window_size: u32,
    nearest_neighbor_count: i32,
}

#[derive(Debug, Clone)]
enum SecretChoice {
    NotRequired,
    KeepStored {
        key: &'static str,
    },
    ImportEnvironment {
        key: &'static str,
        value: String,
    },
    ReplaceWithEnvironment {
        key: &'static str,
        value: String,
    },
    ManualEntry {
        key: &'static str,
        value: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ModelChoice {
    Preset(String),
    Current(String),
    Custom,
}

impl ModelChoice {
    fn value(&self) -> Option<&str> {
        match self {
            Self::Preset(model) => Some(model.as_str()),
            Self::Current(model) => Some(model.as_str()),
            Self::Custom => None,
        }
    }
}

fn run_setup() -> Result<(), AppError> {
    ensure_interactive_terminal()?;
    configure_setup_rendering();

    let paths = AppPaths::from_system()?;
    let model_catalog = refresh_model_catalog(&paths);
    let mut settings = AppSettings::load(&paths)?;
    let mut secrets = SecretStore::load(&paths)?;
    let detected_agents = detect_installed_agents();
    let answers = match collect_setup_answers(&settings, &secrets, &detected_agents, &model_catalog) {
        Ok(Some(answers)) => answers,
        Ok(None) | Err(AppError::SetupCanceled) => {
            println!("Setup canceled. No changes were made.");
            return Ok(());
        }
        Err(error) => return Err(error),
    };

    println!();
    println!("{}", render_review_summary(&answers));
    println!();

    let confirm = Confirm::new("Apply these changes now?")
        .with_default(true)
        .with_help_message(
            "Nothing under ~/.memory_bank or your agent config files will change until you confirm.",
        )
        .prompt_skippable()?;
    if !matches!(confirm, Some(true)) {
        println!("Setup canceled. No changes were made.");
        return Ok(());
    }

    let (health, agent_outcome) = apply_setup_answers(&paths, &mut settings, &mut secrets, &answers)?;
    println!();
    println!(
        "Memory Bank is ready on {} using namespace `{}` and provider `{}`.",
        default_server_url(&settings),
        health.namespace,
        health.llm_provider
    );
    if !agent_outcome.warnings.is_empty() {
        println!("Some agent integrations need attention:");
        for warning in agent_outcome.warnings {
            println!("  - {warning}");
        }
    }
    Ok(())
}

fn ensure_interactive_terminal() -> Result<(), AppError> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(AppError::Message(
            "mb setup requires an interactive terminal. Run it in a TTY or use `mb config` for manual changes.".to_string(),
        ));
    }
    Ok(())
}

fn configure_setup_rendering() {
    set_global_render_config(setup_render_config());
}

fn setup_render_config() -> RenderConfig<'static> {
    let mut config = if no_color_requested() {
        RenderConfig::empty()
    } else {
        RenderConfig::default_colored()
    };
    config.prompt_prefix = Styled::new("mb>");
    config.answered_prompt_prefix = Styled::new("->");
    config.highlighted_option_prefix = Styled::new(">");
    config.selected_checkbox = Styled::new("[x]");
    config.unselected_checkbox = Styled::new("[ ]");
    config.help_message = StyleSheet::new().with_fg(Color::DarkGrey);
    config.answer = StyleSheet::new().with_fg(Color::LightCyan);
    config.selected_option = Some(StyleSheet::new().with_fg(Color::LightBlue));
    config
}

fn no_color_requested() -> bool {
    std::env::var_os("NO_COLOR").is_some()
        || matches!(std::env::var("TERM").ok().as_deref(), Some("dumb"))
}

fn collect_setup_answers(
    settings: &AppSettings,
    secrets: &SecretStore,
    detected_agents: &[AgentKind],
    model_catalog: &ModelCatalog,
) -> Result<Option<SetupAnswers>, AppError> {
    print_setup_intro();

    println!("Core setup");
    let namespace = match prompt_namespace(settings.active_namespace())? {
        Some(namespace) => namespace,
        None => return Ok(None),
    };
    let provider = match prompt_provider(
        settings
            .server
            .as_ref()
            .and_then(|server| server.llm_provider.as_deref()),
    )? {
        Some(provider) => provider,
        None => return Ok(None),
    };
    let current_ollama_url = settings
        .server
        .as_ref()
        .and_then(|server| server.ollama_url.as_deref());
    let current_model = settings
        .server
        .as_ref()
        .and_then(|server| server.llm_model.as_deref());
    let (ollama_url, model) = if provider == "ollama" {
        let ollama_url = match prompt_ollama_url(current_ollama_url)? {
            Some(url) => url,
            None => return Ok(None),
        };
        let model = match prompt_ollama_model(current_model, &ollama_url, model_catalog)? {
            Some(model) => model,
            None => return Ok(None),
        };
        (Some(ollama_url), model)
    } else {
        let model = match prompt_model(&provider, current_model, model_catalog)? {
            Some(model) => model,
            None => return Ok(None),
        };
        (None, model)
    };
    let autostart = match prompt_autostart(
        settings
            .service
            .as_ref()
            .and_then(|service| service.autostart),
    )? {
        Some(autostart) => autostart,
        None => return Ok(None),
    };

    println!();
    println!("Agent integrations");
    println!("Choose one or more agents to configure in this setup run.");
    let selected_agents = match prompt_agents(detected_agents)? {
        Some(selected_agents) => selected_agents,
        None => return Ok(None),
    };
    println!("Selected agents: {}", render_agents_summary(&selected_agents));

    println!();
    println!("Secrets");
    let secret_choice = match collect_secret_choice(&provider, secrets)? {
        Some(secret_choice) => secret_choice,
        None => return Ok(None),
    };

    let mut advanced = AdvancedAnswers::from_settings(settings);
    let has_existing_advanced = advanced.has_overrides();
    println!();
    println!("Advanced settings");
    let configure_advanced = match Confirm::new("Configure advanced settings?")
        .with_default(has_existing_advanced)
        .with_help_message(
            "Most users can skip this. You can change these later with `mb config` if needed.",
        )
        .prompt_skippable()?
    {
        Some(value) => value,
        None => return Ok(None),
    };

    if configure_advanced {
        advanced = match prompt_advanced_settings(settings)? {
            Some(advanced) => advanced,
            None => return Ok(None),
        };
    }

    Ok(Some(SetupAnswers {
        namespace,
        provider,
        model,
        ollama_url,
        autostart,
        selected_agents,
        secret_choice,
        advanced,
    }))
}

fn print_setup_intro() {
    println!("Memory Bank Setup");
    println!("Configure the local Memory Bank service and any detected agent integrations.");
    println!("You will review everything before any changes are applied.");
    println!();
}

fn apply_setup_answers(
    paths: &AppPaths,
    settings: &mut AppSettings,
    secrets: &mut SecretStore,
    answers: &SetupAnswers,
) -> Result<(HealthCheck, AgentSetupOutcome), AppError> {
    let total_steps = 6;
    let preview_settings = build_settings_for_answers(settings, answers, &[]);

    apply_step(1, total_steps, "Install artifacts and update PATH", || {
        paths.ensure_base_dirs()?;
        materialize_install_artifacts(paths)?;
        ensure_path_entry(paths)
    })?;

    let agent_outcome = {
        print_step_start(2, total_steps, "Configure selected agents")?;
        let outcome = configure_selected_agents(paths, &preview_settings, &answers.selected_agents)?;
        if answers.selected_agents.is_empty() {
            println!("done (no agents selected)");
        } else if outcome.warnings.is_empty() {
            println!("done");
        } else {
            println!("done with warnings");
        }
        outcome
    };

    let final_settings = build_settings_for_answers(settings, answers, &agent_outcome.configured);
    *settings = final_settings;

    apply_step(3, total_steps, "Write settings and secrets", || {
        apply_secret_choice(secrets, &answers.secret_choice);
        settings.save(paths)?;
        secrets.save(paths)?;
        Ok(())
    })?;

    apply_step(4, total_steps, "Install managed service", || {
        install_service(paths, settings)
    })?;
    apply_step(5, total_steps, "Start managed service", || start_service(paths))?;
    let health = apply_step(6, total_steps, "Wait for service health", || {
        wait_for_health(settings, HEALTH_STARTUP_TIMEOUT, HEALTH_POLL_INTERVAL)
    })?;

    Ok((health, agent_outcome))
}

fn print_step_start(index: usize, total: usize, label: &str) -> Result<(), AppError> {
    print!("[{index}/{total}] {label}... ");
    io::stdout().flush()?;
    Ok(())
}

fn apply_step<T, F>(index: usize, total: usize, label: &str, action: F) -> Result<T, AppError>
where
    F: FnOnce() -> Result<T, AppError>,
{
    print_step_start(index, total, label)?;
    match action() {
        Ok(value) => {
            println!("done");
            Ok(value)
        }
        Err(error) => {
            println!("failed");
            Err(error)
        }
    }
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
        let health = wait_for_health(&settings, HEALTH_STARTUP_TIMEOUT, HEALTH_POLL_INTERVAL)?;
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

fn prompt_namespace(current: Namespace) -> Result<Option<Namespace>, AppError> {
    let value = Text::new("Active namespace")
        .with_default(current.to_string().as_str())
        .with_help_message(
            "This is the user-level memory space the managed service will run against.",
        )
        .with_placeholder("default")
        .prompt_skippable()?;
    Ok(value.map(Namespace::new))
}

fn prompt_provider(current: Option<&str>) -> Result<Option<String>, AppError> {
    let options = ProviderChoice::all();
    let default_index = options
        .iter()
        .position(|choice| *choice == ProviderChoice::from_config_value(current))
        .unwrap_or(0);
    let choice = Select::new("LLM provider", options)
        .with_starting_cursor(default_index)
        .with_page_size(4)
        .with_help_message(
            "This powers Memory Bank's internal memory analysis, not the coding agent you use directly.",
        )
        .prompt_skippable()?;
    Ok(choice.map(|choice| choice.as_config_value().to_string()))
}

fn prompt_ollama_url(current: Option<&str>) -> Result<Option<String>, AppError> {
    Text::new("Ollama URL")
        .with_default(current.unwrap_or(memory_bank_app::DEFAULT_OLLAMA_URL))
        .with_help_message(
            "Memory Bank will query this Ollama daemon for the local models you already have installed.",
        )
        .with_placeholder("http://localhost:11434")
        .prompt_skippable()
        .map_err(AppError::from)
}

fn prompt_model(
    provider: &str,
    current: Option<&str>,
    catalog: &ModelCatalog,
) -> Result<Option<String>, AppError> {
    let choices = model_choices_for_provider(provider, current, catalog);
    let preferred = current
        .filter(|value| !value.is_empty())
        .or_else(|| Some(default_model_for_provider(provider)));
    let default_index = preferred
        .and_then(|value| choices.iter().position(|choice| choice.value() == Some(value)))
        .unwrap_or(0);
    let prompt = format!("Model for {}", ProviderChoice::from_config_value(Some(provider)));
    let selection = Select::new(&prompt, choices)
        .with_starting_cursor(default_index)
        .with_page_size(8)
        .with_help_message(
            "Choose a popular model ID for this provider. If you need a different one, pick the custom option and type it exactly.",
        )
        .prompt_skippable()?;

    let Some(selection) = selection else {
        return Ok(None);
    };

    match selection {
        ModelChoice::Preset(model) => Ok(Some(model)),
        ModelChoice::Current(model) => Ok(Some(model)),
        ModelChoice::Custom => {
            let default_model = current
                .map(str::to_owned)
                .unwrap_or_else(|| default_model_for_provider(provider).to_string());
            let value = Text::new("Custom model string")
                .with_default(default_model.as_str())
                .with_help_message("Enter the exact model ID for the selected provider.")
                .prompt_skippable()?;
            Ok(value)
        }
    }
}

fn prompt_ollama_model(
    current: Option<&str>,
    ollama_url: &str,
    catalog: &ModelCatalog,
) -> Result<Option<String>, AppError> {
    match fetch_ollama_models_for_setup(ollama_url) {
        Ok(models) if !models.is_empty() => {
            let choices = model_choices_from_values(&models, current);
            let preferred = current
                .filter(|value| !value.is_empty())
                .or_else(|| Some(default_model_for_provider("ollama")));
            let default_index = preferred
                .and_then(|value| choices.iter().position(|choice| choice.value() == Some(value)))
                .unwrap_or(0);
            let selection = Select::new("Model for Ollama (installed locally)", choices)
                .with_starting_cursor(default_index)
                .with_page_size(10)
                .with_help_message(
                    "These models were discovered from your Ollama daemon. If yours is missing, choose the custom option.",
                )
                .prompt_skippable()?;

            let Some(selection) = selection else {
                return Ok(None);
            };

            match selection {
                ModelChoice::Preset(model) | ModelChoice::Current(model) => Ok(Some(model)),
                ModelChoice::Custom => prompt_custom_ollama_model(current, catalog),
            }
        }
        Ok(_) => {
            println!(
                "No local Ollama models were detected at {}.",
                ollama_url.trim_end_matches('/')
            );
            prompt_custom_ollama_model(current, catalog)
        }
        Err(error) => {
            println!("Could not query Ollama at {ollama_url}: {error}");
            prompt_custom_ollama_model(current, catalog)
        }
    }
}

fn prompt_custom_ollama_model(
    current: Option<&str>,
    catalog: &ModelCatalog,
) -> Result<Option<String>, AppError> {
    let suggestions = catalog.models_for_provider("ollama");
    let help = if suggestions.is_empty() {
        "Enter the local Ollama model name you want Memory Bank to use."
    } else {
        "Enter the local Ollama model name you want Memory Bank to use. Common pulls: qwen3, deepseek-r1, llama3.1, qwen2.5-coder."
    };
    Text::new("Ollama model name")
        .with_default(current.unwrap_or(default_model_for_provider("ollama")))
        .with_help_message(help)
        .prompt_skippable()
        .map_err(AppError::from)
}

fn prompt_autostart(current: Option<bool>) -> Result<Option<bool>, AppError> {
    Confirm::new("Start Memory Bank automatically on login?")
        .with_default(current.unwrap_or(true))
        .with_help_message("This installs a user-scoped background service for Memory Bank.")
        .prompt_skippable()
        .map_err(AppError::from)
}

fn prompt_agents(detected: &[AgentKind]) -> Result<Option<Vec<AgentKind>>, AppError> {
    if detected.is_empty() {
        println!(
            "No supported agents were detected on PATH. You can rerun `mb setup` later after installing Claude Code, Gemini CLI, OpenCode, or OpenClaw."
        );
        return Ok(Some(Vec::new()));
    }

    let selected = MultiSelect::new("Select which detected agents to configure now", detected.to_vec())
        .with_all_selected_by_default()
        .with_page_size(detected.len().min(7))
        .with_help_message(
            "Use Space to toggle the highlighted agent. Press Enter to continue with all checked agents.",
        )
        .prompt_skippable()?;
    Ok(selected)
}

fn collect_secret_choice(
    provider: &str,
    secrets: &SecretStore,
) -> Result<Option<SecretChoice>, AppError> {
    let Some(secret_key) = env_key_for_provider(provider) else {
        return Ok(Some(SecretChoice::NotRequired));
    };

    let env_value = std::env::var(secret_key).ok();
    let stored_value = secrets.get(secret_key).map(str::to_owned);

    let choice = match (stored_value.as_deref(), env_value.as_deref()) {
        (Some(stored_value), Some(env_value)) if stored_value != env_value => {
            let replace = Confirm::new(&format!(
                "Replace the stored {secret_key} with the value from your current shell?"
            ))
            .with_default(false)
            .with_help_message(
                "The managed service always reads ~/.memory_bank/secrets.env, not your shell session.",
            )
            .prompt_skippable()?;
            match replace {
                Some(true) => SecretChoice::ReplaceWithEnvironment {
                    key: secret_key,
                    value: env_value.to_string(),
                },
                Some(false) => SecretChoice::KeepStored { key: secret_key },
                None => return Ok(None),
            }
        }
        (Some(_), _) => SecretChoice::KeepStored { key: secret_key },
        (None, Some(env_value)) => {
            let import = Confirm::new(&format!(
                "Import {secret_key} from your current shell into Memory Bank?"
            ))
            .with_default(true)
            .with_help_message(
                "This copies the key into ~/.memory_bank/secrets.env so the managed service can use it later.",
            )
            .prompt_skippable()?;
            match import {
                Some(true) => SecretChoice::ImportEnvironment {
                    key: secret_key,
                    value: env_value.to_string(),
                },
                Some(false) => manual_secret_choice(secret_key)?,
                None => return Ok(None),
            }
        }
        (None, None) => manual_secret_choice(secret_key)?,
    };

    Ok(Some(choice))
}

fn manual_secret_choice(secret_key: &'static str) -> Result<SecretChoice, AppError> {
    let entered = Password::new(&format!("Enter {secret_key}"))
        .with_help_message(
            "This will be stored in ~/.memory_bank/secrets.env for the managed service.",
        )
        .without_confirmation()
        .prompt_skippable()?;
    match entered {
        Some(value) if !value.trim().is_empty() => Ok(SecretChoice::ManualEntry {
            key: secret_key,
            value,
        }),
        Some(_) => Err(AppError::MissingProviderSecret(secret_key)),
        None => Err(AppError::SetupCanceled),
    }
}

fn prompt_advanced_settings(settings: &AppSettings) -> Result<Option<AdvancedAnswers>, AppError> {
    let current = AdvancedAnswers::from_settings(settings);

    let port = match CustomType::<u16>::new("Port")
        .with_default(current.port)
        .with_help_message("Local HTTP port for /mcp, /ingest, and /healthz.")
        .prompt_skippable()?
    {
        Some(value) => value,
        None => return Ok(None),
    };

    let fastembed_model = match Text::new("FastEmbed model override")
        .with_default(current.fastembed_model.as_str())
        .with_help_message(
            "Leave this at the default Jina model unless you know you want a different FastEmbed-compatible model.",
        )
        .prompt_skippable()?
    {
        Some(value) => value,
        None => return Ok(None),
    };

    let history_window_size = match CustomType::<u32>::new("History window size")
        .with_default(current.history_window_size)
        .with_help_message("0 means unlimited prior turns during memory analysis.")
        .prompt_skippable()?
    {
        Some(value) => value,
        None => return Ok(None),
    };

    let nearest_neighbor_count = match CustomType::<i32>::new("Nearest neighbor count")
        .with_default(current.nearest_neighbor_count)
        .with_help_message("How many nearest matches to load during recall and graph updates.")
        .with_validator(|value: &i32| {
            Ok(if *value >= 1 {
                Validation::Valid
            } else {
                Validation::Invalid("Nearest neighbor count must be at least 1".into())
            })
        })
        .prompt_skippable()?
    {
        Some(value) => value,
        None => return Ok(None),
    };

    Ok(Some(AdvancedAnswers {
        port,
        fastembed_model,
        history_window_size,
        nearest_neighbor_count,
    }))
}

fn build_settings_for_answers(
    current: &AppSettings,
    answers: &SetupAnswers,
    configured_agents: &[AgentKind],
) -> AppSettings {
    let mut settings = current.clone();
    settings.schema_version = SETTINGS_SCHEMA_VERSION;
    settings.active_namespace = if answers.namespace.as_ref() == DEFAULT_NAMESPACE_NAME {
        None
    } else {
        Some(answers.namespace.to_string())
    };

    let mut service = settings.service.clone().unwrap_or_default();
    service.autostart = answers.autostart.then_some(true);
    service.port = if answers.advanced.port == DEFAULT_PORT {
        None
    } else {
        Some(answers.advanced.port)
    };
    if service.is_empty() {
        settings.service = None;
    } else {
        settings.service = Some(service);
    }

    let mut server = settings.server.clone().unwrap_or_default();
    server.llm_provider = if answers.provider == "anthropic" {
        None
    } else {
        Some(answers.provider.clone())
    };
    let default_model = default_model_for_provider(&answers.provider);
    server.llm_model = if answers.model == default_model {
        None
    } else {
        Some(answers.model.clone())
    };
    server.ollama_url = if answers.provider == "ollama" {
        match answers.ollama_url.as_deref() {
            Some(url) if url != memory_bank_app::DEFAULT_OLLAMA_URL => Some(url.to_string()),
            _ => None,
        }
    } else {
        None
    };
    server.fastembed_model = if answers.advanced.fastembed_model == DEFAULT_FASTEMBED_MODEL {
        None
    } else {
        Some(answers.advanced.fastembed_model.clone())
    };
    server.history_window_size = if answers.advanced.history_window_size == 0 {
        None
    } else {
        Some(answers.advanced.history_window_size)
    };
    server.nearest_neighbor_count = if answers.advanced.nearest_neighbor_count == 10 {
        None
    } else {
        Some(answers.advanced.nearest_neighbor_count)
    };
    if server.is_empty() {
        settings.server = None;
    } else {
        settings.server = Some(server);
    }

    settings.integrations = Some(IntegrationsSettings {
        claude_code: Some(IntegrationState {
            configured: configured_agents.contains(&AgentKind::ClaudeCode),
        }),
        gemini_cli: Some(IntegrationState {
            configured: configured_agents.contains(&AgentKind::GeminiCli),
        }),
        opencode: Some(IntegrationState {
            configured: configured_agents.contains(&AgentKind::OpenCode),
        }),
        openclaw: Some(IntegrationState {
            configured: configured_agents.contains(&AgentKind::OpenClaw),
        }),
    });

    settings
}

fn apply_secret_choice(secrets: &mut SecretStore, choice: &SecretChoice) {
    match choice {
        SecretChoice::NotRequired | SecretChoice::KeepStored { .. } => {}
        SecretChoice::ImportEnvironment { key, value }
        | SecretChoice::ReplaceWithEnvironment { key, value }
        | SecretChoice::ManualEntry { key, value } => {
            secrets.set(*key, value.clone());
        }
    }
}

fn render_review_summary(answers: &SetupAnswers) -> String {
    let mut lines = vec![
        "Setup review".to_string(),
        format!("  Namespace: {}", answers.namespace),
        format!("  Provider: {}", ProviderChoice::from_config_value(Some(&answers.provider))),
        format!("  Model: {}", answers.model),
        format!("  Autostart: {}", yes_no(answers.autostart)),
        format!("  Agents: {}", render_agents_summary(&answers.selected_agents)),
        format!("  Secret: {}", answers.secret_choice.summary()),
    ];

    if let Some(url) = answers.ollama_url.as_deref() {
        lines.push(format!("  Ollama URL: {url}"));
    }

    let overrides = answers.advanced.override_lines();
    if !overrides.is_empty() {
        lines.push("  Advanced overrides:".to_string());
        for line in overrides {
            lines.push(format!("    {line}"));
        }
    }

    lines.join("\n")
}

fn render_agents_summary(agents: &[AgentKind]) -> String {
    if agents.is_empty() {
        "none selected".to_string()
    } else {
        agents
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl AdvancedAnswers {
    fn from_settings(settings: &AppSettings) -> Self {
        let server = settings.server.as_ref();
        Self {
            port: settings.resolved_port(),
            fastembed_model: server
                .and_then(|server| server.fastembed_model.clone())
                .unwrap_or_else(|| DEFAULT_FASTEMBED_MODEL.to_string()),
            history_window_size: server
                .and_then(|server| server.history_window_size)
                .unwrap_or(0),
            nearest_neighbor_count: server
                .and_then(|server| server.nearest_neighbor_count)
                .unwrap_or(10),
        }
    }

    fn has_overrides(&self) -> bool {
        self.port != DEFAULT_PORT
            || self.fastembed_model != DEFAULT_FASTEMBED_MODEL
            || self.history_window_size != 0
            || self.nearest_neighbor_count != 10
    }

    fn override_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if self.port != DEFAULT_PORT {
            lines.push(format!("Port: {}", self.port));
        }
        if self.fastembed_model != DEFAULT_FASTEMBED_MODEL {
            lines.push(format!("FastEmbed model: {}", self.fastembed_model));
        }
        if self.history_window_size != 0 {
            lines.push(format!(
                "History window size: {}",
                self.history_window_size
            ));
        }
        if self.nearest_neighbor_count != 10 {
            lines.push(format!(
                "Nearest neighbor count: {}",
                self.nearest_neighbor_count
            ));
        }
        lines
    }
}

impl SecretChoice {
    fn summary(&self) -> String {
        match self {
            Self::NotRequired => "Not required for Ollama".to_string(),
            Self::KeepStored { key } => {
                format!("Keep the existing {key} from ~/.memory_bank/secrets.env")
            }
            Self::ImportEnvironment { key, .. } => {
                format!("Import {key} from the current shell")
            }
            Self::ReplaceWithEnvironment { key, .. } => {
                format!("Replace the stored {key} with the current shell value")
            }
            Self::ManualEntry { key, .. } => {
                format!("Store a newly entered {key} in ~/.memory_bank/secrets.env")
            }
        }
    }
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
    let model_catalog_target = &paths.model_catalog_file;

    if opencode_target.exists() && openclaw_target.exists() && model_catalog_target.exists() {
        return Ok(());
    }

    let repo_root = find_repo_root().ok_or_else(|| {
        AppError::Message(
            "failed to locate repo assets for installation".to_string(),
        )
    })?;
    let opencode_source = repo_root.join(".opencode/plugins/memory-bank.js");
    let openclaw_source = repo_root.join(".openclaw/extensions/memory-bank");
    let model_catalog_source = repo_root.join("config/setup-model-catalog.json");

    if !opencode_source.exists() || !openclaw_source.exists() || !model_catalog_source.exists() {
        return Err(AppError::Message(
            "repo asset sources for installation are missing".to_string(),
        ));
    }

    copy_if_needed(&opencode_source, &opencode_target)?;
    copy_dir_recursive(&openclaw_source, &openclaw_target)?;
    copy_if_needed(&model_catalog_source, model_catalog_target)?;
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
    let permissions = source.metadata()?.permissions();
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
) -> Result<AgentSetupOutcome, AppError> {
    let server_url = default_server_url(settings);
    let mut configured = Vec::new();
    let mut warnings = Vec::new();

    for agent in selected_agents {
        let result = match agent {
            AgentKind::ClaudeCode => configure_claude(paths, &server_url),
            AgentKind::GeminiCli => configure_gemini(paths, &server_url),
            AgentKind::OpenCode => configure_opencode(paths, &server_url),
            AgentKind::OpenClaw => configure_openclaw(paths, &server_url),
        };
        match result {
            Ok(()) => configured.push(*agent),
            Err(error) => warnings.push(format!("{}: {}", agent.display_name(), error)),
        }
    }

    Ok(AgentSetupOutcome {
        configured,
        warnings,
    })
}

fn configure_claude(paths: &AppPaths, server_url: &str) -> Result<(), AppError> {
    ensure_claude_user_mcp(server_url)?;

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
        .entry("load".to_string())
        .or_insert_with(|| json!({}));
    upsert_openclaw_plugin_load_path(
        object_mut(plugins.get_mut("load").expect("load"))?,
        extension_path.to_string_lossy().as_ref(),
    )?;
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

fn wait_for_health(
    settings: &AppSettings,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<HealthCheck, AppError> {
    let deadline = Instant::now() + timeout;

    loop {
        match fetch_health(settings) {
            Ok(health) => return Ok(health),
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(AppError::Health(format!(
                        "service did not become healthy within {}s: {}",
                        timeout.as_secs(),
                        error
                    )));
                }
            }
        }

        thread::sleep(poll_interval);
    }
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
            if let Some(url) = server_settings.ollama_url {
                env.insert("MEMORY_BANK_OLLAMA_URL".to_string(), url);
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
        "server.ollama_url" => Ok(settings
            .server
            .as_ref()
            .and_then(|server| server.ollama_url.clone())
            .unwrap_or_else(|| memory_bank_app::DEFAULT_OLLAMA_URL.to_string())),
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
        "server.ollama_url" => {
            let mut server = settings.server.clone().unwrap_or_default();
            server.ollama_url =
                if value.is_empty() || value == memory_bank_app::DEFAULT_OLLAMA_URL {
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
    parse_json_config(&contents, path)
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
    let outcome = run_command_capture(program, args)?;
    if outcome.success {
        return Ok(());
    }

    Err(outcome.into_error())
}

fn run_command_capture(program: &str, args: &[&str]) -> Result<CommandOutcome, AppError> {
    let output = ProcessCommand::new(program).args(args).output().map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            AppError::MissingBinary(program.to_string())
        } else {
            AppError::Io(error)
        }
    })?;

    Ok(CommandOutcome {
        program: program.to_string(),
        args: args.iter().map(|value| (*value).to_string()).collect(),
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

fn shell_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', r#"'"'"'"#))
}

fn parse_json_config(contents: &str, path: &Path) -> Result<Value, AppError> {
    if contents.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }

    match serde_json::from_str(contents) {
        Ok(value) => Ok(value),
        Err(strict_error) => match parse_to_serde_value(contents, &jsonc_parse_options()) {
            Ok(Some(value)) => Ok(value),
            Ok(None) => Ok(Value::Object(Map::new())),
            Err(relaxed_error) => Err(AppError::Message(format!(
                "failed to parse {}: {} (also failed with JSONC parser: {})",
                path.display(),
                strict_error,
                relaxed_error
            ))),
        },
    }
}

fn jsonc_parse_options() -> ParseOptions {
    ParseOptions {
        allow_comments: true,
        allow_loose_object_property_names: false,
        allow_trailing_commas: true,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
    }
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

fn ensure_claude_user_mcp(server_url: &str) -> Result<(), AppError> {
    let desired_url = format!("{server_url}/mcp");
    let current = run_command_capture("claude", &["mcp", "get", "memory-bank"])?;

    if claude_mcp_matches(&current, &desired_url) {
        return Ok(());
    }

    if current.success {
        if claude_mcp_scope(&current).as_deref() == Some("user") {
            let removal = run_command_capture("claude", &["mcp", "remove", "memory-bank", "-s", "user"])?;
            if !removal.success {
                return Err(removal.into_error());
            }
        } else {
            return Err(AppError::Message(
                "Claude Code already has a conflicting `memory-bank` MCP server outside user scope; remove or rename that entry before rerunning setup".to_string(),
            ));
        }
    }

    let addition = run_command_capture(
        "claude",
        &[
            "mcp",
            "add",
            "--transport",
            "http",
            "--scope",
            "user",
            "memory-bank",
            &desired_url,
        ],
    )?;

    if !addition.success {
        let verify = run_command_capture("claude", &["mcp", "get", "memory-bank"])?;
        if claude_mcp_matches(&verify, &desired_url) {
            return Ok(());
        }
        return Err(addition.into_error());
    }

    let verify = run_command_capture("claude", &["mcp", "get", "memory-bank"])?;
    if claude_mcp_matches(&verify, &desired_url) {
        Ok(())
    } else {
        Err(AppError::Message(format!(
            "Claude Code did not report the expected user-scoped HTTP MCP config for memory-bank after setup. Expected URL: {desired_url}"
        )))
    }
}

fn claude_mcp_matches(outcome: &CommandOutcome, desired_url: &str) -> bool {
    outcome.success
        && claude_mcp_scope(outcome).as_deref() == Some("user")
        && outcome.combined_output().contains("Type: http")
        && outcome.combined_output().contains(desired_url)
}

fn claude_mcp_scope(outcome: &CommandOutcome) -> Option<String> {
    for line in outcome.combined_output().lines() {
        let trimmed = line.trim();
        if let Some(scope) = trimmed.strip_prefix("Scope:") {
            let scope = scope.trim().to_ascii_lowercase();
            if scope.starts_with("user") {
                return Some("user".to_string());
            }
            if scope.starts_with("project") || scope.starts_with("local") {
                return Some("project".to_string());
            }
            return Some(scope);
        }
    }
    None
}

fn upsert_openclaw_plugin_load_path(
    load_map: &mut Map<String, Value>,
    desired_path: &str,
) -> Result<(), AppError> {
    let desired = desired_path.to_string();
    let paths_value = load_map
        .entry("paths".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    let paths_array = array_mut(paths_value)?;

    paths_array.retain(|value| {
        let Some(path) = value.as_str() else {
            return true;
        };
        !(path.ends_with("/memory-bank") && path != desired)
    });

    if !paths_array
        .iter()
        .any(|value| value.as_str() == Some(desired.as_str()))
    {
        paths_array.push(Value::String(desired));
    }

    Ok(())
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

impl ModelCatalog {
    fn models_for_provider(&self, provider: &str) -> Vec<&str> {
        self.providers
            .get(provider)
            .map(|provider| {
                provider
                    .models
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }

    fn from_json(contents: &str) -> Result<Self, AppError> {
        let catalog: Self = serde_json::from_str(contents)?;
        Ok(catalog)
    }
}

fn refresh_model_catalog(paths: &AppPaths) -> ModelCatalog {
    if let Ok(catalog) = fetch_remote_model_catalog(paths) {
        return catalog;
    }

    if let Ok(catalog) = load_local_model_catalog(paths) {
        return catalog;
    }

    ModelCatalog::default()
}

fn fetch_remote_model_catalog(paths: &AppPaths) -> Result<ModelCatalog, AppError> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(2))
        .timeout_read(Duration::from_secs(3))
        .timeout_write(Duration::from_secs(3))
        .build();
    let response = agent.get(REMOTE_MODEL_CATALOG_URL).call().map_err(|error| {
        AppError::Message(format!("failed to fetch model catalog: {error}"))
    })?;
    let contents = response.into_string().map_err(|error| {
        AppError::Message(format!("failed to read remote model catalog: {error}"))
    })?;
    let catalog = ModelCatalog::from_json(&contents)?;
    paths.ensure_base_dirs()?;
    fs::write(&paths.model_catalog_file, format!("{contents}\n"))?;
    Ok(catalog)
}

fn load_local_model_catalog(paths: &AppPaths) -> Result<ModelCatalog, AppError> {
    let local_path = if paths.model_catalog_file.exists() {
        paths.model_catalog_file.clone()
    } else {
        find_repo_root()
            .map(|root| root.join("config/setup-model-catalog.json"))
            .ok_or_else(|| {
                AppError::Message(
                    "failed to locate a local model catalog fallback".to_string(),
                )
            })?
    };
    let contents = fs::read_to_string(&local_path)?;
    ModelCatalog::from_json(&contents)
}

fn fetch_ollama_models_for_setup(ollama_url: &str) -> Result<Vec<String>, AppError> {
    let tags_url = format!("{}/api/tags", ollama_url.trim_end_matches('/'));
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(2))
        .timeout_read(Duration::from_secs(3))
        .timeout_write(Duration::from_secs(3))
        .build();
    let response = agent.get(&tags_url).call().map_err(|error| {
        AppError::Message(format!("failed to load installed Ollama models: {error}"))
    })?;
    let tags = response.into_json::<OllamaTagsResponse>().map_err(|error| {
        AppError::Message(format!("failed to parse Ollama model list: {error}"))
    })?;

    let mut seen = std::collections::BTreeSet::new();
    let mut models = Vec::new();
    for model in tags.models {
        let display = ollama_display_name(&model.name);
        if seen.insert(display.clone()) {
            models.push(display);
        }
    }

    Ok(models)
}

fn ollama_display_name(model: &str) -> String {
    model.strip_suffix(":latest").unwrap_or(model).to_string()
}

fn model_choices_for_provider(
    provider: &str,
    current: Option<&str>,
    catalog: &ModelCatalog,
) -> Vec<ModelChoice> {
    model_choices_from_values(&catalog.models_for_provider(provider), current)
}

fn model_choices_from_values<S>(values: &[S], current: Option<&str>) -> Vec<ModelChoice>
where
    S: AsRef<str>,
{
    let mut choices = values
        .iter()
        .map(|model| ModelChoice::Preset(model.as_ref().to_string()))
        .collect::<Vec<_>>();

    if let Some(current_model) = current.filter(|value| !value.is_empty())
        && !values.iter().any(|model| model.as_ref() == current_model)
    {
        choices.push(ModelChoice::Current(current_model.to_string()));
    }

    choices.push(ModelChoice::Custom);
    choices
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

impl CommandOutcome {
    fn combined_output(&self) -> String {
        if self.stderr.is_empty() {
            self.stdout.clone()
        } else if self.stdout.is_empty() {
            self.stderr.clone()
        } else {
            format!("{}\n{}", self.stdout, self.stderr)
        }
    }

    fn into_error(self) -> AppError {
        let details = if self.stderr.is_empty() {
            self.stdout
        } else if self.stdout.is_empty() {
            self.stderr
        } else {
            format!("{}\n{}", self.stderr, self.stdout)
        };
        AppError::CommandFailed(format!("{} {}", self.program, self.args.join(" ")), details)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_bank_app::{ServerSettings, ServiceSettings};
    use tempfile::TempDir;

    fn test_model_catalog() -> ModelCatalog {
        ModelCatalog::from_json(
            r#"{
  "providers": {
    "anthropic": {
      "models": [
        "claude-opus-4-6",
        "claude-sonnet-4-6",
        "claude-haiku-4-5"
      ]
    },
    "open-ai": {
      "models": [
        "gpt-5.4",
        "gpt-5-mini"
      ]
    }
  }
}"#,
        )
        .expect("model catalog")
    }

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

    #[test]
    fn load_json_config_accepts_comments_and_trailing_commas() {
        let temp = TempDir::new().expect("tempdir");
        let config_path = temp.path().join("settings.json");
        fs::write(
            &config_path,
            r#"{
  // comment
  "hooks": {
    "Stop": [
      {
        "hooks": [
          { "command": "echo hi", },
        ],
      },
    ],
  },
}
"#,
        )
        .expect("write config");

        let value = load_json_config(&config_path).expect("load config");
        assert_eq!(
            value["hooks"]["Stop"][0]["hooks"][0]["command"],
            Value::String("echo hi".to_string())
        );
    }

    #[test]
    fn load_json_config_reports_path_on_parse_failure() {
        let temp = TempDir::new().expect("tempdir");
        let config_path = temp.path().join("broken.json");
        fs::write(&config_path, "{ nope").expect("write broken config");

        let error = load_json_config(&config_path).expect_err("expected parse failure");
        let message = error.to_string();
        assert!(message.contains("broken.json"));
        assert!(message.contains("failed to parse"));
    }

    #[test]
    fn claude_mcp_matches_expected_user_http_server() {
        let outcome = CommandOutcome {
            program: "claude".to_string(),
            args: vec!["mcp".to_string(), "get".to_string(), "memory-bank".to_string()],
            success: true,
            stdout: "memory-bank:\n  Scope: User config (available in all your projects)\n  Status: ✗ Failed to connect\n  Type: http\n  URL: http://127.0.0.1:3737/mcp\n".to_string(),
            stderr: String::new(),
        };

        assert!(claude_mcp_matches(
            &outcome,
            "http://127.0.0.1:3737/mcp"
        ));
    }

    #[test]
    fn upsert_openclaw_plugin_load_path_replaces_stale_memory_bank_paths() {
        let mut load_map = Map::new();
        load_map.insert(
            "paths".to_string(),
            json!([
                "/tmp/something-else",
                "/old/repo/.openclaw/extensions/memory-bank"
            ]),
        );

        upsert_openclaw_plugin_load_path(&mut load_map, "/Users/test/.memory_bank/integrations/openclaw/memory-bank")
            .expect("upsert load path");

        assert_eq!(
            load_map.get("paths").expect("paths"),
            &json!([
                "/tmp/something-else",
                "/Users/test/.memory_bank/integrations/openclaw/memory-bank"
            ])
        );
    }

    #[test]
    fn advanced_answers_default_from_settings_have_no_overrides() {
        let settings = AppSettings::default();
        let advanced = AdvancedAnswers::from_settings(&settings);

        assert_eq!(advanced.port, DEFAULT_PORT);
        assert_eq!(advanced.fastembed_model, DEFAULT_FASTEMBED_MODEL);
        assert_eq!(advanced.history_window_size, 0);
        assert_eq!(advanced.nearest_neighbor_count, 10);
        assert!(!advanced.has_overrides());
    }

    #[test]
    fn render_review_summary_hides_secret_value_and_omits_default_advanced() {
        let answers = SetupAnswers {
            namespace: Namespace::new("default"),
            provider: "anthropic".to_string(),
            model: DEFAULT_ANTHROPIC_MODEL.to_string(),
            ollama_url: None,
            autostart: true,
            selected_agents: vec![AgentKind::ClaudeCode, AgentKind::GeminiCli],
            secret_choice: SecretChoice::ManualEntry {
                key: "ANTHROPIC_API_KEY",
                value: "super-secret".to_string(),
            },
            advanced: AdvancedAnswers {
                port: DEFAULT_PORT,
                fastembed_model: DEFAULT_FASTEMBED_MODEL.to_string(),
                history_window_size: 0,
                nearest_neighbor_count: 10,
            },
        };

        let summary = render_review_summary(&answers);
        assert!(summary.contains("Store a newly entered ANTHROPIC_API_KEY"));
        assert!(!summary.contains("super-secret"));
        assert!(!summary.contains("Advanced overrides"));
    }

    #[test]
    fn build_settings_for_answers_applies_advanced_overrides() {
        let current = AppSettings::default();
        let answers = SetupAnswers {
            namespace: Namespace::new("work"),
            provider: "gemini".to_string(),
            model: "gemini-3.1-pro-preview".to_string(),
            ollama_url: None,
            autostart: true,
            selected_agents: vec![AgentKind::OpenCode],
            secret_choice: SecretChoice::NotRequired,
            advanced: AdvancedAnswers {
                port: 4545,
                fastembed_model: "custom/embed-model".to_string(),
                history_window_size: 25,
                nearest_neighbor_count: 15,
            },
        };

        let settings = build_settings_for_answers(&current, &answers, &[AgentKind::OpenCode]);
        let service = settings.service.expect("service settings");
        let server = settings.server.expect("server settings");
        let integrations = settings.integrations.expect("integrations");

        assert_eq!(settings.active_namespace.as_deref(), Some("work"));
        assert_eq!(service.port, Some(4545));
        assert_eq!(service.autostart, Some(true));
        assert_eq!(server.llm_provider.as_deref(), Some("gemini"));
        assert_eq!(server.llm_model.as_deref(), Some("gemini-3.1-pro-preview"));
        assert_eq!(server.fastembed_model.as_deref(), Some("custom/embed-model"));
        assert_eq!(server.history_window_size, Some(25));
        assert_eq!(server.nearest_neighbor_count, Some(15));
        assert_eq!(server.ollama_url, None);
        assert_eq!(
            integrations.opencode.as_ref().map(|state| state.configured),
            Some(true)
        );
        assert_eq!(
            integrations.claude_code.as_ref().map(|state| state.configured),
            Some(false)
        );
    }

    #[test]
    fn model_choices_include_current_saved_model_and_custom_fallback() {
        let catalog = test_model_catalog();
        let choices =
            model_choices_for_provider("anthropic", Some("claude-opus-custom"), &catalog);

        assert_eq!(
            choices,
            vec![
                ModelChoice::Preset("claude-opus-4-6".to_string()),
                ModelChoice::Preset("claude-sonnet-4-6".to_string()),
                ModelChoice::Preset("claude-haiku-4-5".to_string()),
                ModelChoice::Current("claude-opus-custom".to_string()),
                ModelChoice::Custom,
            ]
        );
    }

    #[test]
    fn load_local_model_catalog_reads_installed_copy() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        paths.ensure_base_dirs().expect("base dirs");
        fs::write(
            &paths.model_catalog_file,
            r#"{
  "providers": {
    "gemini": {
      "models": [
        "gemini-3.1-pro-preview",
        "gemini-3-flash-preview"
      ]
    }
  }
}"#,
        )
        .expect("write model catalog");

        let catalog = load_local_model_catalog(&paths).expect("load local catalog");

        assert_eq!(
            catalog.models_for_provider("gemini"),
            vec!["gemini-3.1-pro-preview", "gemini-3-flash-preview"]
        );
    }

    #[test]
    fn empty_model_catalog_still_offers_custom_entry() {
        let choices = model_choices_for_provider("ollama", None, &ModelCatalog::default());
        assert_eq!(choices, vec![ModelChoice::Custom]);
    }

    #[test]
    fn build_settings_for_ollama_answers_persists_non_default_url() {
        let answers = SetupAnswers {
            namespace: Namespace::new("default"),
            provider: "ollama".to_string(),
            model: "qwen3".to_string(),
            ollama_url: Some("http://192.168.1.50:11434".to_string()),
            autostart: false,
            selected_agents: Vec::new(),
            secret_choice: SecretChoice::NotRequired,
            advanced: AdvancedAnswers::from_settings(&AppSettings::default()),
        };

        let settings = build_settings_for_answers(&AppSettings::default(), &answers, &[]);
        let server = settings.server.expect("server settings");

        assert_eq!(server.llm_provider.as_deref(), Some("ollama"));
        assert_eq!(server.ollama_url.as_deref(), Some("http://192.168.1.50:11434"));
    }

    #[test]
    fn ollama_display_name_strips_latest_suffix() {
        assert_eq!(ollama_display_name("qwen3:latest"), "qwen3");
        assert_eq!(ollama_display_name("qwen3:8b"), "qwen3:8b");
    }
}
