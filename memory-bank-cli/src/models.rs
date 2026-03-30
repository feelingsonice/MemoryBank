use crate::AppError;
use crate::assets::find_repo_root;
use crate::constants::{EMBEDDED_MODEL_CATALOG, REMOTE_MODEL_CATALOG_URL};
use memory_bank_app::{
    AppPaths, DEFAULT_ANTHROPIC_MODEL, DEFAULT_GEMINI_MODEL, DEFAULT_OLLAMA_MODEL,
    DEFAULT_OPENAI_MODEL,
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::time::Duration;

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub(crate) struct ModelCatalog {
    #[serde(default)]
    providers: BTreeMap<String, ProviderModelCatalog>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
struct ProviderModelCatalog {
    #[serde(default)]
    models: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    #[serde(default)]
    models: Vec<OllamaTagModel>,
}

#[derive(Debug, Deserialize)]
struct OllamaTagModel {
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModelChoice {
    Preset(String),
    Current(String),
    Custom,
}

impl ModelChoice {
    pub(crate) fn value(&self) -> Option<&str> {
        match self {
            Self::Preset(model) | Self::Current(model) => Some(model.as_str()),
            Self::Custom => None,
        }
    }
}

impl fmt::Display for ModelChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Preset(model) => f.write_str(model),
            Self::Current(model) => write!(f, "Current saved model ({model})"),
            Self::Custom => f.write_str("Enter a custom model..."),
        }
    }
}

pub(crate) fn default_model_for_provider(provider: &str) -> &'static str {
    match provider {
        "anthropic" => DEFAULT_ANTHROPIC_MODEL,
        "gemini" => DEFAULT_GEMINI_MODEL,
        "open-ai" => DEFAULT_OPENAI_MODEL,
        "ollama" => DEFAULT_OLLAMA_MODEL,
        _ => DEFAULT_ANTHROPIC_MODEL,
    }
}

pub(crate) fn refresh_model_catalog(paths: &AppPaths) -> ModelCatalog {
    if let Ok(catalog) = fetch_remote_model_catalog(paths) {
        return catalog;
    }

    load_local_model_catalog(paths).unwrap_or_default()
}

pub(crate) fn load_local_model_catalog(paths: &AppPaths) -> Result<ModelCatalog, AppError> {
    let mut last_error = None;

    if paths.model_catalog_file.exists() {
        match fs::read_to_string(&paths.model_catalog_file)
            .map_err(AppError::from)
            .and_then(|contents| ModelCatalog::from_json(&contents))
        {
            Ok(catalog) => return Ok(catalog),
            Err(error) => last_error = Some(error),
        }
    }

    if let Some(local_path) =
        find_repo_root().map(|root| root.join("config/setup-model-catalog.json"))
    {
        match fs::read_to_string(&local_path)
            .map_err(AppError::from)
            .and_then(|contents| ModelCatalog::from_json(&contents))
        {
            Ok(catalog) => return Ok(catalog),
            Err(error) => last_error = Some(error),
        }
    }

    ModelCatalog::from_json(EMBEDDED_MODEL_CATALOG)
        .map_err(|error| last_error.unwrap_or(error))
}

pub(crate) fn fetch_ollama_models_for_setup(ollama_url: &str) -> Result<Vec<String>, AppError> {
    let tags_url = format!("{}/api/tags", ollama_url.trim_end_matches('/'));
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(2))
        .timeout_read(Duration::from_secs(3))
        .timeout_write(Duration::from_secs(3))
        .build();
    let response = agent.get(&tags_url).call().map_err(|error| {
        AppError::Message(format!("failed to load installed Ollama models: {error}"))
    })?;
    let tags = response
        .into_json::<OllamaTagsResponse>()
        .map_err(|error| {
            AppError::Message(format!("failed to parse Ollama model list: {error}"))
        })?;

    let mut seen = BTreeSet::new();
    let mut models = Vec::new();
    for model in tags.models {
        let display = ollama_display_name(&model.name);
        if seen.insert(display.clone()) {
            models.push(display);
        }
    }

    Ok(models)
}

pub(crate) fn model_choices_for_provider(
    provider: &str,
    current: Option<&str>,
    catalog: &ModelCatalog,
) -> Vec<ModelChoice> {
    model_choices_from_values(&catalog.models_for_provider(provider), current)
}

