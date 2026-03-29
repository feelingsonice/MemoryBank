use super::{
    deserialize_optional_bool, deserialize_optional_u64, parse_object_json, required_non_empty,
    serialize_json,
};
use blake3::Hasher;
use memory_bank_protocol::{
    ConversationFragment, ConversationScope, FragmentBody, INGEST_PROTOCOL_VERSION, IngestEnvelope,
    SourceMeta, Terminality,
};
use serde::Deserialize;
use serde_json::Value;

use crate::error::AppError;

pub(super) struct ClaudeCodeParser<'a> {
    event_flag: &'a str,
}

impl<'a> ClaudeCodeParser<'a> {
    pub(super) fn new(event_flag: &'a str) -> Self {
        Self { event_flag }
    }

    pub(super) fn parse(&self, raw: &[u8]) -> Result<IngestEnvelope, AppError> {
        let raw_json = parse_object_json(raw, "Claude")?;
        let payload = self.parse_payload(raw)?;
        let event = self.resolve_event(&payload)?;
        let fragment = self.build_fragment(&payload, &event, &raw_json)?;
        self.build_envelope(&payload, &event, fragment, raw_json, raw)
    }

    fn parse_payload(&self, raw: &[u8]) -> Result<ClaudeHookPayload, AppError> {
        Ok(serde_json::from_slice(raw)?)
    }

    fn resolve_event(&self, payload: &ClaudeHookPayload) -> Result<ClaudeEvent, AppError> {
        if let Some(payload_event) = payload.hook_event_name.as_deref()
            && payload_event != self.event_flag
        {
            return Err(AppError::Normalize(format!(
                "CLI event '{}' does not match payload hook_event_name '{}'",
                self.event_flag, payload_event
            )));
        }

        Ok(ClaudeEvent::from_name(
            payload
                .hook_event_name
                .as_deref()
                .unwrap_or(self.event_flag),
        ))
    }

    fn build_fragment(
        &self,
        payload: &ClaudeHookPayload,
        event: &ClaudeEvent,
        raw_json: &Value,
    ) -> Result<ConversationFragment, AppError> {
        match event {
            ClaudeEvent::UserPromptSubmit => Ok(ConversationFragment {
                terminality: Terminality::None,
                body: FragmentBody::UserMessage {
                    text: self.user_message_text(payload)?,
                },
            }),
            ClaudeEvent::PreToolUse => Ok(ConversationFragment {
                terminality: Terminality::None,
                body: FragmentBody::ToolCall {
                    name: self.required_tool_name(payload, "PreToolUse")?.to_string(),
                    input_json: self.tool_input_json(payload)?,
                    tool_use_id: payload.tool_use_id.clone(),
                },
            }),
            ClaudeEvent::PostToolUse => Ok(ConversationFragment {
                terminality: Terminality::None,
                body: FragmentBody::ToolResult {
                    name: self.required_tool_name(payload, "PostToolUse")?.to_string(),
                    output_json: self.tool_output_json(payload)?,
                    tool_use_id: payload.tool_use_id.clone(),
                },
            }),
            ClaudeEvent::Stop => Ok(ConversationFragment {
                terminality: if payload.stop_hook_active.unwrap_or(false) {
                    Terminality::Soft
                } else {
                    Terminality::Hard
                },
                body: FragmentBody::AssistantMessage {
                    text: payload
                        .last_assistant_message
                        .clone()
                        .unwrap_or_else(|| "[assistant message unavailable]".to_string()),
                },
            }),
            ClaudeEvent::Other(name) => Ok(ConversationFragment {
                terminality: Terminality::None,
                body: FragmentBody::Other {
                    label: name.clone(),
                    payload: raw_json.clone(),
                },
            }),
        }
    }

    fn build_envelope(
        &self,
        payload: &ClaudeHookPayload,
        event: &ClaudeEvent,
        fragment: ConversationFragment,
        raw_json: Value,
        raw: &[u8],
    ) -> Result<IngestEnvelope, AppError> {
        let conversation_id = required_non_empty(
            Some(payload.session_id.as_str()),
            "Claude payload missing required string 'session_id'",
        )?;

        let envelope = IngestEnvelope {
            protocol_version: INGEST_PROTOCOL_VERSION,
            source: SourceMeta {
                agent: "claude-code".to_string(),
                event: event.as_str().to_string(),
            },
            scope: ConversationScope {
                conversation_id: conversation_id.to_string(),
                turn_id: None,
                fragment_id: self.build_fragment_id(conversation_id, event, raw),
                sequence_hint: payload.sequence_hint,
                emitted_at_rfc3339: payload.timestamp.clone(),
            },
            fragment,
            raw: raw_json,
        };

        envelope.validate().map_err(AppError::Normalize)?;
        Ok(envelope)
    }

    fn build_fragment_id(&self, conversation_id: &str, event: &ClaudeEvent, raw: &[u8]) -> String {
        let mut hasher = Hasher::new();
        hasher.update(b"claude-code");
        hasher.update(conversation_id.as_bytes());
        hasher.update(event.as_str().as_bytes());
        hasher.update(raw);
        format!("claude-{}", hasher.finalize().to_hex())
    }

    fn user_message_text(&self, payload: &ClaudeHookPayload) -> Result<String, AppError> {
        required_non_empty(
            payload
                .prompt
                .as_deref()
                .or_else(|| payload.input.as_ref().and_then(Value::as_str)),
            "UserPromptSubmit payload missing prompt/user_prompt/input string",
        )
        .map(ToOwned::to_owned)
    }

