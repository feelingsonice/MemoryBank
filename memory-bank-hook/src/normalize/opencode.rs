use super::{parse_object_json, required_non_empty, serialize_json};
use blake3::Hasher;
use memory_bank_protocol::{
    ConversationFragment, ConversationScope, FragmentBody, INGEST_PROTOCOL_VERSION, IngestEnvelope,
    SourceMeta, Terminality,
};
use serde::Deserialize;
use serde_json::Value;

use crate::error::AppError;

pub(super) struct OpenCodeParser<'a> {
    event_flag: &'a str,
}

impl<'a> OpenCodeParser<'a> {
    pub(super) fn new(event_flag: &'a str) -> Self {
        Self { event_flag }
    }

    pub(super) fn parse(&self, raw: &[u8]) -> Result<Option<IngestEnvelope>, AppError> {
        let raw_json = parse_object_json(raw, "OpenCode")?;
        let payload = self.parse_payload(raw)?;
        let event = self.resolve_event(&payload)?;
        let fragment = self.build_fragment(&payload, event)?;
        self.build_envelope(&payload, event, fragment, raw_json, raw)
    }

    fn parse_payload(&self, raw: &[u8]) -> Result<OpenCodeHookPayload, AppError> {
        Ok(serde_json::from_slice(raw)?)
    }

    fn resolve_event(&self, payload: &OpenCodeHookPayload) -> Result<OpenCodeEvent, AppError> {
        if let Some(payload_event) = payload.hook_event_name.as_deref()
            && payload_event != self.event_flag
        {
            return Err(AppError::Normalize(format!(
                "CLI event '{}' does not match payload hook_event_name '{}'",
                self.event_flag, payload_event
            )));
        }

        OpenCodeEvent::from_name(
            payload
                .hook_event_name
                .as_deref()
                .unwrap_or(self.event_flag),
        )
        .ok_or_else(|| {
            AppError::Normalize(format!(
                "Unsupported OpenCode hook event '{}'. Supported events: message.updated, tool.execute.before, tool.execute.after, session.idle",
                payload
                    .hook_event_name
                    .as_deref()
                    .unwrap_or(self.event_flag)
            ))
        })
    }

    fn build_fragment(
        &self,
        payload: &OpenCodeHookPayload,
        event: OpenCodeEvent,
    ) -> Result<Option<ConversationFragment>, AppError> {
        match event {
            OpenCodeEvent::MessageUpdated => {
                if !payload.is_memory_worthy_user_message() {
                    return Ok(None);
                }

                let text = payload.user_message_text();
                if text.trim().is_empty() {
                    return Ok(None);
                }

                Ok(Some(ConversationFragment {
                    terminality: Terminality::None,
                    body: FragmentBody::UserMessage { text },
                }))
            }
            OpenCodeEvent::ToolExecuteBefore => Ok(Some(ConversationFragment {
                terminality: Terminality::None,
                body: FragmentBody::ToolCall {
                    name: required_non_empty(
                        payload.tool_name.as_deref(),
                        "tool.execute.before payload missing tool_name",
                    )?
                    .to_string(),
                    input_json: self.tool_arguments_json(payload)?,
                    tool_use_id: payload.part_id.clone(),
                },
            })),
            OpenCodeEvent::ToolExecuteAfter => Ok(Some(ConversationFragment {
                terminality: Terminality::None,
                body: FragmentBody::ToolResult {
                    name: required_non_empty(
                        payload.tool_name.as_deref(),
                        "tool.execute.after payload missing tool_name",
                    )?
                    .to_string(),
                    output_json: self.tool_output_json(payload)?,
                    tool_use_id: payload.part_id.clone(),
                },
            })),
            OpenCodeEvent::SessionIdle => {
                let text = payload
                    .assistant_text
                    .as_deref()
                    .filter(|value| !value.trim().is_empty());

                let Some(text) = text else {
                    return Ok(None);
                };

                Ok(Some(ConversationFragment {
                    terminality: Terminality::Hard,
                    body: FragmentBody::AssistantMessage {
                        text: text.to_string(),
                    },
                }))
            }
        }
    }

