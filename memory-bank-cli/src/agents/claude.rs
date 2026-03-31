use crate::AppError;
use crate::command_utils::{CommandOutcome, run_command_capture};
#[cfg(test)]
use crate::command_utils::{CommandRunOptions, run_command_capture_with_options};
use crate::constants::HOOK_BINARY_NAME;
use crate::json_config::{
    ensure_object, load_json_config, object_mut, write_json_config_with_backups,
};
use memory_bank_app::AppPaths;
use serde_json::{Value, json};

use super::shared::{build_hook_command, ensure_child_array, ensure_child_object};

pub(super) fn configure(paths: &AppPaths, server_url: &str) -> Result<(), AppError> {
    ensure_user_mcp(server_url)?;

    let settings_path = paths.home_dir.join(".claude/settings.json");
    let mut root = load_json_config(&settings_path)?;
    let events = ["UserPromptSubmit", "PreToolUse", "PostToolUse", "Stop"];
    for event in events {
        let command = build_hook_command(
            &paths.binary_path(HOOK_BINARY_NAME),
            "claude-code",
            event,
            server_url,
        );
        upsert_hook(&mut root, event, &command)?;
    }
    write_json_config_with_backups(paths, &settings_path, &root)
}

#[cfg(test)]
pub(super) fn configure_with_options(
    paths: &AppPaths,
    server_url: &str,
    options: &CommandRunOptions,
) -> Result<(), AppError> {
    ensure_user_mcp_with_runner(server_url, |args| {
        run_command_capture_with_options("claude", args, options)
    })?;

    let settings_path = paths.home_dir.join(".claude/settings.json");
    let mut root = load_json_config(&settings_path)?;
    let events = ["UserPromptSubmit", "PreToolUse", "PostToolUse", "Stop"];
    for event in events {
        let command = build_hook_command(
            &paths.binary_path(HOOK_BINARY_NAME),
            "claude-code",
            event,
            server_url,
        );
        upsert_hook(&mut root, event, &command)?;
    }
    write_json_config_with_backups(paths, &settings_path, &root)
}

pub(super) fn ensure_user_mcp(server_url: &str) -> Result<(), AppError> {
    ensure_user_mcp_with_runner(server_url, |args| run_command_capture("claude", args))
}

pub(super) fn ensure_user_mcp_with_runner<F>(server_url: &str, mut run: F) -> Result<(), AppError>
where
    F: FnMut(&[&str]) -> Result<CommandOutcome, AppError>,
{
    let desired_url = format!("{server_url}/mcp");
    let current = run(&["mcp", "get", "memory-bank"])?;

    if mcp_matches(&current, &desired_url) {
        return Ok(());
    }

    if current.success {
        if mcp_scope(&current).as_deref() == Some("user") {
            let removal = run(&["mcp", "remove", "memory-bank", "-s", "user"])?;
            if !removal.success {
                return Err(removal.into_error());
            }
        } else {
            return Err(AppError::Message(
                "Claude Code already has a conflicting `memory-bank` MCP server outside user scope; remove or rename that entry before rerunning setup".to_string(),
            ));
        }
    }

    let addition = run(&[
        "mcp",
        "add",
        "--transport",
        "http",
        "--scope",
        "user",
        "memory-bank",
        &desired_url,
    ])?;

    if !addition.success {
        let verify = run(&["mcp", "get", "memory-bank"])?;
        if mcp_matches(&verify, &desired_url) {
            return Ok(());
        }
        return Err(addition.into_error());
    }

    let verify = run(&["mcp", "get", "memory-bank"])?;
    if mcp_matches(&verify, &desired_url) {
        Ok(())
    } else {
        Err(AppError::Message(format!(
            "Claude Code did not report the expected user-scoped HTTP MCP config for memory-bank after setup. Expected URL: {desired_url}"
        )))
    }
}

pub(super) fn mcp_matches(outcome: &CommandOutcome, desired_url: &str) -> bool {
    outcome.success
        && mcp_scope(outcome).as_deref() == Some("user")
        && outcome.combined_output().contains("Type: http")
        && outcome.combined_output().contains(desired_url)
}

pub(super) fn mcp_scope(outcome: &CommandOutcome) -> Option<String> {
    for line in outcome.combined_output().lines() {
        let trimmed = line.trim();
        if let Some(scope) = trimmed.strip_prefix("Scope:") {
            let scope = scope.trim().to_ascii_lowercase();
            if scope.starts_with("user") {
                return Some("user".to_string());
            }
            if scope.starts_with("project") || scope.starts_with("local") {
                return Some("project".to_string());
            }
            return Some(scope);
        }
    }
    None
}

pub(super) fn upsert_hook(root: &mut Value, event: &str, command: &str) -> Result<(), AppError> {
    ensure_object(root);
    let root_map = object_mut(root)?;
    let hooks_map = ensure_child_object(root_map, "hooks")?;
    let groups_array = ensure_child_array(hooks_map, event)?;
    let marker = format!("--agent claude-code --event {event}");
    let desired = json!({
        "type": "command",
        "command": command,
    });
    if let Some(existing) = groups_array.iter_mut().find_map(|group| {
        group
            .get_mut("hooks")
            .and_then(Value::as_array_mut)
            .and_then(|hooks| {
                hooks.iter_mut().find(|hook| {
                    hook.get("command")
                        .and_then(Value::as_str)
                        .map(|value| value.contains(&marker))
                        .unwrap_or(false)
                })
            })
    }) {
        *existing = desired;
    } else {
        groups_array.push(json!({ "hooks": [desired] }));
    }
    Ok(())
}
