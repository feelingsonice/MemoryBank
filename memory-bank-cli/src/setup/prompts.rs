use crate::AppError;
use crate::agents::AgentKind;
use crate::config::normalize_ollama_url;
use crate::domain::ProviderId;
use crate::models::{
    ModelCatalog, ModelChoice, fetch_ollama_models_for_setup, model_choices_for_provider,
    model_choices_from_values,
};
use crate::output::{no_color_requested, styled_subtle, styled_warning};
use inquire::ui::{Attributes, Color, RenderConfig, StyleSheet, Styled};
use inquire::validator::Validation;
use inquire::{Confirm, CustomType, MultiSelect, Select, Text, set_global_render_config};
use memory_bank_app::{
    AppSettings, DEFAULT_OLLAMA_URL, DEFAULT_OPENAI_URL, Namespace, SecretStore,
    normalize_openai_url,
};
use std::io::{self, IsTerminal};

use super::plan::{AdvancedSettings, SecretChoice, SetupPlan};
use super::render::{print_setup_intro, print_setup_section};

const NO_SUPPORTED_AGENTS_MESSAGE: &str = "No supported agents were detected on PATH. You can rerun `mb setup` later after installing Claude Code, Codex, Gemini CLI, OpenCode, or OpenClaw.";

#[derive(Debug, Clone, PartialEq, Eq)]
enum SecretPromptPlan {
    NotRequired,
    OfferEnvironment { key: &'static str, value: String },
    OfferStored { key: &'static str },
    ManualEntry { key: &'static str },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WizardStep<T> {
    Continue(T),
    Canceled,
}

impl<T> WizardStep<T> {
    fn from_option(value: Option<T>) -> Self {
        match value {
            Some(value) => Self::Continue(value),
            None => Self::Canceled,
        }
    }

    fn into_result(self) -> Result<T, AppError> {
        match self {
            Self::Continue(value) => Ok(value),
            Self::Canceled => Err(AppError::SetupCanceled),
        }
    }
}

pub(super) fn ensure_interactive_terminal() -> Result<(), AppError> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(AppError::Message(
            "mb setup requires an interactive terminal. Run it in a TTY or use `mb config` for manual changes.".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn configure_setup_rendering() {
    set_global_render_config(setup_render_config());
}

fn setup_render_config() -> RenderConfig<'static> {
    let mut config = if no_color_requested() {
        RenderConfig::empty()
    } else {
        RenderConfig::default_colored()
    };
    config.prompt_prefix = Styled::new("mb>")
        .with_fg(Color::LightBlue)
        .with_attr(Attributes::BOLD);
    config.answered_prompt_prefix = Styled::new("->")
        .with_fg(Color::LightGreen)
        .with_attr(Attributes::BOLD);
    config.highlighted_option_prefix = Styled::new(">")
        .with_fg(Color::LightCyan)
        .with_attr(Attributes::BOLD);
    config.selected_checkbox = Styled::new("[x]").with_fg(Color::LightGreen);
    config.unselected_checkbox = Styled::new("[ ]").with_fg(Color::DarkGrey);
    config.prompt = StyleSheet::new().with_attr(Attributes::BOLD);
    config.help_message = StyleSheet::new().with_fg(Color::DarkGrey);
    config.answer = StyleSheet::new()
        .with_fg(Color::LightCyan)
        .with_attr(Attributes::BOLD);
    config.selected_option = Some(StyleSheet::new().with_fg(Color::LightBlue));
    config
}

pub(super) fn collect_setup_plan(
    settings: &AppSettings,
    secrets: &SecretStore,
    detected_agents: &[AgentKind],
    model_catalog: &ModelCatalog,
) -> Result<SetupPlan, AppError> {
    print_setup_intro();

    print_setup_section("Basic");
    let namespace =
        prompt_namespace(settings.active_namespace()).and_then(WizardStep::into_result)?;

    print_setup_section("LLM configuration");
    let current_provider = settings
        .server
        .as_ref()
        .and_then(|server| server.llm_provider.as_deref());
    let provider = prompt_provider(current_provider).and_then(WizardStep::into_result)?;
    let current_ollama_url = settings
        .server
        .as_ref()
        .and_then(|server| server.ollama_url.as_deref());
    let current_model = saved_model_for_selected_provider(
        current_provider,
        provider,
        settings
            .server
            .as_ref()
            .and_then(|server| server.llm_model.as_deref()),
    );
    let (ollama_url, model) = if provider == ProviderId::Ollama {
        let ollama_url = prompt_ollama_url(current_ollama_url).and_then(WizardStep::into_result)?;
        let model = prompt_ollama_model(current_model, &ollama_url, model_catalog)
            .and_then(WizardStep::into_result)?;
        (Some(ollama_url), model)
    } else {
        let model = prompt_model(provider, current_model, model_catalog)
            .and_then(WizardStep::into_result)?;
        (None, model)
    };
    let secret_choice =
        collect_secret_choice(provider, secrets).and_then(WizardStep::into_result)?;

    print_setup_section("Preferences");
    let autostart = prompt_autostart(
        settings
            .service
            .as_ref()
            .and_then(|service| service.autostart),
    )
    .and_then(WizardStep::into_result)?;

    print_setup_section("Agent integrations");
    println!(
        "{}",
        styled_subtle("Choose one or more agents to configure in this setup run.")
    );
    let selected_agents = prompt_agents(detected_agents).and_then(WizardStep::into_result)?;

    let mut advanced = AdvancedSettings::from_settings(settings);
    let has_existing_advanced = advanced.has_overrides();
    print_setup_section("Advanced settings");
    let configure_advanced = WizardStep::from_option(
        Confirm::new("Configure advanced settings?")
            .with_default(has_existing_advanced)
            .with_help_message(
                "Most users can skip this. You can change these later with `mb config` if needed.",
            )
            .prompt_skippable()?,
    )
    .into_result()?;

    if configure_advanced {
        advanced =
            prompt_advanced_settings(settings, provider).and_then(WizardStep::into_result)?;
    }

    Ok(SetupPlan {
        namespace,
        provider,
        model,
        ollama_url,
        autostart,
        selected_agents,
        secret_choice,
        advanced,
    })
}

fn prompt_namespace(current: Namespace) -> Result<WizardStep<Namespace>, AppError> {
    let default_value = current.to_string();
    Ok(WizardStep::from_option(
        Text::new("Active namespace")
            .with_default(default_value.as_str())
            .with_help_message(
                "This is the user-level memory space the managed service will run against.",
            )
            .with_placeholder("default")
            .prompt_skippable()?
            .map(Namespace::new),
    ))
}

fn prompt_provider(current: Option<&str>) -> Result<WizardStep<ProviderId>, AppError> {
    let current_provider = ProviderId::from_config_value(current);
    let options = ProviderId::ALL.to_vec();
    let help_message = provider_prompt_help(current_provider);
    let default_index = options
        .iter()
        .position(|choice| *choice == current_provider)
        .unwrap_or(0);
    Ok(WizardStep::from_option(
        Select::new("LLM provider:", options)
            .with_starting_cursor(default_index)
            .with_page_size(4)
            .with_help_message(&help_message)
            .prompt_skippable()?,
    ))
}

fn provider_prompt_help(current_provider: ProviderId) -> String {
    format!(
        "Currently configured: {}. This powers Memory Bank's internal memory analysis, not the coding agent you use directly.",
        current_provider
    )
}

fn saved_model_for_selected_provider<'a>(
    current_provider: Option<&str>,
    selected_provider: ProviderId,
    current_model: Option<&'a str>,
) -> Option<&'a str> {
    (ProviderId::from_config_value(current_provider) == selected_provider)
        .then_some(current_model)
        .flatten()
}

fn prompt_ollama_url(current: Option<&str>) -> Result<WizardStep<String>, AppError> {
    Ok(WizardStep::from_option(
        Text::new("Ollama URL")
            .with_default(current.unwrap_or(DEFAULT_OLLAMA_URL))
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
            .prompt_skippable()?
            .map(|value| normalize_ollama_url(&value)),
    ))
}

fn prompt_openai_url(current: Option<&str>) -> Result<WizardStep<String>, AppError> {
    Ok(WizardStep::from_option(
        Text::new("OpenAI base URL override")
            .with_default(current.unwrap_or(DEFAULT_OPENAI_URL))
            .with_help_message(
                "Leave this at the default unless you are routing Memory Bank through an OpenAI-compatible endpoint. Custom endpoints may also require a custom model string chosen earlier.",
            )
            .with_placeholder("https://api.openai.com/v1")
            .with_validator(|value: &str| {
                Ok(match normalize_openai_url(value) {
                    Ok(_) => Validation::Valid,
                    Err(error) => Validation::Invalid(error.to_string().into()),
                })
            })
            .prompt_skippable()?
            .map(|value| {
                normalize_openai_url(&value).expect("validator should normalize OpenAI URL")
            }),
    ))
}

fn prompt_model(
    provider: ProviderId,
    current: Option<&str>,
    catalog: &ModelCatalog,
) -> Result<WizardStep<String>, AppError> {
    let choices = model_choices_for_provider(provider.as_str(), current, catalog);
    let preferred = current
        .filter(|value| !value.is_empty())
        .or_else(|| Some(provider.default_model()));
    let default_index = preferred
        .and_then(|value| {
            choices
                .iter()
                .position(|choice| choice.value() == Some(value))
        })
        .unwrap_or(0);
    let prompt = format!("Model for {}:", provider);
    let selection = Select::new(&prompt, choices)
        .with_starting_cursor(default_index)
        .with_page_size(8)
        .with_help_message(
            "Choose a popular model ID for this provider. If you need a different one, pick the custom option and type it exactly.",
        )
        .prompt_skippable()?;

    let Some(selection) = selection else {
        return Ok(WizardStep::Canceled);
    };

    match selection {
        ModelChoice::Preset(model) | ModelChoice::Current(model) => Ok(WizardStep::Continue(model)),
        ModelChoice::Custom => Ok(WizardStep::from_option(
            Text::new("Custom model string")
                .with_default(current.unwrap_or(provider.default_model()))
                .with_help_message("Enter the exact model ID for the selected provider.")
                .with_validator(|value: &str| {
                    Ok(if value.trim().is_empty() {
                        Validation::Invalid("Model ID cannot be empty".into())
                    } else {
                        Validation::Valid
                    })
                })
                .prompt_skippable()?
                .map(|value| value.trim().to_string()),
        )),
    }
}

fn prompt_ollama_model(
    current: Option<&str>,
    ollama_url: &str,
    catalog: &ModelCatalog,
) -> Result<WizardStep<String>, AppError> {
    match fetch_ollama_models_for_setup(ollama_url) {
        Ok(models) if !models.is_empty() => {
            let choices = model_choices_from_values(&models, current);
            let preferred = current
                .filter(|value| !value.is_empty())
                .or_else(|| Some(ProviderId::Ollama.default_model()));
            let default_index = preferred
                .and_then(|value| {
                    choices
                        .iter()
                        .position(|choice| choice.value() == Some(value))
                })
                .unwrap_or(0);
            let selection = Select::new("Model for Ollama (installed locally):", choices)
                .with_starting_cursor(default_index)
                .with_page_size(10)
                .with_help_message(
                    "These models were discovered from your Ollama daemon. If yours is missing, choose the custom option.",
                )
                .prompt_skippable()?;

            let Some(selection) = selection else {
                return Ok(WizardStep::Canceled);
            };

            match selection {
                ModelChoice::Preset(model) | ModelChoice::Current(model) => {
                    Ok(WizardStep::Continue(model))
                }
                ModelChoice::Custom => prompt_custom_ollama_model(current, catalog),
            }
        }
        Ok(_) => {
            println!(
                "{}",
                styled_warning(&format!(
                    "No local Ollama models were detected at {}.",
                    ollama_url.trim_end_matches('/')
                ))
            );
            prompt_custom_ollama_model(current, catalog)
        }
        Err(error) => {
            println!(
                "{}",
                styled_warning(&format!("Could not query Ollama at {ollama_url}: {error}"))
            );
            prompt_custom_ollama_model(current, catalog)
        }
    }
}

fn prompt_custom_ollama_model(
    current: Option<&str>,
    catalog: &ModelCatalog,
) -> Result<WizardStep<String>, AppError> {
    let suggestions = catalog.models_for_provider("ollama");
    let help = if suggestions.is_empty() {
        "Enter the local Ollama model name you want Memory Bank to use."
    } else {
        "Enter the local Ollama model name you want Memory Bank to use. Common pulls: qwen3, deepseek-r1, llama3.1, qwen2.5-coder."
    };
    Ok(WizardStep::from_option(
        Text::new("Ollama model name:")
            .with_default(current.unwrap_or(ProviderId::Ollama.default_model()))
            .with_help_message(help)
            .with_validator(|value: &str| {
                Ok(if value.trim().is_empty() {
                    Validation::Invalid("Model name cannot be empty".into())
                } else {
                    Validation::Valid
                })
            })
            .prompt_skippable()?
            .map(|value| value.trim().to_string()),
    ))
}

fn prompt_autostart(current: Option<bool>) -> Result<WizardStep<bool>, AppError> {
    Ok(WizardStep::from_option(
        Confirm::new("Start Memory Bank automatically on login?")
            .with_default(current.unwrap_or(true))
            .with_help_message("This installs a user-scoped background service for Memory Bank.")
            .prompt_skippable()?,
    ))
}

fn prompt_agents(detected: &[AgentKind]) -> Result<WizardStep<Vec<AgentKind>>, AppError> {
    if detected.is_empty() {
        println!("{}", styled_warning(NO_SUPPORTED_AGENTS_MESSAGE));
        return Ok(WizardStep::Continue(Vec::new()));
    }

    Ok(WizardStep::from_option(
        MultiSelect::new(
            "Select which detected agents to configure now",
            detected.to_vec(),
        )
        .with_all_selected_by_default()
        .with_page_size(detected.len().min(7))
        .with_help_message(
            "Use Space to toggle the highlighted agent. Press Enter to continue with all checked agents.",
        )
        .prompt_skippable()?,
    ))
}

fn collect_secret_choice(
    provider: ProviderId,
    secrets: &SecretStore,
) -> Result<WizardStep<SecretChoice>, AppError> {
    let plan = secret_prompt_plan(
        provider,
        provider
            .secret_env_key()
            .and_then(|key| std::env::var(key).ok()),
        provider
            .secret_env_key()
            .and_then(|key| secrets.get(key).map(str::to_owned)),
    );

    match plan {
        SecretPromptPlan::NotRequired => Ok(WizardStep::Continue(SecretChoice::NotRequired)),
        SecretPromptPlan::OfferEnvironment { key, value } => {
            let use_env = WizardStep::from_option(
                Confirm::new(&format!(
                    "Store and use the current shell {key} for Memory Bank?"
                ))
                .with_default(true)
                .with_help_message(
                    "This writes the key to ~/.memory_bank/secrets.env so the managed service uses the same provider secret every time it starts.",
                )
                .prompt_skippable()?,
            )
            .into_result()?;
            if use_env {
                Ok(WizardStep::Continue(SecretChoice::UseEnvironment {
                    key,
                    value,
                }))
            } else {
                manual_secret_choice(key)
            }
        }
        SecretPromptPlan::OfferStored { key } => {
            let keep = WizardStep::from_option(
                Confirm::new(&format!(
                    "Keep using the stored {key} from ~/.memory_bank/secrets.env?"
                ))
                .with_default(true)
                .with_help_message(
                    "Answer no if you want to replace it with a different key during this setup run.",
                )
                .prompt_skippable()?,
            )
            .into_result()?;
            if keep {
                Ok(WizardStep::Continue(SecretChoice::KeepStored { key }))
            } else {
                manual_secret_choice(key)
            }
        }
        SecretPromptPlan::ManualEntry { key } => manual_secret_choice(key),
    }
}

fn secret_prompt_plan(
    provider: ProviderId,
    env_value: Option<String>,
    stored_value: Option<String>,
) -> SecretPromptPlan {
    let Some(secret_key) = provider.secret_env_key() else {
        return SecretPromptPlan::NotRequired;
    };

    match (
        env_value.filter(|value| !value.trim().is_empty()),
        stored_value.filter(|value| !value.trim().is_empty()),
    ) {
        (Some(value), _) => SecretPromptPlan::OfferEnvironment {
            key: secret_key,
            value,
        },
        (None, Some(_)) => SecretPromptPlan::OfferStored { key: secret_key },
        (None, None) => SecretPromptPlan::ManualEntry { key: secret_key },
    }
}

fn manual_secret_choice(secret_key: &'static str) -> Result<WizardStep<SecretChoice>, AppError> {
    match Text::new(&format!("Enter {secret_key}:"))
        .with_help_message(
            "This will be stored in ~/.memory_bank/secrets.env for the managed service. Input is shown as you type.",
        )
        .with_validator(|value: &str| {
            Ok(if value.trim().is_empty() {
                Validation::Invalid("Secret value cannot be empty".into())
            } else {
                Validation::Valid
            })
        })
        .prompt_skippable()?
    {
        Some(value) if !value.trim().is_empty() => Ok(WizardStep::Continue(
            SecretChoice::ManualEntry {
                key: secret_key,
                value: value.trim().to_string(),
            },
        )),
        Some(_) => Err(AppError::MissingProviderSecret(secret_key)),
        None => Ok(WizardStep::Canceled),
    }
}

fn prompt_advanced_settings(
    settings: &AppSettings,
    provider: ProviderId,
) -> Result<WizardStep<AdvancedSettings>, AppError> {
    let current = AdvancedSettings::from_settings(settings);

    let port = WizardStep::from_option(
        CustomType::<u16>::new("Port")
            .with_default(current.port)
            .with_help_message("Local HTTP port for /mcp, /ingest, and /healthz.")
            .with_validator(|value: &u16| {
                Ok(if *value == 0 {
                    Validation::Invalid("Port must be between 1 and 65535".into())
                } else {
                    Validation::Valid
                })
            })
            .prompt_skippable()?,
    )
    .into_result()?;

    let openai_url = if provider == ProviderId::OpenAi {
        Some(prompt_openai_url(current.openai_url.as_deref()).and_then(WizardStep::into_result)?)
    } else {
        None
    };

    let fastembed_model = WizardStep::from_option(
        Text::new("FastEmbed model override")
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
            .prompt_skippable()?,
    )
    .into_result()?
    .trim()
    .to_string();

    let history_window_size = WizardStep::from_option(
        CustomType::<u32>::new("History window size")
            .with_default(current.history_window_size)
            .with_help_message(
                "For non-Ollama providers, the default is 0 (unlimited). Ollama always uses 5.",
            )
            .prompt_skippable()?,
    )
    .into_result()?;

    let nearest_neighbor_count = WizardStep::from_option(
        CustomType::<i32>::new("Nearest neighbor count")
            .with_default(current.nearest_neighbor_count)
            .with_help_message("How many nearest matches to load during recall and graph updates.")
            .with_validator(|value: &i32| {
                Ok(if *value >= 1 {
                    Validation::Valid
                } else {
                    Validation::Invalid("Nearest neighbor count must be at least 1".into())
                })
            })
            .prompt_skippable()?,
    )
    .into_result()?;

    let max_processing_attempts = WizardStep::from_option(
        CustomType::<u32>::new("Max processing attempts")
            .with_default(current.max_processing_attempts)
            .with_help_message(
                "How many retryable finalized-turn processing failures Memory Bank will allow before marking the turn exhausted.",
            )
            .with_validator(|value: &u32| {
                Ok(if *value >= 1 {
                    Validation::Valid
                } else {
                    Validation::Invalid("Max processing attempts must be at least 1".into())
                })
            })
            .prompt_skippable()?,
    )
    .into_result()?;

    Ok(WizardStep::Continue(AdvancedSettings {
        port,
        openai_url,
        fastembed_model,
        history_window_size,
        nearest_neighbor_count,
        max_processing_attempts,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command_utils::yes_no;

    #[test]
    fn no_supported_agents_message_mentions_codex() {
        assert!(NO_SUPPORTED_AGENTS_MESSAGE.contains("Codex"));
    }

    #[test]
    fn provider_prompt_help_mentions_current_provider() {
        let help = provider_prompt_help(ProviderId::Ollama);

        assert!(help.contains("Currently configured: Ollama"));
        assert!(help.contains("internal memory analysis"));
    }

    #[test]
    fn saved_model_is_only_reused_when_provider_matches() {
        assert_eq!(
            saved_model_for_selected_provider(
                Some("ollama"),
                ProviderId::Ollama,
                Some("qwen3.5:35b-a3b-coding-nvfp4")
            ),
            Some("qwen3.5:35b-a3b-coding-nvfp4")
        );
        assert_eq!(
            saved_model_for_selected_provider(
                Some("ollama"),
                ProviderId::OpenAi,
                Some("qwen3.5:35b-a3b-coding-nvfp4")
            ),
            None
        );
    }

    #[test]
    fn secret_prompt_plan_prefers_shell_key_over_stored_secret() {
        let plan = secret_prompt_plan(
            ProviderId::Anthropic,
            Some("from-shell".to_string()),
            Some("stored".to_string()),
        );

        assert_eq!(
            plan,
            SecretPromptPlan::OfferEnvironment {
                key: "ANTHROPIC_API_KEY",
                value: "from-shell".to_string()
            }
        );
    }

    #[test]
    fn secret_prompt_plan_uses_stored_secret_when_shell_key_is_missing() {
        let plan = secret_prompt_plan(ProviderId::Gemini, None, Some("stored".to_string()));

        assert_eq!(
            plan,
            SecretPromptPlan::OfferStored {
                key: "GEMINI_API_KEY"
            }
        );
    }

    #[test]
    fn secret_prompt_plan_requires_manual_entry_when_no_secret_exists() {
        let plan = secret_prompt_plan(ProviderId::OpenAi, None, None);

        assert_eq!(
            plan,
            SecretPromptPlan::ManualEntry {
                key: "OPENAI_API_KEY"
            }
        );
    }

    #[test]
    fn secret_prompt_plan_ignores_blank_shell_and_stored_values() {
        let plan = secret_prompt_plan(
            ProviderId::Gemini,
            Some("   ".to_string()),
            Some(String::new()),
        );

        assert_eq!(
            plan,
            SecretPromptPlan::ManualEntry {
                key: "GEMINI_API_KEY"
            }
        );
    }

    #[test]
    fn secret_prompt_plan_skips_secret_flow_for_ollama() {
        let plan = secret_prompt_plan(ProviderId::Ollama, Some("ignored".to_string()), None);

        assert_eq!(plan, SecretPromptPlan::NotRequired);
    }

    #[test]
    fn wizard_step_maps_prompt_skips_to_setup_canceled() {
        let error = WizardStep::<String>::Canceled
            .into_result()
            .expect_err("skip should cancel");

        assert!(matches!(error, AppError::SetupCanceled));
    }

    #[test]
    fn yes_no_still_matches_existing_wording() {
        assert_eq!(yes_no(true), "yes");
        assert_eq!(yes_no(false), "no");
    }
}
