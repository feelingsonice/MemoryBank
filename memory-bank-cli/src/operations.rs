mod render;

use crate::AppError;
use crate::agents::{AgentKind, integration_configured};
use crate::assets::{ExposureMode, materialize_and_expose_cli, materialize_install_artifacts};
use crate::cli::{ConfigCommand, NamespaceCommand, ServiceCommand};
use crate::command_utils::yes_no;
use crate::config::{
    get_config_value, llm_provider_value, resolved_encoder_provider, resolved_llm_model,
    resolved_ollama_url, set_config_value,
};
use crate::output::{
    print_action_start, print_key_value, styled_command, styled_failure, styled_section,
    styled_subtle, styled_success, styled_title, styled_warning,
};
use crate::service::{
    ServiceStatus, build_server_launch_spec, collect_doctor_issues, install_service,
    restart_service, service_runtime_summary, service_status, start_service, stop_service,
    tail_log_file,
};
use memory_bank_app::{AppPaths, AppSettings, DEFAULT_NAMESPACE_NAME, Namespace, SecretStore};
use std::fs;
use std::os::unix::process::CommandExt;
use std::process::Command as ProcessCommand;

use self::render::{
    describe_cli_exposure, describe_install_attempt, describe_start_attempt,
    print_install_result, print_live_runtime_section, print_namespace_apply_result,
    print_service_section, print_start_or_restart_result, print_stop_result,
    runtime_health_warning, runtime_mismatch_fields,
};

pub(crate) fn run_status() -> Result<(), AppError> {
    let paths = AppPaths::from_system()?;
    let settings = AppSettings::load(&paths)?;
    let runtime = service_runtime_summary(&paths, &settings)?;
    let provider = llm_provider_value(&settings);
    let model = resolved_llm_model(&settings);
    let encoder = resolved_encoder_provider(&settings);

    println!("{}", styled_title("Memory Bank"));
    println!();
    println!("{}", styled_section("Saved configuration"));
    print_key_value("Namespace", settings.active_namespace());
    print_key_value("URL", &runtime.url);
    print_key_value("Port", settings.resolved_port());
    print_key_value("Provider", provider);
    print_key_value("Model", &model);
    print_key_value("Encoder", encoder);
    if provider == "ollama" {
        print_key_value(
            "Ollama URL",
            resolved_ollama_url(
                settings
                    .server
                    .as_ref()
                    .and_then(|server| server.ollama_url.as_deref()),
            ),
        );
    }
    print_key_value("Log file", paths.log_file.display());

    println!();
    print_service_section(&runtime);
    println!();
    print_live_runtime_section(&runtime);
    println!();
    println!("{}", styled_section("Integrations"));
    for agent in AgentKind::all() {
        let configured = integration_configured(&settings, agent);
        print_key_value(agent.display_name(), yes_no(configured));
    }

    if let Some(health) = runtime.health.as_ref() {
        let mismatch_fields = runtime_mismatch_fields(&settings, provider, encoder, health);
        if !mismatch_fields.is_empty() {
            println!();
            println!(
                "{}",
                styled_warning(&format!(
                    "Warning: Saved configuration differs from the running service for {}. Restart with {} to apply the saved settings.",
                    mismatch_fields.join(", "),
                    styled_command("mb service restart")
                ))
            );
        }
    }
    if let Some(message) = runtime_health_warning(&runtime) {
        println!();
        println!("{}", styled_warning(&format!("Warning: {message}")));
    }

    Ok(())
}

