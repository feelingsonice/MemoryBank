use crate::AppError;
use crate::agents::AgentKind;
use memory_bank_app::{
    DEFAULT_ANTHROPIC_MODEL, DEFAULT_GEMINI_MODEL, DEFAULT_OLLAMA_MODEL, DEFAULT_OPENAI_MODEL,
    IntegrationState, IntegrationsSettings,
};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProviderId {
    Anthropic,
    Gemini,
    OpenAi,
    Ollama,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EncoderProviderId {
    FastEmbed,
    LocalApi,
    RemoteApi,
}

impl ProviderId {
    pub(crate) const ALL: [Self; 4] = [Self::Anthropic, Self::Gemini, Self::OpenAi, Self::Ollama];

    pub(crate) fn from_config_value(value: Option<&str>) -> Self {
        match value {
            Some("gemini") => Self::Gemini,
            Some("open-ai") => Self::OpenAi,
            Some("ollama") => Self::Ollama,
            _ => Self::Anthropic,
        }
    }

    pub(crate) fn parse(value: &str, key: &str) -> Result<Self, AppError> {
        match value {
            "anthropic" => Ok(Self::Anthropic),
            "gemini" => Ok(Self::Gemini),
            "open-ai" => Ok(Self::OpenAi),
            "ollama" => Ok(Self::Ollama),
            _ => Err(AppError::InvalidConfigValue(
                key.to_string(),
                format!("expected one of: {}", Self::allowed_values()),
            )),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::OpenAi => "open-ai",
            Self::Ollama => "ollama",
        }
    }

    pub(crate) fn display_name(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic",
            Self::Gemini => "Gemini",
            Self::OpenAi => "OpenAI",
            Self::Ollama => "Ollama (local)",
        }
    }

    pub(crate) fn default_model(self) -> &'static str {
        match self {
            Self::Anthropic => DEFAULT_ANTHROPIC_MODEL,
            Self::Gemini => DEFAULT_GEMINI_MODEL,
            Self::OpenAi => DEFAULT_OPENAI_MODEL,
            Self::Ollama => DEFAULT_OLLAMA_MODEL,
        }
    }

    pub(crate) fn secret_env_key(self) -> Option<&'static str> {
        match self {
            Self::Anthropic => Some("ANTHROPIC_API_KEY"),
            Self::Gemini => Some("GEMINI_API_KEY"),
            Self::OpenAi => Some("OPENAI_API_KEY"),
            Self::Ollama => None,
        }
    }

    pub(crate) fn allowed_values() -> &'static str {
        "anthropic, gemini, open-ai, ollama"
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

impl EncoderProviderId {
    pub(crate) fn from_config_value(value: Option<&str>) -> Self {
        match value {
            Some("local-api") => Self::LocalApi,
            Some("remote-api") => Self::RemoteApi,
            _ => Self::FastEmbed,
        }
    }

    pub(crate) fn parse(value: &str, key: &str) -> Result<Self, AppError> {
        match value {
            "fast-embed" => Ok(Self::FastEmbed),
            "local-api" => Ok(Self::LocalApi),
            "remote-api" => Ok(Self::RemoteApi),
            _ => Err(AppError::InvalidConfigValue(
                key.to_string(),
                format!("expected one of: {}", Self::allowed_values()),
            )),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::FastEmbed => "fast-embed",
            Self::LocalApi => "local-api",
            Self::RemoteApi => "remote-api",
        }
    }

    pub(crate) fn allowed_values() -> &'static str {
        "fast-embed, local-api, remote-api"
    }
}

impl fmt::Display for EncoderProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub(crate) fn integration_state(
    integrations: Option<&IntegrationsSettings>,
    agent: AgentKind,
) -> Option<&IntegrationState> {
    integrations.and_then(|integrations| match agent {
        AgentKind::ClaudeCode => integrations.claude_code.as_ref(),
        AgentKind::Codex => integrations.codex.as_ref(),
        AgentKind::GeminiCli => integrations.gemini_cli.as_ref(),
        AgentKind::OpenCode => integrations.opencode.as_ref(),
        AgentKind::OpenClaw => integrations.openclaw.as_ref(),
    })
}

pub(crate) fn integration_configured(
    integrations: Option<&IntegrationsSettings>,
    agent: AgentKind,
) -> bool {
    integration_state(integrations, agent)
        .map(|state| state.configured)
        .unwrap_or(false)
}

pub(crate) fn set_integration_configured(
    integrations: &mut IntegrationsSettings,
    agent: AgentKind,
    configured: bool,
) {
    let state = Some(IntegrationState { configured });
    match agent {
        AgentKind::ClaudeCode => integrations.claude_code = state,
        AgentKind::Codex => integrations.codex = state,
        AgentKind::GeminiCli => integrations.gemini_cli = state,
        AgentKind::OpenCode => integrations.opencode = state,
        AgentKind::OpenClaw => integrations.openclaw = state,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_defaults_and_validation_match_expected_values() {
        assert_eq!(ProviderId::from_config_value(None), ProviderId::Anthropic);
        assert_eq!(
            ProviderId::from_config_value(Some("open-ai")),
            ProviderId::OpenAi
        );
        assert_eq!(
            ProviderId::parse("ollama", "server.llm_provider")
                .expect("valid provider")
                .default_model(),
            DEFAULT_OLLAMA_MODEL
        );

        let error = ProviderId::parse("wat", "server.llm_provider").expect_err("invalid provider");
        assert!(error.to_string().contains(ProviderId::allowed_values()));
    }

    #[test]
    fn encoder_defaults_and_validation_match_expected_values() {
        assert_eq!(
            EncoderProviderId::from_config_value(None),
            EncoderProviderId::FastEmbed
        );
        assert_eq!(
            EncoderProviderId::from_config_value(Some("remote-api")),
            EncoderProviderId::RemoteApi
        );
        assert_eq!(
            EncoderProviderId::parse("local-api", "server.encoder_provider")
                .expect("valid encoder")
                .as_str(),
            "local-api"
        );

        let error = EncoderProviderId::parse("wat", "server.encoder_provider")
            .expect_err("invalid encoder");
        assert!(
            error
                .to_string()
                .contains(EncoderProviderId::allowed_values())
        );
    }

    #[test]
    fn integration_helpers_round_trip_agent_state() {
        let mut integrations = IntegrationsSettings::default();

        set_integration_configured(&mut integrations, AgentKind::Codex, true);

        assert!(integration_configured(
            Some(&integrations),
            AgentKind::Codex
        ));
        assert!(!integration_configured(
            Some(&integrations),
            AgentKind::ClaudeCode
        ));
    }
}
