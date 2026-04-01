use super::{deserialize_optional_bool, parse_object_json, required_non_empty, serialize_json};
use blake3::Hasher;
use memory_bank_protocol::{
    ConversationFragment, ConversationScope, FragmentBody, INGEST_PROTOCOL_VERSION, IngestEnvelope,
    SourceMeta, Terminality,
};
use serde::Deserialize;
use serde_json::Value;

use crate::error::AppError;

pub(super) struct CodexParser<'a> {
    event_flag: &'a str,
}

impl<'a> CodexParser<'a> {
    pub(super) fn new(event_flag: &'a str) -> Self {
        Self { event_flag }
    }

    pub(super) fn parse(&self, raw: &[u8]) -> Result<Option<IngestEnvelope>, AppError> {
        let raw_json = parse_object_json(raw, "Codex")?;
        let payload = self.parse_payload(raw)?;
        let event = self.resolve_event(&payload)?;
        let fragment = self.build_fragment(&payload, &event, &raw_json)?;
        self.build_envelope(&payload, &event, fragment, raw_json, raw)
    }

    fn parse_payload(&self, raw: &[u8]) -> Result<CodexHookPayload, AppError> {
        Ok(serde_json::from_slice(raw)?)
    }

    fn resolve_event(&self, payload: &CodexHookPayload) -> Result<CodexEvent, AppError> {
        let payload_event = required_non_empty(
            payload.hook_event_name.as_deref(),
            "Codex payload missing required string 'hook_event_name'",
        )?;

        if payload_event != self.event_flag {
            return Err(AppError::Normalize(format!(
                "CLI event '{}' does not match payload hook_event_name '{}'",
                self.event_flag, payload_event
            )));
        }

        CodexEvent::from_name(payload_event).ok_or_else(|| {
            AppError::Normalize(format!(
                "Unsupported Codex hook event '{payload_event}'. Supported events: UserPromptSubmit, PreToolUse, PostToolUse, Stop"
            ))
        })
    }

    fn build_fragment(
        &self,
        payload: &CodexHookPayload,
        event: &CodexEvent,
        raw_json: &Value,
    ) -> Result<Option<ConversationFragment>, AppError> {
        let terminality = if payload.stop_hook_active.unwrap_or(false) {
            Terminality::Soft
        } else {
            Terminality::Hard
        };

        match event {
            CodexEvent::UserPromptSubmit => {
                let Some(prompt) = optional_non_empty(payload.prompt.as_deref()) else {
                    return Ok(None);
                };

                Ok(Some(ConversationFragment {
                    terminality: Terminality::None,
                    body: FragmentBody::UserMessage {
                        text: prompt.to_string(),
                    },
                }))
            }
            CodexEvent::PreToolUse => {
                let Some(tool_name) = optional_non_empty(payload.tool_name.as_deref()) else {
                    return Ok(None);
                };
                let Some(tool_input) = payload.tool_input.as_ref() else {
                    return Ok(None);
                };

                Ok(Some(ConversationFragment {
                    terminality: Terminality::None,
                    body: FragmentBody::ToolCall {
                        name: tool_name.to_string(),
                        input_json: serialize_json(tool_input)?,
                        tool_use_id: optional_non_empty(payload.tool_use_id.as_deref())
                            .map(ToOwned::to_owned),
                    },
                }))
            }
            CodexEvent::PostToolUse => {
                let Some(tool_name) = optional_non_empty(payload.tool_name.as_deref()) else {
                    return Ok(None);
                };
                let Some(tool_response) = payload.tool_response.as_ref() else {
                    return Ok(None);
                };

                Ok(Some(ConversationFragment {
                    terminality: Terminality::None,
                    body: FragmentBody::ToolResult {
                        name: tool_name.to_string(),
                        output_json: serialize_json(tool_response)?,
                        tool_use_id: optional_non_empty(payload.tool_use_id.as_deref())
                            .map(ToOwned::to_owned),
                    },
                }))
            }
            CodexEvent::Stop => {
                let body = match optional_non_empty(payload.last_assistant_message.as_deref()) {
                    Some(text) => FragmentBody::AssistantMessage {
                        text: text.to_string(),
                    },
                    None => FragmentBody::Other {
                        label: event.as_str().to_string(),
                        payload: raw_json.clone(),
                    },
                };

                Ok(Some(ConversationFragment { terminality, body }))
            }
        }
    }

