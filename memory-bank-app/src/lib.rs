use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use thiserror::Error;

pub const APP_DIR_NAME: &str = ".memory_bank";
pub const DEFAULT_NAMESPACE_NAME: &str = "default";
pub const DEFAULT_PORT: u16 = 3737;
pub const SETTINGS_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_GEMINI_MODEL: &str = "gemini-3-flash-preview";
pub const DEFAULT_ANTHROPIC_MODEL: &str = "claude-sonnet-4-6";
pub const DEFAULT_OPENAI_MODEL: &str = "gpt-5-mini";
pub const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";
pub const DEFAULT_OLLAMA_MODEL: &str = "qwen3";
pub const DEFAULT_FASTEMBED_MODEL: &str = "jinaai/jina-embeddings-v2-base-code";

#[derive(Debug, Error)]
pub enum AppConfigError {
    #[error("failed to locate the home directory")]
    MissingHomeDir,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported settings schema version {0}")]
    UnsupportedSchemaVersion(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Namespace(String);

impl Namespace {
    pub fn new(raw: impl AsRef<str>) -> Self {
        Self(Self::sanitize(raw.as_ref()))
    }

    pub fn sanitize(raw: &str) -> String {
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
            DEFAULT_NAMESPACE_NAME.to_string()
        } else {
            sanitized
        }
    }
}

impl Default for Namespace {
    fn default() -> Self {
        Self(DEFAULT_NAMESPACE_NAME.to_string())
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
        Ok(Self::new(s))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub home_dir: PathBuf,
    pub root: PathBuf,
    pub bin_dir: PathBuf,
    pub config_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub namespaces_dir: PathBuf,
    pub integrations_dir: PathBuf,
    pub backups_dir: PathBuf,
    pub settings_file: PathBuf,
    pub secrets_file: PathBuf,
    pub model_catalog_file: PathBuf,
    pub log_file: PathBuf,
}

impl AppPaths {
    pub fn from_system() -> Result<Self, AppConfigError> {
        let home_dir = dirs::home_dir().ok_or(AppConfigError::MissingHomeDir)?;
        Ok(Self::from_home_dir(home_dir))
    }

    pub fn from_home_dir(home_dir: PathBuf) -> Self {
        let root = home_dir.join(APP_DIR_NAME);
        let bin_dir = root.join("bin");
        let config_dir = root.join("config");
        let logs_dir = root.join("logs");
        let namespaces_dir = root.join("namespaces");
        let integrations_dir = root.join("integrations");
        let backups_dir = root.join("backups");
        Self {
            home_dir,
            settings_file: root.join("settings.json"),
            secrets_file: root.join("secrets.env"),
            model_catalog_file: config_dir.join("setup-model-catalog.json"),
            log_file: logs_dir.join("server.log"),
            root,
            bin_dir,
            config_dir,
            logs_dir,
            namespaces_dir,
            integrations_dir,
            backups_dir,
        }
    }

    pub fn ensure_base_dirs(&self) -> Result<(), std::io::Error> {
        for path in [
            &self.root,
            &self.bin_dir,
            &self.config_dir,
            &self.logs_dir,
            &self.namespaces_dir,
            &self.integrations_dir,
            &self.backups_dir,
        ] {
            fs::create_dir_all(path)?;
        }
        Ok(())
    }

    pub fn namespace_dir(&self, namespace: &Namespace) -> PathBuf {
        self.namespaces_dir.join(namespace.as_ref())
    }

