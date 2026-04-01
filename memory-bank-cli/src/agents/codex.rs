use crate::AppError;
use crate::command_utils::shell_escape;
use crate::constants::HOOK_BINARY_NAME;
use crate::json_config::{
    ensure_object, load_json_config, object_mut, write_json_config_with_backups,
};
use crate::toml_config::{load_toml_config, write_toml_config_with_backups};
use memory_bank_app::AppPaths;
use serde_json::{Value, json};

use super::shared::{build_hook_command, ensure_child_array, ensure_child_object};

const CODEX_HOOK_TIMEOUT_SECS: u64 = 10;

pub(super) fn configure(paths: &AppPaths, server_url: &str) -> Result<(), AppError> {
    configure_mcp(paths, server_url)?;
    configure_hooks(paths, server_url)
}

fn configure_mcp(paths: &AppPaths, server_url: &str) -> Result<(), AppError> {
    let config_path = paths.home_dir.join(".codex/config.toml");
    let current = load_toml_config(&config_path)?;
    let rendered = upsert_codex_config_toml(&current, server_url);
    write_toml_config_with_backups(paths, &config_path, &rendered)
}

fn configure_hooks(paths: &AppPaths, server_url: &str) -> Result<(), AppError> {
    let hooks_path = paths.home_dir.join(".codex/hooks.json");
    let mut root = load_json_config(&hooks_path)?;
    let events = [
        ("UserPromptSubmit", None),
        ("PreToolUse", Some("Bash")),
        ("PostToolUse", Some("Bash")),
        ("Stop", None),
    ];
    for (event, matcher) in events {
        let command = build_hook_command(
            &paths.binary_path(HOOK_BINARY_NAME),
            "codex",
            event,
            server_url,
        );
        upsert_hook(&mut root, event, matcher, &command)?;
    }
    write_json_config_with_backups(paths, &hooks_path, &root)
}

pub(super) fn upsert_hook(
    root: &mut Value,
    event: &str,
    matcher: Option<&str>,
    command: &str,
) -> Result<(), AppError> {
    ensure_object(root);
    let root_map = object_mut(root)?;
    let hooks_map = ensure_child_object(root_map, "hooks")?;
    let groups_array = ensure_child_array(hooks_map, event)?;
    let desired_hook = json!({
        "type": "command",
        "command": command,
        "timeout": CODEX_HOOK_TIMEOUT_SECS,
    });

    for group in groups_array.iter_mut() {
        repair_group_hooks(group, event);
    }
    groups_array.retain(should_keep_group_after_upsert);
    groups_array.push(build_group(matcher, desired_hook));
    Ok(())
}

