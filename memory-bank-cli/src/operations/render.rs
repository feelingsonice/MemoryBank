use crate::command_utils::yes_no;
use crate::constants::HEALTH_STARTUP_TIMEOUT;
use crate::output::{
    print_key_value, styled_command, styled_section, styled_subtle, styled_success, styled_warning,
};
use crate::service::{HealthCheck, ServiceActionKind, ServiceActionReport, ServiceRuntimeSummary};
use memory_bank_app::AppSettings;

pub(super) fn runtime_mismatch_fields<'a>(
    settings: &'a AppSettings,
    provider: &'a str,
    encoder: &'a str,
    health: &'a HealthCheck,
) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if health.namespace != settings.active_namespace().to_string() {
        fields.push("namespace");
    }
    if health.port != settings.resolved_port() {
        fields.push("port");
    }
    if health.llm_provider != provider {
        fields.push("provider");
    }
    if health.encoder_provider != encoder {
        fields.push("encoder");
    }
    fields
}

pub(super) fn runtime_health_warning(runtime: &ServiceRuntimeSummary) -> Option<String> {
    match (runtime.active, runtime.health.is_some()) {
        (true, false) => Some(
            "The service manager reports the service as active, but `/healthz` is unavailable. It may still be starting or it may be unhealthy."
                .to_string(),
        ),
        (false, true) => Some(
            "The health endpoint responded even though the managed service is not active. Another process may be serving this URL."
                .to_string(),
        ),
        _ => None,
    }
}

pub(super) fn print_service_section(runtime: &ServiceRuntimeSummary) {
    println!("{}", styled_section("Managed service"));
    print_key_value("Manager", runtime.manager.display_name());
    print_key_value("Definition", runtime.definition_path.display());
    print_key_value("Installed", yes_no(runtime.installed));
    print_key_value("Active", yes_no(runtime.active));
    print_key_value("URL", &runtime.url);
    print_key_value("Log file", runtime.log_path.display());
    if let Some(pid) = runtime.pid {
        print_key_value("PID", pid);
    }
}

pub(super) fn print_live_runtime_section(runtime: &ServiceRuntimeSummary) {
    println!("{}", styled_section("Live runtime"));
    match runtime.health.as_ref() {
        Some(health) => {
            print_key_value("Health", yes_no(health.ok));
            print_key_value("Namespace", &health.namespace);
            print_key_value("Port", health.port);
            print_key_value("Provider", &health.llm_provider);
            print_key_value("Encoder", &health.encoder_provider);
            print_key_value("Version", &health.version);
        }
        None => {
            print_key_value("Health", "unavailable");
            if let Some(error) = runtime.health_error.as_ref() {
                print_key_value("Detail", error);
            }
        }
    }
}

pub(super) fn print_install_result(report: &ServiceActionReport) {
    let message = if report.installed_before {
        "Success: Updated the managed service definition."
    } else {
        "Success: Installed the managed service definition."
    };
    println!("{}", styled_success(message));
    print_key_value("Manager", report.manager.display_name());
    print_key_value("Definition", report.definition_path.display());
    print_key_value("Autostart", yes_no(report.autostart));
    print_key_value("Active", yes_no(report.active_after));
    print_key_value("Log file", report.log_path.display());
}

pub(super) fn print_start_or_restart_result(report: &ServiceActionReport) {
    let message = if !report.active_after {
        "Warning: Sent the service request, but the managed service does not appear active yet."
    } else {
        match report.action {
            ServiceActionKind::Restart if report.fell_back_to_start => {
                "Success: The service was not running, so restart started it instead."
            }
            ServiceActionKind::Restart => "Success: Restarted Memory Bank service.",
            ServiceActionKind::Start if report.active_before => {
                "Success: Memory Bank service was already active and is still running."
            }
            ServiceActionKind::Start => "Success: Started Memory Bank service.",
            _ => "Success: Updated Memory Bank service state.",
        }
    };
    let rendered = if message.starts_with("Success:") {
        styled_success(message)
    } else {
        styled_warning(message)
    };
    println!("{rendered}");
    if report.installed_during_action {
        println!(
            "{}",
            styled_subtle("Installed the managed service definition as part of this command.")
        );
    }

    print_key_value("Manager", report.manager.display_name());
    print_key_value("URL", &report.url);
    print_key_value("Log file", report.log_path.display());
    if let Some(pid) = report.pid {
        print_key_value("PID", pid);
    }

    match report.health.as_ref() {
        Some(health) => {
            print_key_value("Health", yes_no(health.ok));
            print_key_value("Namespace", &health.namespace);
            print_key_value("Port", health.port);
            print_key_value("Provider", &health.llm_provider);
            print_key_value("Encoder", &health.encoder_provider);
            print_key_value("Version", &health.version);
        }
        None if report.active_after => {
            print_key_value("Health", "still starting");
            if let Some(error) = report.health_error.as_ref() {
                print_key_value("Detail", error);
            }
            println!(
                "{}",
                styled_warning(&format!(
                    "Warning: The service manager reports active, but `/healthz` did not respond within {}s. It may still be starting.",
                    HEALTH_STARTUP_TIMEOUT.as_secs()
                ))
            );
            println!(
                "{}",
                styled_subtle(&format!(
                    "Try {} or {}.",
                    styled_command("mb service status"),
                    styled_command("mb logs -f")
                ))
            );
        }
        None => {
            print_key_value("Health", "unavailable");
            if !report.active_after {
                println!(
                    "{}",
                    styled_subtle(&format!(
                        "Try {} or {} for more detail.",
                        styled_command("mb service status"),
                        styled_command("mb logs -f")
                    ))
                );
            }
        }
    }
}

