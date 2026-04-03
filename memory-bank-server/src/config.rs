use clap::Parser;
use clap::ValueEnum;
use memory_bank_app::{
    AppPaths, AppSettings, DEFAULT_ANTHROPIC_MODEL, DEFAULT_FASTEMBED_MODEL, DEFAULT_GEMINI_MODEL,
    DEFAULT_HISTORY_WINDOW_SIZE, DEFAULT_MAX_PROCESSING_ATTEMPTS, DEFAULT_OLLAMA_MODEL,
    DEFAULT_OLLAMA_URL, DEFAULT_OPENAI_MODEL, Namespace, OLLAMA_HISTORY_WINDOW_SIZE,
    ServerSettings, format_openai_model_id, normalize_openai_url,
};
use std::env;
use std::fmt;
use std::path::PathBuf;

#[derive(Debug)]
pub struct Dirs {
    pub data: PathBuf,
    pub db: PathBuf,
    pub models: PathBuf,
    pub startup_state: PathBuf,
}

impl Dirs {
    pub fn create(paths: &AppPaths, namespace: &Namespace) -> Result<Self, std::io::Error> {
        let data = paths.ensure_namespace_dir(namespace)?;
        let models = paths.models_dir();
        std::fs::create_dir_all(&models)?;
        Ok(Self {
            db: data.join("memory.db"),
            models,
            startup_state: paths.server_startup_state_path(namespace),
            data,
        })
    }
}

#[derive(Debug, Clone, ValueEnum)]
pub enum LlmProviderType {
    Gemini,
    Anthropic,
    OpenAi,
    Ollama,
}

impl std::str::FromStr for LlmProviderType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "gemini" => Ok(Self::Gemini),
            "anthropic" => Ok(Self::Anthropic),
            "open-ai" => Ok(Self::OpenAi),
            "ollama" => Ok(Self::Ollama),
            other => Err(format!("unsupported llm provider '{other}'")),
        }
    }
}

#[derive(Debug, Clone)]
pub enum LlmProviderConfig {
    Gemini {
        api_key: String,
        model: String,
    },
    Anthropic {
        api_key: String,
        model: String,
    },
    OpenAi {
        api_key: String,
        model: String,
        base_url: String,
    },
    Ollama {
        url: String,
        model: String,
    },
}

impl LlmProviderConfig {
    fn from_resolved(
        provider: LlmProviderType,
        settings: Option<&ServerSettings>,
    ) -> Result<Self, crate::error::AppError> {
        match provider {
            LlmProviderType::Gemini => Ok(Self::Gemini {
                api_key: require_env("GEMINI_API_KEY")?,
                model: env_setting_or_default(
                    "MEMORY_BANK_LLM_MODEL",
                    settings.and_then(|s| s.llm_model.as_deref()),
                    DEFAULT_GEMINI_MODEL,
                ),
            }),
            LlmProviderType::Anthropic => Ok(Self::Anthropic {
                api_key: require_env("ANTHROPIC_API_KEY")?,
                model: env_setting_or_default(
                    "MEMORY_BANK_LLM_MODEL",
                    settings.and_then(|s| s.llm_model.as_deref()),
                    DEFAULT_ANTHROPIC_MODEL,
                ),
            }),
            LlmProviderType::OpenAi => Ok(Self::OpenAi {
                api_key: require_env("OPENAI_API_KEY")?,
                model: env_setting_or_default(
                    "MEMORY_BANK_LLM_MODEL",
                    settings.and_then(|s| s.llm_model.as_deref()),
                    DEFAULT_OPENAI_MODEL,
                ),
                base_url: normalize_openai_url(&env_setting_or_default(
                    "OPENAI_BASE_URL",
                    settings.and_then(|s| s.openai_url.as_deref()),
                    memory_bank_app::DEFAULT_OPENAI_URL,
                ))
                .map_err(|error| crate::error::AppError::Config(error.to_string()))?,
            }),
            LlmProviderType::Ollama => Ok(Self::Ollama {
                url: env_setting_or_default(
                    "MEMORY_BANK_OLLAMA_URL",
                    settings.and_then(|s| s.ollama_url.as_deref()),
                    DEFAULT_OLLAMA_URL,
                ),
                model: env_setting_or_default(
                    "MEMORY_BANK_OLLAMA_MODEL",
                    settings.and_then(|s| s.llm_model.as_deref()),
                    DEFAULT_OLLAMA_MODEL,
                ),
            }),
        }
    }

