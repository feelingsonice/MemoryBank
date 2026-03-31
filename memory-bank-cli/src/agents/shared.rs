use crate::AppError;
use crate::command_utils::shell_escape;
use serde_json::{Map, Value};

pub(super) fn build_hook_command(
    hook_binary: &std::path::Path,
    agent: &str,
    event: &str,
    server_url: &str,
) -> String {
    format!(
        "{} --agent {} --event {} --server-url {}",
        shell_escape(hook_binary.to_string_lossy().as_ref()),
        shell_escape(agent),
        shell_escape(event),
        shell_escape(server_url)
    )
}

pub(super) fn ensure_child_object<'a>(
    parent: &'a mut Map<String, Value>,
    key: &str,
) -> Result<&'a mut Map<String, Value>, AppError> {
    let child = parent
        .entry(key.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !child.is_object() {
        *child = Value::Object(Map::new());
    }
    child
        .as_object_mut()
        .ok_or_else(|| AppError::Message("expected JSON object".to_string()))
}

pub(super) fn ensure_child_array<'a>(
    parent: &'a mut Map<String, Value>,
    key: &str,
) -> Result<&'a mut Vec<Value>, AppError> {
    let child = parent
        .entry(key.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !child.is_array() {
        *child = Value::Array(Vec::new());
    }
    child
        .as_array_mut()
        .ok_or_else(|| AppError::Message("expected JSON array".to_string()))
}