pub(crate) fn model_choices_from_values<S>(values: &[S], current: Option<&str>) -> Vec<ModelChoice>
where
    S: AsRef<str>,
{
    let mut choices = values
        .iter()
        .map(|model| ModelChoice::Preset(model.as_ref().to_string()))
        .collect::<Vec<_>>();

    if let Some(current_model) = current.filter(|value| !value.is_empty())
        && !values.iter().any(|model| model.as_ref() == current_model)
    {
        choices.push(ModelChoice::Current(current_model.to_string()));
    }

    choices.push(ModelChoice::Custom);
    choices
}

pub(crate) fn ollama_display_name(model: &str) -> String {
    model.strip_suffix(":latest").unwrap_or(model).to_string()
}

impl ModelCatalog {
    pub(crate) fn models_for_provider(&self, provider: &str) -> Vec<&str> {
        self.providers
            .get(provider)
            .map(|provider| {
                provider
                    .models
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }

    pub(crate) fn from_json(contents: &str) -> Result<Self, AppError> {
        let catalog: Self = serde_json::from_str(contents)?;
        Ok(catalog)
    }
}

fn fetch_remote_model_catalog(paths: &AppPaths) -> Result<ModelCatalog, AppError> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(2))
        .timeout_read(Duration::from_secs(3))
        .timeout_write(Duration::from_secs(3))
        .build();
    let response = agent
        .get(REMOTE_MODEL_CATALOG_URL)
        .call()
        .map_err(|error| AppError::Message(format!("failed to fetch model catalog: {error}")))?;
    let contents = response.into_string().map_err(|error| {
        AppError::Message(format!("failed to read remote model catalog: {error}"))
    })?;
    let catalog = ModelCatalog::from_json(&contents)?;
    paths.ensure_base_dirs()?;
    fs::write(&paths.model_catalog_file, format!("{contents}\n"))?;
    Ok(catalog)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_model_catalog() -> ModelCatalog {
        ModelCatalog::from_json(
            r#"{
  "providers": {
    "anthropic": {
      "models": [
        "claude-opus-4-6",
        "claude-sonnet-4-6",
        "claude-haiku-4-5"
      ]
    },
    "open-ai": {
      "models": [
        "gpt-5.4",
        "gpt-5-mini"
      ]
    }
  }
}"#,
        )
        .expect("model catalog")
    }

    #[test]
    fn model_choices_include_current_saved_model_and_custom_fallback() {
        let catalog = test_model_catalog();
        let choices = model_choices_for_provider("anthropic", Some("claude-opus-custom"), &catalog);

        assert_eq!(
            choices,
            vec![
                ModelChoice::Preset("claude-opus-4-6".to_string()),
                ModelChoice::Preset("claude-sonnet-4-6".to_string()),
                ModelChoice::Preset("claude-haiku-4-5".to_string()),
                ModelChoice::Current("claude-opus-custom".to_string()),
                ModelChoice::Custom,
            ]
        );
    }

    #[test]
    fn load_local_model_catalog_reads_installed_copy() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        paths.ensure_base_dirs().expect("base dirs");
        fs::write(
            &paths.model_catalog_file,
            r#"{
  "providers": {
    "gemini": {
      "models": [
        "gemini-3.1-pro-preview",
        "gemini-3-flash-preview"
      ]
    }
  }
}"#,
        )
        .expect("write model catalog");

        let catalog = load_local_model_catalog(&paths).expect("load local catalog");

        assert_eq!(
            catalog.models_for_provider("gemini"),
            vec!["gemini-3.1-pro-preview", "gemini-3-flash-preview"]
        );
    }

    #[test]
    fn load_local_model_catalog_falls_back_when_installed_copy_is_invalid() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        paths.ensure_base_dirs().expect("base dirs");
        fs::write(&paths.model_catalog_file, "{not valid json").expect("write broken catalog");

        let catalog = load_local_model_catalog(&paths).expect("fallback model catalog");

        assert!(!catalog.models_for_provider("anthropic").is_empty());
    }

    #[test]
    fn empty_model_catalog_still_offers_custom_entry() {
        let choices = model_choices_for_provider("ollama", None, &ModelCatalog::default());
        assert_eq!(choices, vec![ModelChoice::Custom]);
    }

    #[test]
    fn ollama_display_name_strips_latest_suffix() {
        assert_eq!(ollama_display_name("qwen3:latest"), "qwen3");
        assert_eq!(ollama_display_name("qwen3:8b"), "qwen3:8b");
    }
}