    pub fn provider_name(&self) -> &'static str {
        match self {
            Self::Gemini { .. } => "gemini",
            Self::Anthropic { .. } => "anthropic",
            Self::OpenAi { .. } => "open-ai",
            Self::Ollama { .. } => "ollama",
        }
    }
}

impl fmt::Display for LlmProviderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gemini { model, .. } => write!(f, "Gemini::{model}"),
            Self::Anthropic { model, .. } => write!(f, "Anthropic::{model}"),
            Self::OpenAi {
                model, base_url, ..
            } => {
                write!(f, "{}", format_openai_model_id(model, base_url))
            }
            Self::Ollama { model, url } => write!(f, "Ollama::{model}@{url}"),
        }
    }
}

#[derive(Debug, Clone, ValueEnum)]
enum EncoderProviderType {
    FastEmbed,
    LocalApi,
    RemoteApi,
}

impl std::str::FromStr for EncoderProviderType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "fast-embed" => Ok(Self::FastEmbed),
            "local-api" => Ok(Self::LocalApi),
            "remote-api" => Ok(Self::RemoteApi),
            other => Err(format!("unsupported encoder provider '{other}'")),
        }
    }
}

#[derive(Debug, Clone)]
pub enum EncoderProviderConfig {
    FastEmbed { model: String },
    LocalApi { url: String },
    RemoteApi { _api_key: String, url: String },
}

impl EncoderProviderConfig {
    fn from_resolved(
        provider: EncoderProviderType,
        settings: Option<&ServerSettings>,
    ) -> Result<Self, crate::error::AppError> {
        match provider {
            EncoderProviderType::FastEmbed => Ok(Self::FastEmbed {
                model: env_setting_or_default(
                    "MEMORY_BANK_FASTEMBED_MODEL",
                    settings.and_then(|s| s.fastembed_model.as_deref()),
                    DEFAULT_FASTEMBED_MODEL,
                ),
            }),
            EncoderProviderType::LocalApi => Ok(Self::LocalApi {
                url: require_env_or_setting(
                    "MEMORY_BANK_LOCAL_ENCODER_URL",
                    settings.and_then(|s| s.local_encoder_url.as_deref()),
                )?,
            }),
            EncoderProviderType::RemoteApi => Ok(Self::RemoteApi {
                _api_key: require_env("MEMORY_BANK_REMOTE_ENCODER_API_KEY")?,
                url: require_env_or_setting(
                    "MEMORY_BANK_REMOTE_ENCODER_URL",
                    settings.and_then(|s| s.remote_encoder_url.as_deref()),
                )?,
            }),
        }
    }

    pub fn provider_name(&self) -> &'static str {
        match self {
            Self::FastEmbed { .. } => "fast-embed",
            Self::LocalApi { .. } => "local-api",
            Self::RemoteApi { .. } => "remote-api",
        }
    }
}

impl fmt::Display for EncoderProviderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FastEmbed { model } => write!(f, "FastEmbed::{model}"),
            Self::LocalApi { url } => write!(f, "LocalApi::{url}"),
            Self::RemoteApi { url, .. } => write!(f, "RemoteApi::{url}"),
        }
    }
}

#[derive(Debug, Parser)]
#[command(author, version, about = "Memory Bank server")]
pub struct ServeArgs {
    #[arg(long, env = "MEMORY_BANK_PORT")]
    port: Option<u16>,

    #[arg(long, env = "MEMORY_BANK_NAMESPACE")]
    namespace: Option<Namespace>,

    #[arg(long, value_enum, env = "MEMORY_BANK_LLM_PROVIDER")]
    llm_provider: Option<LlmProviderType>,