pub(crate) fn run_doctor(fix: bool) -> Result<(), AppError> {
    let paths = AppPaths::from_system()?;
    let mut settings = AppSettings::load(&paths)?;
    let issues_before = collect_doctor_issues(&paths, &settings)?;
    let mut fix_attempts = Vec::new();
    let issues_after = if fix {
        paths.ensure_base_dirs()?;
        let exposure = materialize_and_expose_cli(&paths)?;
        fix_attempts.push(describe_cli_exposure(exposure.mode));

        let secrets = SecretStore::load(&paths)?;
        let service = service_status(&paths)?;
        if !service.installed {
            let report = install_service(&paths, &settings)?;
            fix_attempts.push(describe_install_attempt(&report));
        }

        let service = service_status(&paths)?;
        if !service.active {
            if build_server_launch_spec(&paths, &settings, &secrets).is_ok() {
                let report = start_service(&paths, &settings)?;
                fix_attempts.push(describe_start_attempt(&report));
            } else {
                fix_attempts.push(
                    "Skipped service start because the current configuration is incomplete."
                        .to_string(),
                );
            }
        }

        settings = AppSettings::load(&paths)?;
        collect_doctor_issues(&paths, &settings)?
    } else {
        issues_before.clone()
    };

    let doctor_outcome = doctor_outcome(fix, &issues_before, &issues_after);
    println!("{}", styled_title("Memory Bank doctor"));
    println!();

    match doctor_outcome {
        DoctorOutcome::Healthy => {
            println!("{}", styled_success("Success: No issues found."));
        }
        DoctorOutcome::IssuesFound => {
            println!(
                "{}",
                styled_warning(&format!(
                    "Warning: Found {} issue{}.",
                    issues_before.len(),
                    plural_suffix(issues_before.len())
                ))
            );
        }
        DoctorOutcome::FixedCleanly => {
            println!(
                "{}",
                styled_success("Success: Automatic fixes completed and no issues remain.")
            );
        }
        DoctorOutcome::FixedPartially => {
            println!(
                "{}",
                styled_warning(&format!(
                    "Warning: Automatic fixes ran, but {} issue{} remain.",
                    issues_after.len(),
                    plural_suffix(issues_after.len())
                ))
            );
        }
    }

    if !issues_before.is_empty() {
        println!();
        println!("{}", styled_section("Issues found"));
        for issue in &issues_before {
            println!("  - {issue}");
        }
    }

    if fix && !fix_attempts.is_empty() {
        println!();
        println!("{}", styled_section("Fix attempts"));
        for attempt in &fix_attempts {
            println!("  - {attempt}");
        }
    }

    if fix && !issues_after.is_empty() {
        println!();
        println!("{}", styled_section("Remaining issues"));
        for issue in &issues_after {
            println!("  - {issue}");
        }
    }

    if matches!(
        doctor_outcome,
        DoctorOutcome::Healthy | DoctorOutcome::FixedCleanly
    ) && let Ok(runtime) = service_runtime_summary(&paths, &settings)
    {
        println!();
        println!("{}", styled_section("Healthy summary"));
        print_key_value("URL", &runtime.url);
        print_key_value("Log file", runtime.log_path.display());
        print_key_value("Service active", yes_no(runtime.active));
        match runtime.health.as_ref() {
            Some(health) => {
                print_key_value("Health", yes_no(health.ok));
                print_key_value("Namespace", &health.namespace);
                print_key_value("Port", health.port);
            }
            None => {
                print_key_value("Health", "unavailable");
            }
        }
    }

    if matches!(
        doctor_outcome,
        DoctorOutcome::IssuesFound | DoctorOutcome::FixedPartially
    ) {
        println!();
        println!(
            "{}",
            styled_warning(&format!(
                "Warning: Check {} and {} for more detail.",
                styled_command("mb service status"),
                styled_command("mb logs -f")
            ))
        );
    }

    Ok(())
}

pub(crate) fn run_logs(follow: bool) -> Result<(), AppError> {
    let paths = AppPaths::from_system()?;
    tail_log_file(&paths.log_file, follow)
}

