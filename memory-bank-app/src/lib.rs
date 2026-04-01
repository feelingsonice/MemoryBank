use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

pub const APP_DIR_NAME: &str = ".memory_bank";
pub const DEFAULT_NAMESPACE_NAME: &str = "default";
pub const DEFAULT_PORT: u16 = 3737;
pub const SETTINGS_SCHEMA_VERSION: u32 = 1;
pub const SETTINGS_FILE_NAME: &str = "settings.toml";
pub const DEFAULT_GEMINI_MODEL: &str = "gemini-3-flash-preview";
pub const DEFAULT_ANTHROPIC_MODEL: &str = "claude-sonnet-4-6";
pub const DEFAULT_OPENAI_MODEL: &str = "gpt-5-mini";
pub const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";
pub const DEFAULT_OLLAMA_MODEL: &str = "qwen3";
pub const DEFAULT_FASTEMBED_MODEL: &str = "jinaai/jina-embeddings-v2-base-code";
pub const DEFAULT_HISTORY_WINDOW_SIZE: u32 = 0;
pub const OLLAMA_HISTORY_WINDOW_SIZE: u32 = 5;
pub const DEFAULT_MAX_PROCESSING_ATTEMPTS: u32 = 10;
pub const SERVER_STARTUP_STATE_FILE_NAME: &str = "server-startup.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServerStartupPhase {
    Reindexing,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerStartupState {
    pub pid: u32,
    pub namespace: String,
    pub phase: ServerStartupPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_count: Option<usize>,
}

#[derive(Debug, Error)]
pub enum AppConfigError {
    #[error("failed to locate the home directory")]
    MissingHomeDir,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("TOML decode error: {0}")]
    TomlDecode(#[from] toml::de::Error),
    #[error("TOML encode error: {0}")]
    TomlEncode(#[from] toml::ser::Error),
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
        let raw = raw.trim();
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
            settings_file: root.join(SETTINGS_FILE_NAME),
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

    pub fn server_startup_state_path(&self, namespace: &Namespace) -> PathBuf {
        self.namespace_dir(namespace)
            .join(SERVER_STARTUP_STATE_FILE_NAME)
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
    #[serde(default = "default_settings_schema_version")]
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
            schema_version: default_settings_schema_version(),
            active_namespace: None,
            service: None,
            server: None,
            integrations: None,
        }
    }
}

fn default_settings_schema_version() -> u32 {
    SETTINGS_SCHEMA_VERSION
}

impl AppSettings {
    pub fn load(paths: &AppPaths) -> Result<Self, AppConfigError> {
        if paths.settings_file.exists() {
            return load_settings_from_toml(&paths.settings_file);
        }

        Ok(Self::default())
    }

    pub fn save(&self, paths: &AppPaths) -> Result<(), AppConfigError> {
        paths.ensure_base_dirs()?;
        let contents = toml::to_string_pretty(self)?;
        write_text_file(&paths.settings_file, &format!("{contents}\n"))?;
        Ok(())
    }

    pub fn to_toml_string(&self) -> Result<String, AppConfigError> {
        Ok(toml::to_string_pretty(self)?)
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

fn load_settings_from_toml(path: &Path) -> Result<AppSettings, AppConfigError> {
    let contents = read_text_file(path)?;
    if contents.trim().is_empty() {
        return Ok(AppSettings::default());
    }

    let settings: AppSettings = toml::from_str(&contents)?;
    validate_schema_version(&settings)?;
    Ok(settings)
}

fn validate_schema_version(settings: &AppSettings) -> Result<(), AppConfigError> {
    if settings.schema_version != SETTINGS_SCHEMA_VERSION {
        return Err(AppConfigError::UnsupportedSchemaVersion(
            settings.schema_version,
        ));
    }

    Ok(())
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
    pub max_processing_attempts: Option<u32>,
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
            && self.max_processing_attempts.is_none()
            && self.local_encoder_url.is_none()
            && self.remote_encoder_url.is_none()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntegrationsSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_code: Option<IntegrationState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<IntegrationState>,
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
            && self.codex.is_none()
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

        let contents = read_text_file(&paths.secrets_file)?;
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
        write_text_file(&paths.secrets_file, &rendered)?;
        Ok(())
    }

    pub fn parse(contents: &str) -> Self {
        let contents = strip_utf8_bom(contents);
        let mut values = BTreeMap::new();
        for raw_line in contents.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let Some((key, value)) = line.split_once('=') else {
                continue;
            };

            let key = normalize_env_key(key);
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
    let contents = serde_json::to_string_pretty(value)?;
    write_text_file(path, &format!("{contents}\n"))?;
    Ok(())
}

fn escape_env_value(value: &str) -> String {
    if value.is_empty() || value.chars().any(|c| !is_shell_safe_env_char(c)) {
        format!("{:?}", value)
    } else {
        value.to_string()
    }
}

fn unescape_env_value(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        return serde_json::from_str::<String>(value)
            .unwrap_or_else(|_| value[1..value.len() - 1].to_string());
    }

    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        return value[1..value.len() - 1]
            .replace("\\'", "'")
            .replace("\\\\", "\\");
    }

