use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod retrieve_memory;

pub const INGEST_PROTOCOL_VERSION: u16 = 1;

pub use retrieve_memory::{
    MEMORY_BANK_SERVER_INSTRUCTIONS, MemoryNote, RETRIEVE_MEMORY_TOOL_NAME,
    RETRIEVE_MEMORY_TOOL_TITLE, RetrieveMemoryArgs, RetrieveMemoryResult,
    mcp_compatible_schema_for,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngestEnvelope {
    pub protocol_version: u16,
    pub source: SourceMeta,
    pub scope: ConversationScope,
    pub fragment: ConversationFragment,
    #[serde(default)]
    pub raw: Value,
}

impl IngestEnvelope {
    pub fn validate(&self) -> Result<(), String> {
        if self.protocol_version != INGEST_PROTOCOL_VERSION {
            return Err(format!(
                "unsupported protocol_version '{}', expected '{}'",
                self.protocol_version, INGEST_PROTOCOL_VERSION
            ));
        }

        if self.source.agent.trim().is_empty() {
            return Err("source.agent must not be empty".to_string());
        }

        if self.source.event.trim().is_empty() {
            return Err("source.event must not be empty".to_string());
        }

        if self.scope.conversation_id.trim().is_empty() {
            return Err("scope.conversation_id must not be empty".to_string());
        }

        if self.scope.fragment_id.trim().is_empty() {
            return Err("scope.fragment_id must not be empty".to_string());
        }

        if let Some(turn_id) = &self.scope.turn_id
            && turn_id.trim().is_empty()
        {
            return Err("scope.turn_id must not be empty when provided".to_string());
        }

        match &self.fragment.body {
            FragmentBody::UserMessage { text } | FragmentBody::AssistantMessage { text } => {
                if text.trim().is_empty() {
                    return Err("fragment text must not be empty".to_string());
                }
            }
            FragmentBody::ToolCall {
                name, input_json, ..
            } => {
                if name.trim().is_empty() {
                    return Err("tool_call.name must not be empty".to_string());
                }
                if input_json.trim().is_empty() {
                    return Err("tool_call.input_json must not be empty".to_string());
                }
            }
            FragmentBody::ToolResult {
                name, output_json, ..
            } => {
                if name.trim().is_empty() {
                    return Err("tool_result.name must not be empty".to_string());
                }
                if output_json.trim().is_empty() {
                    return Err("tool_result.output_json must not be empty".to_string());
                }
            }
            FragmentBody::Thinking { text } => {
                if text.trim().is_empty() {
                    return Err("thinking.text must not be empty".to_string());
                }
            }
            FragmentBody::Other { label, .. } => {
                if label.trim().is_empty() {
                    return Err("other.label must not be empty".to_string());
                }
            }
        }

        Ok(())
    }
}

impl fmt::Display for IngestEnvelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let encoded = serde_json::to_string(self).map_err(|_| fmt::Error)?;
        f.write_str(&encoded)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceMeta {
    pub agent: String,
    pub event: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationScope {
    pub conversation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub fragment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence_hint: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emitted_at_rfc3339: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationFragment {
    pub terminality: Terminality,
    pub body: FragmentBody,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Terminality {
    #[default]
    None,
    Soft,
    Hard,
}

impl Terminality {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Soft => "soft",
            Self::Hard => "hard",
        }
    }

    pub const fn rank(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Soft => 1,
            Self::Hard => 2,
        }
    }
}

impl fmt::Display for Terminality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FragmentBody {
    UserMessage {
        text: String,
    },
    AssistantMessage {
        text: String,
    },
    ToolCall {
        name: String,
        input_json: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
    },
    ToolResult {
        name: String,
        output_json: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
    },
    Thinking {
        text: String,
    },
    Other {
        label: String,
        payload: Value,
    },
}

impl FragmentBody {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::UserMessage { .. } => "user_message",
            Self::AssistantMessage { .. } => "assistant_message",
            Self::ToolCall { .. } => "tool_call",
            Self::ToolResult { .. } => "tool_result",
            Self::Thinking { .. } => "thinking",
            Self::Other { .. } => "other",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConversationFragment, ConversationScope, FragmentBody, INGEST_PROTOCOL_VERSION,
        IngestEnvelope, SourceMeta, Terminality,
    };
    use serde_json::json;

    #[test]
    fn round_trips_typed_payload() {
        let payload = sample_envelope();

        let encoded = serde_json::to_string(&payload).expect("serialize");
        let decoded: IngestEnvelope = serde_json::from_str(&encoded).expect("deserialize");

        assert_eq!(decoded, payload);
    }

    #[test]
    fn validates_required_fields() {
        let mut payload = sample_envelope();
        payload.scope.fragment_id.clear();

        let error = payload.validate().expect_err("validation should fail");
        assert!(error.contains("scope.fragment_id"));
    }

    #[test]
    fn serializes_tagged_fragment_kind() {
        let payload = sample_envelope();
        let encoded = serde_json::to_string(&payload).expect("serialize");
        assert!(encoded.contains("\"kind\":\"user_message\""));
    }

    fn sample_envelope() -> IngestEnvelope {
        IngestEnvelope {
            protocol_version: INGEST_PROTOCOL_VERSION,
            source: SourceMeta {
                agent: "claude-code".to_string(),
                event: "UserPromptSubmit".to_string(),
            },
            scope: ConversationScope {
                conversation_id: "session-123".to_string(),
                turn_id: None,
                fragment_id: "fragment-123".to_string(),
                sequence_hint: Some(1),
                emitted_at_rfc3339: Some("2026-03-05T02:00:00Z".to_string()),
            },
            fragment: ConversationFragment {
                terminality: Terminality::None,
                body: FragmentBody::UserMessage {
                    text: "hello".to_string(),
                },
            },
            raw: json!({"hook_event_name": "UserPromptSubmit"}),
        }
    }
}
