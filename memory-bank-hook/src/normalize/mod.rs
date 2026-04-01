mod claude_code;
mod codex;
mod gemini_cli;
mod openclaw;
mod opencode;

use memory_bank_protocol::IngestEnvelope;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::error::AppError;

pub(crate) struct EnvelopeParser<'a> {
    agent: &'a str,
    event: &'a str,
}

impl<'a> EnvelopeParser<'a> {
    pub(crate) fn new(agent: &'a str, event: &'a str) -> Self {
        Self { agent, event }
    }

    pub(crate) fn parse(&self, raw: &[u8]) -> Result<Option<IngestEnvelope>, AppError> {
        match self.agent {
            "claude-code" => claude_code::ClaudeCodeParser::new(self.event)
                .parse(raw)
                .map(Some),
            "codex" => codex::CodexParser::new(self.event).parse(raw),
            "gemini-cli" => gemini_cli::GeminiCliParser::new(self.event)
                .parse(raw)
                .map(Some),
            "openclaw" => openclaw::OpenClawParser::new(self.event).parse(raw),
            "opencode" => opencode::OpenCodeParser::new(self.event).parse(raw),
            other => Err(AppError::Normalize(format!(
                "Unsupported agent '{other}'. Supported agents: claude-code, codex, gemini-cli, openclaw, opencode"
            ))),
        }
    }
}

pub(super) fn parse_object_json(raw: &[u8], source: &str) -> Result<Value, AppError> {
    let raw_json: Value = serde_json::from_slice(raw)?;
    if !raw_json.is_object() {
        return Err(AppError::Normalize(format!(
            "{source} hook payload must be a JSON object"
        )));
    }

    Ok(raw_json)
}

pub(super) fn required_non_empty<'a>(
    value: Option<&'a str>,
    error: &str,
) -> Result<&'a str, AppError> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::Normalize(error.to_string()))
}

pub(super) fn deserialize_optional_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(Value::Number(number)) => number.as_u64(),
        Some(Value::String(value)) => value.parse::<u64>().ok(),
        _ => None,
    })
}

pub(super) fn deserialize_optional_bool<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(Value::Bool(value)) => Some(value),
        Some(Value::Number(number)) if number.as_u64() == Some(1) => Some(true),
        Some(Value::Number(number)) if number.as_u64() == Some(0) => Some(false),
        Some(Value::String(value)) => match value.as_str() {
            "true" | "TRUE" | "True" | "1" => Some(true),
            "false" | "FALSE" | "False" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    })
}

pub(super) fn serialize_json(value: &Value) -> Result<String, AppError> {
    serde_json::to_string(value).map_err(AppError::from)
}

#[cfg(test)]
mod tests {
    use super::EnvelopeParser;
    use memory_bank_protocol::FragmentBody;
    use serde_json::json;

    #[test]
    fn dispatches_to_claude_code_parser() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "UserPromptSubmit",
            "prompt": "hello world"
        });

        let payload = EnvelopeParser::new("claude-code", "UserPromptSubmit")
            .parse(raw.to_string().as_bytes())
            .expect("parse");
        let payload = payload.expect("payload should not be skipped");

        assert!(matches!(
            payload.fragment.body,
            FragmentBody::UserMessage { ref text } if text == "hello world"
        ));
    }

    #[test]
    fn dispatches_to_gemini_cli_parser() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "BeforeAgent",
            "prompt": "hello gemini"
        });

        let payload = EnvelopeParser::new("gemini-cli", "BeforeAgent")
            .parse(raw.to_string().as_bytes())
            .expect("parse");
        let payload = payload.expect("payload should not be skipped");

        assert!(matches!(
            payload.fragment.body,
            FragmentBody::UserMessage { ref text } if text == "hello gemini"
        ));
    }

    #[test]
    fn dispatches_to_codex_parser() {
        let raw = json!({
            "session_id": "session-1",
            "turn_id": "turn-1",
            "hook_event_name": "UserPromptSubmit",
            "prompt": "hello codex"
        });

        let payload = EnvelopeParser::new("codex", "UserPromptSubmit")
            .parse(raw.to_string().as_bytes())
            .expect("parse");
        let payload = payload.expect("payload should not be skipped");

        assert!(matches!(
            payload.fragment.body,
            FragmentBody::UserMessage { ref text } if text == "hello codex"
        ));
        assert_eq!(payload.scope.turn_id.as_deref(), Some("turn-1"));
    }

    #[test]
    fn dispatches_to_opencode_parser() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "message.updated",
            "message_id": "msg-1",
            "role": "user",
            "parts": [
                {"type": "text", "text": "hello opencode"}
            ]
        });

        let payload = EnvelopeParser::new("opencode", "message.updated")
            .parse(raw.to_string().as_bytes())
            .expect("parse");
        let payload = payload.expect("payload should not be skipped");

        assert!(matches!(
            payload.fragment.body,
            FragmentBody::UserMessage { ref text } if text == "hello opencode"
        ));
    }

    #[test]
    fn dispatches_to_openclaw_parser() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "before_agent_start",
            "prompt": "hello openclaw"
        });

        let payload = EnvelopeParser::new("openclaw", "before_agent_start")
            .parse(raw.to_string().as_bytes())
            .expect("parse");
        let payload = payload.expect("payload should not be skipped");

        assert!(matches!(
            payload.fragment.body,
            FragmentBody::UserMessage { ref text } if text == "hello openclaw"
        ));
    }

    #[test]
    fn rejects_unsupported_agent() {
        let error = EnvelopeParser::new("windsurf", "AfterAgent")
            .parse(br#"{"session_id":"session-1"}"#)
            .expect_err("unsupported agent");

        assert!(error.to_string().contains("Unsupported agent 'windsurf'"));
    }
}
