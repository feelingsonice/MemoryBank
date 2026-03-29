use fastembed::{
    EmbeddingModel, InitOptions, InitOptionsUserDefined, TextEmbedding, TokenizerFiles,
    UserDefinedEmbeddingModel,
};
use hf_hub::api::sync::{ApiBuilder, ApiRepo};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::debug;

use crate::config::EncoderProviderConfig;
use crate::error::{AppError, EncoderError};

// Shared encoder API used across the application.
pub fn embedding_to_bytes(embedding: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(embedding));
    for value in embedding {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

#[derive(Debug, Clone, Copy)]
pub struct EmbeddingInput<'a> {
    pub content: &'a str,
    pub keywords: &'a [String],
    pub tags: &'a [String],
    pub context: &'a str,
}

impl fmt::Display for EmbeddingInput<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Content: {}\nKeywords: ", self.content)?;
        write_joined(f, self.keywords)?;
        f.write_str("\nTags: ")?;
        write_joined(f, self.tags)?;
        write!(f, "\nContext: {}", self.context)
    }
}

pub enum EncoderClient {
    FastEmbed(FastEmbedEncoder),
}

impl EncoderClient {
    pub async fn encode(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EncoderError> {
        match self {
            Self::FastEmbed(client) => client.encode(texts).await,
        }
    }

    pub async fn encode_memory(
        &self,
        payload: &EmbeddingInput<'_>,
    ) -> Result<Vec<f32>, EncoderError> {
        take_single_embedding(
            self.encode_memories(std::slice::from_ref(payload)).await?,
            "single memory encoding",
        )
    }

    pub async fn encode_memories(
        &self,
        payloads: &[EmbeddingInput<'_>],
    ) -> Result<Vec<Vec<f32>>, EncoderError> {
        self.encode(payloads_to_texts(payloads)).await
    }
}

pub struct InitializedEncoder {
    pub client: EncoderClient,
    pub model_id: String,
}

impl InitializedEncoder {
    pub fn provider_name(&self) -> &'static str {
        match &self.client {
            EncoderClient::FastEmbed(_) => "fast-embed",
        }
    }
}

pub fn initialize(
    config: EncoderProviderConfig,
    models_dir: &Path,
) -> Result<InitializedEncoder, AppError> {
    let model_id = config.to_string();
    let client = encoder_client_from_config(config, models_dir)?;

    Ok(InitializedEncoder { client, model_id })
}

fn encoder_client_from_config(
    config: EncoderProviderConfig,
    models_dir: &Path,
) -> Result<EncoderClient, AppError> {
    match config {
        EncoderProviderConfig::FastEmbed { model } => Ok(EncoderClient::FastEmbed(
            FastEmbedEncoder::new(&model, models_dir)?,
        )),
        EncoderProviderConfig::LocalApi { url } => {
            Err(unsupported_encoder_provider("LocalApi", &url))
        }
        EncoderProviderConfig::RemoteApi { url, .. } => {
            Err(unsupported_encoder_provider("RemoteApi", &url))
        }
    }
}

fn unsupported_encoder_provider(provider: &str, url: &str) -> AppError {
    AppError::Encoder(EncoderError::Init(format!(
        "{provider} encoder not yet implemented for url: {url}"
    )))
}

// FastEmbed-backed encoder implementation.
#[derive(Clone)]
pub(crate) struct FastEmbedEncoder {
    model: Arc<Mutex<TextEmbedding>>,
}

impl FastEmbedEncoder {
    fn new(model_name: &str, models_dir: &Path) -> Result<Self, EncoderError> {
        debug!(
            model = model_name,
            models_dir = %models_dir.display(),
            "Initializing FastEmbed encoder"
        );

        std::fs::create_dir_all(models_dir)
            .map_err(|e| EncoderError::Init(format!("Failed to create models directory: {}", e)))?;

        let model = Self::load_model(model_name, models_dir)?;

        debug!(model = model_name, "FastEmbed encoder ready");
        Ok(Self {
            model: Arc::new(Mutex::new(model)),
        })
    }

    fn load_model(model_name: &str, models_dir: &Path) -> Result<TextEmbedding, EncoderError> {
        match model_name.parse::<EmbeddingModel>() {
            Ok(embedding_model) => Self::load_native_model(embedding_model, models_dir),
            Err(_) => {
                debug!(
                    model = model_name,
                    "Model is not bundled with FastEmbed; trying HuggingFace repository layout"
                );
                Self::load_custom_model(model_name, models_dir)
            }
        }
    }