    pub fn ensure_namespace_dir(&self, namespace: &Namespace) -> Result<PathBuf, std::io::Error> {
        let dir = self.namespace_dir(namespace);
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    pub fn db_path(&self, namespace: &Namespace) -> PathBuf {
        self.namespace_dir(namespace).join("memory.db")
    }

    pub fn models_dir(&self) -> PathBuf {
        self.root.join("models")
    }

    pub fn binary_path(&self, name: &str) -> PathBuf {
        self.bin_dir.join(name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppSettings {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<ServiceSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrations: Option<IntegrationsSettings>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            active_namespace: None,
            service: None,
            server: None,
            integrations: None,
        }
    }
}

impl AppSettings {
    pub fn load(paths: &AppPaths) -> Result<Self, AppConfigError> {
        if !paths.settings_file.exists() {
            return Ok(Self::default());
        }

        let contents = fs::read_to_string(&paths.settings_file)?;
        let settings: Self = serde_json::from_str(&contents)?;
        if settings.schema_version != SETTINGS_SCHEMA_VERSION {
            return Err(AppConfigError::UnsupportedSchemaVersion(
                settings.schema_version,
            ));
        }

        Ok(settings)
    }

    pub fn save(&self, paths: &AppPaths) -> Result<(), AppConfigError> {
        paths.ensure_base_dirs()?;
        let contents = serde_json::to_string_pretty(self)?;
        fs::write(&paths.settings_file, format!("{contents}\n"))?;
        Ok(())
    }

    pub fn active_namespace(&self) -> Namespace {
        self.active_namespace
            .as_deref()
            .map(Namespace::new)
            .unwrap_or_default()
    }

    pub fn resolved_port(&self) -> u16 {
        self.service
            .as_ref()
            .and_then(|service| service.port)
            .unwrap_or(DEFAULT_PORT)
    }

    pub fn resolved_autostart(&self) -> bool {
        self.service
            .as_ref()
            .and_then(|service| service.autostart)
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autostart: Option<bool>,
}

impl ServiceSettings {
    pub fn is_empty(&self) -> bool {
        self.port.is_none() && self.autostart.is_none()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ollama_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoder_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fastembed_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_window_size: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nearest_neighbor_count: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_encoder_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_encoder_url: Option<String>,
}

impl ServerSettings {
    pub fn is_empty(&self) -> bool {
        self.llm_provider.is_none()
            && self.llm_model.is_none()
            && self.ollama_url.is_none()
            && self.encoder_provider.is_none()
            && self.fastembed_model.is_none()
            && self.history_window_size.is_none()
            && self.nearest_neighbor_count.is_none()
            && self.local_encoder_url.is_none()
            && self.remote_encoder_url.is_none()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntegrationsSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_code: Option<IntegrationState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gemini_cli: Option<IntegrationState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opencode: Option<IntegrationState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openclaw: Option<IntegrationState>,
}

impl IntegrationsSettings {
    pub fn is_empty(&self) -> bool {
        self.claude_code.is_none()
            && self.gemini_cli.is_none()
            && self.opencode.is_none()
            && self.openclaw.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntegrationState {
    pub configured: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretStore {
    values: BTreeMap<String, String>,
}

impl SecretStore {
    pub fn load(paths: &AppPaths) -> Result<Self, AppConfigError> {
        if !paths.secrets_file.exists() {
            return Ok(Self::default());
        }

        let contents = fs::read_to_string(&paths.secrets_file)?;
        Ok(Self::parse(&contents))
    }

    pub fn save(&self, paths: &AppPaths) -> Result<(), AppConfigError> {
        paths.ensure_base_dirs()?;
        let mut lines = Vec::with_capacity(self.values.len());
        for (key, value) in &self.values {
            lines.push(format!("{key}={}", escape_env_value(value)));
        }
        let mut rendered = lines.join("\n");
        if !rendered.is_empty() {
            rendered.push('\n');
        }
        fs::write(&paths.secrets_file, rendered)?;
        Ok(())
    }

    pub fn parse(contents: &str) -> Self {
        let mut values = BTreeMap::new();
        for raw_line in contents.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let Some((key, value)) = line.split_once('=') else {
                continue;
            };

            let key = key.trim();
            if key.is_empty() {
                continue;
            }

            values.insert(key.to_string(), unescape_env_value(value.trim()));
        }

        Self { values }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.values.insert(key.into(), value.into());
    }

    pub fn remove(&mut self, key: &str) {
        self.values.remove(key);
    }
}

pub fn default_server_url(settings: &AppSettings) -> String {
    format!("http://127.0.0.1:{}", settings.resolved_port())
}

pub fn env_key_for_provider(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "gemini" => Some("GEMINI_API_KEY"),
        "open-ai" => Some("OPENAI_API_KEY"),
        "ollama" => None,
        _ => None,
    }
}

pub fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<(), AppConfigError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let contents = serde_json::to_string_pretty(value)?;
    fs::write(path, format!("{contents}\n"))?;
    Ok(())
}

fn escape_env_value(value: &str) -> String {
    if value
        .chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\'')
    {
        format!("{:?}", value)
    } else {
        value.to_string()
    }
}

fn unescape_env_value(value: &str) -> String {
    if (value.starts_with('"') && value.ends_with('"'))
        || (value.starts_with('\'') && value.ends_with('\''))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn settings_load_defaults_when_file_is_missing() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());

        let settings = AppSettings::load(&paths).expect("load settings");

        assert_eq!(settings.schema_version, SETTINGS_SCHEMA_VERSION);
        assert_eq!(settings.active_namespace(), Namespace::default());
        assert_eq!(settings.resolved_port(), DEFAULT_PORT);
    }

    #[test]
    fn settings_round_trip_sparse_fields() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings = AppSettings {
            schema_version: SETTINGS_SCHEMA_VERSION,
            active_namespace: Some("work".to_string()),
            service: Some(ServiceSettings {
                port: Some(4444),
                autostart: Some(true),
            }),
            server: Some(ServerSettings {
                llm_provider: Some("anthropic".to_string()),
                llm_model: Some(DEFAULT_ANTHROPIC_MODEL.to_string()),
                ..ServerSettings::default()
            }),
            integrations: None,
        };

        settings.save(&paths).expect("save settings");
        let reloaded = AppSettings::load(&paths).expect("reload settings");

        assert_eq!(reloaded, settings);
        let raw = fs::read_to_string(paths.settings_file).expect("settings file");
        assert!(!raw.contains("null"));
    }

    #[test]
    fn secret_store_parses_basic_env_lines() {
        let secrets = SecretStore::parse(
            r#"
# comment
ANTHROPIC_API_KEY=abc123
OPENAI_API_KEY="quoted value"
"#,
        );

        assert_eq!(secrets.get("ANTHROPIC_API_KEY"), Some("abc123"));
        assert_eq!(secrets.get("OPENAI_API_KEY"), Some("quoted value"));
    }

    #[test]
    fn namespace_sanitizes_invalid_characters() {
        let namespace = Namespace::new("team a/1");
        assert_eq!(namespace.as_ref(), "team_a_1");
    }
}
