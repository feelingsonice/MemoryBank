use crate::AppError;
use crate::agents::AgentKind;
use crate::constants::{
    DEFAULT_HISTORY_WINDOW_SIZE, DEFAULT_MAX_PROCESSING_ATTEMPTS, DEFAULT_NEAREST_NEIGHBOR_COUNT,
};
use crate::domain::{
    EncoderProviderId, ProviderId, integration_configured, set_integration_configured,
};
use crate::models::default_model_for_provider;
use memory_bank_app::{
    AppSettings, DEFAULT_FASTEMBED_MODEL, DEFAULT_NAMESPACE_NAME, DEFAULT_OLLAMA_URL,
    DEFAULT_OPENAI_URL, IntegrationsSettings, Namespace, ServerSettings, ServiceSettings,
    format_openai_model_id, normalize_openai_url, normalize_openai_url_override,
};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigKey {
    SchemaVersion,
    ActiveNamespace,
    ServicePort,
    ServiceAutostart,
    ServerLlmProvider,
    ServerLlmModel,
    ServerOllamaUrl,
    ServerOpenAiUrl,
    ServerEncoderProvider,
    ServerFastembedModel,
    ServerHistoryWindowSize,
    ServerNearestNeighborCount,
    ServerMaxProcessingAttempts,
    ServerLocalEncoderUrl,
    ServerRemoteEncoderUrl,
    IntegrationConfigured(AgentKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FastEmbedReindexChange {
    pub(crate) previous_model: String,
    pub(crate) new_model: String,
}

impl FromStr for ConfigKey {
    type Err = AppError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "schema_version" => Ok(Self::SchemaVersion),
            "active_namespace" => Ok(Self::ActiveNamespace),
            "service.port" => Ok(Self::ServicePort),
            "service.autostart" => Ok(Self::ServiceAutostart),
            "server.llm_provider" => Ok(Self::ServerLlmProvider),
            "server.llm_model" => Ok(Self::ServerLlmModel),
            "server.ollama_url" => Ok(Self::ServerOllamaUrl),
            "server.openai_url" => Ok(Self::ServerOpenAiUrl),
            "server.encoder_provider" => Ok(Self::ServerEncoderProvider),
            "server.fastembed_model" => Ok(Self::ServerFastembedModel),
            "server.history_window_size" => Ok(Self::ServerHistoryWindowSize),
            "server.nearest_neighbor_count" => Ok(Self::ServerNearestNeighborCount),
            "server.max_processing_attempts" => Ok(Self::ServerMaxProcessingAttempts),
            "server.local_encoder_url" => Ok(Self::ServerLocalEncoderUrl),
            "server.remote_encoder_url" => Ok(Self::ServerRemoteEncoderUrl),
            "integrations.claude_code.configured" => {
                Ok(Self::IntegrationConfigured(AgentKind::ClaudeCode))
            }
            "integrations.codex.configured" => Ok(Self::IntegrationConfigured(AgentKind::Codex)),
            "integrations.gemini_cli.configured" => {
                Ok(Self::IntegrationConfigured(AgentKind::GeminiCli))
            }
            "integrations.opencode.configured" => {
                Ok(Self::IntegrationConfigured(AgentKind::OpenCode))
            }
            "integrations.openclaw.configured" => {
                Ok(Self::IntegrationConfigured(AgentKind::OpenClaw))
            }
            _ => Err(AppError::InvalidConfigKey(value.to_string())),
        }
    }
}

