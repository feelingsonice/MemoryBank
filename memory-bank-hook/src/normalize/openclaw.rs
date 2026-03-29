use super::{parse_object_json, required_non_empty, serialize_json};
use blake3::Hasher;
use memory_bank_protocol::{
    ConversationFragment, ConversationScope, FragmentBody, INGEST_PROTOCOL_VERSION, IngestEnvelope,
    SourceMeta, Terminality,
};
use serde::Deserialize;
use serde_json::Value;

use crate::error::AppError;

pub(super) struct OpenClawParser<'a> {
    event_flag: &'a str,
}

impl<'a> OpenClawParser<'a> {
    pub(super) fn new(event_flag: &'a str) -> Self {
        Self { event_flag }
    }

    pub(super) fn parse(&self, raw: &[u8]) -> Result<Option<IngestEnvelope>, AppError> {
        let raw_json = parse_object_json(raw, "OpenClaw")?;
        let payload = self.parse_payload(raw)?;
        let event = self.resolve_event(&payload)?;
        let fragment = self.build_fragment(&payload, event)?;
        self.build_envelope(&payload, event, fragment, raw_json, raw)
    }

    fn parse_payload(&self, raw: &[u8]) -> Result<OpenClawHookPayload, AppError> {
        Ok(serde_json::from_slice(raw)?)
    }

    fn resolve_event(&self, payload: &OpenClawHookPayload) -> Result<OpenClawEvent, AppError> {
        if let Some(payload_event) = payload.hook_event_name.as_deref()
            && payload_event != self.event_flag
        {
            return Err(AppError::Normalize(format!(
                "CLI event '{}' does not match payload hook_event_name '{}'",
                self.event_flag, payload_event
            )));
        }

        OpenClawEvent::from_name(
            payload
                .hook_event_name
                .as_deref()
                .unwrap_or(self.event_flag),
        )
        .ok_or_else(|| {
            AppError::Normalize(format!(
                "Unsupported OpenClaw hook event '{}'. Supported events: before_model_resolve, before_prompt_build, before_agent_start, before_tool_call, after_tool_call, agent_end",
                payload
                    .hook_event_name
                    .as_deref()
                    .unwrap_or(self.event_flag)
            ))
        })
    }

    fn build_fragment(
        &self,
        payload: &OpenClawHookPayload,
        event: OpenClawEvent,
    ) -> Result<Option<ConversationFragment>, AppError> {
        match event {
            OpenClawEvent::BeforeModelResolve
            | OpenClawEvent::BeforePromptBuild
            | OpenClawEvent::BeforeAgentStart => {
                let Some(prompt) = payload.prompt_text() else {
                    return Ok(None);
                };

                Ok(Some(ConversationFragment {
                    terminality: Terminality::None,
                    body: FragmentBody::UserMessage { text: prompt },
                }))
            }
            OpenClawEvent::BeforeToolCall => Ok(Some(ConversationFragment {
                terminality: Terminality::None,
                body: FragmentBody::ToolCall {
                    name: required_non_empty(
                        payload.tool_name.as_deref(),
                        "before_tool_call payload missing tool_name",
                    )?
                    .to_string(),
                    input_json: self.tool_arguments_json(payload)?,
                    tool_use_id: payload.tool_use_id(),
                },
            })),
            OpenClawEvent::AfterToolCall => Ok(Some(ConversationFragment {
                terminality: Terminality::None,
                body: FragmentBody::ToolResult {
                    name: required_non_empty(
                        payload.tool_name.as_deref(),
                        "after_tool_call payload missing tool_name",
                    )?
                    .to_string(),
                    output_json: self.tool_output_json(payload)?,
                    tool_use_id: payload.tool_use_id(),
                },
            })),
            OpenClawEvent::AgentEnd => {
                let Some(text) = payload.assistant_text() else {
                    return Ok(None);
                };

                Ok(Some(ConversationFragment {
                    terminality: Terminality::Hard,
                    body: FragmentBody::AssistantMessage { text },
                }))
            }
        }
    }

