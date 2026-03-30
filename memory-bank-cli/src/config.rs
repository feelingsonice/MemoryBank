use crate::AppError;
use crate::agents::AgentKind;
use crate::constants::{DEFAULT_HISTORY_WINDOW_SIZE, DEFAULT_NEAREST_NEIGHBOR_COUNT};
use crate::models::default_model_for_provider;
use memory_bank_app::{
    AppSettings, DEFAULT_FASTEMBED_MODEL, DEFAULT_NAMESPACE_NAME, DEFAULT_OLLAMA_URL,
    IntegrationState, IntegrationsSettings, Namespace, ServerSettings, ServiceSettings,
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
    ServerEncoderProvider,
    ServerFastembedModel,
    ServerHistoryWindowSize,
    ServerNearestNeighborCount,
    ServerLocalEncoderUrl,
    ServerRemoteEncoderUrl,
    IntegrationConfigured(AgentKind),
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
            "server.encoder_provider" => Ok(Self::ServerEncoderProvider),
            "server.fastembed_model" => Ok(Self::ServerFastembedModel),
            "server.history_window_size" => Ok(Self::ServerHistoryWindowSize),
            "server.nearest_neighbor_count" => Ok(Self::ServerNearestNeighborCount),
            "server.local_encoder_url" => Ok(Self::ServerLocalEncoderUrl),
            "server.remote_encoder_url" => Ok(Self::ServerRemoteEncoderUrl),
            "integrations.claude_code.configured" => {
                Ok(Self::IntegrationConfigured(AgentKind::ClaudeCode))
            }
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
        ConfigKey::ServerEncoderProvider => Ok(settings
            .server
            .as_ref()
            .and_then(|server| server.encoder_provider.clone())
            .unwrap_or_else(|| "fast-embed".to_string())),
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
        ConfigKey::IntegrationConfigured(agent) => Ok(settings
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
            .to_string()),
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
            server.llm_provider = if provider == "anthropic" {
                None
            } else {
                Some(provider.to_string())
            };
            if provider != "ollama" {
                server.ollama_url = None;
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
        ConfigKey::ServerEncoderProvider => {
            let provider = validate_encoder_provider(value.trim(), key)?;
            let mut server = settings.server.clone().unwrap_or_default();
            server.encoder_provider = if provider == "fast-embed" {
                None
            } else {
                Some(provider.to_string())
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
            let state = Some(IntegrationState { configured });
            match agent {
                AgentKind::ClaudeCode => integrations.claude_code = state,
                AgentKind::GeminiCli => integrations.gemini_cli = state,
                AgentKind::OpenCode => integrations.opencode = state,
                AgentKind::OpenClaw => integrations.openclaw = state,
            }
            set_integrations(settings, integrations);
        }
    }

    Ok(())
}

pub(crate) fn llm_provider_value(settings: &AppSettings) -> &str {
    settings
        .server
        .as_ref()
        .and_then(|server| server.llm_provider.as_deref())
        .unwrap_or("anthropic")
}

pub(crate) fn resolved_llm_model(settings: &AppSettings) -> String {
    settings
        .server
        .as_ref()
        .and_then(|server| server.llm_model.clone())
        .unwrap_or_else(|| default_model_for_provider(llm_provider_value(settings)).to_string())
}

pub(crate) fn resolved_ollama_url(current: Option<&str>) -> String {
    current
        .map(normalize_ollama_url)
        .unwrap_or_else(|| DEFAULT_OLLAMA_URL.to_string())
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

pub(crate) fn validate_llm_provider<'a>(value: &'a str, key: &str) -> Result<&'a str, AppError> {
    match value {
        "anthropic" | "gemini" | "open-ai" | "ollama" => Ok(value),
        _ => Err(AppError::InvalidConfigValue(
            key.to_string(),
            "expected one of: anthropic, gemini, open-ai, ollama".to_string(),
        )),
    }
}

pub(crate) fn validate_encoder_provider<'a>(
    value: &'a str,
    key: &str,
) -> Result<&'a str, AppError> {
    match value {
        "fast-embed" | "local-api" | "remote-api" => Ok(value),
        _ => Err(AppError::InvalidConfigValue(
            key.to_string(),
            "expected one of: fast-embed, local-api, remote-api".to_string(),
        )),
    }
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
}
