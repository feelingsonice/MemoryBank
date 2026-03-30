use crate::AppError;
use crate::agents::{
    AgentKind, AgentSetupOutcome, configure_selected_agents, detect_installed_agents,
};
use crate::assets::{ensure_path_entry, materialize_install_artifacts};
use crate::command_utils::yes_no;
use crate::config::{normalize_ollama_url, set_integrations, set_server, set_service};
use crate::constants::{
    DEFAULT_HISTORY_WINDOW_SIZE, DEFAULT_NEAREST_NEIGHBOR_COUNT, HEALTH_POLL_INTERVAL,
    HEALTH_STARTUP_TIMEOUT,
};
use crate::models::{
    ModelCatalog, ModelChoice, default_model_for_provider, fetch_ollama_models_for_setup,
    model_choices_for_provider, model_choices_from_values, refresh_model_catalog,
};
use crate::service::{HealthCheck, install_service, start_service, wait_for_health};
use inquire::ui::{Color, RenderConfig, StyleSheet, Styled};
use inquire::validator::Validation;
use inquire::{Confirm, CustomType, MultiSelect, Password, Select, Text, set_global_render_config};
use memory_bank_app::{
    AppPaths, AppSettings, DEFAULT_FASTEMBED_MODEL, DEFAULT_NAMESPACE_NAME, DEFAULT_PORT,
    IntegrationState, IntegrationsSettings, Namespace, SETTINGS_SCHEMA_VERSION, SecretStore,
    env_key_for_provider,
};
use std::fmt;
use std::io::{self, IsTerminal, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProviderChoice {
    Anthropic,
    Gemini,
    OpenAi,
    Ollama,
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
    KeepStored { key: &'static str },
    ImportEnvironment { key: &'static str, value: String },
    ReplaceWithEnvironment { key: &'static str, value: String },
    ManualEntry { key: &'static str, value: String },
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

impl fmt::Display for ProviderChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Anthropic => "Anthropic",
            Self::Gemini => "Gemini",
            Self::OpenAi => "OpenAI",
            Self::Ollama => "Ollama (local)",
        };
        f.write_str(label)
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
                .unwrap_or(DEFAULT_HISTORY_WINDOW_SIZE),
            nearest_neighbor_count: server
                .and_then(|server| server.nearest_neighbor_count)
                .unwrap_or(DEFAULT_NEAREST_NEIGHBOR_COUNT),
        }
    }

    fn has_overrides(&self) -> bool {
        self.port != DEFAULT_PORT
            || self.fastembed_model != DEFAULT_FASTEMBED_MODEL
            || self.history_window_size != DEFAULT_HISTORY_WINDOW_SIZE
            || self.nearest_neighbor_count != DEFAULT_NEAREST_NEIGHBOR_COUNT
    }

    fn override_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if self.port != DEFAULT_PORT {
            lines.push(format!("Port: {}", self.port));
        }
        if self.fastembed_model != DEFAULT_FASTEMBED_MODEL {
            lines.push(format!("FastEmbed model: {}", self.fastembed_model));
        }
        if self.history_window_size != DEFAULT_HISTORY_WINDOW_SIZE {
            lines.push(format!("History window size: {}", self.history_window_size));
        }
        if self.nearest_neighbor_count != DEFAULT_NEAREST_NEIGHBOR_COUNT {
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

pub(crate) fn run_setup() -> Result<(), AppError> {
    ensure_interactive_terminal()?;
    configure_setup_rendering();

    let paths = AppPaths::from_system()?;
    let model_catalog = refresh_model_catalog(&paths);
    let mut settings = AppSettings::load(&paths)?;
    let mut secrets = SecretStore::load(&paths)?;
    let detected_agents = detect_installed_agents();
    let answers = match collect_setup_answers(&settings, &secrets, &detected_agents, &model_catalog)
    {
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

    let (health, agent_outcome) =
        apply_setup_answers(&paths, &mut settings, &mut secrets, &answers)?;
    println!();
    println!(
        "Memory Bank is ready on {} using namespace `{}` and provider `{}`.",
        memory_bank_app::default_server_url(&settings),
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
    println!(
        "Selected agents: {}",
        render_agents_summary(&selected_agents)
    );

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
        let outcome =
            configure_selected_agents(paths, &preview_settings, &answers.selected_agents)?;
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
    apply_step(5, total_steps, "Start managed service", || {
        start_service(paths)
    })?;
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

fn prompt_namespace(current: Namespace) -> Result<Option<Namespace>, AppError> {
    let default_value = current.to_string();
    let value = Text::new("Active namespace")
        .with_default(default_value.as_str())
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
    let value = Text::new("Ollama URL")
        .with_default(current.unwrap_or(memory_bank_app::DEFAULT_OLLAMA_URL))
        .with_help_message(
            "Memory Bank will query this Ollama daemon for the local models you already have installed.",
        )
        .with_placeholder("http://localhost:11434")
        .with_validator(|value: &str| {
            Ok(if value.trim().is_empty() {
                Validation::Invalid("Ollama URL cannot be empty".into())
            } else {
                Validation::Valid
            })
        })
        .prompt_skippable()
        .map_err(AppError::from)?;

    Ok(value.map(|value| normalize_ollama_url(&value)))
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
        .and_then(|value| {
            choices
                .iter()
                .position(|choice| choice.value() == Some(value))
        })
        .unwrap_or(0);
    let prompt = format!(
        "Model for {}",
        ProviderChoice::from_config_value(Some(provider))
    );
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
        ModelChoice::Preset(model) | ModelChoice::Current(model) => Ok(Some(model)),
        ModelChoice::Custom => {
            let default_model = current
                .map(str::to_owned)
                .unwrap_or_else(|| default_model_for_provider(provider).to_string());
            let value = Text::new("Custom model string")
                .with_default(default_model.as_str())
                .with_help_message("Enter the exact model ID for the selected provider.")
                .with_validator(|value: &str| {
                    Ok(if value.trim().is_empty() {
                        Validation::Invalid("Model ID cannot be empty".into())
                    } else {
                        Validation::Valid
                    })
                })
                .prompt_skippable()?;
            Ok(value.map(|value| value.trim().to_string()))
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
                .and_then(|value| {
                    choices
                        .iter()
                        .position(|choice| choice.value() == Some(value))
                })
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
        .with_validator(|value: &str| {
            Ok(if value.trim().is_empty() {
                Validation::Invalid("Model name cannot be empty".into())
            } else {
                Validation::Valid
            })
        })
        .prompt_skippable()
        .map(|value| value.map(|value| value.trim().to_string()))
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
        .with_validator(|value: &str| {
            Ok(if value.trim().is_empty() {
                Validation::Invalid("Secret value cannot be empty".into())
            } else {
                Validation::Valid
            })
        })
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
        .with_validator(|value: &u16| {
            Ok(if *value == 0 {
                Validation::Invalid("Port must be between 1 and 65535".into())
            } else {
                Validation::Valid
            })
        })
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
        .with_validator(|value: &str| {
            Ok(if value.trim().is_empty() {
                Validation::Invalid("FastEmbed model cannot be empty".into())
            } else {
                Validation::Valid
            })
        })
        .prompt_skippable()?
    {
        Some(value) => value.trim().to_string(),
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
    service.port = (answers.advanced.port != DEFAULT_PORT).then_some(answers.advanced.port);
    set_service(&mut settings, service);

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
    server.history_window_size = (answers.advanced.history_window_size
        != DEFAULT_HISTORY_WINDOW_SIZE)
        .then_some(answers.advanced.history_window_size);
    server.nearest_neighbor_count = (answers.advanced.nearest_neighbor_count
        != DEFAULT_NEAREST_NEIGHBOR_COUNT)
        .then_some(answers.advanced.nearest_neighbor_count);
    set_server(&mut settings, server);

    let current_integrations = current.integrations.as_ref();
    set_integrations(
        &mut settings,
        IntegrationsSettings {
            claude_code: Some(IntegrationState {
                configured: integration_status_for(
                    current_integrations,
                    configured_agents,
                    AgentKind::ClaudeCode,
                ),
            }),
            gemini_cli: Some(IntegrationState {
                configured: integration_status_for(
                    current_integrations,
                    configured_agents,
                    AgentKind::GeminiCli,
                ),
            }),
            opencode: Some(IntegrationState {
                configured: integration_status_for(
                    current_integrations,
                    configured_agents,
                    AgentKind::OpenCode,
                ),
            }),
            openclaw: Some(IntegrationState {
                configured: integration_status_for(
                    current_integrations,
                    configured_agents,
                    AgentKind::OpenClaw,
                ),
            }),
        },
    );

    settings
}

fn integration_status_for(
    current: Option<&IntegrationsSettings>,
    configured_agents: &[AgentKind],
    agent: AgentKind,
) -> bool {
    if configured_agents.contains(&agent) {
        return true;
    }

    current_integration_status(current, agent)
}

fn current_integration_status(current: Option<&IntegrationsSettings>, agent: AgentKind) -> bool {
    current
        .and_then(|integrations| match agent {
            AgentKind::ClaudeCode => integrations.claude_code.as_ref(),
            AgentKind::GeminiCli => integrations.gemini_cli.as_ref(),
            AgentKind::OpenCode => integrations.opencode.as_ref(),
            AgentKind::OpenClaw => integrations.openclaw.as_ref(),
        })
        .map(|state| state.configured)
        .unwrap_or(false)
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
        format!(
            "  Provider: {}",
            ProviderChoice::from_config_value(Some(&answers.provider))
        ),
        format!("  Model: {}", answers.model),
        format!("  Autostart: {}", yes_no(answers.autostart)),
        format!(
            "  Agents: {}",
            render_agents_summary(&answers.selected_agents)
        ),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advanced_answers_default_from_settings_have_no_overrides() {
        let settings = AppSettings::default();
        let advanced = AdvancedAnswers::from_settings(&settings);

        assert_eq!(advanced.port, DEFAULT_PORT);
        assert_eq!(advanced.fastembed_model, DEFAULT_FASTEMBED_MODEL);
        assert_eq!(advanced.history_window_size, DEFAULT_HISTORY_WINDOW_SIZE);
        assert_eq!(
            advanced.nearest_neighbor_count,
            DEFAULT_NEAREST_NEIGHBOR_COUNT
        );
        assert!(!advanced.has_overrides());
    }

    #[test]
    fn render_review_summary_hides_secret_value_and_omits_default_advanced() {
        let answers = SetupAnswers {
            namespace: Namespace::new("default"),
            provider: "anthropic".to_string(),
            model: memory_bank_app::DEFAULT_ANTHROPIC_MODEL.to_string(),
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
                history_window_size: DEFAULT_HISTORY_WINDOW_SIZE,
                nearest_neighbor_count: DEFAULT_NEAREST_NEIGHBOR_COUNT,
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
        assert_eq!(
            server.fastembed_model.as_deref(),
            Some("custom/embed-model")
        );
        assert_eq!(server.history_window_size, Some(25));
        assert_eq!(server.nearest_neighbor_count, Some(15));
        assert_eq!(server.ollama_url, None);
        assert_eq!(
            integrations.opencode.as_ref().map(|state| state.configured),
            Some(true)
        );
        assert_eq!(
            integrations
                .claude_code
                .as_ref()
                .map(|state| state.configured),
            Some(false)
        );
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
        assert_eq!(
            server.ollama_url.as_deref(),
            Some("http://192.168.1.50:11434")
        );
    }

    #[test]
    fn build_settings_for_answers_preserves_unselected_integrations() {
        let current = AppSettings {
            integrations: Some(IntegrationsSettings {
                claude_code: Some(IntegrationState { configured: true }),
                gemini_cli: Some(IntegrationState { configured: false }),
                opencode: Some(IntegrationState { configured: true }),
                openclaw: Some(IntegrationState { configured: true }),
            }),
            ..AppSettings::default()
        };
        let answers = SetupAnswers {
            namespace: Namespace::new("default"),
            provider: "anthropic".to_string(),
            model: memory_bank_app::DEFAULT_ANTHROPIC_MODEL.to_string(),
            ollama_url: None,
            autostart: false,
            selected_agents: vec![AgentKind::GeminiCli],
            secret_choice: SecretChoice::NotRequired,
            advanced: AdvancedAnswers::from_settings(&AppSettings::default()),
        };

        let settings = build_settings_for_answers(&current, &answers, &[AgentKind::GeminiCli]);
        let integrations = settings.integrations.expect("integrations");

        assert_eq!(
            integrations
                .claude_code
                .as_ref()
                .map(|state| state.configured),
            Some(true)
        );
        assert_eq!(
            integrations
                .gemini_cli
                .as_ref()
                .map(|state| state.configured),
            Some(true)
        );
        assert_eq!(
            integrations.opencode.as_ref().map(|state| state.configured),
            Some(true)
        );
        assert_eq!(
            integrations.openclaw.as_ref().map(|state| state.configured),
            Some(true)
        );
    }

    #[test]
    fn build_settings_for_default_answers_clear_default_provider_and_model() {
        let answers = SetupAnswers {
            namespace: Namespace::new("default"),
            provider: "anthropic".to_string(),
            model: memory_bank_app::DEFAULT_ANTHROPIC_MODEL.to_string(),
            ollama_url: None,
            autostart: false,
            selected_agents: Vec::new(),
            secret_choice: SecretChoice::NotRequired,
            advanced: AdvancedAnswers::from_settings(&AppSettings::default()),
        };

        let settings = build_settings_for_answers(&AppSettings::default(), &answers, &[]);

        assert_eq!(settings.active_namespace, None);
        assert!(settings.server.is_none());
        assert!(settings.service.is_none());
    }

    #[test]
    fn build_settings_switching_from_ollama_clears_saved_ollama_url() {
        let current = AppSettings {
            server: Some(memory_bank_app::ServerSettings {
                llm_provider: Some("ollama".to_string()),
                ollama_url: Some("http://ollama.internal:11434".to_string()),
                ..memory_bank_app::ServerSettings::default()
            }),
            ..AppSettings::default()
        };
        let answers = SetupAnswers {
            namespace: Namespace::new("default"),
            provider: "gemini".to_string(),
            model: memory_bank_app::DEFAULT_GEMINI_MODEL.to_string(),
            ollama_url: None,
            autostart: false,
            selected_agents: Vec::new(),
            secret_choice: SecretChoice::NotRequired,
            advanced: AdvancedAnswers::from_settings(&AppSettings::default()),
        };

        let settings = build_settings_for_answers(&current, &answers, &[]);
        let server = settings.server.expect("server settings");

        assert_eq!(server.llm_provider.as_deref(), Some("gemini"));
        assert_eq!(server.ollama_url, None);
    }

    #[test]
    fn apply_secret_choice_only_mutates_store_when_needed() {
        let mut secrets = SecretStore::default();
        secrets.set("ANTHROPIC_API_KEY", "stored");

        apply_secret_choice(
            &mut secrets,
            &SecretChoice::KeepStored {
                key: "ANTHROPIC_API_KEY",
            },
        );
        assert_eq!(secrets.get("ANTHROPIC_API_KEY"), Some("stored"));

        apply_secret_choice(
            &mut secrets,
            &SecretChoice::ReplaceWithEnvironment {
                key: "ANTHROPIC_API_KEY",
                value: "updated".to_string(),
            },
        );
        assert_eq!(secrets.get("ANTHROPIC_API_KEY"), Some("updated"));
    }

    #[test]
    fn render_agents_summary_handles_empty_and_multiple_values() {
        assert_eq!(render_agents_summary(&[]), "none selected");
        assert_eq!(
            render_agents_summary(&[AgentKind::ClaudeCode, AgentKind::OpenClaw]),
            "Claude Code, OpenClaw"
        );
    }
}
