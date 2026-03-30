use crate::AppError;
use crate::command_utils::{current_uid, run_command, shell_escape};
use crate::config::{
    llm_provider_value, normalize_ollama_url, validate_encoder_provider, validate_llm_provider,
};
use crate::constants::{
    HOOK_BINARY_NAME, LAUNCHD_LABEL, LOG_TAIL_LINE_COUNT, MB_BINARY_NAME, MCP_PROXY_BINARY_NAME,
    SERVER_BINARY_NAME, SYSTEMD_UNIT_NAME,
};
use memory_bank_app::{
    AppPaths, AppSettings, SecretStore, default_server_url, env_key_for_provider,
};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManagedPlatform {
    MacOs,
    Linux,
}

#[derive(Debug, Deserialize)]
pub(crate) struct HealthCheck {
    pub(crate) ok: bool,
    pub(crate) namespace: String,
    pub(crate) port: u16,
    pub(crate) llm_provider: String,
    pub(crate) encoder_provider: String,
    pub(crate) version: String,
}

#[derive(Debug)]
pub(crate) struct ServiceStatus {
    pub(crate) installed: bool,
    pub(crate) active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServerLaunchSpec {
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<String>,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) remove_env: Vec<&'static str>,
}

impl ManagedPlatform {
    fn detect() -> Result<Self, AppError> {
        match std::env::consts::OS {
            "macos" => Ok(Self::MacOs),
            "linux" => Ok(Self::Linux),
            other => Err(AppError::UnsupportedPlatform(other.to_string())),
        }
    }
}

pub(crate) fn install_service(paths: &AppPaths, settings: &AppSettings) -> Result<(), AppError> {
    paths.ensure_base_dirs()?;
    match ManagedPlatform::detect()? {
        ManagedPlatform::MacOs => install_launchd_service(paths, settings),
        ManagedPlatform::Linux => install_systemd_service(paths, settings),
    }
}

pub(crate) fn start_service(paths: &AppPaths) -> Result<(), AppError> {
    match ManagedPlatform::detect()? {
        ManagedPlatform::MacOs => {
            let settings = AppSettings::load(paths)?;
            let uid = current_uid()?;
            let service_path = launchd_service_path(paths);
            if !service_path.exists() {
                install_launchd_service(paths, &settings)?;
            }
            let domain = format!("gui/{uid}");
            let status = ProcessCommand::new("launchctl")
                .args(["print", &format!("{domain}/{LAUNCHD_LABEL}")])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
            if status.success() {
                run_command(
                    "launchctl",
                    &["kickstart", "-k", &format!("{domain}/{LAUNCHD_LABEL}")],
                )
            } else {
                run_command(
                    "launchctl",
                    &[
                        "bootstrap",
                        &domain,
                        service_path.to_string_lossy().as_ref(),
                    ],
                )?;
                run_command(
                    "launchctl",
                    &["kickstart", "-k", &format!("{domain}/{LAUNCHD_LABEL}")],
                )
            }
        }
        ManagedPlatform::Linux => {
            let settings = AppSettings::load(paths)?;
            if !systemd_service_path(paths).exists() {
                install_systemd_service(paths, &settings)?;
            }
            run_command("systemctl", &["--user", "start", SYSTEMD_UNIT_NAME])
        }
    }
}

pub(crate) fn stop_service(_paths: &AppPaths) -> Result<(), AppError> {
    match ManagedPlatform::detect()? {
        ManagedPlatform::MacOs => {
            let uid = current_uid()?;
            let _ = run_command(
                "launchctl",
                &["bootout", &format!("gui/{uid}/{LAUNCHD_LABEL}")],
            );
            Ok(())
        }
        ManagedPlatform::Linux => {
            let _ = run_command("systemctl", &["--user", "stop", SYSTEMD_UNIT_NAME]);
            Ok(())
        }
    }
}