pub(crate) fn get_config_value(settings: &AppSettings, key: &str) -> Result<String, AppError> {
    match key.parse::<ConfigKey>()? {
        ConfigKey::SchemaVersion => Ok(settings.schema_version.to_string()),
        ConfigKey::ActiveNamespace => Ok(settings.active_namespace().to_string()),
        ConfigKey::ServicePort => Ok(settings.resolved_port().to_string()),
        ConfigKey::ServiceAutostart => Ok(settings.resolved_autostart().to_string()),
        ConfigKey::ServerLlmProvider => Ok(llm_provider_value(settings).to_string()),
        ConfigKey::ServerLlmModel => Ok(resolved_llm_model(settings)),
        ConfigKey::ServerOllamaUrl => Ok(resolved_ollama_url(
            settings
                .server
                .as_ref()
                .and_then(|server| server.ollama_url.as_deref()),
        )),
        ConfigKey::ServerOpenAiUrl => Ok(resolved_openai_url(
            settings
                .server
                .as_ref()
                .and_then(|server| server.openai_url.as_deref()),
        )?),
        ConfigKey::ServerEncoderProvider => Ok(resolved_encoder_provider(settings).to_string()),
        ConfigKey::ServerFastembedModel => Ok(settings
            .server
            .as_ref()
            .and_then(|server| server.fastembed_model.clone())
            .unwrap_or_else(|| DEFAULT_FASTEMBED_MODEL.to_string())),
        ConfigKey::ServerHistoryWindowSize => Ok(settings
            .server
            .as_ref()
            .and_then(|server| server.history_window_size)
            .unwrap_or(DEFAULT_HISTORY_WINDOW_SIZE)
            .to_string()),
        ConfigKey::ServerNearestNeighborCount => Ok(settings
            .server
            .as_ref()
            .and_then(|server| server.nearest_neighbor_count)
            .unwrap_or(DEFAULT_NEAREST_NEIGHBOR_COUNT)
            .to_string()),
        ConfigKey::ServerMaxProcessingAttempts => Ok(settings
            .server
            .as_ref()
            .and_then(|server| server.max_processing_attempts)
            .unwrap_or(DEFAULT_MAX_PROCESSING_ATTEMPTS)
            .to_string()),
        ConfigKey::ServerLocalEncoderUrl => Ok(settings
            .server
            .as_ref()
            .and_then(|server| server.local_encoder_url.clone())
            .unwrap_or_default()),
        ConfigKey::ServerRemoteEncoderUrl => Ok(settings
            .server
            .as_ref()
            .and_then(|server| server.remote_encoder_url.clone())
            .unwrap_or_default()),
        ConfigKey::IntegrationConfigured(agent) => {
            Ok(integration_configured(settings.integrations.as_ref(), agent).to_string())
        }
    }
}

