use chrono::{DateTime, Utc};
use schemars::{JsonSchema, generate::SchemaSettings};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const RETRIEVE_MEMORY_TOOL_NAME: &str = "retrieve_memory";
pub const RETRIEVE_MEMORY_TOOL_TITLE: &str = "Recall Prior Context";
pub const MEMORY_BANK_SERVER_INSTRUCTIONS: &str = "You are connected to Memory Bank, a long-term memory system.\n\n## When to RETRIEVE memories (retrieve_memory)\nBefore answering, retrieve whenever prior context could materially improve the answer.\n- The answer may depend on something learned earlier about the user, project, task, or prior decisions\n- Relevant context may exist in previous conversations even if the user does not mention them explicitly\n- The current request asks you to apply, interpret, or synthesize what was learned earlier rather than just repeat it verbatim\n- The answer would benefit from continuity, personalization, consistency, or factual recall across turns\nIf prior context could plausibly change the answer, retrieve first. When in doubt, retrieve. It is better to search and find nothing than to miss relevant context.";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RetrieveMemoryArgs {
    /// A natural-language query describing what you need to recall. Be specific:
    /// include key concepts, names, or topics so the system can find the most
    /// relevant memories. For example, "user's preferred programming language"
    /// rather than just "preferences".
    pub query: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "A stored long-term memory note returned by memory retrieval.")]
pub struct MemoryNote {
    #[schemars(
        description = "Rendered memory content captured from the original conversation turn."
    )]
    pub content: String,
    #[schemars(description = "RFC 3339 timestamp indicating when the memory note was stored.")]
    pub timestamp: DateTime<Utc>,
    #[schemars(description = "Short retrieval-oriented keywords extracted from the memory note.")]
    pub keywords: Vec<String>,
    #[schemars(description = "Higher-level tags used to group and retrieve related memories.")]
    pub tags: Vec<String>,
    #[schemars(
        description = "Distilled conversation context explaining why this memory mattered."
    )]
    pub context: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Structured response returned by the retrieve_memory tool.")]
pub struct RetrieveMemoryResult {
    #[schemars(
        description = "Memory notes relevant to the query, including their content and retrieval metadata."
    )]
    pub notes: Vec<MemoryNote>,
}

pub fn mcp_compatible_schema_for<T: JsonSchema>() -> Map<String, Value> {
    let mut settings = SchemaSettings::draft07();
    settings
        .transforms
        .push(Box::new(schemars::transform::AddNullable::default()));

    let schema = settings.into_generator().into_root_schema_for::<T>();
    match serde_json::to_value(schema).expect("serialize schema") {
        Value::Object(object) => object,
        other => panic!(
            "schema serialization produced non-object value: expected JSON object but got {:?}",
            other
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{RetrieveMemoryArgs, RetrieveMemoryResult, mcp_compatible_schema_for};

    #[test]
    fn mcp_compatible_schema_uses_draft7_for_tool_interop() {
        let input = serde_json::Value::Object(mcp_compatible_schema_for::<RetrieveMemoryArgs>());
        let output = serde_json::Value::Object(mcp_compatible_schema_for::<RetrieveMemoryResult>());

        let input_json = serde_json::to_string(&input).expect("serialize input schema");
        let output_json = serde_json::to_string(&output).expect("serialize output schema");

        assert!(input_json.contains("draft-07"));
        assert!(output_json.contains("draft-07"));
        assert!(!input_json.contains("draft/2020-12"));
        assert!(!output_json.contains("draft/2020-12"));
        assert!(output_json.contains("\"definitions\""));
        assert!(output_json.contains("#/definitions/"));
    }
}
