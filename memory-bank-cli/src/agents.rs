mod claude;
mod codex;
mod gemini;
mod openclaw;
mod opencode;
mod shared;

use crate::AppError;
use crate::assets::find_on_path;
#[cfg(test)]
use crate::command_utils::CommandOutcome;
#[cfg(test)]
use crate::command_utils::CommandRunOptions;
use crate::domain::integration_configured as integration_configured_for_settings;
use memory_bank_app::{AppPaths, AppSettings, default_server_url};
#[cfg(test)]
use serde_json::{Map, Value, json};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentKind {
    ClaudeCode,
    Codex,
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
    pub(crate) fn all() -> [Self; 5] {
        [
            Self::ClaudeCode,
            Self::Codex,
            Self::GeminiCli,
            Self::OpenCode,
            Self::OpenClaw,
        ]
    }

    pub(crate) fn command_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
            Self::GeminiCli => "gemini",
            Self::OpenCode => "opencode",
            Self::OpenClaw => "openclaw",
        }
    }

    pub(crate) fn display_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
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
    detect_installed_agents_with(|agent| find_on_path(agent.command_name()).is_some())
}

fn detect_installed_agents_with<F>(mut is_installed: F) -> Vec<AgentKind>
where
    F: FnMut(AgentKind) -> bool,
{
    AgentKind::all()
        .into_iter()
        .filter(|agent| is_installed(*agent))
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
            AgentKind::Codex => configure_codex(paths, &server_url),
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
    integration_configured_for_settings(settings.integrations.as_ref(), agent)
}

fn configure_claude(paths: &AppPaths, server_url: &str) -> Result<(), AppError> {
    claude::configure(paths, server_url)
}

fn configure_codex(paths: &AppPaths, server_url: &str) -> Result<(), AppError> {
    codex::configure(paths, server_url)
}

#[cfg(test)]
fn configure_claude_with_options(
    paths: &AppPaths,
    server_url: &str,
    options: &CommandRunOptions,
) -> Result<(), AppError> {
    claude::configure_with_options(paths, server_url, options)
}

fn configure_gemini(paths: &AppPaths, server_url: &str) -> Result<(), AppError> {
    gemini::configure(paths, server_url)
}

fn configure_opencode(paths: &AppPaths, server_url: &str) -> Result<(), AppError> {
    opencode::configure(paths, server_url)
}

fn configure_openclaw(paths: &AppPaths, server_url: &str) -> Result<(), AppError> {
    openclaw::configure(paths, server_url)
}

#[cfg(test)]
fn ensure_claude_user_mcp_with_runner<F>(server_url: &str, mut run: F) -> Result<(), AppError>
where
    F: FnMut(&[&str]) -> Result<CommandOutcome, AppError>,
{
    claude::ensure_user_mcp_with_runner(server_url, &mut run)
}

#[cfg(test)]
fn claude_mcp_matches(outcome: &CommandOutcome, desired_url: &str) -> bool {
    claude::mcp_matches(outcome, desired_url)
}

#[cfg(test)]
fn build_hook_command(
    hook_binary: &std::path::Path,
    agent: &str,
    event: &str,
    server_url: &str,
) -> String {
    shared::build_hook_command(hook_binary, agent, event, server_url)
}

#[cfg(test)]
fn upsert_codex_hook(
    root: &mut Value,
    event: &str,
    matcher: Option<&str>,
    command: &str,
) -> Result<(), AppError> {
    codex::upsert_hook(root, event, matcher, command)
}

#[cfg(test)]
fn upsert_claude_hook(root: &mut Value, event: &str, command: &str) -> Result<(), AppError> {
    claude::upsert_hook(root, event, command)
}

#[cfg(test)]
fn upsert_gemini_hook(
    root: &mut Value,
    event: &str,
    matcher: &str,
    command: &str,
) -> Result<(), AppError> {
    gemini::upsert_hook(root, event, matcher, command)
}