    fn build_envelope(
        &self,
        payload: &OpenCodeHookPayload,
        event: OpenCodeEvent,
        fragment: Option<ConversationFragment>,
        raw_json: Value,
        raw: &[u8],
    ) -> Result<Option<IngestEnvelope>, AppError> {
        let Some(fragment) = fragment else {
            return Ok(None);
        };

        let conversation_id = required_non_empty(
            payload.session_id.as_deref(),
            "OpenCode payload missing required string 'session_id'",
        )?;

        let envelope = IngestEnvelope {
            protocol_version: INGEST_PROTOCOL_VERSION,
            source: SourceMeta {
                agent: "opencode".to_string(),
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
        event: OpenCodeEvent,
        raw: &[u8],
    ) -> String {
        let mut hasher = Hasher::new();
        hasher.update(b"opencode");
        hasher.update(conversation_id.as_bytes());
        hasher.update(event.as_str().as_bytes());
        hasher.update(fragment_key.as_bytes());
        hasher.update(raw);
        format!("opencode-{}", hasher.finalize().to_hex())
    }

    fn tool_arguments_json(&self, payload: &OpenCodeHookPayload) -> Result<String, AppError> {
        match payload.tool_arguments.as_ref() {
            Some(value) => serialize_json(value),
            None => Ok("{}".to_string()),
        }
    }

    fn tool_output_json(&self, payload: &OpenCodeHookPayload) -> Result<String, AppError> {
        match payload.tool_output.as_ref() {
            Some(value) => serialize_json(value),
            None => Ok("null".to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenCodeEvent {
    MessageUpdated,
    ToolExecuteBefore,
    ToolExecuteAfter,
    SessionIdle,
}

impl OpenCodeEvent {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "message.updated" => Some(Self::MessageUpdated),
            "tool.execute.before" => Some(Self::ToolExecuteBefore),
            "tool.execute.after" => Some(Self::ToolExecuteAfter),
            "session.idle" => Some(Self::SessionIdle),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::MessageUpdated => "message.updated",
            Self::ToolExecuteBefore => "tool.execute.before",
            Self::ToolExecuteAfter => "tool.execute.after",
            Self::SessionIdle => "session.idle",
        }
    }
}

#[derive(Debug, Deserialize)]
struct OpenCodeHookPayload {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    hook_event_name: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    message_id: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    summary: Option<bool>,
    #[serde(default)]
    reverted: Option<bool>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    parts: Vec<OpenCodeMessagePart>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_arguments: Option<Value>,
    #[serde(default)]
    tool_output: Option<Value>,
    #[serde(default)]
    part_id: Option<String>,
    #[serde(default)]
    assistant_text: Option<String>,
}

impl OpenCodeHookPayload {
    fn is_memory_worthy_user_message(&self) -> bool {
        matches!(self.role.as_deref(), Some("user"))
            && !self.summary.unwrap_or(false)
            && !self.reverted.unwrap_or(false)
    }

    fn user_message_text(&self) -> String {
        if let Some(text) = self.text.as_deref()
            && !text.trim().is_empty()
        {
            return text.to_string();
        }

        let mut text = String::new();
        for part in &self.parts {
            if part.reverted.unwrap_or(false) {
                continue;
            }

            let part_type = part.part_type.as_deref().unwrap_or("text");
            if !matches!(part_type, "text" | "input_text") {
                continue;
            }

            if let Some(part_text) = part.text.as_deref() {
                text.push_str(part_text);
            }
        }

        text
    }

    fn fragment_key(&self) -> &str {
        self.part_id
            .as_deref()
            .or(self.message_id.as_deref())
            .unwrap_or("raw")
    }
}

#[derive(Debug, Deserialize)]
struct OpenCodeMessagePart {
    #[serde(default, alias = "type")]
    part_type: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    reverted: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::OpenCodeParser;
    use memory_bank_protocol::{FragmentBody, IngestEnvelope, Terminality};
    use serde_json::json;

    fn parse(
        event_flag: &str,
        raw: serde_json::Value,
    ) -> Result<Option<IngestEnvelope>, crate::error::AppError> {
        OpenCodeParser::new(event_flag).parse(raw.to_string().as_bytes())
    }

    #[test]
    fn maps_user_message_updates_to_user_fragments() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "message.updated",
            "message_id": "msg-1",
            "role": "user",
            "parts": [
                {"type": "text", "text": "remember "},
                {"type": "text", "text": "this"}
            ]
        });

        let payload = parse("message.updated", raw)
            .expect("normalize")
            .expect("payload should not be skipped");

        assert!(matches!(
            payload.fragment.body,
            FragmentBody::UserMessage { ref text } if text == "remember this"
        ));
    }

    #[test]
    fn ignores_reverted_message_parts_when_building_user_text() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "message.updated",
            "message_id": "msg-1",
            "role": "user",
            "parts": [
                {"type": "text", "text": "keep "},
                {"type": "text", "text": "discard", "reverted": true},
                {"type": "text", "text": "this"}
            ]
        });

        let payload = parse("message.updated", raw)
            .expect("normalize")
            .expect("payload should not be skipped");

        assert!(matches!(
            payload.fragment.body,
            FragmentBody::UserMessage { ref text } if text == "keep this"
        ));
    }

    #[test]
    fn skips_assistant_message_updates() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "message.updated",
            "message_id": "msg-1",
            "role": "assistant",
            "parts": [
                {"type": "text", "text": "hello"}
            ]
        });

        let payload = parse("message.updated", raw).expect("normalize");
        assert!(payload.is_none());
    }

    #[test]
    fn skips_summary_messages() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "message.updated",
            "message_id": "msg-1",
            "role": "user",
            "summary": true,
            "parts": [
                {"type": "text", "text": "summary"}
            ]
        });

        let payload = parse("message.updated", raw).expect("normalize");
        assert!(payload.is_none());
    }

    #[test]
    fn skips_reverted_messages() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "message.updated",
            "message_id": "msg-1",
            "role": "user",
            "reverted": true,
            "parts": [
                {"type": "text", "text": "hello"}
            ]
        });

        let payload = parse("message.updated", raw).expect("normalize");
        assert!(payload.is_none());
    }

    #[test]
    fn skips_empty_user_messages() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "message.updated",
            "message_id": "msg-1",
            "role": "user",
            "parts": [
                {"type": "text", "text": "   "}
            ]
        });

        let payload = parse("message.updated", raw).expect("normalize");
        assert!(payload.is_none());
    }

    #[test]
    fn maps_tool_execute_before_to_tool_call() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "tool.execute.before",
            "part_id": "part-1",
            "tool_name": "bash",
            "tool_arguments": {
                "command": "pwd"
            }
        });

        let payload = parse("tool.execute.before", raw)
            .expect("normalize")
            .expect("payload should not be skipped");

        assert!(matches!(
            payload.fragment.body,
            FragmentBody::ToolCall {
                ref name,
                ref input_json,
                tool_use_id: Some(ref tool_use_id),
            } if name == "bash" && input_json == "{\"command\":\"pwd\"}" && tool_use_id == "part-1"
        ));
    }

    #[test]
    fn maps_tool_execute_after_to_tool_result() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "tool.execute.after",
            "part_id": "part-1",
            "tool_name": "bash",
            "tool_output": {
                "stdout": "/tmp\n",
                "exitCode": 0
            }
        });

        let expected_output = serde_json::to_string(&raw["tool_output"]).expect("serialize");
        let payload = parse("tool.execute.after", raw)
            .expect("normalize")
            .expect("payload should not be skipped");

        assert!(matches!(
            payload.fragment.body,
            FragmentBody::ToolResult {
                ref name,
                ref output_json,
                tool_use_id: Some(ref tool_use_id),
            } if name == "bash"
                && output_json == &expected_output
                && tool_use_id == "part-1"
        ));
    }

    #[test]
    fn maps_session_idle_to_hard_terminal_assistant_message() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "session.idle",
            "message_id": "msg-2",
            "assistant_text": "All done."
        });

        let payload = parse("session.idle", raw)
            .expect("normalize")
            .expect("payload should not be skipped");

        assert_eq!(payload.fragment.terminality, Terminality::Hard);
        assert!(matches!(
            payload.fragment.body,
            FragmentBody::AssistantMessage { ref text } if text == "All done."
        ));
    }

    #[test]
    fn skips_empty_assistant_messages_on_session_idle() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "session.idle",
            "message_id": "msg-2",
            "assistant_text": "   "
        });

        let payload = parse("session.idle", raw).expect("normalize");
        assert!(payload.is_none());
    }

    #[test]
    fn rejects_mismatched_event_flag() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "session.idle",
            "assistant_text": "done"
        });

        let error = parse("message.updated", raw).expect_err("mismatch");
        assert!(error.to_string().contains("does not match payload"));
    }

    #[test]
    fn rejects_non_object_json() {
        let error = OpenCodeParser::new("message.updated")
            .parse(br#"["not","an","object"]"#)
            .expect_err("non-object should fail");

        assert!(
            error
                .to_string()
                .contains("OpenCode hook payload must be a JSON object")
        );
    }

    #[test]
    fn rejects_missing_session_id_for_tool_events() {
        let raw = json!({
            "hook_event_name": "tool.execute.before",
            "tool_name": "bash"
        });

        let error = parse("tool.execute.before", raw).expect_err("missing session_id");
        assert!(error.to_string().contains("session_id"));
    }

    #[test]
    fn rejects_unsupported_event() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "session.created"
        });

        let error = parse("session.created", raw).expect_err("unsupported event");
        assert!(
            error
                .to_string()
                .contains("Unsupported OpenCode hook event")
        );
    }

    #[test]
    fn fragment_id_is_stable_for_same_payload_and_event() {
        let raw = json!({
            "session_id": "session-1",
            "hook_event_name": "session.idle",
            "message_id": "msg-2",
            "assistant_text": "Done."
        });

        let first = parse("session.idle", raw.clone())
            .expect("first parse")
            .expect("payload should not be skipped");
        let second = parse("session.idle", raw)
            .expect("second parse")
            .expect("payload should not be skipped");

        assert_eq!(first.scope.fragment_id, second.scope.fragment_id);
    }
}
