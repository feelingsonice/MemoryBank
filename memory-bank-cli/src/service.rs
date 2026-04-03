mod definitions;
mod launch;

use crate::AppError;
use crate::command_utils::{current_uid, run_command, run_command_capture};
use crate::constants::{
    HEALTH_POLL_INTERVAL, HEALTH_STARTUP_TIMEOUT, LAUNCHD_LABEL, LOG_TAIL_LINE_COUNT,
    SERVICE_TRANSITION_POLL_INTERVAL, SERVICE_TRANSITION_TIMEOUT, SYSTEMD_UNIT_NAME,
};
use memory_bank_app::{AppPaths, AppSettings, ServerStartupState, default_server_url};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use self::definitions::{
    install_launchd_service, install_systemd_service, launchd_service_path, systemd_service_path,
};
#[cfg(test)]
use self::definitions::{render_launchd_plist, render_systemd_unit};
pub(crate) use self::launch::{build_server_launch_spec, collect_doctor_issues};

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
    #[serde(default)]
    pub(crate) llm_model_id: Option<String>,
    #[serde(default)]
    pub(crate) encoder_model_id: Option<String>,
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
    pub(crate) startup_state: Option<ServerStartupState>,
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
        false,
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
    let before_pid = before
        .active
        .then(|| best_effort_service_pid(paths, platform))
        .flatten();
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
    let after = if before.active {
        wait_for_service_restart(
            paths,
            platform,
            before_pid,
            SERVICE_TRANSITION_TIMEOUT,
            SERVICE_TRANSITION_POLL_INTERVAL,
        )?
    } else {
        wait_for_service_status(
            paths,
            true,
            SERVICE_TRANSITION_TIMEOUT,
            SERVICE_TRANSITION_POLL_INTERVAL,
        )?
    };
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
    let pid = if status.active {
        best_effort_service_pid(paths, platform)
    } else {
        None
    };
    let health_result = fetch_health(settings);
    let (health, health_error) = match health_result {
        Ok(health) => (Some(health), None),
        Err(error) => (None, Some(error.to_string())),
    };
    let startup_state = load_server_startup_state(paths, settings, status.active, pid);

    Ok(ServiceRuntimeSummary {
        manager: platform.manager(),
        definition_path: platform.definition_path(paths),
        log_path: paths.log_file.clone(),
        url: default_server_url(settings),
        installed: status.installed,
        active: status.active,
        pid,
        health,
        health_error,
        startup_state,
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

fn wait_for_service_restart(
    paths: &AppPaths,
    platform: ManagedPlatform,
    before_pid: Option<u32>,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<ServiceStatus, AppError> {
    let deadline = Instant::now() + timeout;
    let mut saw_inactive = false;
    let mut observed_follow_up_poll = false;
    let mut last = service_status(paths)?;

    loop {
        if !last.active {
            saw_inactive = true;
        } else {
            let current_pid = best_effort_service_pid(paths, platform);
            let pid_changed = matches!(
                (before_pid, current_pid),
                (Some(before_pid), Some(current_pid)) if current_pid != before_pid
            );
            if saw_inactive || pid_changed || (before_pid.is_none() && observed_follow_up_poll) {
                return Ok(last);
            }
        }

        if Instant::now() >= deadline {
            return Ok(last);
        }

        thread::sleep(poll_interval);
        observed_follow_up_poll = true;
        last = service_status(paths)?;
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

fn load_server_startup_state(
    paths: &AppPaths,
    settings: &AppSettings,
    service_active: bool,
    pid: Option<u32>,
) -> Option<ServerStartupState> {
    if !service_active {
        return None;
    }

    let path = paths.server_startup_state_path(&settings.active_namespace());
    let contents = fs::read_to_string(path).ok()?;
    let state = serde_json::from_str::<ServerStartupState>(&contents).ok()?;
    if state.namespace != settings.active_namespace().to_string() {
        return None;
    }

    if let Some(pid) = pid
        && state.pid != pid
    {
        return None;
    }

    Some(state)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{MB_BINARY_NAME, OLLAMA_HISTORY_WINDOW_SIZE, SERVER_BINARY_NAME};
    use memory_bank_app::SecretStore;
    use memory_bank_app::{ServerSettings, ServiceSettings};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
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
                history_window_size: Some(7),
                max_processing_attempts: Some(12),
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
        assert!(spec.args.contains(&"7".to_string()));
        assert!(spec.args.contains(&"--max-processing-attempts".to_string()));
        assert!(spec.args.contains(&"12".to_string()));
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
                history_window_size: Some(99),
                nearest_neighbor_count: Some(15),
                max_processing_attempts: Some(11),
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
        assert!(spec.args.contains(&"11".to_string()));
        assert!(spec.args.contains(&OLLAMA_HISTORY_WINDOW_SIZE.to_string()));
        assert!(!spec.args.contains(&"99".to_string()));
    }

    #[test]
    fn launch_spec_includes_custom_openai_endpoint_environment() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings = AppSettings {
            server: Some(ServerSettings {
                llm_provider: Some("open-ai".to_string()),
                llm_model: Some("qwen3.6-plus-free".to_string()),
                openai_url: Some("https://opencode.ai/zen/v1/".to_string()),
                ..ServerSettings::default()
            }),
            ..AppSettings::default()
        };
        let mut secrets = SecretStore::default();
        secrets.set("OPENAI_API_KEY", "openai-secret");

        fs::create_dir_all(&paths.bin_dir).expect("bin dir");
        fs::write(paths.binary_path(SERVER_BINARY_NAME), "").expect("server placeholder");
        #[cfg(unix)]
        make_runnable(&paths.binary_path(SERVER_BINARY_NAME));

        let spec = build_server_launch_spec(&paths, &settings, &secrets).expect("spec");

        assert_eq!(
            spec.env.get("OPENAI_API_KEY").map(String::as_str),
            Some("openai-secret")
        );
        assert_eq!(
            spec.env.get("OPENAI_BASE_URL").map(String::as_str),
            Some("https://opencode.ai/zen/v1")
        );
        assert!(spec.remove_env.contains(&"OPENAI_BASE_URL"));
    }

    #[test]
    fn launch_spec_omits_default_openai_endpoint_environment() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings = AppSettings {
            server: Some(ServerSettings {
                llm_provider: Some("open-ai".to_string()),
                openai_url: Some(memory_bank_app::DEFAULT_OPENAI_URL.to_string()),
                ..ServerSettings::default()
            }),
            ..AppSettings::default()
        };
        let mut secrets = SecretStore::default();
        secrets.set("OPENAI_API_KEY", "openai-secret");

        fs::create_dir_all(&paths.bin_dir).expect("bin dir");
        fs::write(paths.binary_path(SERVER_BINARY_NAME), "").expect("server placeholder");
        #[cfg(unix)]
        make_runnable(&paths.binary_path(SERVER_BINARY_NAME));

        let spec = build_server_launch_spec(&paths, &settings, &secrets).expect("spec");

        assert!(!spec.env.contains_key("OPENAI_BASE_URL"));
        assert!(spec.remove_env.contains(&"OPENAI_BASE_URL"));
    }

    #[test]
    fn launch_spec_rejects_invalid_openai_url() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings = AppSettings {
            server: Some(ServerSettings {
                llm_provider: Some("open-ai".to_string()),
                openai_url: Some("https://opencode.ai/zen/v1?foo=bar".to_string()),
                ..ServerSettings::default()
            }),
            ..AppSettings::default()
        };
        let mut secrets = SecretStore::default();
        secrets.set("OPENAI_API_KEY", "openai-secret");

        fs::create_dir_all(&paths.bin_dir).expect("bin dir");
        fs::write(paths.binary_path(SERVER_BINARY_NAME), "").expect("server placeholder");
        #[cfg(unix)]
        make_runnable(&paths.binary_path(SERVER_BINARY_NAME));

        let error = build_server_launch_spec(&paths, &settings, &secrets)
            .expect_err("invalid openai url should fail");

        assert!(error.to_string().contains("server.openai_url"));
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
    fn launch_spec_rejects_invalid_max_processing_attempts() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings = AppSettings {
            server: Some(ServerSettings {
                llm_provider: Some("ollama".to_string()),
                max_processing_attempts: Some(0),
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
            .expect_err("invalid max processing attempts");

        assert!(error.to_string().contains("server.max_processing_attempts"));
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

    #[test]
    fn load_server_startup_state_uses_matching_pid_and_namespace() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings = AppSettings::default();
        let namespace = settings.active_namespace();
        fs::create_dir_all(paths.namespace_dir(&namespace)).expect("namespace dir");
        let startup_state = memory_bank_app::ServerStartupState {
            pid: 4242,
            namespace: namespace.to_string(),
            phase: memory_bank_app::ServerStartupPhase::Reindexing,
            memory_count: Some(12),
        };
        fs::write(
            paths.server_startup_state_path(&namespace),
            serde_json::to_vec_pretty(&startup_state).expect("startup state json"),
        )
        .expect("startup state file");

        let loaded = load_server_startup_state(&paths, &settings, true, Some(4242));

        assert_eq!(loaded, Some(startup_state));
    }

    #[test]
    fn load_server_startup_state_ignores_pid_mismatch() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings = AppSettings::default();
        let namespace = settings.active_namespace();
        fs::create_dir_all(paths.namespace_dir(&namespace)).expect("namespace dir");
        let startup_state = memory_bank_app::ServerStartupState {
            pid: 1111,
            namespace: namespace.to_string(),
            phase: memory_bank_app::ServerStartupPhase::Reindexing,
            memory_count: Some(12),
        };
        fs::write(
            paths.server_startup_state_path(&namespace),
            serde_json::to_vec_pretty(&startup_state).expect("startup state json"),
        )
        .expect("startup state file");

        let loaded = load_server_startup_state(&paths, &settings, true, Some(4242));

        assert_eq!(loaded, None);
    }
}