    fn build_envelope(
        &self,
        payload: &OpenClawHookPayload,
        event: OpenClawEvent,
        fragment: Option<ConversationFragment>,
        raw_json: Value,
        raw: &[u8],
    ) -> Result<Option<IngestEnvelope>, AppError> {
        let Some(fragment) = fragment else {
            return Ok(None);
        };

        let conversation_id = required_non_empty(
            payload.session_id(),
            "OpenClaw payload missing required session identifier",
        )?;

        let envelope = IngestEnvelope {
            protocol_version: INGEST_PROTOCOL_VERSION,
            source: SourceMeta {
                agent: "openclaw".to_string(),
                event: event.as_str().to_string(),
            },
            scope: ConversationScope {
                conversation_id: conversation_id.to_string(),
                turn_id: None,
                fragment_id: self.build_fragment_id(
                    conversation_id,
                    payload.fragment_key(),
                    event,
                    raw,
                ),
                sequence_hint: None,
                emitted_at_rfc3339: payload.timestamp.clone(),
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
        fragment_key: &str,
        event: OpenClawEvent,
        raw: &[u8],
    ) -> String {
        let mut hasher = Hasher::new();
        hasher.update(b"openclaw");
        hasher.update(conversation_id.as_bytes());
        hasher.update(event.as_str().as_bytes());
        hasher.update(fragment_key.as_bytes());
        hasher.update(raw);
        format!("openclaw-{}", hasher.finalize().to_hex())
    }

    fn tool_arguments_json(&self, payload: &OpenClawHookPayload) -> Result<String, AppError> {
        match payload.tool_arguments.as_ref() {
            Some(value) => serialize_json(value),
            None => Ok("{}".to_string()),
        }
    }

    fn tool_output_json(&self, payload: &OpenClawHookPayload) -> Result<String, AppError> {
        match payload.tool_output.as_ref() {
            Some(value) => serialize_json(value),
            None => Ok("null".to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenClawEvent {
    BeforeModelResolve,
    BeforePromptBuild,
    BeforeAgentStart,
    BeforeToolCall,
    AfterToolCall,
    AgentEnd,
}

impl OpenClawEvent {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "before_model_resolve" => Some(Self::BeforeModelResolve),
            "before_prompt_build" => Some(Self::BeforePromptBuild),
            "before_agent_start" => Some(Self::BeforeAgentStart),
            "before_tool_call" => Some(Self::BeforeToolCall),
            "after_tool_call" => Some(Self::AfterToolCall),
            "agent_end" => Some(Self::AgentEnd),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::BeforeModelResolve => "before_model_resolve",
            Self::BeforePromptBuild => "before_prompt_build",
            Self::BeforeAgentStart => "before_agent_start",
            Self::BeforeToolCall => "before_tool_call",
            Self::AfterToolCall => "after_tool_call",
            Self::AgentEnd => "agent_end",
        }
    }
}

#[derive(Debug, Deserialize)]
struct OpenClawHookPayload {
    #[serde(
        default,
        alias = "sessionId",
        alias = "conversation_id",
        alias = "conversationId"
    )]
    session_id: Option<String>,
    #[serde(default, alias = "runId")]
    run_id: Option<String>,
    #[serde(default, alias = "eventId")]
    event_id: Option<String>,
    #[serde(default)]
    hook_event_name: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    assistant_text: Option<String>,
    #[serde(default)]
    assistant_message_id: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_arguments: Option<Value>,
    #[serde(default)]
    tool_output: Option<Value>,
    #[serde(
        default,
        alias = "toolCallId",
        alias = "tool_call_id",
        alias = "invocation_id",
        alias = "invocationId"
    )]
    tool_use_id: Option<String>,
}

impl OpenClawHookPayload {
    fn session_id(&self) -> Option<&str> {
        self.session_id
            .as_deref()
            .or(self.run_id.as_deref())
            .filter(|value| !value.trim().is_empty())
    }

    fn prompt_text(&self) -> Option<String> {
        self.prompt
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    }

    fn assistant_text(&self) -> Option<String> {
        self.assistant_text
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    }

    fn tool_use_id(&self) -> Option<String> {
        self.tool_use_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned)
    }

    fn fragment_key(&self) -> &str {
        self.tool_use_id
            .as_deref()
            .or(self.assistant_message_id.as_deref())
            .or(self.event_id.as_deref())
            .or(self.run_id.as_deref())
            .unwrap_or("raw")
    }
}

#[cfg(test)]
mod tests {
    use super::OpenClawParser;
    use memory_bank_protocol::{FragmentBody, IngestEnvelope, Terminality};
    use serde_json::json;

    fn parse(
        event_flag: &str,
        raw: serde_json::Value,
    ) -> Result<Option<IngestEnvelope>, crate::error::AppError> {
        OpenClawParser::new(event_flag).parse(raw.to_string().as_bytes())
    }

