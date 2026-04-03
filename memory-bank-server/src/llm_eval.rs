use crate::actor::process_turn_for_real_clients;
use crate::config::{EncoderProviderConfig, LlmProviderConfig, LlmProviderType};
use crate::db::MemoryDb;
use crate::encoder::{self, EmbeddingInput, EncoderClient};
use crate::llm::{self, LlmClient};
use crate::memory_window::{MemoryProjection, MemoryStep, ProjectedConversationWindow};
use crate::retrieval_eval::real_eval_lock;
use chrono::Utc;
use memory_bank_app::{
    AppPaths, AppSettings, DEFAULT_ANTHROPIC_MODEL, DEFAULT_FASTEMBED_MODEL, DEFAULT_GEMINI_MODEL,
    DEFAULT_OLLAMA_MODEL, DEFAULT_OLLAMA_URL, DEFAULT_OPENAI_MODEL, DEFAULT_OPENAI_URL,
    normalize_openai_url,
};
use serde::Serialize;
use sqlx::Row;
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fs;
use std::path::PathBuf;

const LLM_EVAL_ENV: &str = "MEMORY_BANK_LLM_EVALS";
const LLM_EVAL_MODEL_ENV: &str = "MEMORY_BANK_LLM_EVAL_MODEL";
const NOTE_TIMESTAMP: &str = "2026-03-30T00:00:00Z";
const QUORUM_RUNS: usize = 3;
const QUORUM_REQUIRED: usize = 2;
const NEAREST_NEIGHBOR_COUNT: i32 = 3;

#[derive(Debug, Clone)]
struct SeedNote {
    slug: &'static str,
    content: &'static str,
    context: &'static str,
    keywords: &'static [&'static str],
    tags: &'static [&'static str],
}

impl SeedNote {
    fn keywords_as_strings(&self) -> Vec<String> {
        self.keywords
            .iter()
            .map(|value| (*value).to_string())
            .collect()
    }

    fn tags_as_strings(&self) -> Vec<String> {
        self.tags.iter().map(|value| (*value).to_string()).collect()
    }
}

#[derive(Debug, Clone)]
struct FixtureExpectation {
    required_links: Vec<&'static str>,
    forbidden_links: Vec<&'static str>,
}

#[derive(Debug, Clone)]
struct LlmFixture {
    name: &'static str,
    description: &'static str,
    notes: Vec<SeedNote>,
    window: ProjectedConversationWindow,
    expectation: FixtureExpectation,
}

#[derive(Debug, Serialize)]
struct LlmEvalReport {
    suite: &'static str,
    provider_name: String,
    encoder_model_id: String,
    model_id: String,
    nearest_neighbor_count: i32,
    quorum_runs: usize,
    quorum_required: usize,
    fixtures: Vec<FixtureReport>,
}

#[derive(Debug, Serialize)]
struct FixtureReport {
    name: String,
    description: String,
    passed_runs: usize,
    total_runs: usize,
    runs: Vec<FixtureRunReport>,
}

#[derive(Debug, Serialize)]
struct FixtureRunReport {
    fixture_name: String,
    run_index: usize,
    model_id: String,
    stored_memory_id: Option<i64>,
    linked_target_slugs: Vec<String>,
    updated_neighbor_slugs: Vec<String>,
    keywords: Vec<String>,
    tags: Vec<String>,
    context_preview: String,
    turn_status: Option<String>,
    passed: bool,
    pass_reasons: Vec<String>,
    fail_reasons: Vec<String>,
}

struct RealHarness {
    encoder_model_id: String,
    encoder_client: EncoderClient,
    llm_provider_name: String,
    llm_model_id: String,
    llm_client: LlmClient,
}

fn evals_enabled(test_name: &str) -> bool {
    if env::var(LLM_EVAL_ENV).ok().as_deref() == Some("1") {
        true
    } else {
        eprintln!("skipping {test_name} because {LLM_EVAL_ENV}=1 is not set");
        false
    }
}

fn default_models_dir() -> PathBuf {
    let paths = AppPaths::from_system().expect("resolve app paths");
    paths.ensure_base_dirs().expect("create app dirs");
    let models_dir = paths.models_dir();
    fs::create_dir_all(&models_dir).expect("create models dir");
    models_dir
}

