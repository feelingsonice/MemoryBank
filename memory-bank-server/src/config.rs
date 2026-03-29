use clap::Parser;
use clap::ValueEnum;
use std::env;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Namespace(String);

impl Namespace {
    const DEFAULT: &str = "default";
    const APP_DIR: &str = "memory-bank";

    fn sanitize(raw: &str) -> String {
        let sanitized: String = raw
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();

        if sanitized.is_empty() {
            Self::DEFAULT.to_string()
        } else {
            sanitized
        }
    }
}

impl AsRef<str> for Namespace {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Namespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Namespace {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Self::sanitize(s)))
    }
}

#[derive(Debug)]
pub struct Dirs {
    pub data: PathBuf,
    pub db: PathBuf,
    pub models: PathBuf,
}

impl Dirs {
    /// Returns the top-level app directory: `{data_dir}/memory-bank/`.
    fn app_dir() -> PathBuf {
        let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
        base.join(Namespace::APP_DIR)
    }

    pub fn create(namespace: &Namespace) -> Result<Self, std::io::Error> {
        let app_dir = Self::app_dir();
        let data = app_dir.join("namespaces").join(namespace.as_ref());
        let dirs = Self {
            db: data.join("memory.db"),
            models: app_dir.join("models"),
            data,
        };
        std::fs::create_dir_all(&dirs.data)?;
        Ok(dirs)
    }
}

#[derive(Debug, Clone, ValueEnum)]
pub enum LlmProviderType {
    Gemini,
    Anthropic,
    OpenAi,
    Ollama,
}

#[derive(Debug, Clone)]
pub enum LlmProviderConfig {
    Gemini { api_key: String, model: String },
    Anthropic { api_key: String, model: String },
    OpenAi { api_key: String, model: String },
    Ollama { url: String, model: String },
}

impl LlmProviderConfig {
    const DEFAULT_GEMINI_MODEL: &str = "gemini-2.5-flash";
    const DEFAULT_ANTHROPIC_MODEL: &str = "claude-sonnet-4-6";
    const DEFAULT_OPENAI_MODEL: &str = "gpt-4o-mini";
    const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";
    const DEFAULT_OLLAMA_MODEL: &str = "qwen3:4b";
}

impl TryFrom<LlmProviderType> for LlmProviderConfig {
    type Error = crate::error::AppError;

    fn try_from(provider: LlmProviderType) -> Result<Self, Self::Error> {
        match provider {
            LlmProviderType::Gemini => Ok(Self::Gemini {
                api_key: require_env("GEMINI_API_KEY")?,
                model: env_or_default("MEMORY_BANK_LLM_MODEL", Self::DEFAULT_GEMINI_MODEL),
            }),
            LlmProviderType::Anthropic => Ok(Self::Anthropic {
                api_key: require_env("ANTHROPIC_API_KEY")?,
                model: env_or_default("MEMORY_BANK_LLM_MODEL", Self::DEFAULT_ANTHROPIC_MODEL),
            }),
            LlmProviderType::OpenAi => Ok(Self::OpenAi {
                api_key: require_env("OPENAI_API_KEY")?,
                model: env_or_default("MEMORY_BANK_LLM_MODEL", Self::DEFAULT_OPENAI_MODEL),
            }),
            LlmProviderType::Ollama => Ok(Self::Ollama {
                url: env_or_default("MEMORY_BANK_OLLAMA_URL", Self::DEFAULT_OLLAMA_URL),
                model: env_or_default("MEMORY_BANK_OLLAMA_MODEL", Self::DEFAULT_OLLAMA_MODEL),
            }),
        }
    }
}

impl fmt::Display for LlmProviderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gemini { model, .. } => write!(f, "Gemini::{model}"),
            Self::Anthropic { model, .. } => write!(f, "Anthropic::{model}"),
            Self::OpenAi { model, .. } => write!(f, "OpenAi::{model}"),
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

#[derive(Debug, Clone)]
pub enum EncoderProviderConfig {
    FastEmbed { model: String },
    LocalApi { url: String },
    RemoteApi { _api_key: String, url: String },
}

impl EncoderProviderConfig {
    const DEFAULT_FASTEMBED_MODEL: &str = "jinaai/jina-embeddings-v2-base-code";
}

impl TryFrom<EncoderProviderType> for EncoderProviderConfig {
    type Error = crate::error::AppError;

    fn try_from(provider: EncoderProviderType) -> Result<Self, Self::Error> {
        match provider {
            EncoderProviderType::FastEmbed => Ok(Self::FastEmbed {
                model: env_or_default("MEMORY_BANK_FASTEMBED_MODEL", Self::DEFAULT_FASTEMBED_MODEL),
            }),
            EncoderProviderType::LocalApi => Ok(Self::LocalApi {
                url: require_env("MEMORY_BANK_LOCAL_ENCODER_URL")?,
            }),
            EncoderProviderType::RemoteApi => Ok(Self::RemoteApi {
                _api_key: require_env("MEMORY_BANK_REMOTE_ENCODER_API_KEY")?,
                url: require_env("MEMORY_BANK_REMOTE_ENCODER_URL")?,
            }),
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
    #[arg(long, default_value_t = 8080)]
    port: u16,

    #[arg(long, default_value = Namespace::DEFAULT)]
    namespace: Namespace,

    #[arg(long, value_enum, default_value_t = LlmProviderType::Anthropic)]
    llm_provider: LlmProviderType,

    #[arg(long, value_enum, default_value_t = EncoderProviderType::FastEmbed)]
    encoder_provider: EncoderProviderType,

    #[arg(long, default_value_t = 0)]
    history_window_size: u32,

    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(i32).range(1..))]
    nearest_neighbor_count: i32,
}