    #[test]
    fn maps_before_agent_start_to_user_fragment() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "before_agent_start",
            "prompt": "remember that I use helix"
        });

        let payload = parse("before_agent_start", raw)
            .expect("normalize")
            .expect("payload");

        assert!(matches!(
            payload.fragment.body,
            FragmentBody::UserMessage { ref text } if text == "remember that I use helix"
        ));
    }

    #[test]
    fn maps_before_model_resolve_to_user_fragment() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "before_model_resolve",
            "prompt": "remember that I use helix"
        });

        let payload = parse("before_model_resolve", raw)
            .expect("normalize")
            .expect("payload");

        assert!(matches!(
            payload.fragment.body,
            FragmentBody::UserMessage { ref text } if text == "remember that I use helix"
        ));
    }

    #[test]
    fn maps_before_prompt_build_to_user_fragment() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "before_prompt_build",
            "prompt": "remember that I use helix"
        });

        let payload = parse("before_prompt_build", raw)
            .expect("normalize")
            .expect("payload");

        assert!(matches!(
            payload.fragment.body,
            FragmentBody::UserMessage { ref text } if text == "remember that I use helix"
        ));
    }

    #[test]
    fn skips_empty_prompts() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "before_agent_start",
            "prompt": "   "
        });

        let payload = parse("before_agent_start", raw).expect("normalize");
        assert!(payload.is_none());
    }

    #[test]
    fn maps_before_tool_call_to_tool_call_fragment() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "before_tool_call",
            "tool_name": "read_file",
            "tool_arguments": {"path": "README.md"},
            "tool_call_id": "tool-1"
        });

        let payload = parse("before_tool_call", raw)
            .expect("normalize")
            .expect("payload");

        assert!(matches!(
            payload.fragment.body,
            FragmentBody::ToolCall {
                ref name,
                ref input_json,
                tool_use_id: Some(ref tool_use_id),
            } if name == "read_file"
                && input_json == "{\"path\":\"README.md\"}"
                && tool_use_id == "tool-1"
        ));
    }

    #[test]
    fn maps_after_tool_call_to_tool_result_fragment() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "after_tool_call",
            "tool_name": "read_file",
            "tool_output": {"contents": "hello"},
            "invocation_id": "tool-1"
        });

        let payload = parse("after_tool_call", raw)
            .expect("normalize")
            .expect("payload");

        assert!(matches!(
            payload.fragment.body,
            FragmentBody::ToolResult {
                ref name,
                ref output_json,
                tool_use_id: Some(ref tool_use_id),
            } if name == "read_file"
                && output_json == "{\"contents\":\"hello\"}"
                && tool_use_id == "tool-1"
        ));
    }

    #[test]
    fn maps_agent_end_to_hard_terminal_assistant_message() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "agent_end",
            "assistant_text": "All done.",
            "assistant_message_id": "assistant-1"
        });

        let payload = parse("agent_end", raw)
            .expect("normalize")
            .expect("payload");

        assert_eq!(payload.fragment.terminality, Terminality::Hard);
        assert!(matches!(
            payload.fragment.body,
            FragmentBody::AssistantMessage { ref text } if text == "All done."
        ));
    }

    #[test]
    fn skips_empty_agent_end_messages() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "agent_end",
            "assistant_text": "   "
        });

        let payload = parse("agent_end", raw).expect("normalize");
        assert!(payload.is_none());
    }

    #[test]
    fn fragment_id_is_stable_for_same_payload_and_event() {
        let raw = json!({
            "session_id": "session-1",
            "run_id": "run-1",
            "hook_event_name": "before_agent_start",
            "prompt": "hello"
        });

        let first = parse("before_agent_start", raw.clone())
            .expect("normalize")
            .expect("payload");
        let second = parse("before_agent_start", raw)
            .expect("normalize")
            .expect("payload");

        assert_eq!(first.scope.fragment_id, second.scope.fragment_id);
    }

    #[test]
    fn rejects_mismatched_event_flag() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "agent_end",
            "assistant_text": "done"
        });

        let error = parse("before_agent_start", raw).expect_err("mismatch");
        assert!(error.to_string().contains("does not match payload"));
    }

    #[test]
    fn rejects_non_object_json() {
        let error = OpenClawParser::new("before_agent_start")
            .parse(br#"["not","an","object"]"#)
            .expect_err("non-object should fail");

        assert!(
            error
                .to_string()
                .contains("OpenClaw hook payload must be a JSON object")
        );
    }
}