fn initialize_real_harness() -> RealHarness {
    let models_dir = default_models_dir();
    let encoder_model = env::var("MEMORY_BANK_FASTEMBED_MODEL")
        .unwrap_or_else(|_| DEFAULT_FASTEMBED_MODEL.to_string());
    let encoder = encoder::initialize(
        EncoderProviderConfig::FastEmbed {
            model: encoder_model,
        },
        &models_dir,
    )
    .expect("initialize real fastembed encoder");

    let base_llm_config =
        resolve_current_llm_provider_config().expect("resolve configured llm provider");
    let llm_config = match resolve_eval_model_override() {
        Some(model) => base_llm_config.with_model_override(model),
        None => base_llm_config,
    };
    let llm_provider_name = llm_config.provider_name().to_string();
    let llm = llm::initialize(llm_config).expect("initialize real llm provider");

    RealHarness {
        encoder_model_id: encoder.model_id,
        encoder_client: encoder.client,
        llm_provider_name,
        llm_model_id: llm.model_id,
        llm_client: llm.client,
    }
}

fn resolve_current_llm_provider_config() -> Result<LlmProviderConfig, crate::error::AppError> {
    let app_paths = AppPaths::from_system()
        .map_err(|error| crate::error::AppError::Config(error.to_string()))?;
    let settings = AppSettings::load(&app_paths)
        .map_err(|error| crate::error::AppError::Config(error.to_string()))?;
    let server_settings = settings.server.as_ref();
    let env_provider = env::var("MEMORY_BANK_LLM_PROVIDER").ok();
    let env_provider = parse_optional_value(env_provider.as_deref())?;
    let settings_provider =
        parse_optional_value(server_settings.and_then(|value| value.llm_provider.as_deref()))?;
    let provider = env_provider
        .or(settings_provider)
        .unwrap_or(LlmProviderType::Anthropic);

    resolve_llm_provider_config(provider, server_settings)
}

fn resolve_llm_provider_config(
    provider: LlmProviderType,
    settings: Option<&memory_bank_app::ServerSettings>,
) -> Result<LlmProviderConfig, crate::error::AppError> {
    match provider {
        LlmProviderType::Gemini => Ok(LlmProviderConfig::Gemini {
            api_key: require_env("GEMINI_API_KEY")?,
            model: env_setting_or_default(
                "MEMORY_BANK_LLM_MODEL",
                settings.and_then(|value| value.llm_model.as_deref()),
                DEFAULT_GEMINI_MODEL,
            ),
        }),
        LlmProviderType::Anthropic => Ok(LlmProviderConfig::Anthropic {
            api_key: require_env("ANTHROPIC_API_KEY")?,
            model: env_setting_or_default(
                "MEMORY_BANK_LLM_MODEL",
                settings.and_then(|value| value.llm_model.as_deref()),
                DEFAULT_ANTHROPIC_MODEL,
            ),
        }),
        LlmProviderType::OpenAi => Ok(LlmProviderConfig::OpenAi {
            api_key: require_env("OPENAI_API_KEY")?,
            model: env_setting_or_default(
                "MEMORY_BANK_LLM_MODEL",
                settings.and_then(|value| value.llm_model.as_deref()),
                DEFAULT_OPENAI_MODEL,
            ),
            base_url: normalize_openai_url(&env_setting_or_default(
                "OPENAI_BASE_URL",
                settings.and_then(|value| value.openai_url.as_deref()),
                DEFAULT_OPENAI_URL,
            ))
            .map_err(|error| crate::error::AppError::Config(error.to_string()))?,
        }),
        LlmProviderType::Ollama => Ok(LlmProviderConfig::Ollama {
            url: env_setting_or_default(
                "MEMORY_BANK_OLLAMA_URL",
                settings.and_then(|value| value.ollama_url.as_deref()),
                DEFAULT_OLLAMA_URL,
            ),
            model: env_setting_or_default(
                "MEMORY_BANK_OLLAMA_MODEL",
                settings.and_then(|value| value.llm_model.as_deref()),
                DEFAULT_OLLAMA_MODEL,
            ),
        }),
    }
}

fn parse_optional_value<T>(value: Option<&str>) -> Result<Option<T>, crate::error::AppError>
where
    T: std::str::FromStr,
    T::Err: fmt::Display,
{
    match value {
        Some(value) => value
            .parse::<T>()
            .map(Some)
            .map_err(|error| crate::error::AppError::Config(error.to_string())),
        None => Ok(None),
    }
}