    value.to_string()
}

fn normalize_env_key(key: &str) -> &str {
    key.trim()
        .strip_prefix("export ")
        .map(str::trim)
        .unwrap_or_else(|| key.trim())
}

fn is_shell_safe_env_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '+' | '=' | '@')
}

fn strip_utf8_bom(contents: &str) -> &str {
    contents.strip_prefix('\u{feff}').unwrap_or(contents)
}

fn read_text_file(path: &Path) -> Result<String, std::io::Error> {
    let contents = fs::read_to_string(path)?;
    Ok(strip_utf8_bom(&contents).to_string())
}

fn write_text_file(path: &Path, contents: &str) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let temp_path = temp_write_path(path);
    fs::write(&temp_path, contents)?;
    if let Ok(metadata) = fs::metadata(path) {
        fs::set_permissions(&temp_path, metadata.permissions())?;
    }

    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path)?;
    }

    match fs::rename(&temp_path, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temp_path);
            Err(error)
        }
    }
}

fn temp_write_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("memory-bank");
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    path.with_file_name(format!(
        ".{file_name}.tmp-{}-{timestamp}",
        std::process::id()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn app_paths_create_base_dirs_and_namespace_helpers() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());

        paths.ensure_base_dirs().expect("base dirs");
        let namespace = Namespace::new("team a/1");
        let namespace_dir = paths
            .ensure_namespace_dir(&namespace)
            .expect("namespace dir");

        for dir in [
            &paths.root,
            &paths.bin_dir,
            &paths.config_dir,
            &paths.logs_dir,
            &paths.namespaces_dir,
            &paths.integrations_dir,
            &paths.backups_dir,
        ] {
            assert!(dir.is_dir(), "expected {} to exist", dir.display());
        }
        assert_eq!(namespace_dir, paths.namespace_dir(&namespace));
        assert!(namespace_dir.is_dir());
        assert_eq!(paths.db_path(&namespace), namespace_dir.join("memory.db"));
        assert_eq!(paths.binary_path("mb"), paths.bin_dir.join("mb"));
    }

    #[test]
    fn settings_load_defaults_when_file_is_missing() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());

        let settings = AppSettings::load(&paths).expect("load settings");

        assert_eq!(
            paths
                .settings_file
                .file_name()
                .and_then(|name| name.to_str()),
            Some(SETTINGS_FILE_NAME)
        );
        assert_eq!(settings.schema_version, SETTINGS_SCHEMA_VERSION);
        assert_eq!(settings.active_namespace(), Namespace::default());
        assert_eq!(settings.resolved_port(), DEFAULT_PORT);
    }

    #[test]
    fn settings_load_defaults_from_comment_only_toml() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        paths.ensure_base_dirs().expect("base dirs");
        fs::write(&paths.settings_file, "# user comments only\n").expect("write settings");

        let settings = AppSettings::load(&paths).expect("load settings");

        assert_eq!(settings, AppSettings::default());
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
        assert!(raw.contains("schema_version = 1"));
        assert!(raw.contains("active_namespace = \"work\""));
        assert!(raw.contains("[service]"));
        assert!(raw.contains("[server]"));
        assert!(!raw.contains("null"));
    }

    #[test]
    fn settings_save_only_writes_toml_settings_file() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());

        AppSettings::default().save(&paths).expect("save settings");

        assert!(paths.settings_file.exists());
        assert!(!paths.root.join("settings.json").exists());
    }

    #[test]
    fn settings_render_toml_string_matches_saved_format() {
        let settings = AppSettings {
            schema_version: SETTINGS_SCHEMA_VERSION,
            active_namespace: Some("work".to_string()),
            service: Some(ServiceSettings {
                port: Some(4444),
                autostart: Some(true),
            }),
            ..AppSettings::default()
        };

        let rendered = settings.to_toml_string().expect("render toml");

        assert!(rendered.contains("schema_version = 1"));
        assert!(rendered.contains("[service]"));
        assert!(rendered.contains("port = 4444"));
        assert!(rendered.contains("autostart = true"));
    }

    #[test]
    fn settings_load_defaults_schema_version_when_missing() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        paths.ensure_base_dirs().expect("base dirs");
        fs::write(
            &paths.settings_file,
            r#"
active_namespace = "work"

[service]
port = 4444
"#,
        )
        .expect("write settings");

        let settings = AppSettings::load(&paths).expect("load settings");

        assert_eq!(settings.schema_version, SETTINGS_SCHEMA_VERSION);
        assert_eq!(settings.active_namespace.as_deref(), Some("work"));
        assert_eq!(
            settings.service.and_then(|service| service.port),
            Some(4444)
        );
    }

    #[test]
    fn settings_reject_unsupported_schema_versions() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        paths.ensure_base_dirs().expect("base dirs");
        fs::write(
            &paths.settings_file,
            r#"
schema_version = 99
"#,
        )
        .expect("write settings");

        let error = AppSettings::load(&paths).expect_err("unsupported schema version");

        assert!(matches!(
            error,
            AppConfigError::UnsupportedSchemaVersion(99)
        ));
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
    fn secret_store_parses_exported_and_escaped_values() {
        let secrets = SecretStore::parse(
            r#"
export OPENAI_API_KEY="quoted \"value\""
export GEMINI_API_KEY='single quoted value'
"#,
        );

        assert_eq!(secrets.get("OPENAI_API_KEY"), Some(r#"quoted "value""#));
        assert_eq!(secrets.get("GEMINI_API_KEY"), Some("single quoted value"));
    }

    #[test]
    fn secret_store_round_trips_escaped_values() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let mut secrets = SecretStore::default();
        secrets.set("OPENAI_API_KEY", "line one\nline \"two\"");
        secrets.set("GEMINI_API_KEY", r#"C:\Users\me\key"#);

        secrets.save(&paths).expect("save secrets");
        let reloaded = SecretStore::load(&paths).expect("reload secrets");

        assert_eq!(
            reloaded.get("OPENAI_API_KEY"),
            Some("line one\nline \"two\"")
        );
        assert_eq!(reloaded.get("GEMINI_API_KEY"), Some(r#"C:\Users\me\key"#));
    }

    #[test]
    fn secret_store_quotes_shell_sensitive_values_when_saving() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let mut secrets = SecretStore::default();
        secrets.set("OPENAI_API_KEY", "value with $dollar and #hash");

        secrets.save(&paths).expect("save secrets");
        let raw = fs::read_to_string(&paths.secrets_file).expect("secrets file");

        assert!(raw.contains(r#"OPENAI_API_KEY="value with $dollar and #hash""#));
        let reloaded = SecretStore::load(&paths).expect("reload secrets");
        assert_eq!(
            reloaded.get("OPENAI_API_KEY"),
            Some("value with $dollar and #hash")
        );
    }

    #[test]
    fn secret_store_parses_bom_invalid_lines_and_empty_values() {
        let secrets = SecretStore::parse(
            "\u{feff}export OPENAI_API_KEY=abc123\nINVALID\n =oops\nEMPTY=\nSINGLE='C:\\\\Users\\\\me'\n",
        );

        assert_eq!(secrets.get("OPENAI_API_KEY"), Some("abc123"));
        assert_eq!(secrets.get("EMPTY"), Some(""));
        assert_eq!(secrets.get("SINGLE"), Some(r#"C:\Users\me"#));
        assert_eq!(secrets.get("INVALID"), None);
    }

    #[test]
    fn secret_store_save_sorts_keys_and_ends_with_newline() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let mut secrets = SecretStore::default();
        secrets.set("Z_KEY", "last");
        secrets.set("A_KEY", "first");

        secrets.save(&paths).expect("save secrets");
        let raw = fs::read_to_string(&paths.secrets_file).expect("secrets file");

        let lines: Vec<_> = raw.lines().collect();
        assert_eq!(lines, vec!["A_KEY=first", "Z_KEY=last"]);
        assert!(raw.ends_with('\n'));
    }

    #[test]
    fn write_json_file_creates_parent_directories_and_formats_output() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join("nested/config.json");

        write_json_file(&path, &json!({ "ok": true, "count": 2 })).expect("write json");

        let raw = fs::read_to_string(&path).expect("config file");
        assert!(path.exists());
        assert!(raw.ends_with('\n'));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&raw).expect("parse json"),
            json!({ "ok": true, "count": 2 })
        );
    }

    #[test]
    fn env_key_for_provider_maps_supported_values() {
        assert_eq!(env_key_for_provider("anthropic"), Some("ANTHROPIC_API_KEY"));
        assert_eq!(env_key_for_provider("gemini"), Some("GEMINI_API_KEY"));
        assert_eq!(env_key_for_provider("open-ai"), Some("OPENAI_API_KEY"));
        assert_eq!(env_key_for_provider("ollama"), None);
        assert_eq!(env_key_for_provider("unknown"), None);
    }

    #[test]
    fn namespace_sanitizes_invalid_characters() {
        let namespace = Namespace::new("team a/1");
        assert_eq!(namespace.as_ref(), "team_a_1");
    }

    #[test]
    fn namespace_whitespace_falls_back_to_default() {
        let namespace = Namespace::new("   ");
        assert_eq!(namespace, Namespace::default());
    }
}