    #[arg(long, value_enum, env = "MEMORY_BANK_ENCODER_PROVIDER")]
    encoder_provider: Option<EncoderProviderType>,

    #[arg(long, env = "MEMORY_BANK_HISTORY_WINDOW_SIZE")]
    history_window_size: Option<u32>,

    #[arg(long, env = "MEMORY_BANK_NEAREST_NEIGHBOR_COUNT", value_parser = clap::value_parser!(i32).range(1..))]
    nearest_neighbor_count: Option<i32>,

    #[arg(long, env = "MEMORY_BANK_MAX_PROCESSING_ATTEMPTS", value_parser = clap::value_parser!(u32).range(1..))]
    max_processing_attempts: Option<u32>,
}

#[derive(Debug)]
pub struct ServeConfig {
    pub port: u16,
    pub namespace: Namespace,
    pub llm: LlmProviderConfig,
    pub encoder: EncoderProviderConfig,
    pub history_window_size: u32,
    pub nearest_neighbor_count: i32,
    pub max_processing_attempts: u32,
    pub dirs: Dirs,
}

impl ServeArgs {
    pub fn parse() -> Self {
        <Self as Parser>::parse()
    }
}

impl TryFrom<ServeArgs> for ServeConfig {
    type Error = crate::error::AppError;

    fn try_from(args: ServeArgs) -> Result<Self, Self::Error> {
        let app_paths = AppPaths::from_system()
            .map_err(|error| crate::error::AppError::Config(error.to_string()))?;
        let settings = AppSettings::load(&app_paths)
            .map_err(|error| crate::error::AppError::Config(error.to_string()))?;
        let server_settings = settings.server.as_ref();

        let namespace = args
            .namespace
            .unwrap_or_else(|| settings.active_namespace());
        let dirs = Dirs::create(&app_paths, &namespace)?;
        let llm_provider = match args.llm_provider {
            Some(provider) => provider,
            None => parse_optional_value(server_settings.and_then(|s| s.llm_provider.as_deref()))?
                .unwrap_or(LlmProviderType::Anthropic),
        };
        let encoder_provider = match args.encoder_provider {
            Some(provider) => provider,
            None => {
                parse_optional_value(server_settings.and_then(|s| s.encoder_provider.as_deref()))?
                    .unwrap_or(EncoderProviderType::FastEmbed)
            }
        };

        Ok(Self {
            port: args.port.unwrap_or_else(|| settings.resolved_port()),
            namespace,
            llm: LlmProviderConfig::from_resolved(llm_provider.clone(), server_settings)?,
            encoder: EncoderProviderConfig::from_resolved(encoder_provider, server_settings)?,
            history_window_size: effective_history_window_size(
                &llm_provider,
                args.history_window_size
                    .or_else(|| server_settings.and_then(|s| s.history_window_size))
                    .unwrap_or(DEFAULT_HISTORY_WINDOW_SIZE),
            ),
            nearest_neighbor_count: args
                .nearest_neighbor_count
                .or_else(|| server_settings.and_then(|s| s.nearest_neighbor_count))
                .unwrap_or(10),
            max_processing_attempts: resolve_max_processing_attempts(
                args.max_processing_attempts
                    .or_else(|| server_settings.and_then(|s| s.max_processing_attempts)),
            )?,
            dirs,
        })
    }
}

fn resolve_max_processing_attempts(
    configured_value: Option<u32>,
) -> Result<u32, crate::error::AppError> {
    let max_processing_attempts = configured_value.unwrap_or(DEFAULT_MAX_PROCESSING_ATTEMPTS);
    if max_processing_attempts == 0 {
        return Err(crate::error::AppError::Config(
            "max_processing_attempts must be greater than 0".to_string(),
        ));
    }
    Ok(max_processing_attempts)
}

fn effective_history_window_size(provider: &LlmProviderType, configured_value: u32) -> u32 {
    match provider {
        LlmProviderType::Ollama => OLLAMA_HISTORY_WINDOW_SIZE,
        _ => configured_value,
    }
}