pub(crate) fn run_namespace(command: NamespaceCommand) -> Result<(), AppError> {
    let paths = AppPaths::from_system()?;
    let mut settings = AppSettings::load(&paths)?;

    match command {
        NamespaceCommand::List => {
            paths.ensure_base_dirs()?;
            let mut namespaces = list_namespaces(&paths)?;
            let active = settings.active_namespace();
            if !namespaces.contains(&active) {
                namespaces.push(active);
                namespaces.sort();
                namespaces.dedup();
            }

            for namespace in namespaces {
                let suffix = if namespace == settings.active_namespace() {
                    " (active)"
                } else {
                    ""
                };
                println!("{}{}", namespace, suffix);
            }
            Ok(())
        }
        NamespaceCommand::Create { name } => {
            let raw_name = name.trim().to_string();
            let namespace = Namespace::new(&raw_name);
            let existed_before = paths.namespace_dir(&namespace).is_dir();
            let directory = paths.ensure_namespace_dir(&namespace)?;

            if existed_before {
                println!(
                    "{}",
                    styled_warning(&format!(
                        "Warning: Namespace `{namespace}` already existed."
                    ))
                );
            } else {
                println!(
                    "{}",
                    styled_success(&format!("Created namespace `{namespace}`."))
                );
            }
            print_key_value("Directory", directory.display());
            if raw_name != namespace.to_string() {
                println!(
                    "{}",
                    styled_warning(&format!(
                        "Warning: Requested name `{raw_name}` was sanitized to `{namespace}`."
                    ))
                );
            }
            Ok(())
        }
        NamespaceCommand::Use { name } => {
            let raw_name = name.trim().to_string();
            let namespace = Namespace::new(&raw_name);
            let directory = paths.ensure_namespace_dir(&namespace)?;
            settings.active_namespace = if namespace.as_ref() == DEFAULT_NAMESPACE_NAME {
                None
            } else {
                Some(namespace.to_string())
            };
            settings.save(&paths)?;

            println!(
                "{}",
                styled_success(&format!("Active namespace is now `{namespace}`."))
            );
            print_key_value("Directory", directory.display());
            if raw_name != namespace.to_string() {
                println!(
                    "{}",
                    styled_warning(&format!(
                        "Warning: Requested name `{raw_name}` was sanitized to `{namespace}`."
                    ))
                );
            }

            let status = service_status(&paths)?;
            if status.installed {
                println!();
                let report = if status.active {
                    run_action(
                        "Restarting the managed service to apply the new namespace...",
                        || restart_service(&paths, &settings),
                    )?
                } else {
                    run_action(
                        "Starting the managed service to apply the new namespace...",
                        || start_service(&paths, &settings),
                    )?
                };
                print_namespace_apply_result(&report);
            } else {
                println!();
                println!(
                    "{}",
                    styled_warning(
                        "Warning: The managed service is not installed, so this namespace will apply on the next service start."
                    )
                );
                println!(
                    "{}",
                    styled_subtle(&format!(
                        "Try {} when you are ready.",
                        styled_command("mb service start")
                    ))
                );
            }

            Ok(())
        }
        NamespaceCommand::Current => {
            println!("{}", settings.active_namespace());
            Ok(())
        }
    }
}

pub(crate) fn run_service(command: ServiceCommand) -> Result<(), AppError> {
    let paths = AppPaths::from_system()?;
    let settings = AppSettings::load(&paths)?;

    match command {
        ServiceCommand::Install => {
            let report = run_action("Installing the managed service definition...", || {
                materialize_install_artifacts(&paths)?;
                install_service(&paths, &settings)
            })?;
            print_install_result(&report);
            Ok(())
        }
        ServiceCommand::Start => {
            let report = run_action("Starting Memory Bank service...", || {
                materialize_install_artifacts(&paths)?;
                start_service(&paths, &settings)
            })?;
            print_start_or_restart_result(&report);
            Ok(())
        }
        ServiceCommand::Stop => {
            let report = run_action("Stopping Memory Bank service...", || {
                stop_service(&paths, &settings)
            })?;
            print_stop_result(&report);
            Ok(())
        }
        ServiceCommand::Restart => {
            let report = run_action("Restarting Memory Bank service...", || {
                materialize_install_artifacts(&paths)?;
                restart_service(&paths, &settings)
            })?;
            print_start_or_restart_result(&report);
            Ok(())
        }
        ServiceCommand::Status => {
            let runtime = service_runtime_summary(&paths, &settings)?;
            println!("{}", styled_title("Memory Bank service"));
            println!();
            print_service_section(&runtime);
            println!();
            print_live_runtime_section(&runtime);
            if let Some(message) = runtime_health_warning(&runtime) {
                println!();
                println!("{}", styled_warning(&format!("Warning: {message}")));
            }
            Ok(())
        }
        ServiceCommand::Logs { follow } => tail_log_file(&paths.log_file, follow),
    }
}

