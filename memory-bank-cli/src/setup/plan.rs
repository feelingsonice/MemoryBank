use crate::agents::AgentKind;
use crate::config::{set_integrations, set_server, set_service};
use crate::constants::{DEFAULT_HISTORY_WINDOW_SIZE, DEFAULT_NEAREST_NEIGHBOR_COUNT};
use crate::domain::{ProviderId, integration_configured, set_integration_configured};
use memory_bank_app::{
    AppSettings, DEFAULT_FASTEMBED_MODEL, DEFAULT_NAMESPACE_NAME, DEFAULT_OLLAMA_URL, DEFAULT_PORT,
    Namespace, SETTINGS_SCHEMA_VERSION, SecretStore,
};

#[derive(Debug, Clone)]
pub(super) struct SetupPlan {
    pub(super) namespace: Namespace,
    pub(super) provider: ProviderId,
    pub(super) model: String,
    pub(super) ollama_url: Option<String>,
    pub(super) autostart: bool,
    pub(super) selected_agents: Vec<AgentKind>,
    pub(super) secret_choice: SecretChoice,
    pub(super) advanced: AdvancedSettings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AdvancedSettings {
    pub(super) port: u16,
    pub(super) fastembed_model: String,
    pub(super) history_window_size: u32,
    pub(super) nearest_neighbor_count: i32,
}

#[derive(Debug, Clone)]
pub(super) enum SecretChoice {
    NotRequired,
    KeepStored { key: &'static str },
    UseEnvironment { key: &'static str, value: String },
    ManualEntry { key: &'static str, value: String },
}

impl AdvancedSettings {
    pub(super) fn from_settings(settings: &AppSettings) -> Self {
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

    pub(super) fn has_overrides(&self) -> bool {
        self.port != DEFAULT_PORT
            || self.fastembed_model != DEFAULT_FASTEMBED_MODEL
            || self.history_window_size != DEFAULT_HISTORY_WINDOW_SIZE
            || self.nearest_neighbor_count != DEFAULT_NEAREST_NEIGHBOR_COUNT
    }

    pub(super) fn override_lines(&self) -> Vec<String> {
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
    pub(super) fn summary(&self) -> String {
        match self {
            Self::NotRequired => "No provider secret required for Ollama".to_string(),
            Self::KeepStored { key } => {
                format!("Use the existing {key} from ~/.memory_bank/secrets.env")
            }
            Self::UseEnvironment { key, .. } => {
                format!("Use the current shell {key} and store it in ~/.memory_bank/secrets.env")
            }
            Self::ManualEntry { key, .. } => {
                format!("Store a newly entered {key} in ~/.memory_bank/secrets.env")
            }
        }
    }
}

pub(super) fn build_settings_for_plan(
    current: &AppSettings,
    plan: &SetupPlan,
    configured_agents: &[AgentKind],
) -> AppSettings {
    let mut settings = current.clone();
    settings.schema_version = SETTINGS_SCHEMA_VERSION;
    settings.active_namespace = if plan.namespace.as_ref() == DEFAULT_NAMESPACE_NAME {
        None
    } else {
        Some(plan.namespace.to_string())
    };

    let mut service = settings.service.clone().unwrap_or_default();
    service.autostart = plan.autostart.then_some(true);
    service.port = (plan.advanced.port != DEFAULT_PORT).then_some(plan.advanced.port);
    set_service(&mut settings, service);

    let mut server = settings.server.clone().unwrap_or_default();
    server.llm_provider = if plan.provider == ProviderId::Anthropic {
        None
    } else {
        Some(plan.provider.as_str().to_string())
    };
    server.llm_model = if plan.model == plan.provider.default_model() {
        None
    } else {
        Some(plan.model.clone())
    };
    server.ollama_url = if plan.provider == ProviderId::Ollama {
        match plan.ollama_url.as_deref() {
            Some(url) if url != DEFAULT_OLLAMA_URL => Some(url.to_string()),
            _ => None,
        }
    } else {
        None
    };
    server.fastembed_model = if plan.advanced.fastembed_model == DEFAULT_FASTEMBED_MODEL {
        None
    } else {
        Some(plan.advanced.fastembed_model.clone())
    };
    server.history_window_size = (plan.advanced.history_window_size != DEFAULT_HISTORY_WINDOW_SIZE)
        .then_some(plan.advanced.history_window_size);
    server.nearest_neighbor_count = (plan.advanced.nearest_neighbor_count
        != DEFAULT_NEAREST_NEIGHBOR_COUNT)
        .then_some(plan.advanced.nearest_neighbor_count);
    set_server(&mut settings, server);

    let mut integrations = current.integrations.clone().unwrap_or_default();
    for agent in AgentKind::all() {
        let configured = configured_agents.contains(&agent)
            || integration_configured(current.integrations.as_ref(), agent);
        set_integration_configured(&mut integrations, agent, configured);
    }
    set_integrations(&mut settings, integrations);

    settings
}

pub(super) fn apply_secret_choice(secrets: &mut SecretStore, choice: &SecretChoice) {
    match choice {
        SecretChoice::NotRequired | SecretChoice::KeepStored { .. } => {}
        SecretChoice::UseEnvironment { key, value } | SecretChoice::ManualEntry { key, value } => {
            secrets.set(*key, value.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advanced_settings_default_from_settings_have_no_overrides() {
        let settings = AppSettings::default();
        let advanced = AdvancedSettings::from_settings(&settings);

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
    fn build_settings_for_plan_applies_advanced_overrides() {
        let current = AppSettings::default();
        let plan = SetupPlan {
            namespace: Namespace::new("work"),
            provider: ProviderId::Gemini,
            model: "gemini-3.1-pro-preview".to_string(),
            ollama_url: None,
            autostart: true,
            selected_agents: vec![AgentKind::OpenCode],
            secret_choice: SecretChoice::NotRequired,
            advanced: AdvancedSettings {
                port: 4545,
                fastembed_model: "custom/embed-model".to_string(),
                history_window_size: 25,
                nearest_neighbor_count: 15,
            },
        };

        let settings = build_settings_for_plan(&current, &plan, &[AgentKind::OpenCode]);
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
    fn build_settings_for_ollama_plan_persists_non_default_url() {
        let plan = SetupPlan {
            namespace: Namespace::new("default"),
            provider: ProviderId::Ollama,
            model: "qwen3".to_string(),
            ollama_url: Some("http://192.168.1.50:11434".to_string()),
            autostart: false,
            selected_agents: Vec::new(),
            secret_choice: SecretChoice::NotRequired,
            advanced: AdvancedSettings::from_settings(&AppSettings::default()),
        };

        let settings = build_settings_for_plan(&AppSettings::default(), &plan, &[]);
        let server = settings.server.expect("server settings");

        assert_eq!(server.llm_provider.as_deref(), Some("ollama"));
        assert_eq!(
            server.ollama_url.as_deref(),
            Some("http://192.168.1.50:11434")
        );
    }

    #[test]
    fn build_settings_for_plan_preserves_unselected_integrations() {
        let current = AppSettings {
            integrations: Some(memory_bank_app::IntegrationsSettings {
                claude_code: Some(memory_bank_app::IntegrationState { configured: true }),
                gemini_cli: Some(memory_bank_app::IntegrationState { configured: false }),
                opencode: Some(memory_bank_app::IntegrationState { configured: true }),
                openclaw: Some(memory_bank_app::IntegrationState { configured: true }),
            }),
            ..AppSettings::default()
        };
        let plan = SetupPlan {
            namespace: Namespace::new("default"),
            provider: ProviderId::Anthropic,
            model: memory_bank_app::DEFAULT_ANTHROPIC_MODEL.to_string(),
            ollama_url: None,
            autostart: false,
            selected_agents: vec![AgentKind::GeminiCli],
            secret_choice: SecretChoice::NotRequired,
            advanced: AdvancedSettings::from_settings(&AppSettings::default()),
        };

        let settings = build_settings_for_plan(&current, &plan, &[AgentKind::GeminiCli]);
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
    fn build_settings_for_default_plan_clears_default_provider_and_model() {
        let plan = SetupPlan {
            namespace: Namespace::new("default"),
            provider: ProviderId::Anthropic,
            model: memory_bank_app::DEFAULT_ANTHROPIC_MODEL.to_string(),
            ollama_url: None,
            autostart: false,
            selected_agents: Vec::new(),
            secret_choice: SecretChoice::NotRequired,
            advanced: AdvancedSettings::from_settings(&AppSettings::default()),
        };

        let settings = build_settings_for_plan(&AppSettings::default(), &plan, &[]);

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
        let plan = SetupPlan {
            namespace: Namespace::new("default"),
            provider: ProviderId::Gemini,
            model: memory_bank_app::DEFAULT_GEMINI_MODEL.to_string(),
            ollama_url: None,
            autostart: false,
            selected_agents: Vec::new(),
            secret_choice: SecretChoice::NotRequired,
            advanced: AdvancedSettings::from_settings(&AppSettings::default()),
        };

        let settings = build_settings_for_plan(&current, &plan, &[]);
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
            &SecretChoice::UseEnvironment {
                key: "ANTHROPIC_API_KEY",
                value: "updated".to_string(),
            },
        );
        assert_eq!(secrets.get("ANTHROPIC_API_KEY"), Some("updated"));
    }
}