fn require_env(name: &str) -> Result<String, crate::error::AppError> {
    env::var(name).map_err(|_| crate::error::AppError::Config(format!("{name} must be set")))
}

fn env_setting_or_default(name: &str, setting: Option<&str>, default: &str) -> String {
    env::var(name)
        .ok()
        .or_else(|| setting.map(str::to_owned))
        .unwrap_or_else(|| default.to_string())
}

trait TestLlmConfigExt {
    fn with_model_override(self, model: String) -> Self;
}

impl TestLlmConfigExt for LlmProviderConfig {
    fn with_model_override(self, model: String) -> Self {
        match self {
            LlmProviderConfig::Gemini { api_key, .. } => {
                LlmProviderConfig::Gemini { api_key, model }
            }
            LlmProviderConfig::Anthropic { api_key, .. } => {
                LlmProviderConfig::Anthropic { api_key, model }
            }
            LlmProviderConfig::OpenAi {
                api_key, base_url, ..
            } => LlmProviderConfig::OpenAi {
                api_key,
                model,
                base_url,
            },
            LlmProviderConfig::Ollama { url, .. } => LlmProviderConfig::Ollama { url, model },
        }
    }
}

fn resolve_eval_model_override() -> Option<String> {
    env::var(LLM_EVAL_MODEL_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

async fn seed_fixture_db(
    notes: &[SeedNote],
    encoder: &EncoderClient,
) -> (MemoryDb, HashMap<String, i64>, HashMap<i64, String>) {
    let owned_keywords: Vec<Vec<String>> =
        notes.iter().map(SeedNote::keywords_as_strings).collect();
    let owned_tags: Vec<Vec<String>> = notes.iter().map(SeedNote::tags_as_strings).collect();
    let payloads: Vec<_> = notes
        .iter()
        .zip(&owned_keywords)
        .zip(&owned_tags)
        .map(|((note, keywords), tags)| EmbeddingInput {
            content: note.content,
            keywords: keywords.as_slice(),
            tags: tags.as_slice(),
            context: note.context,
        })
        .collect();
    let embeddings = encoder
        .encode_memories(&payloads)
        .await
        .expect("encode seed notes");
    let dimension = embeddings
        .first()
        .map(std::vec::Vec::len)
        .expect("seed embedding dimension");
    let db = MemoryDb::open_in_memory_for_tests(dimension)
        .await
        .expect("open in-memory eval db");
    create_processing_turn_table(&db).await;

    let mut slug_to_id = HashMap::with_capacity(notes.len());
    let mut id_to_slug = HashMap::with_capacity(notes.len());

    let mut tx = db.begin().await.expect("begin note tx");
    for (note, embedding) in notes.iter().zip(embeddings) {
        let id = db
            .insert_memory(
                &mut *tx,
                note.content,
                NOTE_TIMESTAMP,
                note.context,
                &serde_json::to_string(note.keywords).expect("keywords json"),
                &serde_json::to_string(note.tags).expect("tags json"),
            )
            .await
            .expect("insert seeded memory");
        db.insert_embedding(&mut *tx, id, &encoder::embedding_to_bytes(&embedding))
            .await
            .expect("insert seeded embedding");
        slug_to_id.insert(note.slug.to_string(), id);
        id_to_slug.insert(id, note.slug.to_string());
    }
    tx.commit().await.expect("commit note tx");

    (db, slug_to_id, id_to_slug)
}

async fn create_processing_turn_table(db: &MemoryDb) {
    sqlx::query(
        "CREATE TABLE ingest_turns (
            id INTEGER PRIMARY KEY,
            status TEXT NOT NULL,
            last_error TEXT,
            next_attempt_at TEXT,
            processing_started_at TEXT,
            stored_at TEXT,
            updated_at TEXT
        );",
    )
    .execute(db.pool_for_tests())
    .await
    .expect("create ingest_turns");
}

async fn insert_processing_turn(db: &MemoryDb, turn_id: i64) {
    sqlx::query(
        "INSERT INTO ingest_turns (
            id,
            status,
            next_attempt_at,
            processing_started_at,
            updated_at
        ) VALUES (?, 'processing', ?, ?, ?);",
    )
    .bind(turn_id)
    .bind(NOTE_TIMESTAMP)
    .bind(NOTE_TIMESTAMP)
    .bind(NOTE_TIMESTAMP)
    .execute(db.pool_for_tests())
    .await
    .expect("insert processing turn");
}