pub(crate) fn run_config(command: ConfigCommand) -> Result<(), AppError> {
    let paths = AppPaths::from_system()?;
    let mut settings = AppSettings::load(&paths)?;

    match command {
        ConfigCommand::Show => {
            let rendered = settings.to_toml_string()?;
            println!("{rendered}");
            Ok(())
        }
        ConfigCommand::Get { key } => {
            let value = get_config_value(&settings, &key)?;
            println!("{value}");
            Ok(())
        }
        ConfigCommand::Set { key, value } => {
            let previous = get_config_value(&settings, &key)?;
            set_config_value(&mut settings, &key, &value)?;
            settings.save(&paths)?;
            let current = get_config_value(&settings, &key)?;
            let service = service_status(&paths)?;

            println!("{}", styled_success(&format!("Updated `{key}`.")));
            print_key_value("Old value", &previous);
            print_key_value("New value", &current);
            if previous == current {
                println!(
                    "{}",
                    styled_warning("Warning: The saved value is unchanged after normalization.")
                );
            }
            println!(
                "{}",
                styled_subtle(&format!(
                    "Next step: {}",
                    config_change_hint(&key, &service)
                ))
            );
            Ok(())
        }
    }
}

pub(crate) fn run_internal_server() -> Result<(), AppError> {
    let paths = AppPaths::from_system()?;
    let settings = AppSettings::load(&paths)?;
    let secrets = SecretStore::load(&paths)?;
    let spec = build_server_launch_spec(&paths, &settings, &secrets)?;

    let mut command = ProcessCommand::new(&spec.program);
    command.args(&spec.args);
    for key in &spec.remove_env {
        command.env_remove(key);
    }
    for (key, value) in &spec.env {
        command.env(key, value);
    }

    Err(AppError::Io(command.exec()))
}

pub(crate) fn run_internal_bootstrap_install() -> Result<(), AppError> {
    let paths = AppPaths::from_system()?;
    let exposure = materialize_and_expose_cli(&paths)?;

    println!(
        "{}",
        styled_success(&format!(
            "Memory Bank binaries are installed under {}.",
            paths.root.display()
        ))
    );
    match exposure.mode {
        ExposureMode::Direct => {
            println!(
                "{}",
                styled_success("`mb` is available directly in this shell.")
            );
        }
        ExposureMode::Launcher => {
            println!(
                "{}",
                styled_success("A managed `mb` launcher was installed on your current PATH.")
            );
        }
        ExposureMode::ShellInitFallback => {
            println!(
                "{}",
                styled_warning(
                    "Warning: Managed shell startup files were updated for future shells."
                )
            );
            println!(
                "{}",
                styled_subtle(&format!(
                    "Use `{}` in this shell until you start a new terminal.",
                    paths.binary_path("mb").display()
                ))
            );
        }
    }

    Ok(())
}