    fn load_native_model(
        embedding_model: EmbeddingModel,
        models_dir: &Path,
    ) -> Result<TextEmbedding, EncoderError> {
        let init_options =
            InitOptions::new(embedding_model).with_cache_dir(models_dir.to_path_buf());
        TextEmbedding::try_new(init_options).map_err(|e| EncoderError::Init(e.to_string()))
    }

    fn load_custom_model(repo_id: &str, cache_dir: &Path) -> Result<TextEmbedding, EncoderError> {
        debug!(
            repo_id,
            cache_dir = %cache_dir.display(),
            "Loading custom embedding model from HuggingFace cache"
        );
        let custom_model = UserDefinedEmbeddingModel::try_from(
            DownloadedCustomModelFiles::download(repo_id, cache_dir)?,
        )?;

        TextEmbedding::try_new_from_user_defined(custom_model, InitOptionsUserDefined::default())
            .map_err(|e| {
                EncoderError::Init(format!("Failed to load custom model '{}': {}", repo_id, e))
            })
    }

    fn encode_blocking(
        model: Arc<Mutex<TextEmbedding>>,
        texts: Vec<String>,
    ) -> Result<Vec<Vec<f32>>, EncoderError> {
        let mut guard = model
            .lock()
            .map_err(|_| EncoderError::Encode("Mutex poisoned".to_string()))?;
        let embeddings: Result<Vec<Vec<f32>>, anyhow::Error> = guard.embed(texts, None);
        embeddings.map_err(|e| EncoderError::Encode(e.to_string()))
    }

