use crate::AppError;
use crate::constants::{HOOK_BINARY_NAME, MCP_PROXY_BINARY_NAME};
use crate::json_config::{
    array_mut, ensure_object, load_json_config, object_mut, write_json_config_with_backups,
};
use memory_bank_app::AppPaths;
use serde_json::{Map, Value, json};

use super::shared::ensure_child_object;

pub(super) fn configure(paths: &AppPaths, server_url: &str) -> Result<(), AppError> {
    let extension_path = paths.integrations_dir.join("openclaw/memory-bank");
    let settings_path = paths.home_dir.join(".openclaw/openclaw.json");
    let mut root = load_json_config(&settings_path)?;
    ensure_object(&mut root);
    let root_map = object_mut(&mut root)?;
    let mcp = ensure_child_object(root_map, "mcp")?;
    ensure_child_object(mcp, "servers")?.insert(
        "memory-bank".to_string(),
        json!({
            "command": paths.binary_path(MCP_PROXY_BINARY_NAME),
            "args": ["--server-url", server_url]
        }),
    );

    let plugins = ensure_child_object(root_map, "plugins")?;
    upsert_plugin_load_path(
        ensure_child_object(plugins, "load")?,
        extension_path.to_string_lossy().as_ref(),
    )?;
    ensure_child_object(plugins, "entries")?.insert(
        "memory-bank".to_string(),
        json!({
            "enabled": true,
            "config": {
                "hookBinary": paths.binary_path(HOOK_BINARY_NAME),
                "serverUrl": server_url
            }
        }),
    );
    ensure_child_object(plugins, "slots")?
        .insert("memory".to_string(), Value::String("none".to_string()));

    write_json_config_with_backups(paths, &settings_path, &root)
}

pub(super) fn upsert_plugin_load_path(
    load_map: &mut Map<String, Value>,
    desired_path: &str,
) -> Result<(), AppError> {
    let desired = desired_path.to_string();
    let paths_value = load_map
        .entry("paths".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    let paths_array = array_mut(paths_value)?;

    paths_array.retain(|value| {
        let Some(path) = value.as_str() else {
            return true;
        };
        !(path.ends_with("/memory-bank") && path != desired)
    });

    if !paths_array
        .iter()
        .any(|value| value.as_str() == Some(desired.as_str()))
    {
        paths_array.push(Value::String(desired));
    }

    Ok(())
}
