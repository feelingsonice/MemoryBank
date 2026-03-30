use crate::AppError;
use crate::assets::{copy_if_needed, find_on_path};
use crate::command_utils::{CommandOutcome, run_command_capture, shell_escape};
use crate::constants::{HOOK_BINARY_NAME, MCP_PROXY_BINARY_NAME};
use crate::json_config::{
    array_mut, ensure_object, load_json_config, object_mut, write_json_config_with_backups,
};
use memory_bank_app::{AppPaths, AppSettings, default_server_url};
use serde_json::{Map, Value, json};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentKind {
    ClaudeCode,
    GeminiCli,
    OpenCode,
    OpenClaw,
}

#[derive(Debug)]
pub(crate) struct AgentSetupOutcome {
    pub(crate) configured: Vec<AgentKind>,
    pub(crate) warnings: Vec<String>,
}

impl AgentKind {
    pub(crate) fn all() -> [Self; 4] {
        [
            Self::ClaudeCode,
            Self::GeminiCli,
            Self::OpenCode,
            Self::OpenClaw,
        ]
    }

    pub(crate) fn command_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::GeminiCli => "gemini",
            Self::OpenCode => "opencode",
            Self::OpenClaw => "openclaw",
        }
    }

    pub(crate) fn display_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::GeminiCli => "Gemini CLI",
            Self::OpenCode => "OpenCode",
            Self::OpenClaw => "OpenClaw",
        }
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

pub(crate) fn detect_installed_agents() -> Vec<AgentKind> {
    AgentKind::all()
        .into_iter()
        .filter(|agent| find_on_path(agent.command_name()).is_some())
        .collect()
}

pub(crate) fn configure_selected_agents(
    paths: &AppPaths,
    settings: &AppSettings,
    selected_agents: &[AgentKind],
) -> Result<AgentSetupOutcome, AppError> {
    let server_url = default_server_url(settings);
    let mut configured = Vec::new();
    let mut warnings = Vec::new();

    for agent in selected_agents {
        let result = match agent {
            AgentKind::ClaudeCode => configure_claude(paths, &server_url),
            AgentKind::GeminiCli => configure_gemini(paths, &server_url),
            AgentKind::OpenCode => configure_opencode(paths, &server_url),
            AgentKind::OpenClaw => configure_openclaw(paths, &server_url),
        };
        match result {
            Ok(()) => configured.push(*agent),
            Err(error) => warnings.push(format!("{}: {}", agent.display_name(), error)),
        }
    }

    Ok(AgentSetupOutcome {
        configured,
        warnings,
    })
}

pub(crate) fn integration_configured(settings: &AppSettings, agent: AgentKind) -> bool {
    settings
        .integrations
        .as_ref()
        .and_then(|integrations| match agent {
            AgentKind::ClaudeCode => integrations.claude_code.as_ref(),
            AgentKind::GeminiCli => integrations.gemini_cli.as_ref(),
            AgentKind::OpenCode => integrations.opencode.as_ref(),
            AgentKind::OpenClaw => integrations.openclaw.as_ref(),
        })
        .map(|state| state.configured)
        .unwrap_or(false)
}

fn configure_claude(paths: &AppPaths, server_url: &str) -> Result<(), AppError> {
    ensure_claude_user_mcp(server_url)?;

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
        upsert_claude_hook(&mut root, event, &command)?;
    }
    write_json_config_with_backups(paths, &settings_path, &root)
}

fn configure_gemini(paths: &AppPaths, server_url: &str) -> Result<(), AppError> {
    let settings_path = paths.home_dir.join(".gemini/settings.json");
    let mut root = load_json_config(&settings_path)?;
    ensure_object(&mut root);
    object_mut(&mut root)?
        .entry("mcpServers".to_string())
        .or_insert_with(|| json!({}));
    object_mut(
        object_mut(&mut root)?
            .get_mut("mcpServers")
            .expect("mcpServers"),
    )?
    .insert(
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
        upsert_gemini_hook(&mut root, event, matcher, &command)?;
    }
    write_json_config_with_backups(paths, &settings_path, &root)
}