    async fn encode(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EncoderError> {
        let model = Arc::clone(&self.model);

        tokio::task::spawn_blocking(move || Self::encode_blocking(model, texts))
            .await
            .map_err(|e| EncoderError::Encode(format!("Tokio Join Error: {}", e)))?
    }
}

fn payloads_to_texts(payloads: &[EmbeddingInput<'_>]) -> Vec<String> {
    let mut texts = Vec::with_capacity(payloads.len());
    for payload in payloads {
        texts.push(payload.to_string());
    }
    texts
}

pub(crate) fn validate_embedding_count(
    actual: usize,
    expected: usize,
    operation: &str,
) -> Result<(), EncoderError> {
    if actual == expected {
        return Ok(());
    }

    Err(EncoderError::Encode(format!(
        "{operation} returned {actual} embedding(s) for {expected} input(s)"
    )))
}

pub(crate) fn take_single_embedding(
    mut embeddings: Vec<Vec<f32>>,
    operation: &str,
) -> Result<Vec<f32>, EncoderError> {
    validate_embedding_count(embeddings.len(), 1, operation)?;
    embeddings
        .pop()
        .ok_or_else(|| EncoderError::Encode(format!("{operation} returned no embeddings")))
}

// HuggingFace custom model download and loading helpers.
const MODEL_ONNX_CANDIDATES: [&str; 2] = ["onnx/model.onnx", "model.onnx"];
const TOKENIZER_JSON: &str = "tokenizer.json";
const CONFIG_JSON: &str = "config.json";
const SPECIAL_TOKENS_MAP_JSON: &str = "special_tokens_map.json";
const TOKENIZER_CONFIG_JSON: &str = "tokenizer_config.json";

struct DownloadedCustomModelFiles {
    onnx: PathBuf,
    tokenizer: PathBuf,
    config: PathBuf,
    special_tokens_map: PathBuf,
    tokenizer_config: PathBuf,
}

impl DownloadedCustomModelFiles {
    fn download(repo_id: &str, cache_dir: &Path) -> Result<Self, EncoderError> {
        let repo = ApiBuilder::new()
            .with_cache_dir(cache_dir.to_path_buf())
            .build()
            .map_err(|e| {
                EncoderError::Init(format!("Failed to initialize HuggingFace API: {}", e))
            })?
            .model(repo_id.to_string());

        let [tokenizer, config, special_tokens_map, tokenizer_config] = [
            TOKENIZER_JSON,
            CONFIG_JSON,
            SPECIAL_TOKENS_MAP_JSON,
            TOKENIZER_CONFIG_JSON,
        ]
        .map(|path| Self::download_required(&repo, repo_id, path));

        Ok(Self {
            onnx: Self::download_onnx(&repo, repo_id)?,
            tokenizer: tokenizer?,
            config: config?,
            special_tokens_map: special_tokens_map?,
            tokenizer_config: tokenizer_config?,
        })
    }

    fn download_required(
        repo: &ApiRepo,
        repo_id: &str,
        path: &str,
    ) -> Result<PathBuf, EncoderError> {
        repo.get(path).map_err(|e| {
            EncoderError::Init(format!(
                "Failed to download {path} from '{}': {}",
                repo_id, e
            ))
        })
    }

    fn download_onnx(repo: &ApiRepo, repo_id: &str) -> Result<PathBuf, EncoderError> {
        let mut last_error = None;
        for path in MODEL_ONNX_CANDIDATES {
            match repo.get(path) {
                Ok(downloaded_path) => return Ok(downloaded_path),
                Err(error) => last_error = Some(error),
            }
        }

        let suffix = last_error
            .map(|error| format!(": {}", error))
            .unwrap_or_default();
        Err(EncoderError::Init(format!(
            "Failed to download ONNX model from '{}' (tried {} and {}){}",
            repo_id, MODEL_ONNX_CANDIDATES[0], MODEL_ONNX_CANDIDATES[1], suffix
        )))
    }
}

impl TryFrom<DownloadedCustomModelFiles> for UserDefinedEmbeddingModel {
    type Error = EncoderError;

    fn try_from(files: DownloadedCustomModelFiles) -> Result<Self, Self::Error> {
        Ok(UserDefinedEmbeddingModel::new(
            read_model_file(files.onnx, MODEL_ONNX_FILE_NAME)?,
            TokenizerFiles {
                tokenizer_file: read_model_file(files.tokenizer, TOKENIZER_JSON)?,
                config_file: read_model_file(files.config, CONFIG_JSON)?,
                special_tokens_map_file: read_model_file(
                    files.special_tokens_map,
                    SPECIAL_TOKENS_MAP_JSON,
                )?,
                tokenizer_config_file: read_model_file(
                    files.tokenizer_config,
                    TOKENIZER_CONFIG_JSON,
                )?,
            },
        ))
    }
}

const MODEL_ONNX_FILE_NAME: &str = "model.onnx";

fn read_model_file(path: PathBuf, name: &str) -> Result<Vec<u8>, EncoderError> {
    std::fs::read(&path).map_err(|e| EncoderError::Init(format!("Failed to read {}: {}", name, e)))
}

fn write_joined(f: &mut fmt::Formatter<'_>, values: &[String]) -> fmt::Result {
    let mut values = values.iter();
    let Some(first) = values.next() else {
        return Ok(());
    };

    f.write_str(first)?;
    for value in values {
        f.write_str(", ")?;
        f.write_str(value)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_to_bytes_uses_little_endian_layout() {
        let embedding = [1.0_f32, -2.5_f32];

        let bytes = embedding_to_bytes(&embedding);

        assert_eq!(
            bytes,
            embedding
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn payloads_to_texts_preserves_layout() {
        let keywords_a = vec!["alpha".to_string()];
        let tags_a = vec!["tag-a".to_string()];
        let keywords_b = vec!["beta".to_string(), "gamma".to_string()];
        let tags_b = vec!["tag-b".to_string()];
        let payloads = vec![
            EmbeddingInput {
                content: "first",
                keywords: &keywords_a,
                tags: &tags_a,
                context: "ctx-a",
            },
            EmbeddingInput {
                content: "second",
                keywords: &keywords_b,
                tags: &tags_b,
                context: "ctx-b",
            },
        ];

        assert_eq!(
            payloads_to_texts(&payloads),
            vec![
                "Content: first\nKeywords: alpha\nTags: tag-a\nContext: ctx-a".to_string(),
                "Content: second\nKeywords: beta, gamma\nTags: tag-b\nContext: ctx-b".to_string(),
            ]
        );
    }

    #[test]
    fn take_single_embedding_returns_the_single_embedding() {
        let embedding =
            take_single_embedding(vec![vec![42.0, 7.0]], "single memory encoding").unwrap();

        assert_eq!(embedding, vec![42.0, 7.0]);
    }

    #[test]
    fn take_single_embedding_rejects_missing_embeddings() {
        let error = take_single_embedding(Vec::new(), "single memory encoding").unwrap_err();

        assert!(matches!(error, EncoderError::Encode(message)
                if message == "single memory encoding returned 0 embedding(s) for 1 input(s)"));
    }

    #[test]
    fn take_single_embedding_rejects_multiple_embeddings() {
        let error = take_single_embedding(
            vec![vec![1.0, 2.0], vec![3.0, 4.0]],
            "single memory encoding",
        )
        .unwrap_err();

        assert!(matches!(error, EncoderError::Encode(message)
            if message == "single memory encoding returned 2 embedding(s) for 1 input(s)"));
    }
}