pub(super) fn print_stop_result(report: &ServiceActionReport) {
    let message = if !report.installed_before {
        "Warning: The managed service is not installed."
    } else if !report.active_before {
        "Warning: The managed service is already stopped."
    } else if report.active_after {
        "Warning: Sent a stop request, but the service still appears active."
    } else {
        "Success: Stopped Memory Bank service."
    };
    let rendered = if message.starts_with("Success:") {
        styled_success(message)
    } else {
        styled_warning(message)
    };
    println!("{rendered}");
    print_key_value("Manager", report.manager.display_name());
    print_key_value("Definition", report.definition_path.display());
    print_key_value("Installed", yes_no(report.installed_after));
    print_key_value("Active", yes_no(report.active_after));
    print_key_value("Log file", report.log_path.display());
    if report.active_after {
        println!(
            "{}",
            styled_subtle(&format!(
                "Try {} or {} for more detail.",
                styled_command("mb service status"),
                styled_command("mb logs -f")
            ))
        );
    }
}

pub(super) fn print_namespace_apply_result(report: &ServiceActionReport) {
    let action_label = if !report.active_after {
        "Warning: Saved the namespace change, but the managed service does not appear active yet."
    } else if report.fell_back_to_start {
        "Success: Applied the namespace by starting the managed service."
    } else if matches!(report.action, ServiceActionKind::Restart) {
        "Success: Applied the namespace by restarting the managed service."
    } else {
        "Success: Applied the namespace by starting the managed service."
    };
    let rendered = if action_label.starts_with("Success:") {
        styled_success(action_label)
    } else {
        styled_warning(action_label)
    };
    println!("{rendered}");
    print_key_value("URL", &report.url);
    print_key_value("Log file", report.log_path.display());
    if let Some(health) = report.health.as_ref() {
        print_key_value("Health", yes_no(health.ok));
        print_key_value("Namespace", &health.namespace);
        print_key_value("Port", health.port);
    } else if report.active_after {
        print_key_value("Health", "still starting");
        if let Some(error) = report.health_error.as_ref() {
            print_key_value("Detail", error);
        }
    } else {
        println!(
            "{}",
            styled_subtle(&format!(
                "Try {} or {} for more detail.",
                styled_command("mb service status"),
                styled_command("mb logs -f")
            ))
        );
    }
}

pub(super) fn describe_cli_exposure(mode: crate::assets::ExposureMode) -> String {
    match mode {
        crate::assets::ExposureMode::Direct => {
            "`mb` is available directly in this shell.".to_string()
        }
        crate::assets::ExposureMode::Launcher => {
            "Installed or refreshed the managed `mb` launcher on PATH.".to_string()
        }
        crate::assets::ExposureMode::ShellInitFallback => {
            "Updated managed shell startup files for future shells.".to_string()
        }
    }
}

pub(super) fn describe_install_attempt(report: &ServiceActionReport) -> String {
    if report.installed_during_action {
        format!(
            "Installed the managed service definition at {}.",
            report.definition_path.display()
        )
    } else {
        format!(
            "Refreshed the managed service definition at {}.",
            report.definition_path.display()
        )
    }
}

pub(super) fn describe_start_attempt(report: &ServiceActionReport) -> String {
    if report.active_after && report.health.is_some() {
        format!(
            "Started the managed service and verified health on {}.",
            report.url
        )
    } else if report.active_after {
        format!(
            "Started the managed service, but `/healthz` is still unavailable on {}.",
            report.url
        )
    } else {
        "Tried to start the managed service, but it does not appear active yet.".to_string()
    }
}