fn configure_opencode(paths: &AppPaths, server_url: &str) -> Result<(), AppError> {
    let plugin_target = paths
        .home_dir
        .join(".config/opencode/plugins/memory-bank.js");
    copy_if_needed(
        &paths.integrations_dir.join("opencode/memory-bank.js"),
        &plugin_target,
    )?;

    let settings_path = paths.home_dir.join(".config/opencode/opencode.json");
    let mut root = load_json_config(&settings_path)?;
    ensure_object(&mut root);
    object_mut(&mut root)?
        .entry("mcp".to_string())
        .or_insert_with(|| json!({}));
    object_mut(object_mut(&mut root)?.get_mut("mcp").expect("mcp"))?.insert(
        "memory-bank".to_string(),
        json!({
            "type": "remote",
            "url": format!("{server_url}/mcp"),
            "enabled": true
        }),
    );
    write_json_config_with_backups(paths, &settings_path, &root)
}

fn configure_openclaw(paths: &AppPaths, server_url: &str) -> Result<(), AppError> {
    let extension_path = paths.integrations_dir.join("openclaw/memory-bank");
    let settings_path = paths.home_dir.join(".openclaw/openclaw.json");
    let mut root = load_json_config(&settings_path)?;
    ensure_object(&mut root);
    {
        let root_map = object_mut(&mut root)?;
        root_map
            .entry("mcp".to_string())
            .or_insert_with(|| json!({}));
        root_map
            .entry("plugins".to_string())
            .or_insert_with(|| json!({}));
    }
    let mcp = object_mut(object_mut(&mut root)?.get_mut("mcp").expect("mcp"))?;
    mcp.entry("servers".to_string())
        .or_insert_with(|| json!({}));
    object_mut(mcp.get_mut("servers").expect("servers"))?.insert(
        "memory-bank".to_string(),
        json!({
            "command": paths.binary_path(MCP_PROXY_BINARY_NAME),
            "args": ["--server-url", server_url]
        }),
    );

    let plugins = object_mut(object_mut(&mut root)?.get_mut("plugins").expect("plugins"))?;
    plugins
        .entry("load".to_string())
        .or_insert_with(|| json!({}));
    upsert_openclaw_plugin_load_path(
        object_mut(plugins.get_mut("load").expect("load"))?,
        extension_path.to_string_lossy().as_ref(),
    )?;
    plugins
        .entry("entries".to_string())
        .or_insert_with(|| json!({}));
    object_mut(plugins.get_mut("entries").expect("entries"))?.insert(
        "memory-bank".to_string(),
        json!({
            "enabled": true,
            "config": {
                "hookBinary": paths.binary_path(HOOK_BINARY_NAME),
                "serverUrl": server_url
            }
        }),
    );
    plugins
        .entry("slots".to_string())
        .or_insert_with(|| json!({}));
    object_mut(plugins.get_mut("slots").expect("slots"))?
        .insert("memory".to_string(), Value::String("none".to_string()));

    write_json_config_with_backups(paths, &settings_path, &root)
}

fn ensure_claude_user_mcp(server_url: &str) -> Result<(), AppError> {
    let desired_url = format!("{server_url}/mcp");
    let current = run_command_capture("claude", &["mcp", "get", "memory-bank"])?;

    if claude_mcp_matches(&current, &desired_url) {
        return Ok(());
    }

    if current.success {
        if claude_mcp_scope(&current).as_deref() == Some("user") {
            let removal =
                run_command_capture("claude", &["mcp", "remove", "memory-bank", "-s", "user"])?;
            if !removal.success {
                return Err(removal.into_error());
            }
        } else {
            return Err(AppError::Message(
                "Claude Code already has a conflicting `memory-bank` MCP server outside user scope; remove or rename that entry before rerunning setup".to_string(),
            ));
        }
    }

    let addition = run_command_capture(
        "claude",
        &[
            "mcp",
            "add",
            "--transport",
            "http",
            "--scope",
            "user",
            "memory-bank",
            &desired_url,
        ],
    )?;

    if !addition.success {
        let verify = run_command_capture("claude", &["mcp", "get", "memory-bank"])?;
        if claude_mcp_matches(&verify, &desired_url) {
            return Ok(());
        }
        return Err(addition.into_error());
    }

    let verify = run_command_capture("claude", &["mcp", "get", "memory-bank"])?;
    if claude_mcp_matches(&verify, &desired_url) {
        Ok(())
    } else {
        Err(AppError::Message(format!(
            "Claude Code did not report the expected user-scoped HTTP MCP config for memory-bank after setup. Expected URL: {desired_url}"
        )))
    }
}