fn build_window(
    user_message: &'static str,
    assistant_reply: &'static str,
) -> ProjectedConversationWindow {
    ProjectedConversationWindow {
        previous_turns: Vec::new(),
        current_turn: MemoryProjection {
            user_message: user_message.to_string(),
            assistant_reply: assistant_reply.to_string(),
            steps: vec![MemoryStep::Thinking {
                text: "Reviewing nearby memories before responding.".to_string(),
            }],
        },
    }
}

fn build_fixtures() -> Vec<LlmFixture> {
    vec![
        LlmFixture {
            name: "links_obvious_related_memory",
            description: "Links a paraphrased follow-up to the clearly related existing memory only.",
            notes: vec![
                SeedNote {
                    slug: "tokio-related",
                    content: "Tokio mpsc backpressure means bounded senders have to wait when the receiver falls behind.",
                    context: "We discussed how bounded async queues in Rust intentionally slow producers under load.",
                    keywords: &["tokio", "mpsc", "backpressure"],
                    tags: &["rust", "async", "queue"],
                },
                SeedNote {
                    slug: "http-distractor",
                    content: "Trace correlation headers keep logs grouped under the same HTTP request identifier.",
                    context: "This note is about observability headers, not queues or producer pressure.",
                    keywords: &["http", "trace", "correlation"],
                    tags: &["observability", "http", "middleware"],
                },
                SeedNote {
                    slug: "cache-distractor",
                    content: "Single-flight cache refresh prevents a thundering herd on an expired hot key.",
                    context: "This note shares infra vocabulary but focuses on caching rather than async queues.",
                    keywords: &["cache", "single-flight", "stampede"],
                    tags: &["cache", "performance", "concurrency"],
                },
            ],
            window: build_window(
                "What was that Rust queue concept where producers have to slow down if a bounded channel fills up?",
                "That's the backpressure behavior in Tokio's bounded mpsc channels: senders wait until the receiver drains capacity.",
            ),
            expectation: FixtureExpectation {
                required_links: vec!["tokio-related"],
                forbidden_links: vec!["http-distractor", "cache-distractor"],
            },
        },
        LlmFixture {
            name: "avoids_bogus_links_for_shared_infra_words",
            description: "Avoids linking notes that only share broad infrastructure vocabulary.",
            notes: vec![
                SeedNote {
                    slug: "queue-neighbor",
                    content: "Queue pressure metrics help operators see when a worker backlog starts to rise.",
                    context: "This note is about generic queue monitoring, not HTTP correlation or request tracing.",
                    keywords: &["queue", "metrics", "backlog"],
                    tags: &["operations", "queueing", "monitoring"],
                },
                SeedNote {
                    slug: "cache-neighbor",
                    content: "Cache invalidation runs after deployment so stale hot keys are refreshed gradually.",
                    context: "This note is about cache rollout hygiene, not per-request identifiers.",
                    keywords: &["cache", "deployment", "refresh"],
                    tags: &["cache", "release", "operations"],
                },
                SeedNote {
                    slug: "storage-neighbor",
                    content: "Object storage retries should resume completed parts instead of starting the upload over.",
                    context: "This note concerns multipart uploads, not HTTP tracing or request metadata.",
                    keywords: &["storage", "multipart", "retry"],
                    tags: &["storage", "transfer", "resilience"],
                },
            ],
            window: build_window(
                "We should keep the same request id on every server span so one API call is easy to follow in logs.",
                "Yes. We should propagate a single trace correlation header through middleware and downstream handlers.",
            ),
            expectation: FixtureExpectation {
                required_links: Vec::new(),
                forbidden_links: vec!["queue-neighbor", "cache-neighbor", "storage-neighbor"],
            },
        },
        LlmFixture {
            name: "links_followup_decision_across_rewording",
            description: "Links a reworded follow-up back to the prior decision memory only.",
            notes: vec![
                SeedNote {
                    slug: "decision-note",
                    content: "The rollout decision was to keep a kill switch and automatic rollback guard during the staged release.",
                    context: "The team chose a safety-first staged rollout with a fast stop option if user-facing health degrades.",
                    keywords: &["rollout", "kill-switch", "rollback"],
                    tags: &["release", "safety", "operations"],
                },
                SeedNote {
                    slug: "schema-distractor",
                    content: "Schema changes should stay additive so older API clients keep working.",
                    context: "This note is about compatibility policy, not rollout decisions or feature flags.",
                    keywords: &["schema", "compatibility", "api"],
                    tags: &["api", "schema", "compatibility"],
                },
                SeedNote {
                    slug: "token-distractor",
                    content: "Refresh token rotation should detect replay of a stale credential.",
                    context: "This note is about credential security, not staged rollout safety decisions.",
                    keywords: &["oauth", "refresh-token", "rotation"],
                    tags: &["security", "authentication", "token-management"],
                },
            ],
            window: build_window(
                "What did we agree to do for that risky feature launch if health drops after the first partial release?",
                "We agreed to keep the staged release guarded by a fast kill switch and rollback path instead of pushing straight to everyone.",
            ),
            expectation: FixtureExpectation {
                required_links: vec!["decision-note"],
                forbidden_links: vec!["schema-distractor", "token-distractor"],
            },
        },
    ]
}