    fn build_envelope(
        &self,
        payload: &CodexHookPayload,
        event: &CodexEvent,
        fragment: Option<ConversationFragment>,
        raw_json: Value,
        raw: &[u8],
    ) -> Result<Option<IngestEnvelope>, AppError> {
        let Some(fragment) = fragment else {
            return Ok(None);
        };

        let conversation_id = required_non_empty(
            payload.session_id.as_deref(),
            "Codex payload missing required string 'session_id'",
        )?;
        let turn_id = optional_non_empty(payload.turn_id.as_deref()).map(ToOwned::to_owned);

        let envelope = IngestEnvelope {
            protocol_version: INGEST_PROTOCOL_VERSION,
            source: SourceMeta {
                agent: "codex".to_string(),
                event: event.as_str().to_string(),
            },
            scope: ConversationScope {
                conversation_id: conversation_id.to_string(),
                turn_id: turn_id.clone(),
                fragment_id: self.build_fragment_id(
                    conversation_id,
                    turn_id.as_deref(),
                    optional_non_empty(payload.tool_use_id.as_deref()),
                    event,
                    raw,
                ),
                sequence_hint: None,
                emitted_at_rfc3339: None,
            },
            fragment,
            raw: raw_json,
        };

        envelope.validate().map_err(AppError::Normalize)?;
        Ok(Some(envelope))
    }

    fn build_fragment_id(
        &self,
        conversation_id: &str,
        turn_id: Option<&str>,
        tool_use_id: Option<&str>,
        event: &CodexEvent,
        raw: &[u8],
    ) -> String {
        let mut hasher = Hasher::new();
        hasher.update(b"codex");
        hasher.update(conversation_id.as_bytes());
        hasher.update(b"\0");
        if let Some(turn_id) = turn_id {
            hasher.update(turn_id.as_bytes());
        }
        hasher.update(b"\0");
        hasher.update(event.as_str().as_bytes());
        hasher.update(b"\0");
        if let Some(tool_use_id) = tool_use_id {
            hasher.update(tool_use_id.as_bytes());
        }
        hasher.update(b"\0");
        hasher.update(raw);
        format!("codex-{}", hasher.finalize().to_hex())
    }
}

fn optional_non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CodexEvent {
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    Stop,
}

impl CodexEvent {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "UserPromptSubmit" => Some(Self::UserPromptSubmit),
            "PreToolUse" => Some(Self::PreToolUse),
            "PostToolUse" => Some(Self::PostToolUse),
            "Stop" => Some(Self::Stop),
            _ => None,
        }
    }

    fn as_str(&self) -> &str {
        match self {
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::Stop => "Stop",
        }
    }
}

#[derive(Debug, Deserialize)]
struct CodexHookPayload {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    turn_id: Option<String>,
    #[serde(default)]
    hook_event_name: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_use_id: Option<String>,
    #[serde(default)]
    tool_input: Option<Value>,
    #[serde(default)]
    tool_response: Option<Value>,
    #[serde(default)]
    last_assistant_message: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_bool")]
    stop_hook_active: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::CodexParser;
    use memory_bank_protocol::{FragmentBody, IngestEnvelope, Terminality};
    use serde_json::json;

    fn parse(
        event_flag: &str,
        raw: serde_json::Value,
    ) -> Result<Option<IngestEnvelope>, crate::error::AppError> {
        CodexParser::new(event_flag).parse(raw.to_string().as_bytes())
    }

    #[test]
    fn maps_user_prompt_submit_with_turn_id() {
        let raw = json!({
            "session_id": "session-1",
            "turn_id": "turn-1",
            "hook_event_name": "UserPromptSubmit",
            "prompt": "remember this"
        });

        let payload = parse("UserPromptSubmit", raw)
            .expect("normalize")
            .expect("payload");

        assert_eq!(payload.scope.turn_id.as_deref(), Some("turn-1"));
        assert!(matches!(
            payload.fragment.body,
            FragmentBody::UserMessage { ref text } if text == "remember this"
        ));
    }