pub(crate) fn set_config_value(
    settings: &mut AppSettings,
    key: &str,
    value: &str,
) -> Result<(), AppError> {
    match key.parse::<ConfigKey>()? {
        ConfigKey::SchemaVersion => return Err(AppError::InvalidConfigKey(key.to_string())),
        ConfigKey::ActiveNamespace => {
            let namespace = Namespace::new(value);
            settings.active_namespace = if namespace.as_ref() == DEFAULT_NAMESPACE_NAME {
                None
            } else {
                Some(namespace.to_string())
            };
        }
        ConfigKey::ServicePort => {
            let port: u16 = value.trim().parse::<u16>().map_err(|error| {
                AppError::InvalidConfigValue(key.to_string(), error.to_string())
            })?;
            if port == 0 {
                return Err(AppError::InvalidConfigValue(
                    key.to_string(),
                    "must be between 1 and 65535".to_string(),
                ));
            }
            let mut service = settings.service.clone().unwrap_or_default();
            service.port = (port != memory_bank_app::DEFAULT_PORT).then_some(port);
            set_service(settings, service);
        }
        ConfigKey::ServiceAutostart => {
            let autostart = parse_bool(value, key)?;
            let mut service = settings.service.clone().unwrap_or_default();
            service.autostart = autostart.then_some(true);
            set_service(settings, service);
        }
        ConfigKey::ServerLlmProvider => {
            let provider = validate_llm_provider(value.trim(), key)?;
            let mut server = settings.server.clone().unwrap_or_default();
            server.llm_provider = if provider == ProviderId::Anthropic {
                None
            } else {
                Some(provider.as_str().to_string())
            };
            if provider != ProviderId::Ollama {
                server.ollama_url = None;
            }
            if provider != ProviderId::OpenAi {
                server.openai_url = None;
            }
            set_server(settings, server);
        }
        ConfigKey::ServerLlmModel => {
            let provider = llm_provider_value(settings);
            let mut server = settings.server.clone().unwrap_or_default();
            server.llm_model = normalize_optional_string(value).and_then(|value| {
                if value == default_model_for_provider(provider) {
                    None
                } else {
                    Some(value)
                }
            });
            set_server(settings, server);
        }
        ConfigKey::ServerOllamaUrl => {
            let mut server = settings.server.clone().unwrap_or_default();
            server.ollama_url = normalize_optional_string(value).and_then(|value| {
                let normalized = normalize_ollama_url(&value);
                if normalized == DEFAULT_OLLAMA_URL {
                    None
                } else {
                    Some(normalized)
                }
            });
            set_server(settings, server);
        }
        ConfigKey::ServerOpenAiUrl => {
            let mut server = settings.server.clone().unwrap_or_default();
            server.openai_url = normalize_optional_string(value)
                .map(|value| {
                    normalize_openai_url_override(&value).map_err(|error| {
                        AppError::InvalidConfigValue(key.to_string(), error.to_string())
                    })
                })
                .transpose()?
                .flatten();
            set_server(settings, server);
        }
        ConfigKey::ServerEncoderProvider => {
            let provider = validate_encoder_provider(value.trim(), key)?;
            let mut server = settings.server.clone().unwrap_or_default();
            server.encoder_provider = if provider == EncoderProviderId::FastEmbed {
                None
            } else {
                Some(provider.as_str().to_string())
            };
            set_server(settings, server);
        }
        ConfigKey::ServerFastembedModel => {
            let mut server = settings.server.clone().unwrap_or_default();
            server.fastembed_model = normalize_optional_string(value).and_then(|value| {
                if value == DEFAULT_FASTEMBED_MODEL {
                    None
                } else {
                    Some(value)
                }
            });
            set_server(settings, server);
        }
        ConfigKey::ServerHistoryWindowSize => {
            let parsed: u32 = value.trim().parse::<u32>().map_err(|error| {
                AppError::InvalidConfigValue(key.to_string(), error.to_string())
            })?;
            let mut server = settings.server.clone().unwrap_or_default();
            server.history_window_size = (parsed != DEFAULT_HISTORY_WINDOW_SIZE).then_some(parsed);
            set_server(settings, server);
        }
        ConfigKey::ServerNearestNeighborCount => {
            let parsed: i32 = value.trim().parse::<i32>().map_err(|error| {
                AppError::InvalidConfigValue(key.to_string(), error.to_string())
            })?;
            if parsed < 1 {
                return Err(AppError::InvalidConfigValue(
                    key.to_string(),
                    "must be at least 1".to_string(),
                ));
            }
            let mut server = settings.server.clone().unwrap_or_default();
            server.nearest_neighbor_count =
                (parsed != DEFAULT_NEAREST_NEIGHBOR_COUNT).then_some(parsed);
            set_server(settings, server);
        }
        ConfigKey::ServerMaxProcessingAttempts => {
            let parsed: u32 = value.trim().parse::<u32>().map_err(|error| {
                AppError::InvalidConfigValue(key.to_string(), error.to_string())
            })?;
            if parsed < 1 {
                return Err(AppError::InvalidConfigValue(
                    key.to_string(),
                    "must be at least 1".to_string(),
                ));
            }
            let mut server = settings.server.clone().unwrap_or_default();
            server.max_processing_attempts =
                (parsed != DEFAULT_MAX_PROCESSING_ATTEMPTS).then_some(parsed);
            set_server(settings, server);
        }
        ConfigKey::ServerLocalEncoderUrl => {
            let mut server = settings.server.clone().unwrap_or_default();
            server.local_encoder_url = normalize_optional_string(value);
            set_server(settings, server);
        }
        ConfigKey::ServerRemoteEncoderUrl => {
            let mut server = settings.server.clone().unwrap_or_default();
            server.remote_encoder_url = normalize_optional_string(value);
            set_server(settings, server);
        }
        ConfigKey::IntegrationConfigured(agent) => {
            let configured = parse_bool(value, key)?;
            let mut integrations = settings.integrations.clone().unwrap_or_default();
            set_integration_configured(&mut integrations, agent, configured);
            set_integrations(settings, integrations);
        }
    }

    Ok(())
}

pub(crate) fn llm_provider_value(settings: &AppSettings) -> &str {
    llm_provider(settings).as_str()
}

pub(crate) fn llm_provider(settings: &AppSettings) -> ProviderId {
    ProviderId::from_config_value(
        settings
            .server
            .as_ref()
            .and_then(|server| server.llm_provider.as_deref()),
    )
}

pub(crate) fn resolved_encoder_provider(settings: &AppSettings) -> &str {
    encoder_provider(settings).as_str()
}

pub(crate) fn encoder_provider(settings: &AppSettings) -> EncoderProviderId {
    EncoderProviderId::from_config_value(
        settings
            .server
            .as_ref()
            .and_then(|server| server.encoder_provider.as_deref()),
    )
}

