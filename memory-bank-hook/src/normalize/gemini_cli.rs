use super::{deserialize_optional_bool, parse_object_json, required_non_empty, serialize_json};
use blake3::Hasher;
use memory_bank_protocol::{
    ConversationFragment, ConversationScope, FragmentBody, INGEST_PROTOCOL_VERSION, IngestEnvelope,
    SourceMeta, Terminality,
};
use serde::Deserialize;
use serde_json::Value;

use crate::error::AppError;

pub(super) struct GeminiCliParser<'a> {
    event_flag: &'a str,
}

impl<'a> GeminiCliParser<'a> {
    pub(super) fn new(event_flag: &'a str) -> Self {
        Self { event_flag }
    }

    pub(super) fn parse(&self, raw: &[u8]) -> Result<IngestEnvelope, AppError> {
        let raw_json = parse_object_json(raw, "Gemini")?;
        let payload = self.parse_payload(raw)?;
        let event = self.resolve_event(&payload)?;
        let fragment = self.build_fragment(&payload, event)?;
        self.build_envelope(&payload, event, fragment, raw_json, raw)
    }

    fn parse_payload(&self, raw: &[u8]) -> Result<GeminiHookPayload, AppError> {
        Ok(serde_json::from_slice(raw)?)
    }

    fn resolve_event(&self, payload: &GeminiHookPayload) -> Result<GeminiEvent, AppError> {
        let payload_event = required_non_empty(
            payload.hook_event_name.as_deref(),
            "Gemini payload missing required string 'hook_event_name'",
        )?;

        if payload_event != self.event_flag {
            return Err(AppError::Normalize(format!(
                "CLI event '{}' does not match payload hook_event_name '{}'",
                self.event_flag, payload_event
            )));
        }

        GeminiEvent::from_name(payload_event).ok_or_else(|| {
            AppError::Normalize(format!(
                "Unsupported Gemini hook event '{payload_event}'. Supported events: BeforeAgent, BeforeTool, AfterTool, AfterAgent"
            ))
        })
    }

    fn build_fragment(
        &self,
        payload: &GeminiHookPayload,
        event: GeminiEvent,
    ) -> Result<ConversationFragment, AppError> {
        match event {
            GeminiEvent::BeforeAgent => Ok(ConversationFragment {
                terminality: Terminality::None,
                body: FragmentBody::UserMessage {
                    text: required_non_empty(
                        payload.prompt.as_deref(),
                        "BeforeAgent payload missing prompt",
                    )?
                    .to_string(),
                },
            }),
            GeminiEvent::BeforeTool => Ok(ConversationFragment {
                terminality: Terminality::None,
                body: FragmentBody::ToolCall {
                    name: required_non_empty(
                        payload.tool_name.as_deref(),
                        "BeforeTool payload missing tool_name",
                    )?
                    .to_string(),
                    input_json: serialize_json(required_json(
                        payload.tool_input.as_ref(),
                        "BeforeTool payload missing tool_input",
                    )?)?,
                    tool_use_id: None,
                },
            }),
            GeminiEvent::AfterTool => Ok(ConversationFragment {
                terminality: Terminality::None,
                body: FragmentBody::ToolResult {
                    name: required_non_empty(
                        payload.tool_name.as_deref(),
                        "AfterTool payload missing tool_name",
                    )?
                    .to_string(),
                    output_json: serialize_json(required_json(
                        payload.tool_response.as_ref(),
                        "AfterTool payload missing tool_response",
                    )?)?,
                    tool_use_id: None,
                },
            }),
            GeminiEvent::AfterAgent => Ok(ConversationFragment {
                terminality: if payload.stop_hook_active.unwrap_or(false) {
                    Terminality::Soft
                } else {
                    Terminality::Hard
                },
                body: FragmentBody::AssistantMessage {
                    text: required_non_empty(
                        payload.prompt_response.as_deref(),
                        "AfterAgent payload missing prompt_response",
                    )?
                    .to_string(),
                },
            }),
        }
    }

