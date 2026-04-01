use crate::agents::AgentKind;
use crate::assets::ExposureOutcome;
use crate::command_utils::yes_no;
use crate::output::{styled_command, styled_section, styled_subtle, styled_title};

use super::plan::SetupPlan;

pub(super) fn print_setup_intro() {
    println!("{}", styled_title("Memory Bank Setup"));
    println!(
        "{}",
        styled_subtle(
            "Configure the local Memory Bank service and any detected agent integrations."
        )
    );
    println!(
        "{}",
        styled_subtle("You will review everything before any changes are applied.")
    );
}

pub(super) fn print_setup_section(title: &str) {
    println!();
    println!("{}", styled_section(title));
}

pub(super) fn render_review_summary(plan: &SetupPlan) -> String {
    let mut lines = vec![
        styled_section("Setup review"),
        "  Basic".to_string(),
        format!("    Namespace: {}", plan.namespace),
        String::new(),
        "  LLM configuration".to_string(),
        format!("    Provider: {}", plan.provider),
        format!("    Model: {}", plan.model),
        format!("    Secret: {}", plan.secret_choice.summary()),
        String::new(),
        "  Preferences".to_string(),
        format!("    Autostart: {}", yes_no(plan.autostart)),
        String::new(),
        "  Agent integrations".to_string(),
        format!(
            "    Selected: {}",
            render_agents_summary(&plan.selected_agents)
        ),
    ];

    if let Some(url) = plan.ollama_url.as_deref() {
        lines.insert(6, format!("    Ollama URL: {url}"));
    }

    let overrides = plan.advanced.override_lines();
    lines.push(String::new());
    lines.push("  Advanced settings".to_string());
    if overrides.is_empty() {
        lines.push("    Using defaults".to_string());
    } else {
        for line in overrides {
            lines.push(format!("    {line}"));
        }
    }

    lines.join("\n")
}

pub(super) fn render_post_setup_help(exposure: &ExposureOutcome) -> String {
    [
        styled_section("What's next"),
        "A few useful commands after setup:".to_string(),
        format!(
            "  {}  Review the saved configuration",
            styled_command(&format!("{} config show", exposure.command_prefix))
        ),
        format!(
            "  {}  Run the guided setup again any time",
            styled_command(&format!("{} setup", exposure.command_prefix))
        ),
        format!(
            "  {}  Check for common install or config issues",
            styled_command(&format!("{} doctor", exposure.command_prefix))
        ),
    ]
    .join("\n")
}

pub(super) fn render_agents_summary(agents: &[AgentKind]) -> String {
    if agents.is_empty() {
        "none selected".to_string()
    } else {
        agents
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::super::plan::{AdvancedSettings, SecretChoice};
    use super::*;
    use crate::assets::ExposureMode;
    use crate::domain::ProviderId;
    use memory_bank_app::{AppSettings, Namespace};

    #[test]
    fn render_review_summary_hides_secret_value_and_omits_default_advanced() {
        let plan = SetupPlan {
            namespace: Namespace::new("default"),
            provider: ProviderId::Anthropic,
            model: memory_bank_app::DEFAULT_ANTHROPIC_MODEL.to_string(),
            ollama_url: None,
            autostart: true,
            selected_agents: vec![AgentKind::ClaudeCode, AgentKind::GeminiCli],
            secret_choice: SecretChoice::ManualEntry {
                key: "ANTHROPIC_API_KEY",
                value: "super-secret".to_string(),
            },
            advanced: AdvancedSettings::from_settings(&AppSettings::default()),
        };

        let summary = render_review_summary(&plan);
        assert!(summary.contains("Store a newly entered ANTHROPIC_API_KEY"));
        assert!(!summary.contains("super-secret"));
        assert!(!summary.contains("Advanced overrides"));
    }

    #[test]
    fn render_post_setup_help_mentions_key_commands() {
        let help = render_post_setup_help(&ExposureOutcome {
            mode: ExposureMode::Launcher,
            bare_command_works_now: true,
            command_prefix: "mb".to_string(),
        });

        assert!(help.contains("mb config show"));
        assert!(help.contains("mb setup"));
        assert!(help.contains("mb doctor"));
    }

    #[test]
    fn render_post_setup_help_uses_absolute_path_when_bare_mb_is_not_ready() {
        let help = render_post_setup_help(&ExposureOutcome {
            mode: ExposureMode::ShellInitFallback,
            bare_command_works_now: false,
            command_prefix: "/tmp/.memory_bank/bin/mb".to_string(),
        });

        assert!(help.contains("/tmp/.memory_bank/bin/mb config show"));
        assert!(help.contains("/tmp/.memory_bank/bin/mb setup"));
        assert!(help.contains("/tmp/.memory_bank/bin/mb doctor"));
    }

    #[test]
    fn render_agents_summary_handles_empty_and_multiple_values() {
        assert_eq!(render_agents_summary(&[]), "none selected");
        assert_eq!(
            render_agents_summary(&[AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::OpenClaw]),
            "Claude Code, Codex, OpenClaw"
        );
    }
}