#[derive(Debug)]
pub struct ServeConfig {
    pub port: u16,
    pub namespace: Namespace,
    pub llm: LlmProviderConfig,
    pub encoder: EncoderProviderConfig,
    pub history_window_size: u32,
    pub nearest_neighbor_count: i32,
    pub dirs: Dirs,
}

impl ServeArgs {
    pub fn parse() -> Self {
        <Self as Parser>::parse()
    }
}

impl TryFrom<ServeArgs> for ServeConfig {
    type Error = crate::error::AppError;

    fn try_from(config: ServeArgs) -> Result<Self, Self::Error> {
        let dirs = Dirs::create(&config.namespace)?;

        Ok(Self {
            port: config.port,
            namespace: config.namespace,
            llm: config.llm_provider.try_into()?,
            encoder: config.encoder_provider.try_into()?,
            history_window_size: config.history_window_size,
            nearest_neighbor_count: config.nearest_neighbor_count,
            dirs,
        })
    }
}

fn require_env(name: &str) -> Result<String, crate::error::AppError> {
    env::var(name).map_err(|_| crate::error::AppError::Config(format!("{} must be set", name)))
}

fn env_or_default(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn serve_parses_with_defaults() {
        let args = ServeArgs::try_parse_from(["memory-bank-server"]).expect("parse serve");

        assert_eq!(args.port, 8080);
        assert_eq!(args.namespace.as_ref(), "default");
        assert!(matches!(args.llm_provider, LlmProviderType::Anthropic));
        assert!(matches!(
            args.encoder_provider,
            EncoderProviderType::FastEmbed
        ));
        assert_eq!(args.history_window_size, 0);
        assert_eq!(args.nearest_neighbor_count, 10);
    }

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
        ])
        .expect("parse serve");

        assert_eq!(args.port, 9000);
        assert_eq!(args.namespace.as_ref(), "team-a");
        assert!(matches!(args.llm_provider, LlmProviderType::OpenAi));
        assert!(matches!(
            args.encoder_provider,
            EncoderProviderType::FastEmbed
        ));
        assert_eq!(args.history_window_size, 42);
        assert_eq!(args.nearest_neighbor_count, 24);
    }

    #[test]
    fn serve_rejects_zero_nearest_neighbor_count() {
        assert!(
            ServeArgs::try_parse_from(["memory-bank-server", "--nearest-neighbor-count", "0",])
                .is_err()
        );
    }

    #[test]
    fn parse_rejects_old_subcommand_shape() {
        assert!(ServeArgs::try_parse_from(["memory-bank-server", "serve"]).is_err());
    }

    #[test]
    fn serve_parses_ollama_provider() {
        let args = ServeArgs::try_parse_from(["memory-bank-server", "--llm-provider", "ollama"])
            .expect("parse serve");

        assert!(matches!(args.llm_provider, LlmProviderType::Ollama));
    }

    #[test]
    fn ollama_provider_uses_defaults() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let _guard = EnvVarGuard::new(&["MEMORY_BANK_OLLAMA_URL", "MEMORY_BANK_OLLAMA_MODEL"]);

        let config = LlmProviderConfig::try_from(LlmProviderType::Ollama).expect("ollama config");

        assert!(matches!(
            config,
            LlmProviderConfig::Ollama { url, model }
            if url == "http://localhost:11434" && model == "qwen3:4b"
        ));
    }

    #[test]
    fn ollama_provider_reads_env_overrides() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let _guard = EnvVarGuard::new(&["MEMORY_BANK_OLLAMA_URL", "MEMORY_BANK_OLLAMA_MODEL"]);

        unsafe {
            env::set_var("MEMORY_BANK_OLLAMA_URL", "http://127.0.0.1:11434");
            env::set_var("MEMORY_BANK_OLLAMA_MODEL", "qwen3:8b");
        }

        let config = LlmProviderConfig::try_from(LlmProviderType::Ollama).expect("ollama config");

        assert!(matches!(
            config,
            LlmProviderConfig::Ollama { url, model }
            if url == "http://127.0.0.1:11434" && model == "qwen3:8b"
        ));
    }

    #[test]
    fn namespace_sanitizes_unsupported_characters() {
        let namespace: Namespace = format!(
            "server-http-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        )
        .parse()
        .expect("namespace");

        assert!(!namespace.as_ref().contains(' '));
        assert!(!namespace.as_ref().contains('/'));
    }

    #[test]
    fn empty_namespace_falls_back_to_default() {
        let namespace: Namespace = "".parse().expect("namespace");

        assert_eq!(namespace.as_ref(), "default");
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