pub(crate) fn resolved_llm_model(settings: &AppSettings) -> String {
    settings
        .server
        .as_ref()
        .and_then(|server| server.llm_model.clone())
        .unwrap_or_else(|| default_model_for_provider(llm_provider_value(settings)).to_string())
}

pub(crate) fn resolved_llm_model_id(settings: &AppSettings) -> Result<String, AppError> {
    let model = resolved_llm_model(settings);
    Ok(match llm_provider(settings) {
        ProviderId::Anthropic => format!("Anthropic::{model}"),
        ProviderId::Gemini => format!("Gemini::{model}"),
        ProviderId::OpenAi => format_openai_model_id(
            &model,
            &resolved_openai_url(
                settings
                    .server
                    .as_ref()
                    .and_then(|server| server.openai_url.as_deref()),
            )?,
        ),
        ProviderId::Ollama => format!(
            "Ollama::{model}@{}",
            resolved_ollama_url(
                settings
                    .server
                    .as_ref()
                    .and_then(|server| server.ollama_url.as_deref()),
            )
        ),
    })
}

pub(crate) fn resolved_fastembed_model(settings: &AppSettings) -> String {
    settings
        .server
        .as_ref()
        .and_then(|server| server.fastembed_model.clone())
        .unwrap_or_else(|| DEFAULT_FASTEMBED_MODEL.to_string())
}

pub(crate) fn fastembed_reindex_change(
    current: &AppSettings,
    updated: &AppSettings,
) -> Option<FastEmbedReindexChange> {
    if resolved_encoder_provider(updated) != EncoderProviderId::FastEmbed.as_str() {
        return None;
    }

    let previous_model = resolved_fastembed_model(current);
    let new_model = resolved_fastembed_model(updated);
    if previous_model == new_model {
        None
    } else {
        Some(FastEmbedReindexChange {
            previous_model,
            new_model,
        })
    }
}

pub(crate) fn resolved_ollama_url(current: Option<&str>) -> String {
    current
        .map(normalize_ollama_url)
        .unwrap_or_else(|| DEFAULT_OLLAMA_URL.to_string())
}

pub(crate) fn resolved_openai_url(current: Option<&str>) -> Result<String, AppError> {
    match current {
        Some(value) => normalize_openai_url(value).map_err(|error| {
            AppError::InvalidConfigValue("server.openai_url".to_string(), error.to_string())
        }),
        None => Ok(DEFAULT_OPENAI_URL.to_string()),
    }
}

pub(crate) fn normalize_ollama_url(value: &str) -> String {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        DEFAULT_OLLAMA_URL.to_string()
    } else {
        trimmed.to_string()
    }
}

pub(crate) fn normalize_optional_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(crate) fn validate_llm_provider(value: &str, key: &str) -> Result<ProviderId, AppError> {
    ProviderId::parse(value, key)
}

pub(crate) fn validate_encoder_provider(
    value: &str,
    key: &str,
) -> Result<EncoderProviderId, AppError> {
    EncoderProviderId::parse(value, key)
}

pub(crate) fn set_service(settings: &mut AppSettings, service: ServiceSettings) {
    settings.service = (!service.is_empty()).then_some(service);
}

pub(crate) fn set_server(settings: &mut AppSettings, server: ServerSettings) {
    settings.server = (!server.is_empty()).then_some(server);
}

pub(crate) fn set_integrations(settings: &mut AppSettings, integrations: IntegrationsSettings) {
    settings.integrations = (!integrations.is_empty()).then_some(integrations);
}

