use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::Write;

const MAX_TOOL_INPUT_CHARS: usize = 1000;
const MAX_TOOL_RESULT_CHARS: usize = 2000;
const MAX_THINKING_CHARS: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) struct MemoryProjection {
    pub user_message: String,
    pub assistant_reply: String,
    pub steps: Vec<MemoryStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) struct ProjectedConversationWindow {
    pub previous_turns: Vec<MemoryProjection>,
    pub current_turn: MemoryProjection,
}

#[allow(dead_code, reason = "Retained for future ingestion integrations")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum MemoryStep {
    ToolCall { name: String, input: String },
    ToolResult { name: String, output: String },
    Thinking { text: String },
}

pub(crate) fn render_projection_markdown(projection: &MemoryProjection) -> String {
    let mut rendered = String::with_capacity(
        projection.user_message.len()
            + projection.assistant_reply.len()
            + projection.steps.len() * 48,
    );
    rendered.push_str("## User\n");
    rendered.push_str(&projection.user_message);

    if !projection.steps.is_empty() {
        rendered.push_str("\n\n## Steps");
        for step in &projection.steps {
            rendered.push_str("\n\n");
            rendered.push_str(&render_step(step));
        }
    }

    rendered.push_str("\n\n## Assistant\n");
    rendered.push_str(&projection.assistant_reply);
    rendered
}

pub(crate) fn render_projection_history(turns: &[MemoryProjection]) -> String {
    let mut rendered = String::new();
    for (index, turn) in turns.iter().enumerate() {
        if index > 0 {
            rendered.push_str("\n\n");
        }

        let _ = write!(rendered, "# Previous Turn {}\n", index + 1);
        rendered.push_str(&render_projection_markdown(turn));
    }

    rendered
}

fn render_step(step: &MemoryStep) -> String {
    match step {
        MemoryStep::ToolCall { name, input } => {
            render_code_step("Tool Call", name, input, MAX_TOOL_INPUT_CHARS)
        }
        MemoryStep::ToolResult { name, output } => {
            render_code_step("Tool Result", name, output, MAX_TOOL_RESULT_CHARS)
        }
        MemoryStep::Thinking { text } => {
            format!(
                "### Thinking\n{}",
                truncate_context(text, MAX_THINKING_CHARS)
            )
        }
    }
}

fn render_code_step(label: &str, name: &str, payload: &str, max_chars: usize) -> String {
    let payload = truncate_context(payload, max_chars);
    let language = code_block_language(&payload);
    format!("### {label}: {name}\n```{language}\n{payload}\n```")
}

fn code_block_language(payload: &str) -> &'static str {
    if serde_json::from_str::<Value>(payload).is_ok() {
        "json"
    } else {
        "text"
    }
}

fn truncate_context(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        s.to_string()
    } else {
        format!("{}...[truncated]", &s[..s.floor_char_boundary(max_chars)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projection_with_steps() -> MemoryProjection {
        MemoryProjection {
            user_message: "What time is it?".to_string(),
            assistant_reply: "It is noon.".to_string(),
            steps: vec![
                MemoryStep::ToolCall {
                    name: "shell".to_string(),
                    input: "{\"command\":\"date\"}".to_string(),
                },
                MemoryStep::ToolResult {
                    name: "shell".to_string(),
                    output: "Tue Mar  3 12:00:00 PST 2026".to_string(),
                },
                MemoryStep::Thinking {
                    text: "Checking the local timezone.".to_string(),
                },
            ],
        }
    }

    #[test]
    fn renders_basic_note_without_steps() {
        let projection = MemoryProjection {
            user_message: "Hi".to_string(),
            assistant_reply: "Hello".to_string(),
            steps: Vec::new(),
        };

        assert_eq!(
            render_projection_markdown(&projection),
            "## User\nHi\n\n## Assistant\nHello"
        );
    }

    #[test]
    fn renders_step_sections_in_order() {
        let rendered = render_projection_markdown(&projection_with_steps());
        assert!(rendered.contains("## User\nWhat time is it?"));
        assert!(rendered.contains("## Steps"));
        assert!(rendered.contains("### Tool Call: shell"));
        assert!(rendered.contains("### Tool Result: shell"));
        assert!(rendered.contains("### Thinking\nChecking the local timezone."));
        assert!(rendered.ends_with("## Assistant\nIt is noon."));
    }

    #[test]
    fn uses_json_fence_for_json_payloads() {
        let rendered = render_projection_markdown(&projection_with_steps());
        assert!(rendered.contains("```json\n{\"command\":\"date\"}\n```"));
    }

    #[test]
    fn uses_text_fence_for_non_json_payloads() {
        let rendered = render_projection_markdown(&projection_with_steps());
        assert!(rendered.contains("```text\nTue Mar  3 12:00:00 PST 2026\n```"));
    }

    #[test]
    fn truncates_long_step_payloads() {
        let projection = MemoryProjection {
            user_message: "Hi".to_string(),
            assistant_reply: "Hello".to_string(),
            steps: vec![MemoryStep::Thinking {
                text: "a".repeat(MAX_THINKING_CHARS + 5),
            }],
        };

        let rendered = render_projection_markdown(&projection);
        assert!(rendered.contains("...[truncated]"));
    }

    #[test]
    fn renders_history_in_chronological_order() {
        let turns = vec![
            MemoryProjection {
                user_message: "First".to_string(),
                assistant_reply: "One".to_string(),
                steps: Vec::new(),
            },
            MemoryProjection {
                user_message: "Second".to_string(),
                assistant_reply: "Two".to_string(),
                steps: Vec::new(),
            },
        ];

        let rendered = render_projection_history(&turns);
        assert!(rendered.starts_with("# Previous Turn 1\n## User\nFirst"));
        assert!(rendered.contains("\n\n# Previous Turn 2\n## User\nSecond"));
    }
}
