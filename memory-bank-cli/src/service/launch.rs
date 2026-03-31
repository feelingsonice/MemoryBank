use crate::AppError;
use crate::assets::{ExposureCheck, inspect_cli_exposure};
use crate::config::{
    llm_provider_value, normalize_ollama_url, validate_encoder_provider, validate_llm_provider,
};
use crate::constants::{
    HOOK_BINARY_NAME, MB_BINARY_NAME, MCP_PROXY_BINARY_NAME, SERVER_BINARY_NAME,
};
use crate::domain::{EncoderProviderId, ProviderId};
use memory_bank_app::{AppPaths, AppSettings, SecretStore};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::ServerLaunchSpec;

pub(crate) fn build_server_launch_spec(
    paths: &AppPaths,
    settings: &AppSettings,
    secrets: &SecretStore,
) -> Result<ServerLaunchSpec, AppError> {
    let port = settings.resolved_port();
    if port == 0 {
        return Err(AppError::InvalidConfigValue(
            "service.port".to_string(),
            "must be between 1 and 65535".to_string(),
        ));
    }

    let program = if is_runnable_file(&paths.binary_path(SERVER_BINARY_NAME)) {
        paths.binary_path(SERVER_BINARY_NAME)
    } else {
        let current_exe = std::env::current_exe()?;
        let sibling = current_exe
            .parent()
            .ok_or_else(|| AppError::MissingBinary(SERVER_BINARY_NAME.to_string()))?
            .join(SERVER_BINARY_NAME);
        if is_runnable_file(&sibling) {
            sibling
        } else {
            return Err(AppError::MissingBinary(SERVER_BINARY_NAME.to_string()));
        }
    };

    let server_settings = settings.server.clone().unwrap_or_default();
    let provider = validate_llm_provider(llm_provider_value(settings), "server.llm_provider")?;
    let encoder_provider = validate_encoder_provider(
        server_settings
            .encoder_provider
            .as_deref()
            .unwrap_or("fast-embed"),
        "server.encoder_provider",
    )?;
    match encoder_provider {
        EncoderProviderId::LocalApi => {
            if normalized_non_empty(server_settings.local_encoder_url.as_deref()).is_none() {
                return Err(AppError::InvalidConfigValue(
                    "server.local_encoder_url".to_string(),
                    "must be set when encoder provider is local-api".to_string(),
                ));
            }
        }
        EncoderProviderId::RemoteApi => {
            if normalized_non_empty(server_settings.remote_encoder_url.as_deref()).is_none() {
                return Err(AppError::InvalidConfigValue(
                    "server.remote_encoder_url".to_string(),
                    "must be set when encoder provider is remote-api".to_string(),
                ));
            }
        }
        EncoderProviderId::FastEmbed => {}
    }
    let nearest_neighbor_count = server_settings.nearest_neighbor_count.unwrap_or(10);
    if nearest_neighbor_count < 1 {
        return Err(AppError::InvalidConfigValue(
            "server.nearest_neighbor_count".to_string(),
            "must be at least 1".to_string(),
        ));
    }

    let mut env = std::collections::BTreeMap::new();
    if let Some(secret_key) = provider.secret_env_key() {
        let secret = require_non_empty_secret(secrets, secret_key)
            .ok_or(AppError::MissingProviderSecret(secret_key))?;
        env.insert(secret_key.to_string(), secret.to_string());
    }
    if encoder_provider == EncoderProviderId::RemoteApi {
        let secret = require_non_empty_secret(secrets, "MEMORY_BANK_REMOTE_ENCODER_API_KEY")
            .ok_or(AppError::Message(
                "missing required remote encoder secret `MEMORY_BANK_REMOTE_ENCODER_API_KEY` in ~/.memory_bank/secrets.env"
                    .to_string(),
            ))?;
        env.insert(
            "MEMORY_BANK_REMOTE_ENCODER_API_KEY".to_string(),
            secret.to_string(),
        );
    }

    match provider {
        ProviderId::Ollama => {
            if let Some(model) = server_settings.llm_model.clone() {
                env.insert("MEMORY_BANK_OLLAMA_MODEL".to_string(), model);
            }
            if let Some(url) = server_settings.ollama_url.clone() {
                env.insert(
                    "MEMORY_BANK_OLLAMA_URL".to_string(),
                    normalize_ollama_url(&url),
                );
            }
        }
        _ => {
            if let Some(model) = server_settings.llm_model.clone() {
                env.insert("MEMORY_BANK_LLM_MODEL".to_string(), model);
            }
        }
    }
    if let Some(model) = server_settings.fastembed_model.clone() {
        env.insert("MEMORY_BANK_FASTEMBED_MODEL".to_string(), model);
    }
    if let Some(url) = server_settings.local_encoder_url.clone() {
        env.insert("MEMORY_BANK_LOCAL_ENCODER_URL".to_string(), url);
    }
    if let Some(url) = server_settings.remote_encoder_url.clone() {
        env.insert("MEMORY_BANK_REMOTE_ENCODER_URL".to_string(), url);
    }

    Ok(ServerLaunchSpec {
        program,
        args: vec![
            "--port".to_string(),
            port.to_string(),
            "--namespace".to_string(),
            settings.active_namespace().to_string(),
            "--llm-provider".to_string(),
            provider.as_str().to_string(),
            "--encoder-provider".to_string(),
            encoder_provider.as_str().to_string(),
            "--history-window-size".to_string(),
            server_settings.history_window_size.unwrap_or(0).to_string(),
            "--nearest-neighbor-count".to_string(),
            nearest_neighbor_count.to_string(),
        ],
        env,
        remove_env: vec![
            "ANTHROPIC_API_KEY",
            "GEMINI_API_KEY",
            "OPENAI_API_KEY",
            "MEMORY_BANK_LLM_MODEL",
            "MEMORY_BANK_FASTEMBED_MODEL",
            "MEMORY_BANK_LOCAL_ENCODER_URL",
            "MEMORY_BANK_REMOTE_ENCODER_URL",
            "MEMORY_BANK_OLLAMA_MODEL",
            "MEMORY_BANK_OLLAMA_URL",
            "MEMORY_BANK_REMOTE_ENCODER_API_KEY",
        ],
    })
}