fn run_action<T, F>(message: &str, action: F) -> Result<T, AppError>
where
    F: FnOnce() -> Result<T, AppError>,
{
    print_action_start(message)?;
    match action() {
        Ok(value) => {
            println!("{}", styled_success("done"));
            Ok(value)
        }
        Err(error) => {
            println!("{}", styled_failure("failed"));
            Err(error)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigChangeEffect {
    RestartRequired,
    FutureStartsOnly,
    MetadataOnly,
}

fn config_change_effect(key: &str) -> ConfigChangeEffect {
    match key {
        "active_namespace" | "service.port" => ConfigChangeEffect::RestartRequired,
        "service.autostart" => ConfigChangeEffect::FutureStartsOnly,
        key if key.starts_with("server.") => ConfigChangeEffect::RestartRequired,
        key if key.starts_with("integrations.") => ConfigChangeEffect::MetadataOnly,
        _ => ConfigChangeEffect::MetadataOnly,
    }
}

fn config_change_hint(key: &str, service: &ServiceStatus) -> String {
    match config_change_effect(key) {
        ConfigChangeEffect::RestartRequired => {
            if service.active {
                format!(
                    "Restart the managed service with {} to apply this change to the running server.",
                    styled_command("mb service restart")
                )
            } else if service.installed {
                format!(
                    "Start the managed service with {} to apply this change.",
                    styled_command("mb service start")
                )
            } else {
                "This change will apply the next time the managed service starts.".to_string()
            }
        }
        ConfigChangeEffect::FutureStartsOnly => {
            "Autostart affects future launches only; it does not change the current running service."
                .to_string()
        }
        ConfigChangeEffect::MetadataOnly => {
            "This updates saved metadata only. No service restart is required.".to_string()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoctorOutcome {
    Healthy,
    IssuesFound,
    FixedCleanly,
    FixedPartially,
}

fn doctor_outcome(fix: bool, issues_before: &[String], issues_after: &[String]) -> DoctorOutcome {
    if fix {
        if issues_after.is_empty() {
            DoctorOutcome::FixedCleanly
        } else {
            DoctorOutcome::FixedPartially
        }
    } else if issues_before.is_empty() {
        DoctorOutcome::Healthy
    } else {
        DoctorOutcome::IssuesFound
    }
}

fn plural_suffix(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn list_namespaces(paths: &AppPaths) -> Result<Vec<Namespace>, AppError> {
    if !paths.namespaces_dir.exists() {
        return Ok(Vec::new());
    }

    let mut namespaces = Vec::new();
    for entry in fs::read_dir(&paths.namespaces_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            namespaces.push(Namespace::new(entry.file_name().to_string_lossy()));
        }
    }
    namespaces.sort();
    Ok(namespaces)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::HealthCheck;
    use tempfile::TempDir;

    #[test]
    fn list_namespaces_returns_sorted_directories_only() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        fs::create_dir_all(paths.namespaces_dir.join("zeta")).expect("zeta");
        fs::create_dir_all(paths.namespaces_dir.join("alpha team")).expect("alpha");
        fs::write(paths.namespaces_dir.join("README.txt"), "ignore").expect("file");

        let namespaces = list_namespaces(&paths).expect("list namespaces");

        assert_eq!(
            namespaces,
            vec![Namespace::new("alpha team"), Namespace::new("zeta")]
        );
    }

    #[test]
    fn runtime_mismatch_fields_reports_changed_runtime_values() {
        let settings = AppSettings {
            active_namespace: Some("work".to_string()),
            service: Some(memory_bank_app::ServiceSettings {
                port: Some(4545),
                autostart: Some(true),
            }),
            ..AppSettings::default()
        };
        let health = HealthCheck {
            ok: true,
            namespace: "default".to_string(),
            port: 3737,
            llm_provider: "gemini".to_string(),
            encoder_provider: "remote-api".to_string(),
            version: "test".to_string(),
        };

        let fields = runtime_mismatch_fields(&settings, "anthropic", "fast-embed", &health);

        assert_eq!(fields, vec!["namespace", "port", "provider", "encoder"]);
    }

    #[test]
    fn config_change_hint_depends_on_key_and_service_state() {
        let active_service = ServiceStatus {
            installed: true,
            active: true,
        };
        let installed_inactive = ServiceStatus {
            installed: true,
            active: false,
        };
        let missing_service = ServiceStatus {
            installed: false,
            active: false,
        };

        assert!(
            config_change_hint("server.llm_provider", &active_service)
                .contains("mb service restart")
        );
        assert!(
            config_change_hint("service.port", &installed_inactive).contains("mb service start")
        );
        assert!(
            config_change_hint("active_namespace", &missing_service)
                .contains("next time the managed service starts")
        );
        assert!(
            config_change_hint("service.autostart", &active_service)
                .contains("future launches only")
        );
        assert!(
            config_change_hint("integrations.opencode.configured", &active_service)
                .contains("No service restart is required")
        );
    }

    #[test]
    fn doctor_outcome_distinguishes_clean_and_partial_results() {
        let issue = vec!["one issue".to_string()];

        assert_eq!(doctor_outcome(false, &[], &[]), DoctorOutcome::Healthy);
        assert_eq!(
            doctor_outcome(false, &issue, &issue),
            DoctorOutcome::IssuesFound
        );
        assert_eq!(
            doctor_outcome(true, &issue, &[]),
            DoctorOutcome::FixedCleanly
        );
        assert_eq!(
            doctor_outcome(true, &issue, &issue),
            DoctorOutcome::FixedPartially
        );
    }
}