async fn run_fixture_once(
    fixture: &LlmFixture,
    run_index: usize,
    harness: &RealHarness,
) -> FixtureRunReport {
    let (db, _slug_to_id, id_to_slug) =
        seed_fixture_db(&fixture.notes, &harness.encoder_client).await;
    let turn_id = 10_000 + run_index as i64;
    insert_processing_turn(&db, turn_id).await;

    let mut pass_reasons = Vec::new();
    let mut fail_reasons = Vec::new();

    let result = process_turn_for_real_clients(
        &db,
        &harness.llm_client,
        &harness.encoder_client,
        NEAREST_NEIGHBOR_COUNT,
        turn_id,
        fixture.window.clone(),
        Utc::now(),
    )
    .await;

    if let Err(error) = result {
        fail_reasons.push(format!("processing failed: {error}"));
        return FixtureRunReport {
            fixture_name: fixture.name.to_string(),
            run_index,
            model_id: harness.llm_model_id.clone(),
            stored_memory_id: None,
            linked_target_slugs: Vec::new(),
            updated_neighbor_slugs: Vec::new(),
            keywords: Vec::new(),
            tags: Vec::new(),
            context_preview: String::new(),
            turn_status: None,
            passed: false,
            pass_reasons,
            fail_reasons,
        };
    }

    let stored_memory_id = sqlx::query_scalar::<_, i64>("SELECT MAX(id) FROM memories")
        .fetch_one(db.pool_for_tests())
        .await
        .expect("load stored memory id");
    let memory_row = sqlx::query("SELECT context, keywords, tags FROM memories WHERE id = ?")
        .bind(stored_memory_id)
        .fetch_one(db.pool_for_tests())
        .await
        .expect("load stored memory row");
    let context = memory_row.get::<String, _>("context");
    let keywords_json = memory_row.get::<String, _>("keywords");
    let tags_json = memory_row.get::<String, _>("tags");
    let keywords: Vec<String> = serde_json::from_str(&keywords_json).unwrap_or_default();
    let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();

    let linked_target_ids = sqlx::query_scalar::<_, i64>(
        "SELECT target_id FROM memory_links WHERE source_id = ? ORDER BY target_id",
    )
    .bind(stored_memory_id)
    .fetch_all(db.pool_for_tests())
    .await
    .expect("load linked target ids");
    let linked_target_slugs: Vec<String> = linked_target_ids
        .iter()
        .map(|id| {
            id_to_slug
                .get(id)
                .cloned()
                .unwrap_or_else(|| format!("unknown-id:{id}"))
        })
        .collect();

    let updated_neighbor_slugs =
        compute_updated_neighbor_slugs(&db, &fixture.notes, &id_to_slug).await;
    let turn_status =
        sqlx::query_scalar::<_, String>("SELECT status FROM ingest_turns WHERE id = ?")
            .bind(turn_id)
            .fetch_one(db.pool_for_tests())
            .await
            .ok();

    if turn_status.as_deref() == Some("stored") {
        pass_reasons.push("ingest turn transitioned to stored".to_string());
    } else {
        fail_reasons.push("ingest turn did not transition to stored".to_string());
    }

    if !context.trim().is_empty() {
        pass_reasons.push("stored memory has non-empty context".to_string());
    } else {
        fail_reasons.push("stored memory context was empty".to_string());
    }

    if !keywords.is_empty() {
        pass_reasons.push("stored memory has at least one keyword".to_string());
    } else {
        fail_reasons.push("stored memory had no keywords".to_string());
    }

    if !tags.is_empty() {
        pass_reasons.push("stored memory has at least one tag".to_string());
    } else {
        fail_reasons.push("stored memory had no tags".to_string());
    }

    for required in &fixture.expectation.required_links {
        if linked_target_slugs.iter().any(|slug| slug == required) {
            pass_reasons.push(format!("required link present: {required}"));
        } else {
            fail_reasons.push(format!("missing required link: {required}"));
        }
    }

    for forbidden in &fixture.expectation.forbidden_links {
        if linked_target_slugs.iter().any(|slug| slug == forbidden) {
            fail_reasons.push(format!("unexpected link present: {forbidden}"));
        } else {
            pass_reasons.push(format!("forbidden link absent: {forbidden}"));
        }
    }

    let context_preview = if context.chars().count() <= 180 {
        context
    } else {
        let preview: String = context.chars().take(180).collect();
        format!("{preview}...")
    };

    FixtureRunReport {
        fixture_name: fixture.name.to_string(),
        run_index,
        model_id: harness.llm_model_id.clone(),
        stored_memory_id: Some(stored_memory_id),
        linked_target_slugs,
        updated_neighbor_slugs,
        keywords,
        tags,
        context_preview,
        turn_status,
        passed: fail_reasons.is_empty(),
        pass_reasons,
        fail_reasons,
    }
}