pub(crate) fn collect_doctor_issues(
    paths: &AppPaths,
    settings: &AppSettings,
) -> Result<Vec<String>, AppError> {
    let mut issues = Vec::new();
    let secrets = SecretStore::load(paths)?;

    if !paths.settings_file.exists() {
        issues.push(format!("{} is missing", paths.settings_file.display()));
    }
    if !is_runnable_file(&paths.binary_path(MB_BINARY_NAME)) {
        issues.push("mb is not installed under ~/.memory_bank/bin".to_string());
    }
    for binary in [SERVER_BINARY_NAME, HOOK_BINARY_NAME, MCP_PROXY_BINARY_NAME] {
        if !is_runnable_file(&paths.binary_path(binary)) {
            issues.push(format!("{binary} is missing from ~/.memory_bank/bin"));
        }
    }
    match inspect_cli_exposure(paths)? {
        ExposureCheck::Active(_) => {}
        ExposureCheck::Missing => {
            issues.push(
                "no managed `mb` exposure was found for the current shell or future shells"
                    .to_string(),
            );
        }
        ExposureCheck::Collision(path) => {
            issues.push(format!(
                "another `mb` executable already exists on PATH at {}",
                path.display()
            ));
        }
    }

    if let Some(env_key) = ProviderId::from_config_value(Some(llm_provider_value(settings)))
        .secret_env_key()
        && require_non_empty_secret(&secrets, env_key).is_none()
    {
        issues.push(format!("missing {env_key} in ~/.memory_bank/secrets.env"));
    }

    match settings
        .server
        .as_ref()
        .and_then(|server| server.encoder_provider.as_deref())
        .unwrap_or("fast-embed")
    {
        "local-api" => {
            if normalized_non_empty(
                settings
                    .server
                    .as_ref()
                    .and_then(|server| server.local_encoder_url.as_deref()),
            )
            .is_none()
            {
                issues.push("server.local_encoder_url must be set for local-api".to_string());
            }
        }
        "remote-api" => {
            if normalized_non_empty(
                settings
                    .server
                    .as_ref()
                    .and_then(|server| server.remote_encoder_url.as_deref()),
            )
            .is_none()
            {
                issues.push("server.remote_encoder_url must be set for remote-api".to_string());
            }
            if require_non_empty_secret(&secrets, "MEMORY_BANK_REMOTE_ENCODER_API_KEY").is_none() {
                issues.push(
                    "missing MEMORY_BANK_REMOTE_ENCODER_API_KEY in ~/.memory_bank/secrets.env"
                        .to_string(),
                );
            }
        }
        _ => {}
    }

    let service = super::service_status(paths)?;
    if !service.installed {
        issues.push("managed service is not installed".to_string());
    } else if !service.active {
        issues.push("managed service is not active".to_string());
    }

    if super::fetch_health(settings).is_err() {
        issues.push("health check to /healthz failed".to_string());
    }

    Ok(issues)
}

fn normalized_non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn require_non_empty_secret<'a>(secrets: &'a SecretStore, key: &str) -> Option<&'a str> {
    secrets.get(key).filter(|value| !value.trim().is_empty())
}

pub(super) fn is_runnable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        path.metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }

    #[cfg(not(unix))]
    {
        true
    }
}