fn parse_bool(value: &str, key: &str) -> Result<bool, AppError> {
    value
        .trim()
        .parse::<bool>()
        .map_err(|error| AppError::InvalidConfigValue(key.to_string(), error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_bank_app::ServerSettings;

    #[test]
    fn config_get_uses_provider_specific_default_model() {
        let settings = AppSettings {
            server: Some(ServerSettings {
                llm_provider: Some("gemini".to_string()),
                ..ServerSettings::default()
            }),
            ..AppSettings::default()
        };

        let model = get_config_value(&settings, "server.llm_model").expect("model value");

        assert_eq!(model, memory_bank_app::DEFAULT_GEMINI_MODEL);
    }

    #[test]
    fn fastembed_reindex_change_detects_saved_model_change() {
        let current = AppSettings::default();
        let updated = AppSettings {
            server: Some(ServerSettings {
                fastembed_model: Some("custom/embed-model".to_string()),
                ..ServerSettings::default()
            }),
            ..AppSettings::default()
        };

        assert_eq!(
            fastembed_reindex_change(&current, &updated),
            Some(FastEmbedReindexChange {
                previous_model: DEFAULT_FASTEMBED_MODEL.to_string(),
                new_model: "custom/embed-model".to_string(),
            })
        );
    }

    #[test]
    fn fastembed_reindex_change_ignores_unchanged_effective_model() {
        let current = AppSettings::default();
        let updated = AppSettings {
            server: Some(ServerSettings {
                fastembed_model: Some(DEFAULT_FASTEMBED_MODEL.to_string()),
                ..ServerSettings::default()
            }),
            ..AppSettings::default()
        };

        assert_eq!(fastembed_reindex_change(&current, &updated), None);
    }

    #[test]
    fn fastembed_reindex_change_ignores_non_fastembed_provider() {
        let current = AppSettings::default();
        let updated = AppSettings {
            server: Some(ServerSettings {
                encoder_provider: Some("local-api".to_string()),
                fastembed_model: Some("custom/embed-model".to_string()),
                ..ServerSettings::default()
            }),
            ..AppSettings::default()
        };

        assert_eq!(fastembed_reindex_change(&current, &updated), None);
    }

    #[test]
    fn config_set_rejects_invalid_provider_and_zero_port() {
        let mut settings = AppSettings::default();

        let provider_error = set_config_value(&mut settings, "server.llm_provider", "wat")
            .expect_err("invalid provider should fail");
        assert!(provider_error.to_string().contains("expected one of"));

        let port_error = set_config_value(&mut settings, "service.port", "0")
            .expect_err("zero port should fail");
        assert!(port_error.to_string().contains("1 and 65535"));
    }

    #[test]
    fn config_set_trims_and_normalizes_model_and_ollama_url() {
        let mut settings = AppSettings {
            server: Some(ServerSettings {
                llm_provider: Some("ollama".to_string()),
                ..ServerSettings::default()
            }),
            ..AppSettings::default()
        };

        set_config_value(&mut settings, "server.llm_model", "  qwen3  ").expect("set model");
        set_config_value(
            &mut settings,
            "server.ollama_url",
            "  http://localhost:11434/  ",
        )
        .expect("set ollama url");

        let server = settings.server.expect("server settings");
        assert_eq!(server.llm_model, None);
        assert_eq!(server.ollama_url, None);
    }

    #[test]
    fn config_get_uses_default_openai_url_when_unset() {
        let settings = AppSettings::default();

        let openai_url = get_config_value(&settings, "server.openai_url").expect("openai url");

        assert_eq!(openai_url, DEFAULT_OPENAI_URL);
    }

    #[test]
    fn config_set_round_trips_openai_url_and_clears_default() {
        let mut settings = AppSettings::default();

        set_config_value(
            &mut settings,
            "server.openai_url",
            " https://opencode.ai/zen/v1/ ",
        )
        .expect("set openai url");
        assert_eq!(
            get_config_value(&settings, "server.openai_url").expect("get openai url"),
            "https://opencode.ai/zen/v1"
        );
        assert_eq!(
            settings
                .server
                .as_ref()
                .and_then(|server| server.openai_url.as_deref()),
            Some("https://opencode.ai/zen/v1")
        );

        set_config_value(&mut settings, "server.openai_url", DEFAULT_OPENAI_URL)
            .expect("reset openai url");
        assert_eq!(
            get_config_value(&settings, "server.openai_url").expect("get default openai url"),
            DEFAULT_OPENAI_URL
        );
        assert!(settings.server.is_none());
    }

    #[test]
    fn config_set_default_values_clear_overrides_and_sections() {
        let mut settings = AppSettings::default();

        set_config_value(&mut settings, "active_namespace", "work").expect("set namespace");
        set_config_value(&mut settings, "service.port", "4545").expect("set port");
        set_config_value(&mut settings, "service.port", "3737").expect("reset port");
        set_config_value(&mut settings, "active_namespace", " default ").expect("reset namespace");

        assert_eq!(settings.active_namespace, None);
        assert!(settings.service.is_none());
    }

    #[test]
    fn config_set_switching_away_from_ollama_clears_saved_ollama_url() {
        let mut settings = AppSettings {
            server: Some(ServerSettings {
                llm_provider: Some("ollama".to_string()),
                ollama_url: Some("http://ollama.internal:11434".to_string()),
                ..ServerSettings::default()
            }),
            ..AppSettings::default()
        };

        set_config_value(&mut settings, "server.llm_provider", "anthropic").expect("set provider");

        assert!(settings.server.is_none());
    }

    #[test]
    fn config_set_switching_away_from_openai_clears_saved_openai_url() {
        let mut settings = AppSettings {
            server: Some(ServerSettings {
                llm_provider: Some("open-ai".to_string()),
                openai_url: Some("https://opencode.ai/zen/v1".to_string()),
                ..ServerSettings::default()
            }),
            ..AppSettings::default()
        };

        set_config_value(&mut settings, "server.llm_provider", "anthropic").expect("set provider");

        assert!(settings.server.is_none());
    }

    #[test]
    fn resolved_llm_model_id_includes_custom_openai_endpoint() {
        let settings = AppSettings {
            server: Some(ServerSettings {
                llm_provider: Some("open-ai".to_string()),
                llm_model: Some("qwen3.6-plus-free".to_string()),
                openai_url: Some("https://opencode.ai/zen/v1".to_string()),
                ..ServerSettings::default()
            }),
            ..AppSettings::default()
        };

        let model_id = resolved_llm_model_id(&settings).expect("llm model id");

        assert_eq!(
            model_id,
            "OpenAi::qwen3.6-plus-free@https://opencode.ai/zen/v1"
        );
    }

    #[test]
    fn config_set_integration_flags_round_trip() {
        let mut settings = AppSettings::default();

        set_config_value(&mut settings, "integrations.codex.configured", "true")
            .expect("set integration");
        assert_eq!(
            get_config_value(&settings, "integrations.codex.configured").expect("get integration"),
            "true"
        );

        set_config_value(&mut settings, "integrations.codex.configured", "false")
            .expect("unset integration");
        assert_eq!(
            get_config_value(&settings, "integrations.codex.configured").expect("get integration"),
            "false"
        );
    }

    #[test]
    fn config_set_rejects_invalid_bool_and_neighbor_count() {
        let mut settings = AppSettings::default();

        let bool_error = set_config_value(&mut settings, "service.autostart", "sometimes")
            .expect_err("invalid bool");
        assert!(bool_error.to_string().contains("provided string was not"));

        let count_error = set_config_value(&mut settings, "server.nearest_neighbor_count", "0")
            .expect_err("invalid count");
        assert!(count_error.to_string().contains("at least 1"));
    }

    #[test]
    fn config_get_uses_default_max_processing_attempts_when_unset() {
        let settings = AppSettings::default();

        let attempts =
            get_config_value(&settings, "server.max_processing_attempts").expect("attempts");

        assert_eq!(attempts, DEFAULT_MAX_PROCESSING_ATTEMPTS.to_string());
    }

    #[test]
    fn config_set_round_trips_max_processing_attempts_and_clears_default() {
        let mut settings = AppSettings::default();

        set_config_value(&mut settings, "server.max_processing_attempts", "12")
            .expect("set attempts");
        assert_eq!(
            get_config_value(&settings, "server.max_processing_attempts").expect("get attempts"),
            "12"
        );
        assert_eq!(
            settings
                .server
                .as_ref()
                .and_then(|server| server.max_processing_attempts),
            Some(12)
        );

        set_config_value(
            &mut settings,
            "server.max_processing_attempts",
            &DEFAULT_MAX_PROCESSING_ATTEMPTS.to_string(),
        )
        .expect("reset attempts");
        assert_eq!(
            get_config_value(&settings, "server.max_processing_attempts").expect("get default"),
            DEFAULT_MAX_PROCESSING_ATTEMPTS.to_string()
        );
        assert!(settings.server.is_none());
    }

    #[test]
    fn config_set_rejects_zero_max_processing_attempts() {
        let mut settings = AppSettings::default();

        let attempts_error = set_config_value(&mut settings, "server.max_processing_attempts", "0")
            .expect_err("invalid max attempts");
        assert!(attempts_error.to_string().contains("at least 1"));
    }
}
