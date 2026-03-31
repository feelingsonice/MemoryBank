use crate::AppError;
use crate::assets::{ExposureCheck, inspect_cli_exposure};
use crate::command_utils::{current_uid, run_command, run_command_capture, shell_escape};
use crate::config::{
    llm_provider_value, normalize_ollama_url, validate_encoder_provider, validate_llm_provider,
};
use crate::constants::{
    HEALTH_POLL_INTERVAL, HEALTH_STARTUP_TIMEOUT, HOOK_BINARY_NAME, LAUNCHD_LABEL,
    LOG_TAIL_LINE_COUNT, MB_BINARY_NAME, MCP_PROXY_BINARY_NAME, SERVER_BINARY_NAME,
    SERVICE_TRANSITION_POLL_INTERVAL, SERVICE_TRANSITION_TIMEOUT, SYSTEMD_UNIT_NAME,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ServiceManager {
    Launchd,
    Systemd,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct HealthCheck {
    pub(crate) ok: bool,
    pub(crate) namespace: String,
    pub(crate) port: u16,
    pub(crate) llm_provider: String,
    pub(crate) encoder_provider: String,
    pub(crate) version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceStatus {
    pub(crate) installed: bool,
    pub(crate) active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceRuntimeSummary {
    pub(crate) manager: ServiceManager,
    pub(crate) definition_path: PathBuf,
    pub(crate) log_path: PathBuf,
    pub(crate) url: String,
    pub(crate) installed: bool,
    pub(crate) active: bool,
    pub(crate) pid: Option<u32>,
    pub(crate) health: Option<HealthCheck>,
    pub(crate) health_error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ServiceActionKind {
    Install,
    Start,
    Restart,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceActionReport {
    pub(crate) action: ServiceActionKind,
    pub(crate) manager: ServiceManager,
    pub(crate) definition_path: PathBuf,
    pub(crate) log_path: PathBuf,
    pub(crate) url: String,
    pub(crate) autostart: bool,
    pub(crate) installed_before: bool,
    pub(crate) active_before: bool,
    pub(crate) installed_after: bool,
    pub(crate) active_after: bool,
    pub(crate) installed_during_action: bool,
    pub(crate) fell_back_to_start: bool,
    pub(crate) pid: Option<u32>,
    pub(crate) health: Option<HealthCheck>,
    pub(crate) health_error: Option<String>,
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

    fn manager(self) -> ServiceManager {
        match self {
            Self::MacOs => ServiceManager::Launchd,
            Self::Linux => ServiceManager::Systemd,
        }
    }

    fn definition_path(self, paths: &AppPaths) -> PathBuf {
        match self {
            Self::MacOs => launchd_service_path(paths),
            Self::Linux => systemd_service_path(paths),
        }
    }
}

impl ServiceManager {
    pub(crate) fn display_name(self) -> &'static str {
        match self {
            Self::Launchd => "launchd",
            Self::Systemd => "systemd --user",
        }
    }
}

pub(crate) fn install_service(
    paths: &AppPaths,
    settings: &AppSettings,
) -> Result<ServiceActionReport, AppError> {
    paths.ensure_base_dirs()?;
    let platform = ManagedPlatform::detect()?;
    let before = service_status(paths)?;
    match platform {
        ManagedPlatform::MacOs => install_launchd_service(paths, settings)?,
        ManagedPlatform::Linux => install_systemd_service(paths, settings)?,
    }
    let after = service_status(paths)?;

    Ok(ServiceActionReport {
        action: ServiceActionKind::Install,
        manager: platform.manager(),
        definition_path: platform.definition_path(paths),
        log_path: paths.log_file.clone(),
        url: default_server_url(settings),
        autostart: settings.resolved_autostart(),
        installed_before: before.installed,
        active_before: before.active,
        installed_after: after.installed,
        active_after: after.active,
        installed_during_action: !before.installed && after.installed,
        fell_back_to_start: false,
        pid: if after.active {
            best_effort_service_pid(paths, platform)
        } else {
            None
        },
        health: None,
        health_error: None,
    })
}

pub(crate) fn start_service(
    paths: &AppPaths,
    settings: &AppSettings,
) -> Result<ServiceActionReport, AppError> {
    let platform = ManagedPlatform::detect()?;
    let before = service_status(paths)?;
    let installed_during_action = match platform {
        ManagedPlatform::MacOs => {
            let uid = current_uid()?;
            let service_path = launchd_service_path(paths);
            let mut installed_during_action = false;
            if !service_path.exists() {
                install_launchd_service(paths, settings)?;
                installed_during_action = true;
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
                )?;
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
                )?;
            }
            installed_during_action
        }
        ManagedPlatform::Linux => {
            let mut installed_during_action = false;
            if !systemd_service_path(paths).exists() {
                install_systemd_service(paths, settings)?;
                installed_during_action = true;
            }
            run_command("systemctl", &["--user", "start", SYSTEMD_UNIT_NAME])?;
            installed_during_action
        }
    };

    let after = wait_for_service_status(
        paths,
        true,
        SERVICE_TRANSITION_TIMEOUT,
        SERVICE_TRANSITION_POLL_INTERVAL,
    )?;
    let (health, health_error) = wait_for_service_health(settings, after.active);

    Ok(ServiceActionReport {
        action: ServiceActionKind::Start,
        manager: platform.manager(),
        definition_path: platform.definition_path(paths),
        log_path: paths.log_file.clone(),
        url: default_server_url(settings),
        autostart: settings.resolved_autostart(),
        installed_before: before.installed,
        active_before: before.active,
        installed_after: after.installed,
        active_after: after.active,
        installed_during_action,
        fell_back_to_start: false,
        pid: if after.active {
            best_effort_service_pid(paths, platform)
        } else {
            None
        },
        health,
        health_error,
    })
}

pub(crate) fn stop_service(
    paths: &AppPaths,
    settings: &AppSettings,
) -> Result<ServiceActionReport, AppError> {
    let platform = ManagedPlatform::detect()?;
    let before = service_status(paths)?;
    let stop_error = if before.installed && before.active {
        match platform {
            ManagedPlatform::MacOs => {
                let uid = current_uid()?;
                run_command(
                    "launchctl",
                    &["bootout", &format!("gui/{uid}/{LAUNCHD_LABEL}")],
                )
                .err()
            }
            ManagedPlatform::Linux => {
                run_command("systemctl", &["--user", "stop", SYSTEMD_UNIT_NAME]).err()
            }
        }
    } else {
        None
    };
    let after = wait_for_service_status(
        paths,
        true,
        SERVICE_TRANSITION_TIMEOUT,
        SERVICE_TRANSITION_POLL_INTERVAL,
    )?;
    if let Some(error) = stop_error
        && after.active
    {
        return Err(error);
    }

    Ok(ServiceActionReport {
        action: ServiceActionKind::Stop,
        manager: platform.manager(),
        definition_path: platform.definition_path(paths),
        log_path: paths.log_file.clone(),
        url: default_server_url(settings),
        autostart: settings.resolved_autostart(),
        installed_before: before.installed,
        active_before: before.active,
        installed_after: after.installed,
        active_after: after.active,
        installed_during_action: false,
        fell_back_to_start: false,
        pid: if after.active {
            best_effort_service_pid(paths, platform)
        } else {
            None
        },
        health: None,
        health_error: None,
    })
}

pub(crate) fn restart_service(
    paths: &AppPaths,
    settings: &AppSettings,
) -> Result<ServiceActionReport, AppError> {
    let platform = ManagedPlatform::detect()?;
    let before = service_status(paths)?;
    let (installed_during_action, fell_back_to_start) = match platform {
        ManagedPlatform::MacOs => {
            if before.active {
                let uid = current_uid()?;
                run_command(
                    "launchctl",
                    &["kickstart", "-k", &format!("gui/{uid}/{LAUNCHD_LABEL}")],
                )?;
                (false, false)
            } else {
                let report = start_service(paths, settings)?;
                return Ok(ServiceActionReport {
                    action: ServiceActionKind::Restart,
                    fell_back_to_start: true,
                    ..report
                });
            }
        }
        ManagedPlatform::Linux => {
            if !systemd_service_path(paths).exists() {
                let report = start_service(paths, settings)?;
                return Ok(ServiceActionReport {
                    action: ServiceActionKind::Restart,
                    fell_back_to_start: true,
                    ..report
                });
            }
            if before.active {
                run_command("systemctl", &["--user", "restart", SYSTEMD_UNIT_NAME])?;
                (false, false)
            } else {
                let report = start_service(paths, settings)?;
                return Ok(ServiceActionReport {
                    action: ServiceActionKind::Restart,
                    fell_back_to_start: true,
                    ..report
                });
            }
        }
    };
    let after = wait_for_service_status(
        paths,
        true,
        SERVICE_TRANSITION_TIMEOUT,
        SERVICE_TRANSITION_POLL_INTERVAL,
    )?;
    let (health, health_error) = wait_for_service_health(settings, after.active);

    Ok(ServiceActionReport {
        action: ServiceActionKind::Restart,
        manager: platform.manager(),
        definition_path: platform.definition_path(paths),
        log_path: paths.log_file.clone(),
        url: default_server_url(settings),
        autostart: settings.resolved_autostart(),
        installed_before: before.installed,
        active_before: before.active,
        installed_after: after.installed,
        active_after: after.active,
        installed_during_action,
        fell_back_to_start,
        pid: if after.active {
            best_effort_service_pid(paths, platform)
        } else {
            None
        },
        health,
        health_error,
    })
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

pub(crate) fn service_runtime_summary(
    paths: &AppPaths,
    settings: &AppSettings,
) -> Result<ServiceRuntimeSummary, AppError> {
    let platform = ManagedPlatform::detect()?;
    let status = service_status(paths)?;
    let health_result = fetch_health(settings);
    let (health, health_error) = match health_result {
        Ok(health) => (Some(health), None),
        Err(error) => (None, Some(error.to_string())),
    };

    Ok(ServiceRuntimeSummary {
        manager: platform.manager(),
        definition_path: platform.definition_path(paths),
        log_path: paths.log_file.clone(),
        url: default_server_url(settings),
        installed: status.installed,
        active: status.active,
        pid: if status.active {
            best_effort_service_pid(paths, platform)
        } else {
            None
        },
        health,
        health_error,
    })
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

fn wait_for_service_status(
    paths: &AppPaths,
    desired_active: bool,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<ServiceStatus, AppError> {
    let deadline = Instant::now() + timeout;
    let mut last = service_status(paths)?;
    if last.active == desired_active {
        return Ok(last);
    }

    loop {
        if Instant::now() >= deadline {
            return Ok(last);
        }

        thread::sleep(poll_interval);
        last = service_status(paths)?;
        if last.active == desired_active {
            return Ok(last);
        }
    }
}

fn wait_for_service_health(
    settings: &AppSettings,
    service_active: bool,
) -> (Option<HealthCheck>, Option<String>) {
    if !service_active {
        return (None, None);
    }

    match wait_for_health(settings, HEALTH_STARTUP_TIMEOUT, HEALTH_POLL_INTERVAL) {
        Ok(health) => (Some(health), None),
        Err(error) => (None, Some(error.to_string())),
    }
}

fn best_effort_service_pid(_paths: &AppPaths, platform: ManagedPlatform) -> Option<u32> {
    match platform {
        ManagedPlatform::MacOs => {
            let uid = current_uid().ok()?;
            let domain = format!("gui/{uid}/{LAUNCHD_LABEL}");
            let outcome = run_command_capture("launchctl", &["print", &domain]).ok()?;
            launchctl_pid_from_output(&outcome.combined_output())
        }
        ManagedPlatform::Linux => {
            let outcome = run_command_capture(
                "systemctl",
                &[
                    "--user",
                    "show",
                    SYSTEMD_UNIT_NAME,
                    "--property",
                    "MainPID",
                    "--value",
                ],
            )
            .ok()?;
            systemd_main_pid_from_output(&outcome.combined_output())
        }
    }
}

fn launchctl_pid_from_output(output: &str) -> Option<u32> {
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("pid = ") {
            return rest.trim().parse::<u32>().ok().filter(|pid| *pid > 0);
        }
        if let Some((_, rest)) = trimmed.split_once("pid = ") {
            return rest.trim().parse::<u32>().ok().filter(|pid| *pid > 0);
        }
    }
    None
}

fn systemd_main_pid_from_output(output: &str) -> Option<u32> {
    let trimmed = output.trim();
    if trimmed.is_empty() || trimmed == "0" {
        None
    } else {
        trimmed.parse::<u32>().ok().filter(|pid| *pid > 0)
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
                "log file does not exist yet: {}. The managed service may not have started yet. Try `mb service status` or `mb service start`.",
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
        let secret = require_non_empty_secret(secrets, secret_key)
            .ok_or(AppError::MissingProviderSecret(secret_key))?;
        env.insert(secret_key.to_string(), secret.to_string());
    }
    if encoder_provider == "remote-api" {
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

    if let Some(env_key) = env_key_for_provider(llm_provider_value(settings))
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

fn require_non_empty_secret<'a>(secrets: &'a SecretStore, key: &str) -> Option<&'a str> {
    secrets.get(key).filter(|value| !value.trim().is_empty())
}

fn is_runnable_file(path: &Path) -> bool {
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

        assert!(
            error
                .to_string()
                .contains("MEMORY_BANK_REMOTE_ENCODER_API_KEY")
        );
    }

    #[test]
    fn launch_spec_requires_provider_secret_for_hosted_models() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings = AppSettings {
            server: Some(ServerSettings {
                llm_provider: Some("gemini".to_string()),
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
            build_server_launch_spec(&paths, &settings, &secrets).expect_err("missing secret");

        assert!(matches!(
            error,
            AppError::MissingProviderSecret("GEMINI_API_KEY")
        ));
    }

    #[test]
    fn launch_spec_rejects_blank_provider_secret_values() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings = AppSettings {
            server: Some(ServerSettings {
                llm_provider: Some("gemini".to_string()),
                ..ServerSettings::default()
            }),
            ..AppSettings::default()
        };
        let mut secrets = SecretStore::default();
        secrets.set("GEMINI_API_KEY", "   ");

        fs::create_dir_all(&paths.bin_dir).expect("bin dir");
        fs::write(paths.binary_path(SERVER_BINARY_NAME), "").expect("server placeholder");
        #[cfg(unix)]
        make_runnable(&paths.binary_path(SERVER_BINARY_NAME));

        let error =
            build_server_launch_spec(&paths, &settings, &secrets).expect_err("blank secret");

        assert!(matches!(
            error,
            AppError::MissingProviderSecret("GEMINI_API_KEY")
        ));
    }

    #[test]
    fn launch_spec_requires_local_encoder_url_for_local_api() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings = AppSettings {
            server: Some(ServerSettings {
                llm_provider: Some("ollama".to_string()),
                encoder_provider: Some("local-api".to_string()),
                ..ServerSettings::default()
            }),
            ..AppSettings::default()
        };
        let secrets = SecretStore::default();

        fs::create_dir_all(&paths.bin_dir).expect("bin dir");
        fs::write(paths.binary_path(SERVER_BINARY_NAME), "").expect("server placeholder");
        #[cfg(unix)]
        make_runnable(&paths.binary_path(SERVER_BINARY_NAME));

        let error = build_server_launch_spec(&paths, &settings, &secrets).expect_err("missing url");

        assert!(error.to_string().contains("server.local_encoder_url"));
    }

    #[test]
    fn launch_spec_includes_ollama_and_remote_encoder_environment() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings = AppSettings {
            service: Some(ServiceSettings {
                port: Some(4040),
                autostart: None,
            }),
            server: Some(ServerSettings {
                llm_provider: Some("ollama".to_string()),
                llm_model: Some("llama3.1".to_string()),
                ollama_url: Some("http://ollama.internal:11434/".to_string()),
                encoder_provider: Some("remote-api".to_string()),
                remote_encoder_url: Some("https://encoder.example.com".to_string()),
                nearest_neighbor_count: Some(15),
                ..ServerSettings::default()
            }),
            ..AppSettings::default()
        };
        let mut secrets = SecretStore::default();
        secrets.set("MEMORY_BANK_REMOTE_ENCODER_API_KEY", "remote-secret");

        fs::create_dir_all(&paths.bin_dir).expect("bin dir");
        fs::write(paths.binary_path(SERVER_BINARY_NAME), "").expect("server placeholder");
        #[cfg(unix)]
        make_runnable(&paths.binary_path(SERVER_BINARY_NAME));

        let spec = build_server_launch_spec(&paths, &settings, &secrets).expect("spec");

        assert_eq!(
            spec.env.get("MEMORY_BANK_OLLAMA_MODEL").map(String::as_str),
            Some("llama3.1")
        );
        assert_eq!(
            spec.env.get("MEMORY_BANK_OLLAMA_URL").map(String::as_str),
            Some("http://ollama.internal:11434")
        );
        assert_eq!(
            spec.env
                .get("MEMORY_BANK_REMOTE_ENCODER_API_KEY")
                .map(String::as_str),
            Some("remote-secret")
        );
        assert_eq!(
            spec.env
                .get("MEMORY_BANK_REMOTE_ENCODER_URL")
                .map(String::as_str),
            Some("https://encoder.example.com")
        );
        assert!(spec.args.contains(&"4040".to_string()));
        assert!(spec.args.contains(&"15".to_string()));
    }

    #[test]
    fn launch_spec_rejects_invalid_nearest_neighbor_count() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings = AppSettings {
            server: Some(ServerSettings {
                llm_provider: Some("ollama".to_string()),
                nearest_neighbor_count: Some(0),
                ..ServerSettings::default()
            }),
            ..AppSettings::default()
        };
        let secrets = SecretStore::default();

        fs::create_dir_all(&paths.bin_dir).expect("bin dir");
        fs::write(paths.binary_path(SERVER_BINARY_NAME), "").expect("server placeholder");
        #[cfg(unix)]
        make_runnable(&paths.binary_path(SERVER_BINARY_NAME));

        let error = build_server_launch_spec(&paths, &settings, &secrets)
            .expect_err("invalid nearest neighbor count");

        assert!(error.to_string().contains("at least 1"));
    }

    #[test]
    fn launchctl_pid_parser_extracts_pid_when_present() {
        let output = "service = com.memory-bank.mb\n    pid = 4242\n";

        assert_eq!(launchctl_pid_from_output(output), Some(4242));
        assert_eq!(
            launchctl_pid_from_output("service = com.memory-bank.mb"),
            None
        );
    }

    #[test]
    fn systemd_pid_parser_ignores_empty_and_zero_values() {
        assert_eq!(systemd_main_pid_from_output("4242"), Some(4242));
        assert_eq!(systemd_main_pid_from_output("0"), None);
        assert_eq!(systemd_main_pid_from_output(""), None);
    }
}