    fn required_tool_name<'b>(
        &self,
        payload: &'b ClaudeHookPayload,
        event: &str,
    ) -> Result<&'b str, AppError> {
        required_non_empty(
            payload.tool_name.as_deref(),
            &format!("{event} payload missing tool_name"),
        )
    }

    fn tool_input_json(&self, payload: &ClaudeHookPayload) -> Result<String, AppError> {
        match payload.tool_input.as_ref().or(payload.input.as_ref()) {
            Some(value) => serialize_json(value),
            None => Ok("{}".to_string()),
        }
    }

    fn tool_output_json(&self, payload: &ClaudeHookPayload) -> Result<String, AppError> {
        match payload
            .tool_output
            .as_ref()
            .or(payload.output.as_ref())
            .or(payload.result.as_ref())
        {
            Some(value) => serialize_json(value),
            None => Ok("null".to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClaudeEvent {
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    Stop,
    Other(String),
}

impl ClaudeEvent {
    fn from_name(name: &str) -> Self {
        match name {
            "UserPromptSubmit" => Self::UserPromptSubmit,
            "PreToolUse" => Self::PreToolUse,
            "PostToolUse" => Self::PostToolUse,
            "Stop" => Self::Stop,
            other => Self::Other(other.to_string()),
        }
    }

    fn as_str(&self) -> &str {
        match self {
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::Stop => "Stop",
            Self::Other(name) => name.as_str(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ClaudeHookPayload {
    session_id: String,
    #[serde(default)]
    hook_event_name: Option<String>,
    #[serde(
        default,
        alias = "sequence",
        alias = "event_index",
        deserialize_with = "deserialize_optional_u64"
    )]
    sequence_hint: Option<u64>,
    #[serde(default, alias = "event_timestamp")]
    timestamp: Option<String>,
    #[serde(default, alias = "user_prompt")]
    prompt: Option<String>,
    #[serde(default)]
    input: Option<Value>,
    #[serde(default, alias = "tool")]
    tool_name: Option<String>,
    #[serde(default, alias = "tool_input")]
    tool_input: Option<Value>,
    #[serde(default)]
    tool_output: Option<Value>,
    #[serde(default)]
    output: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default, alias = "assistant_message", alias = "response")]
    last_assistant_message: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_bool")]
    stop_hook_active: Option<bool>,
    #[serde(default)]
    tool_use_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::ClaudeCodeParser;
    use memory_bank_protocol::{FragmentBody, IngestEnvelope, Terminality};
    use serde_json::json;

    fn parse(
        event_flag: &str,
        raw: serde_json::Value,
    ) -> Result<IngestEnvelope, crate::error::AppError> {
        ClaudeCodeParser::new(event_flag).parse(raw.to_string().as_bytes())
    }

    #[test]
    fn maps_user_prompt_submit_to_user_message() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "UserPromptSubmit",
            "prompt": "hello world"
        });

        let payload = parse("UserPromptSubmit", raw).expect("normalize");
        assert_eq!(payload.source.event, "UserPromptSubmit");
        assert_eq!(payload.fragment.terminality, Terminality::None);
        assert!(matches!(
            payload.fragment.body,
            FragmentBody::UserMessage { ref text } if text == "hello world"
        ));
    }

    #[test]
    fn maps_user_prompt_submit_from_input_alias() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "UserPromptSubmit",
            "input": "hello from input"
        });

        let payload = parse("UserPromptSubmit", raw).expect("normalize");
        assert!(matches!(
            payload.fragment.body,
            FragmentBody::UserMessage { ref text } if text == "hello from input"
        ));
    }

    #[test]
    fn maps_stop_to_hard_terminal_assistant_message() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "Stop",
            "stop_hook_active": false,
            "last_assistant_message": "Done."
        });

        let payload = parse("Stop", raw).expect("normalize");
        assert_eq!(payload.fragment.terminality, Terminality::Hard);
        assert!(matches!(
            payload.fragment.body,
            FragmentBody::AssistantMessage { ref text } if text == "Done."
        ));
    }

    #[test]
    fn maps_pre_tool_use_from_alias_fields() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "PreToolUse",
            "tool": "shell",
            "input": {"command": "date"},
            "tool_use_id": "tool-1"
        });

        let payload = parse("PreToolUse", raw).expect("normalize");
        assert!(matches!(
            payload.fragment.body,
            FragmentBody::ToolCall {
                ref name,
                ref input_json,
                tool_use_id: Some(ref tool_use_id),
            } if name == "shell"
                && input_json == "{\"command\":\"date\"}"
                && tool_use_id == "tool-1"
        ));
    }

    #[test]
    fn rejects_mismatched_event_flag() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "Stop",
            "last_assistant_message": "Done."
        });

        let error = parse("UserPromptSubmit", raw).expect_err("mismatch");
        assert!(error.to_string().contains("does not match payload"));
    }

    #[test]
    fn rejects_non_object_json() {
        let error = ClaudeCodeParser::new("Stop")
            .parse(br#"["not","an","object"]"#)
            .expect_err("non-object should fail");

        assert!(
            error
                .to_string()
                .contains("Claude hook payload must be a JSON object")
        );
    }

    #[test]
    fn maps_unknown_event_to_other_fragment() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "SessionStart",
            "extra": true
        });

        let payload = parse("SessionStart", raw).expect("normalize");
        assert_eq!(payload.source.event, "SessionStart");
        assert!(matches!(
            payload.fragment.body,
            FragmentBody::Other { ref label, .. } if label == "SessionStart"
        ));
    }

    #[test]
    fn fragment_id_is_stable_for_same_payload_and_event() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "Stop",
            "last_assistant_message": "Done."
        });

        let first = parse("Stop", raw.clone()).expect("first parse");
        let second = parse("Stop", raw).expect("second parse");

        assert_eq!(first.scope.fragment_id, second.scope.fragment_id);
    }
}