fn repair_group_hooks(group: &mut Value, event: &str) {
    let Some(group_map) = group.as_object_mut() else {
        *group = json!({ "hooks": [] });
        return;
    };

    let hooks_value = group_map
        .entry("hooks".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !hooks_value.is_array() {
        *hooks_value = Value::Array(Vec::new());
    }

    if let Some(hooks) = hooks_value.as_array_mut() {
        hooks.retain(|hook| {
            hook.is_object()
                && !hook
                    .get("command")
                    .and_then(Value::as_str)
                    .map(|value| is_owned_codex_command(value, event))
                    .unwrap_or(false)
        });
    }
}

fn should_keep_group_after_upsert(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .map(|hooks| !hooks.is_empty())
        .unwrap_or(true)
}

fn build_group(matcher: Option<&str>, hook: Value) -> Value {
    match matcher {
        Some(matcher) => json!({
            "matcher": matcher,
            "hooks": [hook],
        }),
        None => json!({
            "hooks": [hook],
        }),
    }
}

fn is_owned_codex_command(command: &str, event: &str) -> bool {
    let quoted_agent = format!("--agent {}", shell_escape("codex"));
    let quoted_event = format!("--event {}", shell_escape(event));
    let legacy_agent = "--agent codex";
    let legacy_event = format!("--event {event}");

    (command.contains(&quoted_agent) || command.contains(legacy_agent))
        && (command.contains(&quoted_event) || command.contains(&legacy_event))
}

fn upsert_codex_config_toml(contents: &str, server_url: &str) -> String {
    let mut sections = split_sections(contents);
    let mut rendered_sections = Vec::new();
    let mut features_seen = false;
    let mut memory_bank_mcp_seen = false;

    for mut section in sections.drain(..) {
        match section.header.as_deref() {
            None => {
                section
                    .lines
                    .retain(|line| !is_conflicting_top_level_assignment(line, "features"));
                section
                    .lines
                    .retain(|line| !is_conflicting_top_level_assignment(line, "mcp_servers"));
                rendered_sections.push(section);
            }
            Some("features") => {
                if features_seen {
                    continue;
                }
                features_seen = true;
                section.lines = upsert_features_lines(&section.lines);
                rendered_sections.push(section);
            }
            Some("mcp_servers.memory-bank") => {
                if memory_bank_mcp_seen {
                    continue;
                }
                memory_bank_mcp_seen = true;
                section.lines = desired_memory_bank_mcp_lines(server_url);
                rendered_sections.push(section);
            }
            _ => rendered_sections.push(section),
        }
    }

    if !features_seen {
        rendered_sections.push(Section {
            header: Some("features".to_string()),
            lines: upsert_features_lines(&[]),
        });
    }

    if !memory_bank_mcp_seen {
        rendered_sections.push(Section {
            header: Some("mcp_servers.memory-bank".to_string()),
            lines: desired_memory_bank_mcp_lines(server_url),
        });
    }

    render_sections(&rendered_sections)
}

fn desired_memory_bank_mcp_lines(server_url: &str) -> Vec<String> {
    vec![
        format!("url = \"{server_url}/mcp\""),
        "enabled = true".to_string(),
    ]
}

fn upsert_features_lines(lines: &[String]) -> Vec<String> {
    let mut rendered = Vec::new();
    let mut codex_hooks_set = false;

    for line in lines {
        if is_assignment(line, "codex_hooks") {
            if !codex_hooks_set {
                rendered.push("codex_hooks = true".to_string());
                codex_hooks_set = true;
            }
            continue;
        }
        rendered.push(line.clone());
    }

    if !codex_hooks_set {
        rendered.push("codex_hooks = true".to_string());
    }

    rendered
}

fn split_sections(contents: &str) -> Vec<Section> {
    let mut sections = vec![Section {
        header: None,
        lines: Vec::new(),
    }];

    for line in contents.lines() {
        if let Some(header) = parse_table_header(line) {
            sections.push(Section {
                header: Some(header),
                lines: Vec::new(),
            });
        } else {
            sections
                .last_mut()
                .expect("sections always has a prelude")
                .lines
                .push(line.to_string());
        }
    }

    sections
}

fn render_sections(sections: &[Section]) -> String {
    let mut rendered = String::new();
    let mut wrote_anything = false;

    for section in sections {
        if section.header.is_none() && section.lines.is_empty() {
            continue;
        }

        if wrote_anything && !rendered.ends_with("\n\n") {
            if !rendered.ends_with('\n') {
                rendered.push('\n');
            }
            rendered.push('\n');
        }

        if let Some(header) = &section.header {
            rendered.push('[');
            rendered.push_str(header);
            rendered.push_str("]\n");
        }

        for line in &section.lines {
            rendered.push_str(line);
            rendered.push('\n');
        }

        wrote_anything = true;
    }

    rendered
}

fn parse_table_header(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with('[') || trimmed.starts_with("[[") {
        return None;
    }

    let close_index = trimmed.find(']')?;
    let header = &trimmed[1..close_index];
    let remainder = trimmed[close_index + 1..].trim();
    if !remainder.is_empty() && !remainder.starts_with('#') {
        return None;
    }

    Some(header.trim().to_string())
}

fn is_conflicting_top_level_assignment(line: &str, key: &str) -> bool {
    is_assignment(line, key)
}

fn is_assignment(line: &str, key: &str) -> bool {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix(key) else {
        return false;
    };
    let remainder = rest.trim_start();
    remainder.starts_with('=')
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Section {
    header: Option<String>,
    lines: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::upsert_codex_config_toml;
    use super::{repair_group_hooks, should_keep_group_after_upsert};
    use serde_json::{Value, json};

    #[test]
    fn upsert_codex_config_toml_preserves_unrelated_sections_and_comments() {
        let rendered = upsert_codex_config_toml(
            r#"# top comment
model = "gpt-5.4"

[features]
# keep me
multi_agent = true

[mcp_servers.context7]
command = "npx"
"#,
            "http://127.0.0.1:3737",
        );

        assert!(rendered.contains("# top comment"));
        assert!(rendered.contains("# keep me"));
        assert!(rendered.contains("multi_agent = true"));
        assert!(rendered.contains("[features]"));
        assert!(rendered.contains("codex_hooks = true"));
        assert!(rendered.contains("[mcp_servers.context7]"));
        assert!(rendered.contains("[mcp_servers.memory-bank]"));
        assert!(rendered.contains("url = \"http://127.0.0.1:3737/mcp\""));
        assert!(rendered.contains("enabled = true"));
    }

    #[test]
    fn upsert_codex_config_toml_replaces_conflicting_top_level_assignments() {
        let rendered = upsert_codex_config_toml(
            r#"features = []
mcp_servers = {}
"#,
            "http://127.0.0.1:3737",
        );

        assert!(!rendered.contains("features = []"));
        assert!(!rendered.contains("mcp_servers = {}"));
        assert!(rendered.contains("[features]"));
        assert!(rendered.contains("[mcp_servers.memory-bank]"));
    }

    #[test]
    fn repair_group_hooks_replaces_malformed_groups_with_empty_hook_arrays() {
        let mut malformed_group = Value::String("broken".to_string());
        repair_group_hooks(&mut malformed_group, "PreToolUse");

        assert_eq!(malformed_group, json!({ "hooks": [] }));
        assert!(!should_keep_group_after_upsert(&malformed_group));
    }
}