    fn build_envelope(
        &self,
        payload: &GeminiHookPayload,
        event: GeminiEvent,
        fragment: ConversationFragment,
        raw_json: Value,
        raw: &[u8],
    ) -> Result<IngestEnvelope, AppError> {
        let conversation_id = required_non_empty(
            payload.session_id.as_deref(),
            "Gemini payload missing required string 'session_id'",
        )?;

        let envelope = IngestEnvelope {
            protocol_version: INGEST_PROTOCOL_VERSION,
            source: SourceMeta {
                agent: "gemini-cli".to_string(),
                event: event.as_str().to_string(),
            },
            scope: ConversationScope {
                conversation_id: conversation_id.to_string(),
                turn_id: None,
                fragment_id: self.build_fragment_id(conversation_id, event, raw),
                sequence_hint: None,
                emitted_at_rfc3339: payload.timestamp.clone(),
            },
            fragment,
            raw: raw_json,
        };

        envelope.validate().map_err(AppError::Normalize)?;
        Ok(envelope)
    }

    fn build_fragment_id(&self, conversation_id: &str, event: GeminiEvent, raw: &[u8]) -> String {
        let mut hasher = Hasher::new();
        hasher.update(b"gemini-cli");
        hasher.update(conversation_id.as_bytes());
        hasher.update(event.as_str().as_bytes());
        hasher.update(raw);
        format!("gemini-cli-{}", hasher.finalize().to_hex())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GeminiEvent {
    BeforeAgent,
    BeforeTool,
    AfterTool,
    AfterAgent,
}

impl GeminiEvent {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "BeforeAgent" => Some(Self::BeforeAgent),
            "BeforeTool" => Some(Self::BeforeTool),
            "AfterTool" => Some(Self::AfterTool),
            "AfterAgent" => Some(Self::AfterAgent),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::BeforeAgent => "BeforeAgent",
            Self::BeforeTool => "BeforeTool",
            Self::AfterTool => "AfterTool",
            Self::AfterAgent => "AfterAgent",
        }
    }
}

#[derive(Debug, Deserialize)]
struct GeminiHookPayload {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    hook_event_name: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_input: Option<Value>,
    #[serde(default)]
    tool_response: Option<Value>,
    #[serde(default)]
    prompt_response: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_bool")]
    stop_hook_active: Option<bool>,
}

