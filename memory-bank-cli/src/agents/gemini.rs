use crate::AppError;
use crate::constants::HOOK_BINARY_NAME;
use crate::json_config::{
    ensure_object, load_json_config, object_mut, write_json_config_with_backups,
};
use memory_bank_app::AppPaths;
use serde_json::{Value, json};

use super::shared::{build_hook_command, ensure_child_array, ensure_child_object};

pub(super) fn configure(paths: &AppPaths, server_url: &str) -> Result<(), AppError> {
    let settings_path = paths.home_dir.join(".gemini/settings.json");
    let mut root = load_json_config(&settings_path)?;
    ensure_object(&mut root);
    ensure_child_object(object_mut(&mut root)?, "mcpServers")?.insert(
        "memory-bank".to_string(),
        json!({ "httpUrl": format!("{server_url}/mcp") }),
    );

    let hook_events = [
        ("BeforeAgent", "*"),
        ("BeforeTool", ".*"),
        ("AfterTool", ".*"),
        ("AfterAgent", "*"),
    ];
    for (event, matcher) in hook_events {
        let command = build_hook_command(
            &paths.binary_path(HOOK_BINARY_NAME),
            "gemini-cli",
            event,
            server_url,
        );
        upsert_hook(&mut root, event, matcher, &command)?;
    }
    write_json_config_with_backups(paths, &settings_path, &root)
}

pub(super) fn upsert_hook(
    root: &mut Value,
    event: &str,
    matcher: &str,
    command: &str,
) -> Result<(), AppError> {
    ensure_object(root);
    let root_map = object_mut(root)?;
    let hooks_map = ensure_child_object(root_map, "hooks")?;
    let groups_array = ensure_child_array(hooks_map, event)?;
    let desired_hook = json!({
        "name": "memory-bank",
        "type": "command",
        "command": command,
    });
    if let Some(existing_group) = groups_array.iter_mut().find(|group| {
        group
            .get("hooks")
            .and_then(Value::as_array)
            .map(|hooks| {
                hooks.iter().any(|hook| {
                    hook.get("name")
                        .and_then(Value::as_str)
                        .map(|value| value == "memory-bank")
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    }) {
        existing_group["matcher"] = Value::String(matcher.to_string());
        existing_group["sequential"] = Value::Bool(true);
        if let Some(hooks) = existing_group
            .get_mut("hooks")
            .and_then(Value::as_array_mut)
            && let Some(existing_hook) = hooks.iter_mut().find(|hook| {
                hook.get("name")
                    .and_then(Value::as_str)
                    .map(|value| value == "memory-bank")
                    .unwrap_or(false)
            })
        {
            *existing_hook = desired_hook;
        }
    } else {
        groups_array.push(json!({
            "matcher": matcher,
            "sequential": true,
            "hooks": [desired_hook],
        }));
    }
    Ok(())
}