fn claude_mcp_matches(outcome: &CommandOutcome, desired_url: &str) -> bool {
    outcome.success
        && claude_mcp_scope(outcome).as_deref() == Some("user")
        && outcome.combined_output().contains("Type: http")
        && outcome.combined_output().contains(desired_url)
}

fn claude_mcp_scope(outcome: &CommandOutcome) -> Option<String> {
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

fn build_hook_command(
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

fn upsert_claude_hook(root: &mut Value, event: &str, command: &str) -> Result<(), AppError> {
    ensure_object(root);
    let root_map = object_mut(root)?;
    let hooks = root_map
        .entry("hooks".to_string())
        .or_insert_with(|| json!({}));
    let hooks_map = object_mut(hooks)?;
    let groups = hooks_map
        .entry(event.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    let groups_array = array_mut(groups)?;
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

fn upsert_gemini_hook(
    root: &mut Value,
    event: &str,
    matcher: &str,
    command: &str,
) -> Result<(), AppError> {
    ensure_object(root);
    let root_map = object_mut(root)?;
    let hooks = root_map
        .entry("hooks".to_string())
        .or_insert_with(|| json!({}));
    let hooks_map = object_mut(hooks)?;
    let groups = hooks_map
        .entry(event.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    let groups_array = array_mut(groups)?;
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

fn upsert_openclaw_plugin_load_path(
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn claude_mcp_matches_expected_user_http_server() {
        let outcome = CommandOutcome {
            program: "claude".to_string(),
            args: vec!["mcp".to_string(), "get".to_string(), "memory-bank".to_string()],
            success: true,
            stdout: "memory-bank:\n  Scope: User config (available in all your projects)\n  Status: ✗ Failed to connect\n  Type: http\n  URL: http://127.0.0.1:3737/mcp\n".to_string(),
            stderr: String::new(),
        };

        assert!(claude_mcp_matches(&outcome, "http://127.0.0.1:3737/mcp"));
    }

    #[test]
    fn upsert_openclaw_plugin_load_path_replaces_stale_memory_bank_paths() {
        let mut load_map = Map::new();
        load_map.insert(
            "paths".to_string(),
            json!([
                "/tmp/something-else",
                "/old/repo/.openclaw/extensions/memory-bank"
            ]),
        );

        upsert_openclaw_plugin_load_path(
            &mut load_map,
            "/Users/test/.memory_bank/integrations/openclaw/memory-bank",
        )
        .expect("upsert load path");

        assert_eq!(
            load_map.get("paths").expect("paths"),
            &json!([
                "/tmp/something-else",
                "/Users/test/.memory_bank/integrations/openclaw/memory-bank"
            ])
        );
    }

    #[test]
    fn upsert_gemini_hook_refreshes_matcher_and_command() {
        let mut root = json!({
            "hooks": {
                "BeforeTool": [
                    {
                        "matcher": "stale",
                        "sequential": false,
                        "hooks": [
                            {
                                "name": "memory-bank",
                                "type": "command",
                                "command": "old-command"
                            }
                        ]
                    }
                ]
            }
        });

        upsert_gemini_hook(&mut root, "BeforeTool", ".*", "new-command")
            .expect("upsert gemini hook");

        let group = &root["hooks"]["BeforeTool"][0];
        assert_eq!(group["matcher"], Value::String(".*".to_string()));
        assert_eq!(group["sequential"], Value::Bool(true));
        assert_eq!(
            group["hooks"][0]["command"],
            Value::String("new-command".to_string())
        );
    }

    #[test]
    fn build_hook_command_shell_escapes_paths_with_spaces() {
        let command = build_hook_command(
            std::path::Path::new("/Users/test/Memory Bank/bin/memory-bank-hook"),
            "gemini-cli",
            "BeforeAgent",
            "http://127.0.0.1:3737",
        );

        assert_eq!(
            command,
            "'/Users/test/Memory Bank/bin/memory-bank-hook' --agent 'gemini-cli' --event 'BeforeAgent' --server-url 'http://127.0.0.1:3737'"
        );
    }

    #[test]
    fn upsert_claude_hook_replaces_existing_memory_bank_command() {
        let mut root = json!({
            "hooks": {
                "Stop": [
                    {
                        "hooks": [
                            {
                                "type": "command",
                                "command": "old --agent claude-code --event Stop"
                            }
                        ]
                    }
                ]
            }
        });

        upsert_claude_hook(&mut root, "Stop", "new-command").expect("upsert claude hook");

        assert_eq!(
            root["hooks"]["Stop"][0]["hooks"][0]["command"],
            Value::String("new-command".to_string())
        );
    }

    #[test]
    fn configure_gemini_writes_mcp_hooks_and_backup() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings_path = paths.home_dir.join(".gemini/settings.json");
        fs::create_dir_all(settings_path.parent().expect("parent")).expect("parent");
        fs::write(&settings_path, r#"{ "existing": true }"#).expect("existing config");

        configure_gemini(&paths, "http://127.0.0.1:4545").expect("configure gemini");

        let rendered = load_json_config(&settings_path).expect("load gemini config");
        assert_eq!(
            rendered["mcpServers"]["memory-bank"]["httpUrl"],
            Value::String("http://127.0.0.1:4545/mcp".to_string())
        );
        assert_eq!(
            rendered["hooks"]["BeforeTool"][0]["hooks"][0]["name"],
            Value::String("memory-bank".to_string())
        );
        assert!(settings_path.with_extension("json.mb_backup").exists());
    }

    #[test]
    fn configure_opencode_copies_plugin_and_sets_remote_mcp_entry() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let plugin_source = paths.integrations_dir.join("opencode/memory-bank.js");
        fs::create_dir_all(plugin_source.parent().expect("parent")).expect("parent");
        fs::write(&plugin_source, "// plugin").expect("plugin source");

        configure_opencode(&paths, "http://127.0.0.1:3737").expect("configure opencode");

        let plugin_target = paths
            .home_dir
            .join(".config/opencode/plugins/memory-bank.js");
        assert_eq!(
            fs::read_to_string(plugin_target).expect("plugin"),
            "// plugin"
        );

        let settings = load_json_config(&paths.home_dir.join(".config/opencode/opencode.json"))
            .expect("load opencode config");
        assert_eq!(
            settings["mcp"]["memory-bank"]["url"],
            Value::String("http://127.0.0.1:3737/mcp".to_string())
        );
        assert_eq!(settings["mcp"]["memory-bank"]["enabled"], Value::Bool(true));
    }

    #[test]
    fn configure_openclaw_rewrites_plugin_load_paths_and_proxy_config() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings_path = paths.home_dir.join(".openclaw/openclaw.json");
        fs::create_dir_all(settings_path.parent().expect("parent")).expect("parent");
        fs::write(
            &settings_path,
            json!({
                "plugins": {
                    "load": {
                        "paths": [
                            "/tmp/other-plugin",
                            "/old/repo/.openclaw/extensions/memory-bank"
                        ]
                    }
                }
            })
            .to_string(),
        )
        .expect("seed config");

        configure_openclaw(&paths, "http://127.0.0.1:6000").expect("configure openclaw");

        let rendered = load_json_config(&settings_path).expect("load openclaw config");
        assert_eq!(
            rendered["mcp"]["servers"]["memory-bank"]["command"],
            Value::String(
                paths
                    .binary_path(MCP_PROXY_BINARY_NAME)
                    .to_string_lossy()
                    .to_string()
            )
        );
        assert_eq!(
            rendered["plugins"]["entries"]["memory-bank"]["config"]["serverUrl"],
            Value::String("http://127.0.0.1:6000".to_string())
        );
        assert_eq!(
            rendered["plugins"]["load"]["paths"],
            json!([
                "/tmp/other-plugin",
                paths
                    .integrations_dir
                    .join("openclaw/memory-bank")
                    .to_string_lossy()
                    .to_string()
            ])
        );
    }
}