pub(crate) fn restart_service(paths: &AppPaths) -> Result<(), AppError> {
    match ManagedPlatform::detect()? {
        ManagedPlatform::MacOs => {
            let status = service_status(paths)?;
            if status.active {
                let uid = current_uid()?;
                run_command(
                    "launchctl",
                    &["kickstart", "-k", &format!("gui/{uid}/{LAUNCHD_LABEL}")],
                )
            } else {
                start_service(paths)
            }
        }
        ManagedPlatform::Linux => {
            if !systemd_service_path(paths).exists() {
                return start_service(paths);
            }
            run_command("systemctl", &["--user", "restart", SYSTEMD_UNIT_NAME])
        }
    }
}

pub(crate) fn service_status(paths: &AppPaths) -> Result<ServiceStatus, AppError> {
    match ManagedPlatform::detect()? {
        ManagedPlatform::MacOs => {
            let installed = launchd_service_path(paths).exists();
            let uid = current_uid()?;
            let active = ProcessCommand::new("launchctl")
                .args(["print", &format!("gui/{uid}/{LAUNCHD_LABEL}")])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?
                .success();
            Ok(ServiceStatus { installed, active })
        }
        ManagedPlatform::Linux => {
            let installed = systemd_service_path(paths).exists();
            let active = ProcessCommand::new("systemctl")
                .args(["--user", "is-active", "--quiet", SYSTEMD_UNIT_NAME])
                .status()?
                .success();
            Ok(ServiceStatus { installed, active })
        }
    }
}

pub(crate) fn fetch_health(settings: &AppSettings) -> Result<HealthCheck, AppError> {
    let health_url = format!("{}/healthz", default_server_url(settings));
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(2))
        .timeout_read(Duration::from_secs(2))
        .timeout_write(Duration::from_secs(2))
        .build();
    let response = agent
        .get(&health_url)
        .call()
        .map_err(|error| AppError::Health(error.to_string()))?;
    response
        .into_json::<HealthCheck>()
        .map_err(|error| AppError::Health(error.to_string()))
}

pub(crate) fn wait_for_health(
    settings: &AppSettings,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<HealthCheck, AppError> {
    let deadline = Instant::now() + timeout;

    loop {
        match fetch_health(settings) {
            Ok(health) => return Ok(health),
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(AppError::Health(format!(
                        "service did not become healthy within {}s: {}",
                        timeout.as_secs(),
                        error
                    )));
                }
            }
        }

        thread::sleep(poll_interval);
    }
}

pub(crate) fn tail_log_file(path: &Path, follow: bool) -> Result<(), AppError> {
    if !path.exists() {
        if follow {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, "")?;
        } else {
            return Err(AppError::Message(format!(
                "log file does not exist yet: {}",
                path.display()
            )));
        }
    }

    let mut command = ProcessCommand::new("tail");
    if follow {
        command.args(["-n", LOG_TAIL_LINE_COUNT, "-f"]);
    } else {
        command.args(["-n", LOG_TAIL_LINE_COUNT]);
    }
    command.arg(path);
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(AppError::CommandFailed(
            "tail".to_string(),
            format!("tail exited with status {status}"),
        ))
    }
}

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
        "local-api" => {
            if normalized_non_empty(server_settings.local_encoder_url.as_deref()).is_none() {
                return Err(AppError::InvalidConfigValue(
                    "server.local_encoder_url".to_string(),
                    "must be set when encoder provider is local-api".to_string(),
                ));
            }
        }
        "remote-api" => {
            if normalized_non_empty(server_settings.remote_encoder_url.as_deref()).is_none() {
                return Err(AppError::InvalidConfigValue(
                    "server.remote_encoder_url".to_string(),
                    "must be set when encoder provider is remote-api".to_string(),
                ));
            }
        }
        _ => {}
    }
    let nearest_neighbor_count = server_settings.nearest_neighbor_count.unwrap_or(10);
    if nearest_neighbor_count < 1 {
        return Err(AppError::InvalidConfigValue(
            "server.nearest_neighbor_count".to_string(),
            "must be at least 1".to_string(),
        ));
    }

    let mut env = BTreeMap::new();
    if let Some(secret_key) = env_key_for_provider(provider) {
        let secret = secrets
            .get(secret_key)
            .ok_or(AppError::MissingProviderSecret(secret_key))?;
        env.insert(secret_key.to_string(), secret.to_string());
    }
    if encoder_provider == "remote-api" {
        let secret = secrets
            .get("MEMORY_BANK_REMOTE_ENCODER_API_KEY")
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
        "ollama" => {
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
            provider.to_string(),
            "--encoder-provider".to_string(),
            encoder_provider.to_string(),
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

    if let Some(env_key) = env_key_for_provider(llm_provider_value(settings))
        && secrets.get(env_key).is_none()
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
            if secrets.get("MEMORY_BANK_REMOTE_ENCODER_API_KEY").is_none() {
                issues.push(
                    "missing MEMORY_BANK_REMOTE_ENCODER_API_KEY in ~/.memory_bank/secrets.env"
                        .to_string(),
                );
            }
        }
        _ => {}
    }

    let service = service_status(paths)?;
    if !service.installed {
        issues.push("managed service is not installed".to_string());
    } else if !service.active {
        issues.push("managed service is not active".to_string());
    }

    if fetch_health(settings).is_err() {
        issues.push("health check to /healthz failed".to_string());
    }

    Ok(issues)
}

