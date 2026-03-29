use chrono::{DateTime, Utc};
use rig::agent::{Agent, AgentBuilder};
use rig::client::{CompletionClient, Nothing};
use rig::completion::{CompletionModel, TypedPrompt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::warn;
use url::Url;

use crate::config::LlmProviderConfig;
use crate::error::{AppError, LlmError};

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub struct ExtractedMemoryAnalysis {
    #[serde(default)]
    pub context: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub struct EvolvedNeighbor {
    /// The exact integer ID of the existing neighbor memory to update.
    pub id: i64,
    /// The newly updated context for this neighbor.
    pub context: String,
    /// The newly updated tags for this neighbor.
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub struct MemoryEvolution {
    /// IDs of existing neighbor memories that the NEW memory should be linked to.
    /// Leave empty if no connections are relevant.
    #[serde(default)]
    pub suggested_connections: Vec<i64>,

    /// If analyzing the neighbors reveals better tags for the NEW memory, provide the completely updated list of tags here.
    /// Leave empty to keep the NEW memory's original tags.
    #[serde(default)]
    pub updated_new_memory_tags: Vec<String>,

    /// Sparse updates for any existing neighbors whose context or tags need to change based on the NEW memory.
    /// ONLY include neighbors that actually require modification.
    #[serde(default)]
    pub neighbor_updates: Vec<EvolvedNeighbor>,
}

#[derive(Serialize)]
struct MemoryAnalysisPrompt<'a> {
    previous_turns: &'a str,
    current_turn: &'a str,
    timestamp: DateTime<Utc>,
}

pub const MEMORY_ANALYSIS_PREAMBLE: &str = r#"Analyze a captured conversation window for long-term memory storage.
You will be given:
1. `previous_turns`: earlier completed turns from the same conversation, in chronological order
2. `current_turn`: the latest turn being stored
3. `timestamp`: the time the memory is being created

Use the whole window to understand the conversation state after the current turn.

Your tasks:
- produce a reusable conversation context for future turns based on the full window
- extract keywords for the memory represented by the current turn
- extract tags for the memory represented by the current turn

Rules:
- `previous_turns` and `current_turn` are intentionally separated; keep that distinction in mind
- the returned `context` should describe the ongoing conversation state after the current turn
- do not produce a turn-by-turn transcript
- keep the context concise but useful as future background

Format the response as a JSON object:
{
    "context": "...",
    "keywords": [
        // several specific, distinct keywords that capture key concepts and terminology
        // Order from most to least important
        // Don't include keywords that are the name of the speaker or time
        // At least three keywords, but don't be too redundant.
    ],
    "tags": [
        // several broad categories/themes for classification
        // Include domain, format, and type tags
        // At least three tags, but don't be too redundant.
    ]
}"#;

pub const MEMORY_EVOLUTION_PREAMBLE: &str = r#"You are an AI memory evolution agent managing a knowledge graph.
You will be provided with a NEW memory note, along with several of its existing NEIGHBOR memories.

Your task is to determine how the knowledge graph should evolve by analyzing the relationship between the NEW memory and the existing NEIGHBORS.

1. CONNECTING MEMORIES: Determine if the NEW memory should form direct relationship links with any of the NEIGHBORS.
2. REFINING THE NEW MEMORY: Decide if the NEW memory's tags should be refined now that you see the context of its neighbors.
3. UPDATING EXISTING NEIGHBORS: Determine if any EXISTING neighbors need their context or tags updated in light of the NEW memory.

CRITICAL RULES:
- Use the exact integer `id` to refer to existing neighbor memories.
- ONLY output neighbor updates for neighbors that ACTUALLY require modification. If a neighbor does not need its context or tags updated, completely omit it.
- Format your response exactly according to the provided JSON schema."#;

pub(crate) struct RigStructuredLlm<M: CompletionModel> {
    model_label: String,
    analysis_agent: Agent<M>,
    evolve_agent: Agent<M>,
}

impl<M: CompletionModel> RigStructuredLlm<M> {
    fn new(model_label: String, analysis_agent: Agent<M>, evolve_agent: Agent<M>) -> Self {
        Self {
            model_label,
            analysis_agent,
            evolve_agent,
        }
    }

    fn from_model(model: M, model_label: impl Into<String>) -> Self
    where
        M: Clone,
    {
        let analysis_agent = AgentBuilder::new(model.clone())
            .preamble(MEMORY_ANALYSIS_PREAMBLE)
            .temperature(0.0)
            .build();

        let evolve_agent = AgentBuilder::new(model)
            .preamble(MEMORY_EVOLUTION_PREAMBLE)
            .temperature(0.0)
            .build();

        Self::new(model_label.into(), analysis_agent, evolve_agent)
    }
}

impl<M> RigStructuredLlm<M>
where
    M: CompletionModel + Send + Sync,
{
    async fn analyze_memory_window(
        &self,
        previous_turns: &str,
        current_turn: &str,
        timestamp: DateTime<Utc>,
    ) -> Result<ExtractedMemoryAnalysis, LlmError> {
        let prompt_content = serde_json::to_string(&MemoryAnalysisPrompt {
            previous_turns,
            current_turn,
            timestamp,
        })
        .map_err(|e| LlmError::Api(e.to_string()))?;
        self.analysis_agent
            .prompt_typed::<ExtractedMemoryAnalysis>(prompt_content)
            .await
            .map_err(|e| {
                warn!(
                    model = %self.model_label,
                    error = %e,
                    "Structured memory analysis failed"
                );
                LlmError::Api(e.to_string())
            })
    }

    async fn generate_memory_evolution(
        &self,
        context: &str,
        content: &str,
        keywords: &[String],
        neighbors_json: &str,
    ) -> Result<MemoryEvolution, LlmError> {
        let prompt = format!(
            "The NEW memory:\nConversation context: {}\nCurrent memory note: {}\nKeywords: {:?}\n\nThe EXISTING nearest neighbor memories:\n{}",
            context, content, keywords, neighbors_json
        );
        self.evolve_agent
            .prompt_typed::<MemoryEvolution>(prompt)
            .await
            .map_err(|e| {
                warn!(
                    model = %self.model_label,
                    error = %e,
                    "Structured memory evolution failed"
                );
                LlmError::Api(e.to_string())
            })
    }
}

type GeminiStructuredLlm = RigStructuredLlm<rig::providers::gemini::CompletionModel>;
type AnthropicStructuredLlm =
    RigStructuredLlm<rig::providers::anthropic::completion::CompletionModel>;
type OpenAiStructuredLlm =
    RigStructuredLlm<rig::providers::openai::responses_api::ResponsesCompletionModel>;
type OllamaStructuredLlm = RigStructuredLlm<rig::providers::ollama::CompletionModel>;

pub enum LlmClient {
    Gemini(GeminiStructuredLlm),
    Anthropic(AnthropicStructuredLlm),
    OpenAi(OpenAiStructuredLlm),
    Ollama(OllamaStructuredLlm),
}

impl LlmClient {
    pub async fn analyze_memory_window(
        &self,
        previous_turns: &str,
        current_turn: &str,
        timestamp: DateTime<Utc>,
    ) -> Result<ExtractedMemoryAnalysis, LlmError> {
        match self {
            Self::Gemini(client) => {
                client
                    .analyze_memory_window(previous_turns, current_turn, timestamp)
                    .await
            }
            Self::Anthropic(client) => {
                client
                    .analyze_memory_window(previous_turns, current_turn, timestamp)
                    .await
            }
            Self::OpenAi(client) => {
                client
                    .analyze_memory_window(previous_turns, current_turn, timestamp)
                    .await
            }
            Self::Ollama(client) => {
                client
                    .analyze_memory_window(previous_turns, current_turn, timestamp)
                    .await
            }
        }
    }

    pub async fn generate_memory_evolution(
        &self,
        context: &str,
        content: &str,
        keywords: &[String],
        neighbors_json: &str,
    ) -> Result<MemoryEvolution, LlmError> {
        match self {
            Self::Gemini(client) => {
                client
                    .generate_memory_evolution(context, content, keywords, neighbors_json)
                    .await
            }
            Self::Anthropic(client) => {
                client
                    .generate_memory_evolution(context, content, keywords, neighbors_json)
                    .await
            }
            Self::OpenAi(client) => {
                client
                    .generate_memory_evolution(context, content, keywords, neighbors_json)
                    .await
            }
            Self::Ollama(client) => {
                client
                    .generate_memory_evolution(context, content, keywords, neighbors_json)
                    .await
            }
        }
    }
}

pub struct InitializedLlm {
    pub client: LlmClient,
    pub model_id: String,
}

pub fn initialize(config: LlmProviderConfig) -> Result<InitializedLlm, AppError> {
    let model_id = config.to_string();
    let client = llm_client_from_config(config)?;

    Ok(InitializedLlm { client, model_id })
}

fn llm_client_from_config(config: LlmProviderConfig) -> Result<LlmClient, AppError> {
    match config {
        LlmProviderConfig::Gemini { api_key, model } => build_gemini_llm(&api_key, &model),
        LlmProviderConfig::Anthropic { api_key, model } => build_anthropic_llm(&api_key, &model),
        LlmProviderConfig::OpenAi { api_key, model } => build_openai_llm(&api_key, &model),
        LlmProviderConfig::Ollama { url, model } => build_ollama_llm(&url, &model),
    }
}

fn build_gemini_llm(api_key: &str, model: &str) -> Result<LlmClient, AppError> {
    let client = rig::providers::gemini::Client::new(api_key)
        .map_err(|e| llm_initialization_error(e.to_string()))?;
    // Gemini implicit caching is enabled by the provider on supported models.
    Ok(LlmClient::Gemini(build_rig_structured_llm(
        client.completion_model(model),
        format!("Gemini::{model}"),
    )))
}

fn build_anthropic_llm(api_key: &str, model: &str) -> Result<LlmClient, AppError> {
    let client = rig::providers::anthropic::Client::new(api_key)
        .map_err(|e| llm_initialization_error(e.to_string()))?;
    let completion_model = anthropic_completion_model(&client, model);

    Ok(LlmClient::Anthropic(build_rig_structured_llm(
        completion_model,
        format!("Anthropic::{model}"),
    )))
}

fn build_openai_llm(api_key: &str, model: &str) -> Result<LlmClient, AppError> {
    let client = rig::providers::openai::Client::new(api_key)
        .map_err(|e| llm_initialization_error(e.to_string()))?;
    // OpenAI prompt caching is automatic on supported models.
    Ok(build_openai_responses_llm(&client, model))
}

fn build_ollama_llm(url: &str, model: &str) -> Result<LlmClient, AppError> {
    let base_url = validate_ollama_url(url)?;
    verify_ollama_model(&base_url, model)?;

    let client = rig::providers::ollama::Client::builder()
        .api_key(Nothing)
        .base_url(&base_url)
        .build()
        .map_err(|e| llm_initialization_error(e.to_string()))?;
    Ok(LlmClient::Ollama(build_rig_structured_llm(
        client.completion_model(model),
        format!("Ollama::{model}@{base_url}"),
    )))
}

fn build_openai_responses_llm(client: &rig::providers::openai::Client, model: &str) -> LlmClient {
    LlmClient::OpenAi(build_rig_structured_llm(
        client.completion_model(model),
        format!("OpenAi::{model}"),
    ))
}

fn anthropic_completion_model(
    client: &rig::providers::anthropic::Client,
    model: &str,
) -> rig::providers::anthropic::completion::CompletionModel {
    client.completion_model(model).with_prompt_caching()
}

fn build_rig_structured_llm<M>(model: M, model_label: impl Into<String>) -> RigStructuredLlm<M>
where
    M: CompletionModel + Clone,
{
    RigStructuredLlm::from_model(model, model_label)
}

fn llm_initialization_error(message: String) -> AppError {
    AppError::Llm(LlmError::Init(message))
}

#[derive(Debug, Deserialize, Serialize)]
struct OllamaTagsResponse {
    #[serde(default)]
    models: Vec<OllamaModel>,
}

#[derive(Debug, Deserialize, Serialize)]
struct OllamaModel {
    name: String,
}

fn validate_ollama_url(url: &str) -> Result<String, AppError> {
    let parsed = Url::parse(url)
        .map_err(|e| llm_initialization_error(format!("Invalid Ollama URL '{url}': {e}")))?;
    let path = parsed.path().trim_end_matches('/');

    if path.ends_with("/v1") {
        return Err(llm_initialization_error(format!(
            "Ollama URL must point to the native API root, not '/v1': {url}. Use http://localhost:11434 or your Ollama base URL."
        )));
    }

    if !path.is_empty() {
        return Err(llm_initialization_error(format!(
            "Ollama URL must point to the native API root without a path: {url}. Use http://localhost:11434 or your Ollama base URL."
        )));
    }

    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(llm_initialization_error(format!(
            "Ollama URL must not include query parameters or fragments: {url}. Use http://localhost:11434 or your Ollama base URL."
        )));
    }

    Ok(url.trim_end_matches('/').to_string())
}

fn verify_ollama_model(base_url: &str, model: &str) -> Result<(), AppError> {
    let tags_url = format!("{base_url}/api/tags");
    let response = ureq::get(&tags_url).call().map_err(|error| {
        llm_initialization_error(match error {
            ureq::Error::Status(status, _) => format!(
                "Failed to verify Ollama at {tags_url}: HTTP {status}. Ensure the URL points to the native Ollama API root."
            ),
            ureq::Error::Transport(transport) => format!(
                "Unable to reach Ollama at {base_url}: {transport}. Make sure the Ollama daemon is running."
            ),
        })
    })?;

    let tags = response.into_json::<OllamaTagsResponse>().map_err(|e| {
        llm_initialization_error(format!(
            "Failed to parse Ollama model list from {tags_url}: {e}"
        ))
    })?;

    if tags.models.iter().any(|candidate| candidate.name == model) {
        return Ok(());
    }

    let available_models = tags
        .models
        .iter()
        .map(|candidate| candidate.name.as_str())
        .collect::<Vec<_>>();

    let available_suffix = if available_models.is_empty() {
        String::new()
    } else {
        format!(" Available models: {}.", available_models.join(", "))
    };

    Err(llm_initialization_error(format!(
        "Ollama model '{model}' is not available at {base_url}. Run `ollama pull {model}` and try again.{available_suffix}"
    )))
}

#[cfg(test)]
mod tests {
    use super::{
        ExtractedMemoryAnalysis, InitializedLlm, LlmClient, MEMORY_ANALYSIS_PREAMBLE,
        MemoryAnalysisPrompt, OllamaTagsResponse, anthropic_completion_model,
        llm_client_from_config, validate_ollama_url, verify_ollama_model,
    };
    use crate::config::LlmProviderConfig;
    use chrono::Utc;
    use rig::providers::anthropic::Client;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn anthropic_prompt_caching_is_enabled() {
        let client = Client::new("test-key").expect("anthropic client");
        let model = anthropic_completion_model(&client, "claude-sonnet-4");

        assert!(model.prompt_caching);
    }

    #[tokio::test]
    async fn structured_agents_use_zero_temperature() {
        let client = Client::new("test-key").expect("anthropic client");
        let model = anthropic_completion_model(&client, "claude-sonnet-4");
        let llm = super::build_rig_structured_llm(model, "Anthropic::claude-sonnet-4");

        assert_eq!(llm.analysis_agent.temperature, Some(0.0));
        assert_eq!(llm.evolve_agent.temperature, Some(0.0));
    }

    #[test]
    fn memory_analysis_prompt_serializes_in_stable_field_order() {
        let prompt = MemoryAnalysisPrompt {
            previous_turns: "history",
            current_turn: "current",
            timestamp: Utc::now(),
        };

        let json = serde_json::to_string(&prompt).expect("serialize");
        assert!(json.contains("\"previous_turns\":\"history\""));
        assert!(json.contains("\"current_turn\":\"current\""));
        assert!(json.contains("\"timestamp\":"));
        assert!(json.find("\"previous_turns\"").unwrap() < json.find("\"current_turn\"").unwrap());
        assert!(json.find("\"current_turn\"").unwrap() < json.find("\"timestamp\"").unwrap());
    }

    #[test]
    fn memory_analysis_prompt_contains_previous_and_current_turns() {
        let prompt = MemoryAnalysisPrompt {
            previous_turns: "history",
            current_turn: "latest",
            timestamp: Utc::now(),
        };

        let json = serde_json::to_value(&prompt).expect("serialize");
        assert_eq!(json["previous_turns"], "history");
        assert_eq!(json["current_turn"], "latest");
        assert!(json.get("timestamp").is_some());
    }

    #[test]
    fn extracted_memory_analysis_contains_context_keywords_and_tags() {
        let metadata = ExtractedMemoryAnalysis {
            context: "ctx".to_string(),
            keywords: vec!["one".to_string()],
            tags: vec!["two".to_string()],
        };

        let json = serde_json::to_value(metadata).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "context": "ctx",
                "keywords": ["one"],
                "tags": ["two"]
            })
        );
    }

    #[test]
    fn preamble_reflects_unified_memory_analysis_role() {
        assert!(MEMORY_ANALYSIS_PREAMBLE.contains("previous_turns"));
        assert!(MEMORY_ANALYSIS_PREAMBLE.contains("current_turn"));
        assert!(MEMORY_ANALYSIS_PREAMBLE.contains("\"context\""));
        assert!(MEMORY_ANALYSIS_PREAMBLE.contains("\"keywords\""));
        assert!(MEMORY_ANALYSIS_PREAMBLE.contains("\"tags\""));
        assert!(!MEMORY_ANALYSIS_PREAMBLE.contains("prior conversation context"));
        assert!(!MEMORY_ANALYSIS_PREAMBLE.contains("Do not generate a context field"));
    }

    #[tokio::test]
    async fn llm_clients_build_expected_variants() {
        assert!(matches!(
            llm_client_from_config(LlmProviderConfig::Gemini {
                api_key: "test-key".to_string(),
                model: "gemini-2.5-flash".to_string(),
            })
            .expect("gemini client"),
            LlmClient::Gemini(_)
        ));

        assert!(matches!(
            llm_client_from_config(LlmProviderConfig::Anthropic {
                api_key: "test-key".to_string(),
                model: "claude-sonnet-4-6".to_string(),
            })
            .expect("anthropic client"),
            LlmClient::Anthropic(_)
        ));

        assert!(matches!(
            llm_client_from_config(LlmProviderConfig::OpenAi {
                api_key: "test-key".to_string(),
                model: "gpt-4o-mini".to_string(),
            })
            .expect("openai client"),
            LlmClient::OpenAi(_)
        ));
    }

    #[tokio::test]
    async fn initialize_preserves_model_ids_for_remote_providers() {
        let InitializedLlm { client, model_id } = super::initialize(LlmProviderConfig::OpenAi {
            api_key: "test-key".to_string(),
            model: "gpt-4o-mini".to_string(),
        })
        .expect("initialize openai");

        assert!(matches!(client, LlmClient::OpenAi(_)));
        assert_eq!(model_id, "OpenAi::gpt-4o-mini");
    }

    #[test]
    fn ollama_url_rejects_openai_compat_suffix() {
        let error = validate_ollama_url("http://localhost:11434/v1")
            .expect_err("stale /v1 url should fail");

        assert!(error.to_string().contains("not '/v1'"));
    }

    #[test]
    fn ollama_url_rejects_non_root_paths() {
        let error = validate_ollama_url("http://localhost:11434/api")
            .expect_err("non-root path should fail");

        assert!(error.to_string().contains("without a path"));
    }

    #[test]
    fn ollama_url_rejects_query_parameters() {
        let error = validate_ollama_url("http://localhost:11434?foo=bar")
            .expect_err("query parameters should fail");

        assert!(
            error
                .to_string()
                .contains("must not include query parameters")
        );
    }

    #[test]
    fn ollama_verification_succeeds_when_model_exists() {
        let mock = MockOllamaServer::spawn(
            200,
            serde_json::to_string(&serde_json::json!({
                "models": [
                    { "name": "qwen3:4b" },
                    { "name": "llama3.2" }
                ]
            }))
            .expect("mock response"),
        );

        verify_ollama_model(&mock.base_url, "qwen3:4b").expect("verified model");
        mock.join();
    }

    #[test]
    fn ollama_verification_reports_missing_model() {
        let mock = MockOllamaServer::spawn(
            200,
            serde_json::to_string(&serde_json::json!({
                "models": [{ "name": "llama3.2" }]
            }))
            .expect("mock response"),
        );

        let error =
            verify_ollama_model(&mock.base_url, "qwen3:4b").expect_err("missing model should fail");
        mock.join();

        assert!(error.to_string().contains("ollama pull qwen3:4b"));
        assert!(error.to_string().contains("llama3.2"));
    }

    #[test]
    fn ollama_verification_reports_unreachable_daemon() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("reserve port");
        let port = listener.local_addr().expect("local addr").port();
        drop(listener);

        let error = verify_ollama_model(&format!("http://127.0.0.1:{port}"), "qwen3:4b")
            .expect_err("unreachable daemon should fail");

        assert!(error.to_string().contains("Unable to reach Ollama"));
    }

    #[tokio::test]
    async fn ollama_initialize_builds_client_and_model_id() {
        let mock = MockOllamaServer::spawn(
            200,
            serde_json::to_string(&OllamaTagsResponse {
                models: vec![super::OllamaModel {
                    name: "qwen3:4b".to_string(),
                }],
            })
            .expect("mock response"),
        );

        let InitializedLlm { client, model_id } = super::initialize(LlmProviderConfig::Ollama {
            url: mock.base_url.clone(),
            model: "qwen3:4b".to_string(),
        })
        .expect("initialize ollama");
        let base_url = mock.base_url.clone();
        mock.join();

        assert!(matches!(client, LlmClient::Ollama(_)));
        assert_eq!(model_id, format!("Ollama::qwen3:4b@{}", base_url));
    }

    struct MockOllamaServer {
        base_url: String,
        handle: thread::JoinHandle<()>,
    }

    impl MockOllamaServer {
        fn spawn(status: u16, body: String) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
            let base_url = format!(
                "http://127.0.0.1:{}",
                listener.local_addr().expect("local addr").port()
            );
            let handle = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept request");
                let mut buffer = [0_u8; 1024];
                let _ = stream.read(&mut buffer);

                let response = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write response");
            });

            Self { base_url, handle }
        }

        fn join(self) {
            self.handle.join().expect("join mock server");
        }
    }
}