#[cfg(test)]
fn upsert_openclaw_plugin_load_path(
    load_map: &mut Map<String, Value>,
    desired_path: &str,
) -> Result<(), AppError> {
    openclaw::upsert_plugin_load_path(load_map, desired_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{HOOK_BINARY_NAME, MCP_PROXY_BINARY_NAME};
    use crate::json_config::load_json_config;
    use crate::real_binary_tests::RealBinaryTestEnv;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn claude_mcp_matches_expected_user_http_server() {
        let outcome = CommandOutcome {
            program: "claude".to_string(),
            args: vec!["mcp".to_string(), "get".to_string(), "memory-bank".to_string()],
            exit_code: Some(0),
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
    fn upsert_codex_hook_rewrites_owned_handler_and_timeout() {
        let mut root = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "stale",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "old --agent codex --event PreToolUse",
                                "timeout": 5
                            }
                        ]
                    }
                ]
            }
        });

        upsert_codex_hook(&mut root, "PreToolUse", Some("Bash"), "new-command")
            .expect("upsert codex hook");

        let group = &root["hooks"]["PreToolUse"][0];
        assert_eq!(group["matcher"], Value::String("Bash".to_string()));
        assert_eq!(
            group["hooks"][0]["command"],
            Value::String("new-command".to_string())
        );
        assert_eq!(group["hooks"][0]["timeout"], Value::from(10));
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
    fn upsert_codex_hook_repairs_malformed_groups_and_preserves_unrelated_hooks() {
        let mut root = json!({
            "hooks": {
                "PreToolUse": [
                    "broken",
                    {
                        "matcher": "Bash",
                        "hooks": "not-an-array"
                    },
                    {
                        "matcher": "Bash",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "old --agent codex --event PreToolUse",
                                "timeout": 5
                            },
                            {
                                "type": "command",
                                "command": "python3 ~/.codex/hooks/keep.py"
                            },
                            42
                        ]
                    }
                ]
            }
        });

        upsert_codex_hook(&mut root, "PreToolUse", Some("Bash"), "new-command")
            .expect("upsert codex hook");

        let groups = root["hooks"]["PreToolUse"]
            .as_array()
            .expect("pre-tool groups");
        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups[0]["hooks"][0]["command"],
            Value::String("python3 ~/.codex/hooks/keep.py".to_string())
        );
        assert_eq!(
            groups[1]["hooks"][0]["command"],
            Value::String("new-command".to_string())
        );
        assert_eq!(groups[1]["matcher"], Value::String("Bash".to_string()));
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
    fn configure_gemini_repairs_malformed_sections() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings_path = paths.home_dir.join(".gemini/settings.json");
        fs::create_dir_all(settings_path.parent().expect("parent")).expect("parent");
        fs::write(
            &settings_path,
            r#"{
  "mcpServers": [],
  "hooks": {
    "BeforeTool": {}
  }
}"#,
        )
        .expect("seed config");

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
    }

    #[test]
    fn configure_codex_writes_user_config_and_hooks() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());

        configure_codex(&paths, "http://127.0.0.1:4545").expect("configure codex");

        let config_path = paths.home_dir.join(".codex/config.toml");
        let hooks_path = paths.home_dir.join(".codex/hooks.json");
        let config = fs::read_to_string(&config_path).expect("read codex config");
        let hooks = load_json_config(&hooks_path).expect("load hooks");

        assert!(config.contains("[features]"));
        assert!(config.contains("codex_hooks = true"));
        assert!(config.contains("[mcp_servers.memory-bank]"));
        assert!(config.contains("url = \"http://127.0.0.1:4545/mcp\""));
        assert!(config.contains("enabled = true"));

        assert_eq!(
            hooks["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
            Value::String(build_hook_command(
                &paths.binary_path(HOOK_BINARY_NAME),
                "codex",
                "UserPromptSubmit",
                "http://127.0.0.1:4545",
            ))
        );
        assert_eq!(
            hooks["hooks"]["PreToolUse"][0]["matcher"],
            Value::String("Bash".to_string())
        );
        assert_eq!(
            hooks["hooks"]["PreToolUse"][0]["hooks"][0]["timeout"],
            Value::from(10)
        );
    }

    #[test]
    fn configure_codex_preserves_unrelated_entries_and_repairs_owned_config() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let config_path = paths.home_dir.join(".codex/config.toml");
        let hooks_path = paths.home_dir.join(".codex/hooks.json");
        fs::create_dir_all(config_path.parent().expect("parent")).expect("parent");
        fs::write(
            &config_path,
            r#"# keep me
model = "gpt-5.4"
features = []

[mcp_servers.context7]
command = "npx"

[mcp_servers.memory-bank]
url = "http://stale/mcp"
enabled = false
"#,
        )
        .expect("seed config");
        fs::write(
            &hooks_path,
            json!({
                "hooks": {
                    "PreToolUse": [
                        {
                            "matcher": "Bash",
                            "hooks": [
                                {
                                    "type": "command",
                                    "command": "old --agent codex --event PreToolUse",
                                    "timeout": 5
                                }
                            ]
                        }
                    ],
                    "SessionStart": [
                        {
                            "matcher": "startup|resume",
                            "hooks": [
                                {
                                    "type": "command",
                                    "command": "python3 ~/.codex/hooks/session_start.py"
                                }
                            ]
                        }
                    ]
                }
            })
            .to_string(),
        )
        .expect("seed hooks");

        configure_codex(&paths, "http://127.0.0.1:6000").expect("configure codex");

        let config = fs::read_to_string(&config_path).expect("read codex config");
        let hooks = load_json_config(&hooks_path).expect("load hooks");

        assert!(config.contains("# keep me"));
        assert!(config.contains("[mcp_servers.context7]"));
        assert!(config.contains("command = \"npx\""));
        assert!(config.contains("codex_hooks = true"));
        assert!(config.contains("url = \"http://127.0.0.1:6000/mcp\""));
        assert!(config_path.with_extension("toml.mb_backup").exists());

        assert_eq!(
            hooks["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            Value::String("python3 ~/.codex/hooks/session_start.py".to_string())
        );
        assert_eq!(
            hooks["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            Value::String(build_hook_command(
                &paths.binary_path(HOOK_BINARY_NAME),
                "codex",
                "PreToolUse",
                "http://127.0.0.1:6000",
            ))
        );
        assert_eq!(
            hooks["hooks"]["PreToolUse"][0]["hooks"][0]["timeout"],
            Value::from(10)
        );
        assert!(hooks_path.with_extension("json.mb_backup").exists());
    }

    #[test]
    fn configure_codex_repairs_malformed_hook_groups_and_keeps_unrelated_entries() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let hooks_path = paths.home_dir.join(".codex/hooks.json");
        fs::create_dir_all(hooks_path.parent().expect("parent")).expect("parent");
        fs::write(
            &hooks_path,
            json!({
                "hooks": {
                    "UserPromptSubmit": "bad-shape",
                    "PreToolUse": [
                        "broken",
                        {
                            "matcher": "Bash",
                            "hooks": "bad-shape"
                        },
                        {
                            "matcher": "Bash",
                            "hooks": [
                                {
                                    "type": "command",
                                    "command": "python3 ~/.codex/hooks/keep.py"
                                },
                                17,
                                {
                                    "type": "command",
                                    "command": "old --agent codex --event PreToolUse",
                                    "timeout": 5
                                }
                            ]
                        }
                    ]
                }
            })
            .to_string(),
        )
        .expect("seed malformed hooks");

        configure_codex(&paths, "http://127.0.0.1:8123").expect("configure codex");

        let hooks = load_json_config(&hooks_path).expect("load hooks");
        let pre_tool_groups = hooks["hooks"]["PreToolUse"]
            .as_array()
            .expect("pre-tool groups");
        assert_eq!(pre_tool_groups.len(), 2);
        assert_eq!(
            pre_tool_groups[0]["hooks"][0]["command"],
            Value::String("python3 ~/.codex/hooks/keep.py".to_string())
        );
        assert_eq!(
            pre_tool_groups[1]["hooks"][0]["command"],
            Value::String(build_hook_command(
                &paths.binary_path(HOOK_BINARY_NAME),
                "codex",
                "PreToolUse",
                "http://127.0.0.1:8123",
            ))
        );
        assert_eq!(
            hooks["hooks"]["UserPromptSubmit"]
                .as_array()
                .expect("user prompt groups")
                .len(),
            1
        );
    }

    #[test]
    fn configure_codex_is_idempotent_across_reruns() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());

        configure_codex(&paths, "http://127.0.0.1:3737").expect("first configure");
        configure_codex(&paths, "http://127.0.0.1:3737").expect("second configure");

        let config = fs::read_to_string(paths.home_dir.join(".codex/config.toml"))
            .expect("read codex config");
        let hooks = load_json_config(&paths.home_dir.join(".codex/hooks.json")).expect("hooks");

        assert_eq!(config.matches("[mcp_servers.memory-bank]").count(), 1);
        assert_eq!(config.matches("codex_hooks = true").count(), 1);
        assert_eq!(
            hooks["hooks"]["UserPromptSubmit"]
                .as_array()
                .expect("user prompt groups")
                .len(),
            1
        );
        assert_eq!(
            hooks["hooks"]["PreToolUse"]
                .as_array()
                .expect("pre tool groups")
                .len(),
            1
        );
        assert_eq!(
            hooks["hooks"]["PostToolUse"]
                .as_array()
                .expect("post tool groups")
                .len(),
            1
        );
        assert_eq!(
            hooks["hooks"]["Stop"]
                .as_array()
                .expect("stop groups")
                .len(),
            1
        );
    }

    #[test]
    fn detect_installed_agents_includes_codex_when_present() {
        let detected = detect_installed_agents_with(|agent| {
            matches!(agent, AgentKind::ClaudeCode | AgentKind::Codex)
        });

        assert_eq!(detected, vec![AgentKind::ClaudeCode, AgentKind::Codex]);
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

    #[test]
    fn configure_openclaw_repairs_malformed_sections() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let settings_path = paths.home_dir.join(".openclaw/openclaw.json");
        fs::create_dir_all(settings_path.parent().expect("parent")).expect("parent");
        fs::write(
            &settings_path,
            r#"{
  "mcp": [],
  "plugins": {
    "load": [],
    "entries": [],
    "slots": []
  }
}"#,
        )
        .expect("seed config");

        configure_openclaw(&paths, "http://127.0.0.1:6000").expect("configure openclaw");

        let rendered = load_json_config(&settings_path).expect("load openclaw config");
        assert_eq!(
            rendered["mcp"]["servers"]["memory-bank"]["args"],
            json!(["--server-url", "http://127.0.0.1:6000"])
        );
        assert_eq!(
            rendered["plugins"]["slots"]["memory"],
            Value::String("none".to_string())
        );
    }

    #[test]
    fn configure_gemini_is_idempotent_across_reruns() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());

        configure_gemini(&paths, "http://127.0.0.1:3737").expect("first configure");
        configure_gemini(&paths, "http://127.0.0.1:3737").expect("second configure");

        let rendered =
            fs::read_to_string(paths.home_dir.join(".gemini/settings.json")).expect("settings");
        assert_eq!(
            rendered
                .matches("\"httpUrl\": \"http://127.0.0.1:3737/mcp\"")
                .count(),
            1
        );
        assert_eq!(rendered.matches("\"name\": \"memory-bank\"").count(), 4);
    }

    #[test]
    fn configure_opencode_is_idempotent_across_reruns() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let plugin_source = paths.integrations_dir.join("opencode/memory-bank.js");
        fs::create_dir_all(plugin_source.parent().expect("parent")).expect("parent");
        fs::write(&plugin_source, "// plugin").expect("plugin source");

        configure_opencode(&paths, "http://127.0.0.1:3737").expect("first configure");
        configure_opencode(&paths, "http://127.0.0.1:3737").expect("second configure");

        let rendered = fs::read_to_string(paths.home_dir.join(".config/opencode/opencode.json"))
            .expect("settings");
        assert_eq!(rendered.matches("\"memory-bank\"").count(), 1);
        assert_eq!(rendered.matches("http://127.0.0.1:3737/mcp").count(), 1);
    }

    #[test]
    fn configure_openclaw_is_idempotent_across_reruns() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());

        configure_openclaw(&paths, "http://127.0.0.1:3737").expect("first configure");
        configure_openclaw(&paths, "http://127.0.0.1:3737").expect("second configure");

        let rendered =
            fs::read_to_string(paths.home_dir.join(".openclaw/openclaw.json")).expect("settings");
        let load_path = paths
            .integrations_dir
            .join("openclaw/memory-bank")
            .to_string_lossy()
            .to_string();
        assert_eq!(rendered.matches(&load_path).count(), 1);
        assert_eq!(rendered.matches("\"memory-bank\"").count(), 2);
    }

    #[test]
    fn ensure_claude_user_mcp_rejects_conflicting_project_scope() {
        let error = ensure_claude_user_mcp_with_runner("http://127.0.0.1:3737", |_| {
            Ok(CommandOutcome {
                program: "claude".to_string(),
                args: vec!["mcp".to_string(), "get".to_string(), "memory-bank".to_string()],
                exit_code: Some(0),
                success: true,
                stdout: "memory-bank:\n  Scope: Project config\n  Type: http\n  URL: http://127.0.0.1:3737/mcp\n".to_string(),
                stderr: String::new(),
            })
        })
        .expect_err("conflicting project config should fail");

        assert!(
            error
                .to_string()
                .contains("conflicting `memory-bank` MCP server outside user scope")
        );
    }

    #[test]
    fn ensure_claude_user_mcp_accepts_verification_after_add_failure() {
        let responses = RefCell::new(VecDeque::from([
            CommandOutcome {
                program: "claude".to_string(),
                args: vec!["mcp".to_string(), "get".to_string(), "memory-bank".to_string()],
                exit_code: Some(1),
                success: false,
                stdout: String::new(),
                stderr: "not found".to_string(),
            },
            CommandOutcome {
                program: "claude".to_string(),
                args: vec![
                    "mcp".to_string(),
                    "add".to_string(),
                    "--transport".to_string(),
                    "http".to_string(),
                    "--scope".to_string(),
                    "user".to_string(),
                    "memory-bank".to_string(),
                    "http://127.0.0.1:3737/mcp".to_string(),
                ],
                exit_code: Some(1),
                success: false,
                stdout: String::new(),
                stderr: "transient add failure".to_string(),
            },
            CommandOutcome {
                program: "claude".to_string(),
                args: vec!["mcp".to_string(), "get".to_string(), "memory-bank".to_string()],
                exit_code: Some(0),
                success: true,
                stdout: "memory-bank:\n  Scope: User config\n  Type: http\n  URL: http://127.0.0.1:3737/mcp\n".to_string(),
                stderr: String::new(),
            },
        ]));

        ensure_claude_user_mcp_with_runner("http://127.0.0.1:3737", |_| {
            responses
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| AppError::Message("missing scripted response".to_string()))
        })
        .expect("verify should recover after add failure");
    }

    #[test]
    fn real_claude_configuration_round_trip() {
        let Some(env) = RealBinaryTestEnv::for_binary("claude") else {
            return;
        };
        env.seed_agent_artifacts().expect("agent artifacts");

        configure_claude_with_options(&env.paths, "http://127.0.0.1:3737", &env.command_options())
            .expect("configure claude");

        let outcome = env
            .run_cli("claude", &["mcp", "get", "memory-bank"])
            .expect("claude mcp get");

        assert!(outcome.success);
        let output = outcome.combined_output();
        assert!(output.contains("Scope: User config"));
        assert!(output.contains("Type: http"));
        assert!(output.contains("http://127.0.0.1:3737/mcp"));

        let settings =
            fs::read_to_string(env.paths.home_dir.join(".claude/settings.json")).expect("settings");
        assert!(settings.contains("UserPromptSubmit"));
        assert!(settings.contains("PostToolUse"));
        assert!(settings.contains("memory-bank-hook"));
    }

    #[test]
    fn real_gemini_configuration_round_trip() {
        let Some(env) = RealBinaryTestEnv::for_binary("gemini") else {
            return;
        };
        env.seed_agent_artifacts().expect("agent artifacts");
        env.seed_gemini_home().expect("gemini home");

        configure_gemini(&env.paths, "http://127.0.0.1:3737").expect("configure gemini");

        let outcome = env
            .run_cli("gemini", &["mcp", "list"])
            .expect("gemini mcp list");

        assert!(outcome.success);
        let output = outcome.combined_output();
        assert!(output.contains("memory-bank"));
        assert!(output.contains("http://127.0.0.1:3737/mcp"));
    }

    #[test]
    fn real_opencode_configuration_round_trip() {
        let Some(env) = RealBinaryTestEnv::for_binary("opencode") else {
            return;
        };
        env.seed_agent_artifacts().expect("agent artifacts");

        configure_opencode(&env.paths, "http://127.0.0.1:3737").expect("configure opencode");

        let outcome = env
            .run_cli("opencode", &["mcp", "list"])
            .expect("opencode mcp list");

        assert!(outcome.success);
        let output = outcome.combined_output();
        assert!(output.contains("memory-bank"));
        assert!(output.contains("http://127.0.0.1:3737/mcp"));
    }

    #[test]
    fn real_openclaw_configuration_round_trip() {
        let Some(env) = RealBinaryTestEnv::for_binary("openclaw") else {
            return;
        };
        env.seed_agent_artifacts().expect("agent artifacts");

        configure_openclaw(&env.paths, "http://127.0.0.1:3737").expect("configure openclaw");

        let outcome = env
            .run_cli("openclaw", &["config", "validate", "--json"])
            .expect("openclaw validate");

        assert!(outcome.success);
        let output = outcome.combined_output();
        assert!(output.contains(r#""valid":true"#) || output.contains(r#""valid": true"#));

        let settings =
            fs::read_to_string(env.paths.home_dir.join(".openclaw/openclaw.json")).expect("config");
        assert!(settings.contains("memory-bank-mcp-proxy"));
        assert!(settings.contains("memory-bank-hook"));
        assert!(settings.contains("http://127.0.0.1:3737"));
    }

    #[test]
    fn real_codex_configuration_round_trip() {
        let Some(env) = RealBinaryTestEnv::for_binary("codex") else {
            return;
        };
        env.seed_agent_artifacts().expect("agent artifacts");

        configure_codex(&env.paths, "http://127.0.0.1:3737").expect("configure codex");

        let config = fs::read_to_string(env.paths.home_dir.join(".codex/config.toml"))
            .expect("codex config");
        let hooks =
            fs::read_to_string(env.paths.home_dir.join(".codex/hooks.json")).expect("codex hooks");

        assert!(config.contains("codex_hooks = true"));
        assert!(config.contains("http://127.0.0.1:3737/mcp"));
        assert!(hooks.contains("--agent codex --event UserPromptSubmit"));
        assert!(hooks.contains("--agent codex --event Stop"));
    }
}