async fn compute_updated_neighbor_slugs(
    db: &MemoryDb,
    notes: &[SeedNote],
    id_to_slug: &HashMap<i64, String>,
) -> Vec<String> {
    let mut updated = Vec::new();
    for note in notes {
        let row = sqlx::query("SELECT context, keywords, tags FROM memories WHERE content = ?")
            .bind(note.content)
            .fetch_one(db.pool_for_tests())
            .await
            .expect("load seeded note row");
        let context = row.get::<String, _>("context");
        let keywords = row.get::<String, _>("keywords");
        let tags = row.get::<String, _>("tags");
        let original_keywords = serde_json::to_string(note.keywords).expect("orig keywords json");
        let original_tags = serde_json::to_string(note.tags).expect("orig tags json");
        if context != note.context || keywords != original_keywords || tags != original_tags {
            let id = sqlx::query_scalar::<_, i64>("SELECT id FROM memories WHERE content = ?")
                .bind(note.content)
                .fetch_one(db.pool_for_tests())
                .await
                .expect("load updated neighbor id");
            updated.push(
                id_to_slug
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| format!("unknown-id:{id}")),
            );
        }
    }
    updated.sort();
    updated.dedup();
    updated
}

async fn run_fixture(fixture: &LlmFixture, harness: &RealHarness) -> FixtureReport {
    let mut runs = Vec::with_capacity(QUORUM_RUNS);
    let mut passed_runs = 0;

    for run_index in 1..=QUORUM_RUNS {
        let report = run_fixture_once(fixture, run_index, harness).await;
        if report.passed {
            passed_runs += 1;
        }
        runs.push(report);
    }

    FixtureReport {
        name: fixture.name.to_string(),
        description: fixture.description.to_string(),
        passed_runs,
        total_runs: QUORUM_RUNS,
        runs,
    }
}

async fn run_llm_eval(harness: &RealHarness) -> LlmEvalReport {
    let fixtures = build_fixtures();
    let mut reports = Vec::with_capacity(fixtures.len());

    for fixture in &fixtures {
        reports.push(run_fixture(fixture, harness).await);
    }

    LlmEvalReport {
        suite: "llm",
        provider_name: harness.llm_provider_name.clone(),
        encoder_model_id: harness.encoder_model_id.clone(),
        model_id: harness.llm_model_id.clone(),
        nearest_neighbor_count: NEAREST_NEIGHBOR_COUNT,
        quorum_runs: QUORUM_RUNS,
        quorum_required: QUORUM_REQUIRED,
        fixtures: reports,
    }
}

#[tokio::test]
#[ignore]
async fn real_llm_functional_eval() {
    if !evals_enabled("real_llm_functional_eval") {
        return;
    }

    let _guard = real_eval_lock().lock().await;
    let harness = initialize_real_harness();
    let report = run_llm_eval(&harness).await;
    let pretty = serde_json::to_string_pretty(&report).expect("serialize llm eval report");
    println!("{pretty}");

    for fixture in &report.fixtures {
        assert!(fixture.passed_runs >= QUORUM_REQUIRED, "{pretty}");
    }
}