fn required_json<'a>(value: Option<&'a Value>, error: &str) -> Result<&'a Value, AppError> {
    value.ok_or_else(|| AppError::Normalize(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::GeminiCliParser;
    use memory_bank_protocol::{FragmentBody, IngestEnvelope, Terminality};
    use serde_json::json;

    fn parse(
        event_flag: &str,
        raw: serde_json::Value,
    ) -> Result<IngestEnvelope, crate::error::AppError> {
        GeminiCliParser::new(event_flag).parse(raw.to_string().as_bytes())
    }

    #[test]
    fn maps_before_agent_to_user_message() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "BeforeAgent",
            "prompt": "remember this"
        });

        let payload = parse("BeforeAgent", raw).expect("normalize");
        assert_eq!(payload.source.event, "BeforeAgent");
        assert_eq!(payload.fragment.terminality, Terminality::None);
        assert!(matches!(
            payload.fragment.body,
            FragmentBody::UserMessage { ref text } if text == "remember this"
        ));
    }

    #[test]
    fn maps_before_tool_to_tool_call() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "BeforeTool",
            "tool_name": "run_shell_command",
            "tool_input": {
                "command": "pwd",
                "description": "show cwd"
            }
        });

        let expected_input = serde_json::to_string(&raw["tool_input"]).expect("serialize");
        let payload = parse("BeforeTool", raw).expect("normalize");

        assert!(matches!(
            payload.fragment.body,
            FragmentBody::ToolCall {
                ref name,
                ref input_json,
                tool_use_id: None,
            } if name == "run_shell_command" && input_json == &expected_input
        ));
    }

    #[test]
    fn maps_after_tool_to_tool_result() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "AfterTool",
            "tool_name": "run_shell_command",
            "tool_response": {
                "llmContent": "pwd => /tmp",
                "returnDisplay": "/tmp\n",
                "error": null
            }
        });

        let expected_output = serde_json::to_string(&raw["tool_response"]).expect("serialize");
        let payload = parse("AfterTool", raw).expect("normalize");

        assert!(matches!(
            payload.fragment.body,
            FragmentBody::ToolResult {
                ref name,
                ref output_json,
                tool_use_id: None,
            } if name == "run_shell_command" && output_json == &expected_output
        ));
    }

    #[test]
    fn maps_after_agent_to_hard_terminal_assistant_message() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "AfterAgent",
            "prompt_response": "All done.",
            "stop_hook_active": false
        });

        let payload = parse("AfterAgent", raw).expect("normalize");
        assert_eq!(payload.fragment.terminality, Terminality::Hard);
        assert!(matches!(
            payload.fragment.body,
            FragmentBody::AssistantMessage { ref text } if text == "All done."
        ));
    }

    #[test]
    fn maps_after_agent_to_soft_terminal_when_retry_active() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "AfterAgent",
            "prompt_response": "Retry me.",
            "stop_hook_active": true
        });

        let payload = parse("AfterAgent", raw).expect("normalize");
        assert_eq!(payload.fragment.terminality, Terminality::Soft);
    }

    #[test]
    fn rejects_mismatched_event_flag() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "AfterAgent",
            "prompt_response": "done"
        });

        let error = parse("BeforeAgent", raw).expect_err("mismatch");
        assert!(error.to_string().contains("does not match payload"));
    }

    #[test]
    fn rejects_non_object_json() {
        let error = GeminiCliParser::new("BeforeAgent")
            .parse(br#"["not","an","object"]"#)
            .expect_err("non-object should fail");

        assert!(
            error
                .to_string()
                .contains("Gemini hook payload must be a JSON object")
        );
    }

    #[test]
    fn rejects_missing_session_id() {
        let raw = json!({
            "hook_event_name": "BeforeAgent",
            "prompt": "hello"
        });

        let error = parse("BeforeAgent", raw).expect_err("missing session_id");
        assert!(error.to_string().contains("session_id"));
    }

    #[test]
    fn rejects_missing_prompt_for_before_agent() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "BeforeAgent"
        });

        let error = parse("BeforeAgent", raw).expect_err("missing prompt");
        assert!(
            error
                .to_string()
                .contains("BeforeAgent payload missing prompt")
        );
    }

    #[test]
    fn rejects_missing_tool_name() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "BeforeTool",
            "tool_input": {"command": "pwd"}
        });

        let error = parse("BeforeTool", raw).expect_err("missing tool_name");
        assert!(
            error
                .to_string()
                .contains("BeforeTool payload missing tool_name")
        );
    }

    #[test]
    fn rejects_missing_tool_input() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "BeforeTool",
            "tool_name": "run_shell_command"
        });

        let error = parse("BeforeTool", raw).expect_err("missing tool_input");
        assert!(
            error
                .to_string()
                .contains("BeforeTool payload missing tool_input")
        );
    }

    #[test]
    fn rejects_missing_tool_response() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "AfterTool",
            "tool_name": "run_shell_command"
        });

        let error = parse("AfterTool", raw).expect_err("missing tool_response");
        assert!(
            error
                .to_string()
                .contains("AfterTool payload missing tool_response")
        );
    }

    #[test]
    fn rejects_missing_prompt_response() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "AfterAgent"
        });

        let error = parse("AfterAgent", raw).expect_err("missing prompt_response");
        assert!(
            error
                .to_string()
                .contains("AfterAgent payload missing prompt_response")
        );
    }

    #[test]
    fn rejects_unsupported_gemini_event() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "SessionStart"
        });

        let error = parse("SessionStart", raw).expect_err("unsupported event");
        assert!(error.to_string().contains("Unsupported Gemini hook event"));
    }

    #[test]
    fn fragment_id_is_stable_for_same_payload_and_event() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "AfterAgent",
            "prompt_response": "Done."
        });

        let first = parse("AfterAgent", raw.clone()).expect("first parse");
        let second = parse("AfterAgent", raw).expect("second parse");

        assert_eq!(first.scope.fragment_id, second.scope.fragment_id);
    }
}