fn parse_optional_value<T>(value: Option<&str>) -> Result<Option<T>, crate::error::AppError>
where
    T: std::str::FromStr,
    T::Err: fmt::Display,
{
    match value {
        Some(value) => value
            .parse::<T>()
            .map(Some)
            .map_err(|error| crate::error::AppError::Config(error.to_string())),
        None => Ok(None),
    }
}

fn require_env(name: &str) -> Result<String, crate::error::AppError> {
    env::var(name).map_err(|_| crate::error::AppError::Config(format!("{} must be set", name)))
}

fn require_env_or_setting(
    name: &str,
    setting: Option<&str>,
) -> Result<String, crate::error::AppError> {
    env::var(name)
        .ok()
        .or_else(|| setting.map(str::to_owned))
        .ok_or_else(|| crate::error::AppError::Config(format!("{name} must be set")))
}

fn env_setting_or_default(name: &str, setting: Option<&str>, default: &str) -> String {
    env::var(name)
        .ok()
        .or_else(|| setting.map(str::to_owned))
        .unwrap_or_else(|| default.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_bank_app::{DEFAULT_PORT, SETTINGS_SCHEMA_VERSION, ServiceSettings};
    use std::sync::Mutex;
    use tempfile::TempDir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn serve_parses_with_explicit_values() {
        let args = ServeArgs::try_parse_from([
            "memory-bank-server",
            "--port",
            "9000",
            "--namespace",
            "team-a",
            "--llm-provider",
            "open-ai",
            "--encoder-provider",
            "fast-embed",
            "--history-window-size",
            "42",
            "--nearest-neighbor-count",
            "24",
            "--max-processing-attempts",
            "17",
        ])
        .expect("parse serve");

        assert_eq!(args.port, Some(9000));
        assert_eq!(args.namespace.expect("namespace").as_ref(), "team-a");
        assert!(matches!(args.llm_provider, Some(LlmProviderType::OpenAi)));
        assert!(matches!(
            args.encoder_provider,
            Some(EncoderProviderType::FastEmbed)
        ));
        assert_eq!(args.history_window_size, Some(42));
        assert_eq!(args.nearest_neighbor_count, Some(24));
        assert_eq!(args.max_processing_attempts, Some(17));
    }

    #[test]
    fn serve_rejects_zero_nearest_neighbor_count() {
        assert!(
            ServeArgs::try_parse_from(["memory-bank-server", "--nearest-neighbor-count", "0",])
                .is_err()
        );
    }

    #[test]
    fn serve_rejects_zero_max_processing_attempts() {
        assert!(
            ServeArgs::try_parse_from(["memory-bank-server", "--max-processing-attempts", "0",])
                .is_err()
        );
    }

    #[test]
    fn parse_rejects_old_subcommand_shape() {
        assert!(ServeArgs::try_parse_from(["memory-bank-server", "serve"]).is_err());
    }

    #[test]
    fn ollama_provider_uses_defaults() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let _guard = EnvVarGuard::new(&["MEMORY_BANK_OLLAMA_URL", "MEMORY_BANK_OLLAMA_MODEL"]);

        let config =
            LlmProviderConfig::from_resolved(LlmProviderType::Ollama, None).expect("ollama");

        assert!(matches!(
            config,
            LlmProviderConfig::Ollama { url, model }
            if url == DEFAULT_OLLAMA_URL && model == DEFAULT_OLLAMA_MODEL
        ));
    }

    #[test]
    fn ollama_provider_reads_env_overrides() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let _guard = EnvVarGuard::new(&["MEMORY_BANK_OLLAMA_URL", "MEMORY_BANK_OLLAMA_MODEL"]);

        unsafe {
            env::set_var("MEMORY_BANK_OLLAMA_URL", "http://127.0.0.1:11434");
            env::set_var("MEMORY_BANK_OLLAMA_MODEL", "qwen3");
        }

        let config =
            LlmProviderConfig::from_resolved(LlmProviderType::Ollama, None).expect("ollama");

        assert!(matches!(
            config,
            LlmProviderConfig::Ollama { url, model }
            if url == "http://127.0.0.1:11434" && model == "qwen3"
        ));
    }

    #[test]
    fn openai_provider_uses_default_url() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let _guard = EnvVarGuard::new(&["OPENAI_API_KEY", "OPENAI_BASE_URL"]);
        unsafe {
            env::set_var("OPENAI_API_KEY", "secret");
        }

        let config =
            LlmProviderConfig::from_resolved(LlmProviderType::OpenAi, None).expect("openai");

        assert!(matches!(
            config,
            LlmProviderConfig::OpenAi { ref base_url, .. } if base_url == memory_bank_app::DEFAULT_OPENAI_URL
        ));
        assert_eq!(config.to_string(), "OpenAi::gpt-5-mini");
    }

    #[test]
    fn openai_provider_normalizes_settings_override() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let _guard = EnvVarGuard::new(&["OPENAI_API_KEY", "OPENAI_BASE_URL"]);
        unsafe {
            env::set_var("OPENAI_API_KEY", "secret");
        }

        let config = LlmProviderConfig::from_resolved(
            LlmProviderType::OpenAi,
            Some(&ServerSettings {
                openai_url: Some(" https://opencode.ai/zen/v1/ ".to_string()),
                ..ServerSettings::default()
            }),
        )
        .expect("openai");

        assert!(matches!(
            config,
            LlmProviderConfig::OpenAi { ref base_url, .. } if base_url == "https://opencode.ai/zen/v1"
        ));
        assert_eq!(
            config.to_string(),
            "OpenAi::gpt-5-mini@https://opencode.ai/zen/v1"
        );
    }

    #[test]
    fn openai_provider_env_override_takes_precedence() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let _guard = EnvVarGuard::new(&["OPENAI_API_KEY", "OPENAI_BASE_URL"]);
        unsafe {
            env::set_var("OPENAI_API_KEY", "secret");
            env::set_var("OPENAI_BASE_URL", "https://env.example.com/v1/");
        }

        let config = LlmProviderConfig::from_resolved(
            LlmProviderType::OpenAi,
            Some(&ServerSettings {
                openai_url: Some("https://settings.example.com/v1".to_string()),
                ..ServerSettings::default()
            }),
        )
        .expect("openai");

        assert!(matches!(
            config,
            LlmProviderConfig::OpenAi { ref base_url, .. } if base_url == "https://env.example.com/v1"
        ));
    }

    #[test]
    fn openai_provider_rejects_invalid_base_url() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let _guard = EnvVarGuard::new(&["OPENAI_API_KEY", "OPENAI_BASE_URL"]);
        unsafe {
            env::set_var("OPENAI_API_KEY", "secret");
            env::set_var("OPENAI_BASE_URL", "https://example.com/v1?foo=bar");
        }

        let error = LlmProviderConfig::from_resolved(LlmProviderType::OpenAi, None)
            .expect_err("invalid openai url should fail");

        assert!(error.to_string().contains("query parameters or fragments"));
    }

    #[test]
    fn serve_config_uses_settings_defaults_when_flags_are_omitted() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let temp = TempDir::new().expect("tempdir");
        let _guard = EnvVarGuard::new(&[
            "HOME",
            "MEMORY_BANK_PORT",
            "MEMORY_BANK_NAMESPACE",
            "MEMORY_BANK_LLM_PROVIDER",
            "MEMORY_BANK_ENCODER_PROVIDER",
            "MEMORY_BANK_HISTORY_WINDOW_SIZE",
            "MEMORY_BANK_NEAREST_NEIGHBOR_COUNT",
            "MEMORY_BANK_MAX_PROCESSING_ATTEMPTS",
            "ANTHROPIC_API_KEY",
        ]);
        unsafe {
            env::set_var("HOME", temp.path());
            env::set_var("ANTHROPIC_API_KEY", "secret");
        }

        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings = AppSettings {
            schema_version: SETTINGS_SCHEMA_VERSION,
            active_namespace: Some("team-a".to_string()),
            service: Some(ServiceSettings {
                port: Some(4555),
                autostart: None,
            }),
            server: Some(ServerSettings {
                llm_provider: Some("anthropic".to_string()),
                history_window_size: Some(9),
                nearest_neighbor_count: Some(12),
                max_processing_attempts: Some(7),
                ..ServerSettings::default()
            }),
            integrations: None,
        };
        settings.save(&paths).expect("save settings");

        let config = ServeConfig::try_from(
            ServeArgs::try_parse_from(["memory-bank-server"]).expect("parse"),
        )
        .expect("config");

        assert_eq!(config.port, 4555);
        assert_eq!(config.namespace.as_ref(), "team-a");
        assert_eq!(config.history_window_size, 9);
        assert_eq!(config.nearest_neighbor_count, 12);
        assert_eq!(config.max_processing_attempts, 7);
        assert!(config.dirs.db.ends_with("team-a/memory.db"));
        assert!(config.dirs.models.ends_with(".memory_bank/models"));
    }

    #[test]
    fn serve_config_uses_default_history_window_when_unset() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let temp = TempDir::new().expect("tempdir");
        let _guard = EnvVarGuard::new(&[
            "HOME",
            "MEMORY_BANK_PORT",
            "MEMORY_BANK_NAMESPACE",
            "MEMORY_BANK_LLM_PROVIDER",
            "MEMORY_BANK_ENCODER_PROVIDER",
            "MEMORY_BANK_HISTORY_WINDOW_SIZE",
            "MEMORY_BANK_NEAREST_NEIGHBOR_COUNT",
            "MEMORY_BANK_MAX_PROCESSING_ATTEMPTS",
            "ANTHROPIC_API_KEY",
        ]);
        unsafe {
            env::set_var("HOME", temp.path());
            env::set_var("ANTHROPIC_API_KEY", "secret");
        }

        let config = ServeConfig::try_from(
            ServeArgs::try_parse_from(["memory-bank-server"]).expect("parse"),
        )
        .expect("config");

        assert_eq!(config.history_window_size, DEFAULT_HISTORY_WINDOW_SIZE);
        assert_eq!(
            config.max_processing_attempts,
            DEFAULT_MAX_PROCESSING_ATTEMPTS
        );
    }

    #[test]
    fn serve_config_hardcodes_ollama_history_window() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let temp = TempDir::new().expect("tempdir");
        let _guard = EnvVarGuard::new(&[
            "HOME",
            "MEMORY_BANK_PORT",
            "MEMORY_BANK_NAMESPACE",
            "MEMORY_BANK_LLM_PROVIDER",
            "MEMORY_BANK_ENCODER_PROVIDER",
            "MEMORY_BANK_HISTORY_WINDOW_SIZE",
            "MEMORY_BANK_NEAREST_NEIGHBOR_COUNT",
            "MEMORY_BANK_MAX_PROCESSING_ATTEMPTS",
        ]);
        unsafe {
            env::set_var("HOME", temp.path());
        }

        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings = AppSettings {
            schema_version: SETTINGS_SCHEMA_VERSION,
            active_namespace: None,
            service: None,
            server: Some(ServerSettings {
                llm_provider: Some("ollama".to_string()),
                history_window_size: Some(42),
                ..ServerSettings::default()
            }),
            integrations: None,
        };
        settings.save(&paths).expect("save settings");

        let config = ServeConfig::try_from(
            ServeArgs::try_parse_from(["memory-bank-server"]).expect("parse"),
        )
        .expect("config");

        assert_eq!(config.history_window_size, OLLAMA_HISTORY_WINDOW_SIZE);
    }

    #[test]
    fn serve_config_hardcodes_ollama_history_window_even_with_flag() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let temp = TempDir::new().expect("tempdir");
        let _guard = EnvVarGuard::new(&[
            "HOME",
            "MEMORY_BANK_PORT",
            "MEMORY_BANK_NAMESPACE",
            "MEMORY_BANK_LLM_PROVIDER",
            "MEMORY_BANK_ENCODER_PROVIDER",
            "MEMORY_BANK_HISTORY_WINDOW_SIZE",
            "MEMORY_BANK_NEAREST_NEIGHBOR_COUNT",
            "MEMORY_BANK_MAX_PROCESSING_ATTEMPTS",
        ]);
        unsafe {
            env::set_var("HOME", temp.path());
        }

        let config = ServeConfig::try_from(
            ServeArgs::try_parse_from([
                "memory-bank-server",
                "--llm-provider",
                "ollama",
                "--history-window-size",
                "99",
            ])
            .expect("parse"),
        )
        .expect("config");

        assert_eq!(config.history_window_size, OLLAMA_HISTORY_WINDOW_SIZE);
    }

    #[test]
    fn serve_config_reads_max_processing_attempts_from_env() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let temp = TempDir::new().expect("tempdir");
        let _guard = EnvVarGuard::new(&[
            "HOME",
            "MEMORY_BANK_PORT",
            "MEMORY_BANK_NAMESPACE",
            "MEMORY_BANK_LLM_PROVIDER",
            "MEMORY_BANK_ENCODER_PROVIDER",
            "MEMORY_BANK_HISTORY_WINDOW_SIZE",
            "MEMORY_BANK_NEAREST_NEIGHBOR_COUNT",
            "MEMORY_BANK_MAX_PROCESSING_ATTEMPTS",
            "ANTHROPIC_API_KEY",
        ]);
        unsafe {
            env::set_var("HOME", temp.path());
            env::set_var("ANTHROPIC_API_KEY", "secret");
            env::set_var("MEMORY_BANK_MAX_PROCESSING_ATTEMPTS", "13");
        }

        let config = ServeConfig::try_from(
            ServeArgs::try_parse_from(["memory-bank-server"]).expect("parse"),
        )
        .expect("config");

        assert_eq!(config.max_processing_attempts, 13);
    }

    #[test]
    fn serve_config_rejects_zero_settings_max_processing_attempts() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let temp = TempDir::new().expect("tempdir");
        let _guard = EnvVarGuard::new(&[
            "HOME",
            "MEMORY_BANK_PORT",
            "MEMORY_BANK_NAMESPACE",
            "MEMORY_BANK_LLM_PROVIDER",
            "MEMORY_BANK_ENCODER_PROVIDER",
            "MEMORY_BANK_HISTORY_WINDOW_SIZE",
            "MEMORY_BANK_NEAREST_NEIGHBOR_COUNT",
            "MEMORY_BANK_MAX_PROCESSING_ATTEMPTS",
            "ANTHROPIC_API_KEY",
        ]);
        unsafe {
            env::set_var("HOME", temp.path());
            env::set_var("ANTHROPIC_API_KEY", "secret");
        }

        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings = AppSettings {
            schema_version: SETTINGS_SCHEMA_VERSION,
            active_namespace: None,
            service: None,
            server: Some(ServerSettings {
                llm_provider: Some("anthropic".to_string()),
                max_processing_attempts: Some(0),
                ..ServerSettings::default()
            }),
            integrations: None,
        };
        settings.save(&paths).expect("save settings");

        let error = ServeConfig::try_from(
            ServeArgs::try_parse_from(["memory-bank-server"]).expect("parse"),
        )
        .expect_err("zero max_processing_attempts should fail");
        assert!(matches!(
            error,
            crate::error::AppError::Config(message)
                if message.contains("max_processing_attempts")
        ));
    }

    #[test]
    fn settings_fallback_uses_new_default_port() {
        let settings = AppSettings::default();
        assert_eq!(settings.resolved_port(), DEFAULT_PORT);
    }

    struct EnvVarGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvVarGuard {
        fn new(keys: &[&'static str]) -> Self {
            let saved = keys
                .iter()
                .copied()
                .map(|key| {
                    let value = env::var(key).ok();
                    unsafe {
                        env::remove_var(key);
                    }
                    (key, value)
                })
                .collect();
            Self { saved }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            for (key, value) in &self.saved {
                match value {
                    Some(value) => unsafe {
                        env::set_var(key, value);
                    },
                    None => unsafe {
                        env::remove_var(key);
                    },
                }
            }
        }
    }
}