fn install_launchd_service(paths: &AppPaths, settings: &AppSettings) -> Result<(), AppError> {
    let service_path = launchd_service_path(paths);
    if let Some(parent) = service_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        &service_path,
        render_launchd_plist(paths, settings.resolved_autostart()),
    )?;
    Ok(())
}

fn install_systemd_service(paths: &AppPaths, settings: &AppSettings) -> Result<(), AppError> {
    let service_path = systemd_service_path(paths);
    if let Some(parent) = service_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&service_path, render_systemd_unit(paths))?;
    run_command("systemctl", &["--user", "daemon-reload"])?;
    if settings.resolved_autostart() {
        run_command("systemctl", &["--user", "enable", SYSTEMD_UNIT_NAME])?;
    } else {
        let _ = run_command("systemctl", &["--user", "disable", SYSTEMD_UNIT_NAME]);
    }
    Ok(())
}

fn render_launchd_plist(paths: &AppPaths, autostart: bool) -> String {
    let launch_flag = if autostart { "<true/>" } else { "<false/>" };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
  <dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
      <string>{program}</string>
      <string>internal</string>
      <string>run-server</string>
    </array>
    <key>RunAtLoad</key>
    {launch_flag}
    <key>KeepAlive</key>
    {launch_flag}
    <key>StandardOutPath</key>
    <string>{log_file}</string>
    <key>StandardErrorPath</key>
    <string>{log_file}</string>
  </dict>
</plist>
"#,
        label = LAUNCHD_LABEL,
        program = paths.binary_path(MB_BINARY_NAME).display(),
        log_file = paths.log_file.display(),
        launch_flag = launch_flag,
    )
}

fn render_systemd_unit(paths: &AppPaths) -> String {
    let escaped_mb = shell_escape(paths.binary_path(MB_BINARY_NAME).to_string_lossy().as_ref());
    let escaped_log = shell_escape(paths.log_file.to_string_lossy().as_ref());
    format!(
        "[Unit]\nDescription=Memory Bank\nAfter=network.target\n\n[Service]\nType=simple\nExecStart=/bin/sh -lc 'exec {escaped_mb} internal run-server >> {escaped_log} 2>&1'\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n"
    )
}

fn launchd_service_path(paths: &AppPaths) -> PathBuf {
    paths
        .home_dir
        .join("Library/LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist"))
}

fn systemd_service_path(paths: &AppPaths) -> PathBuf {
    paths
        .home_dir
        .join(".config/systemd/user")
        .join(SYSTEMD_UNIT_NAME)
}

