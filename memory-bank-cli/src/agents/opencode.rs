use crate::AppError;
use crate::assets::copy_if_needed;
use crate::json_config::{ensure_object, load_json_config, object_mut, write_json_config_with_backups};
use memory_bank_app::AppPaths;
use serde_json::json;

use super::shared::ensure_child_object;

pub(super) fn configure(paths: &AppPaths, server_url: &str) -> Result<(), AppError> {
    let plugin_target = paths.home_dir.join(".config/opencode/plugins/memory-bank.js");
    copy_if_needed(
        &paths.integrations_dir.join("opencode/memory-bank.js"),
        &plugin_target,
    )?;

    let settings_path = paths.home_dir.join(".config/opencode/opencode.json");
    let mut root = load_json_config(&settings_path)?;
    ensure_object(&mut root);
    ensure_child_object(object_mut(&mut root)?, "mcp")?.insert(
        "memory-bank".to_string(),
        json!({
            "type": "remote",
            "url": format!("{server_url}/mcp"),
            "enabled": true
        }),
    );
    write_json_config_with_backups(paths, &settings_path, &root)
}
