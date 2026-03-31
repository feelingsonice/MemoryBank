use crate::AppError;
use crate::command_utils::{run_command, shell_escape};
use crate::constants::{LAUNCHD_LABEL, MB_BINARY_NAME, SYSTEMD_UNIT_NAME};
use memory_bank_app::{AppPaths, AppSettings};
use std::fs;
use std::path::PathBuf;

pub(super) fn install_launchd_service(
    paths: &AppPaths,
    settings: &AppSettings,
) -> Result<(), AppError> {
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

pub(super) fn install_systemd_service(
    paths: &AppPaths,
    settings: &AppSettings,
) -> Result<(), AppError> {
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

pub(super) fn render_launchd_plist(paths: &AppPaths, autostart: bool) -> String {
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

pub(super) fn render_systemd_unit(paths: &AppPaths) -> String {
    let escaped_mb = shell_escape(paths.binary_path(MB_BINARY_NAME).to_string_lossy().as_ref());
    let escaped_log = shell_escape(paths.log_file.to_string_lossy().as_ref());
    format!(
        "[Unit]\nDescription=Memory Bank\nAfter=network.target\n\n[Service]\nType=simple\nExecStart=/bin/sh -lc 'exec {escaped_mb} internal run-server >> {escaped_log} 2>&1'\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n"
    )
}

pub(super) fn launchd_service_path(paths: &AppPaths) -> PathBuf {
    paths
        .home_dir
        .join("Library/LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist"))
}

pub(super) fn systemd_service_path(paths: &AppPaths) -> PathBuf {
    paths
        .home_dir
        .join(".config/systemd/user")
        .join(SYSTEMD_UNIT_NAME)
}