fn normalized_non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn is_runnable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        path
            .metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }

    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_bank_app::{ServerSettings, ServiceSettings};
    use tempfile::TempDir;

    #[cfg(unix)]
    fn make_runnable(path: &Path) {
        let mut permissions = fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("set permissions");
    }

    #[test]
    fn launch_spec_uses_secrets_env_and_strips_ambient_keys() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings = AppSettings {
            schema_version: memory_bank_app::SETTINGS_SCHEMA_VERSION,
            active_namespace: Some("work".to_string()),
            service: Some(ServiceSettings {
                port: Some(4545),
                autostart: Some(true),
            }),
            server: Some(ServerSettings {
                llm_provider: Some("anthropic".to_string()),
                llm_model: Some("claude-custom".to_string()),
                ..ServerSettings::default()
            }),
            integrations: None,
        };
        let mut secrets = SecretStore::default();
        secrets.set("ANTHROPIC_API_KEY", "from-secrets");

        fs::create_dir_all(&paths.bin_dir).expect("bin dir");
        fs::write(paths.binary_path(SERVER_BINARY_NAME), "").expect("server placeholder");
        #[cfg(unix)]
        make_runnable(&paths.binary_path(SERVER_BINARY_NAME));
        let spec = build_server_launch_spec(&paths, &settings, &secrets).expect("spec");

        assert_eq!(spec.program, paths.binary_path(SERVER_BINARY_NAME));
        assert!(spec.args.contains(&"--port".to_string()));
        assert_eq!(
            spec.env.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some("from-secrets")
        );
        assert!(spec.remove_env.contains(&"ANTHROPIC_API_KEY"));
        assert_eq!(
            spec.env.get("MEMORY_BANK_LLM_MODEL").map(String::as_str),
            Some("claude-custom")
        );
    }

    #[test]
    fn launchd_plist_points_to_mb_binary_and_log_file() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let rendered = render_launchd_plist(&paths, true);
        assert!(rendered.contains("internal"));
        assert!(rendered.contains("run-server"));
        assert!(rendered.contains(paths.binary_path(MB_BINARY_NAME).to_string_lossy().as_ref()));
        assert!(rendered.contains(paths.log_file.to_string_lossy().as_ref()));
        assert!(rendered.contains("<true/>"));
    }

    #[test]
    fn launchd_plist_disables_run_at_load_when_autostart_is_off() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let rendered = render_launchd_plist(&paths, false);

        assert!(rendered.contains("<key>RunAtLoad</key>\n    <false/>"));
        assert!(rendered.contains("<key>KeepAlive</key>\n    <false/>"));
    }

    #[test]
    fn systemd_unit_redirects_to_app_log_file() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let rendered = render_systemd_unit(&paths);
        assert!(rendered.contains("ExecStart=/bin/sh -lc"));
        assert!(rendered.contains("internal run-server"));
        assert!(rendered.contains("server.log"));
    }

    #[test]
    fn launch_spec_requires_remote_encoder_api_key_when_remote_api_is_selected() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings = AppSettings {
            server: Some(ServerSettings {
                llm_provider: Some("ollama".to_string()),
                encoder_provider: Some("remote-api".to_string()),
                remote_encoder_url: Some("https://encoder.example.com".to_string()),
                ..ServerSettings::default()
            }),
            ..AppSettings::default()
        };
        let secrets = SecretStore::default();

        fs::create_dir_all(&paths.bin_dir).expect("bin dir");
        fs::write(paths.binary_path(SERVER_BINARY_NAME), "").expect("server placeholder");
        #[cfg(unix)]
        make_runnable(&paths.binary_path(SERVER_BINARY_NAME));

        let error =
            build_server_launch_spec(&paths, &settings, &secrets).expect_err("missing api key");

        assert!(error
            .to_string()
            .contains("MEMORY_BANK_REMOTE_ENCODER_API_KEY"));
    }
}