    #[test]
    fn skips_user_prompt_submit_without_prompt() {
        let raw = json!({
            "session_id": "session-1",
            "turn_id": "turn-1",
            "hook_event_name": "UserPromptSubmit"
        });

        let payload = parse("UserPromptSubmit", raw).expect("normalize");
        assert!(payload.is_none());
    }

    #[test]
    fn maps_pre_tool_use_without_turn_id() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_use_id": "tool-1",
            "tool_input": {"command": "pwd"}
        });

        let payload = parse("PreToolUse", raw)
            .expect("normalize")
            .expect("payload");

        assert_eq!(payload.scope.turn_id, None);
        assert!(matches!(
            payload.fragment.body,
            FragmentBody::ToolCall {
                ref name,
                ref input_json,
                tool_use_id: Some(ref tool_use_id),
            } if name == "Bash"
                && input_json == "{\"command\":\"pwd\"}"
                && tool_use_id == "tool-1"
        ));
    }

    #[test]
    fn skips_pre_tool_use_without_tool_input() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash"
        });

        let payload = parse("PreToolUse", raw).expect("normalize");
        assert!(payload.is_none());
    }

    #[test]
    fn maps_post_tool_use_with_string_tool_response() {
        let raw = json!({
            "session_id": "session-1",
            "turn_id": "turn-1",
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "tool_use_id": "tool-1",
            "tool_response": "output"
        });

        let payload = parse("PostToolUse", raw)
            .expect("normalize")
            .expect("payload");

        assert!(matches!(
            payload.fragment.body,
            FragmentBody::ToolResult {
                ref name,
                ref output_json,
                tool_use_id: Some(ref tool_use_id),
            } if name == "Bash"
                && output_json == "\"output\""
                && tool_use_id == "tool-1"
        ));
    }

    #[test]
    fn maps_post_tool_use_with_object_tool_response() {
        let raw = json!({
            "session_id": "session-1",
            "turn_id": "turn-1",
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "tool_response": {"exit_code": 0}
        });

        let payload = parse("PostToolUse", raw)
            .expect("normalize")
            .expect("payload");

        assert!(matches!(
            payload.fragment.body,
            FragmentBody::ToolResult {
                ref output_json, ..
            } if output_json == "{\"exit_code\":0}"
        ));
    }

    #[test]
    fn skips_post_tool_use_without_tool_response() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash"
        });

        let payload = parse("PostToolUse", raw).expect("normalize");
        assert!(payload.is_none());
    }

    #[test]
    fn maps_stop_to_soft_terminal_assistant_message() {
        let raw = json!({
            "session_id": "session-1",
            "turn_id": "turn-1",
            "hook_event_name": "Stop",
            "stop_hook_active": true,
            "last_assistant_message": "Need one more pass."
        });

        let payload = parse("Stop", raw).expect("normalize").expect("payload");

        assert_eq!(payload.fragment.terminality, Terminality::Soft);
        assert!(matches!(
            payload.fragment.body,
            FragmentBody::AssistantMessage { ref text } if text == "Need one more pass."
        ));
    }

    #[test]
    fn maps_stop_without_assistant_message_to_other_fragment() {
        let raw = json!({
            "session_id": "session-1",
            "turn_id": "turn-1",
            "hook_event_name": "Stop",
            "last_assistant_message": null
        });

        let payload = parse("Stop", raw).expect("normalize").expect("payload");

        assert_eq!(payload.fragment.terminality, Terminality::Hard);
        assert!(matches!(
            payload.fragment.body,
            FragmentBody::Other { ref label, .. } if label == "Stop"
        ));
    }

    #[test]
    fn rejects_unsupported_event() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "SessionStart"
        });

        let error = parse("SessionStart", raw).expect_err("unsupported event");
        assert!(error.to_string().contains("Unsupported Codex hook event"));
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
}
