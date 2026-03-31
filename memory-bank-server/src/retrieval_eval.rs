use crate::actor::retrieve_with_encoder;
use crate::config::EncoderProviderConfig;
use crate::db::MemoryDb;
use crate::encoder::{self, EmbeddingInput, EncoderClient};
use memory_bank_app::{AppPaths, DEFAULT_FASTEMBED_MODEL};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;
use tokio::sync::Mutex;

const RETRIEVAL_EVAL_ENV: &str = "MEMORY_BANK_RETRIEVAL_EVALS";
const NOTE_BATCH_SIZE: usize = 256;
const FIXED_TIMESTAMP: &str = "2026-03-30T00:00:00Z";

#[derive(Debug, Clone)]
struct SeedNote {
    slug: String,
    content: String,
    context: String,
    keywords: Vec<String>,
    tags: Vec<String>,
    linked_to: Vec<String>,
}

impl SeedNote {
    fn embedding_input(&self) -> EmbeddingInput<'_> {
        EmbeddingInput {
            content: &self.content,
            keywords: &self.keywords,
            tags: &self.tags,
            context: &self.context,
        }
    }
}

#[derive(Debug, Clone)]
struct QueryCase {
    name: String,
    query: String,
    expected_slugs: Vec<String>,
    expected_linked_slug: Option<String>,
}

#[derive(Debug)]
struct EvalCorpus {
    notes: Vec<SeedNote>,
    queries: Vec<QueryCase>,
}

struct SeededCorpus {
    db: MemoryDb,
    slug_by_content: HashMap<String, String>,
    slug_by_id: HashMap<i64, String>,
    note_count: usize,
}

#[derive(Debug, Serialize)]
struct QueryEvalReport {
    name: String,
    query: String,
    expected_slugs: Vec<String>,
    returned_slugs: Vec<String>,
    knn_returned_slugs: Vec<String>,
    recall: f64,
    reciprocal_rank: f64,
    knn_reciprocal_rank: f64,
    linked_hit: Option<bool>,
    duration_ms: f64,
    knn_matches: usize,
    expanded_matches: usize,
    returned_count: usize,
    link_expansion_inflation: f64,
}

#[derive(Debug, Serialize)]
struct MatrixRowReport {
    label: String,
    note_count: usize,
    query_count: usize,
    nearest_neighbor_count: i32,
    mean_recall: f64,
    mean_reciprocal_rank: f64,
    mean_knn_reciprocal_rank: f64,
    linked_hit_rate: Option<f64>,
    query_p50_ms: f64,
    query_p95_ms: f64,
    total_runtime_ms: f64,
    avg_link_expansion_inflation: f64,
    queries: Vec<QueryEvalReport>,
}

#[derive(Debug, Serialize)]
struct GoldThresholds {
    min_recall_at_5: f64,
    min_recall_at_10: f64,
    min_mrr_at_10: f64,
    min_linked_hit_rate_at_10: f64,
}

#[derive(Debug, Serialize)]
struct GoldEvalReport {
    suite: &'static str,
    encoder_model: String,
    note_count: usize,
    family_count: usize,
    thresholds: GoldThresholds,
    rows: Vec<MatrixRowReport>,
}

#[derive(Debug, Clone, Copy)]
struct GoldFamilySpec {
    slug: &'static str,
    canonical_term: &'static str,
    alias_phrase: &'static str,
    primary_focus: &'static str,
    support_focus: &'static str,
    distractor_focus: &'static str,
    exact_query: &'static str,
    alias_query: &'static str,
    keywords: [&'static str; 3],
    tags: [&'static str; 3],
    linked: bool,
}

const GOLD_FAMILY_SPECS: &[GoldFamilySpec] = &[
    GoldFamilySpec {
        slug: "tokio-backpressure",
        canonical_term: "Tokio mpsc backpressure",
        alias_phrase: "bounded async queue pressure",
        primary_focus: "bounded senders have to slow down when the receiver lags behind",
        support_focus: "producer throttling and channel sizing in Rust services",
        distractor_focus: "oneshot reply routing for a single response path",
        exact_query: "tokio mpsc backpressure",
        alias_query: "How do bounded Rust message queues slow producers when receivers fall behind?",
        keywords: ["tokio", "mpsc", "backpressure"],
        tags: ["rust", "async", "queue"],
        linked: true,
    },
    GoldFamilySpec {
        slug: "sqlite-vec",
        canonical_term: "sqlite-vec nearest-neighbor retrieval",
        alias_phrase: "embedding distance lookup in SQLite",
        primary_focus: "semantic matches are ordered by vector distance inside a local SQLite index",
        support_focus: "query-side embeddings feed top-k neighbor selection before graph expansion",
        distractor_focus: "SQL migration ordering and transactional schema setup",
        exact_query: "sqlite vec nearest neighbor retrieval",
        alias_query: "semantic lookup by embedding distance in SQLite",
        keywords: ["sqlite-vec", "embedding", "knn"],
        tags: ["database", "retrieval", "vector-search"],
        linked: true,
    },
    GoldFamilySpec {
        slug: "trace-correlation",
        canonical_term: "HTTP trace correlation header",
        alias_phrase: "request-scoped trace identifier",
        primary_focus: "incoming requests carry a header that keeps logs and spans tied together",
        support_focus: "middleware propagates the same identifier through downstream handlers",
        distractor_focus: "listener bind addresses and local development ports",
        exact_query: "HTTP trace correlation header",
        alias_query: "how do we keep one request identifier attached to every server span",
        keywords: ["http", "trace", "correlation"],
        tags: ["observability", "http", "middleware"],
        linked: true,
    },
    GoldFamilySpec {
        slug: "staged-rollout",
        canonical_term: "staged release safety guard",
        alias_phrase: "progressive rollout kill switch",
        primary_focus: "a feature flag rollout pauses automatically when user-facing health degrades",
        support_focus: "operators keep a fast rollback and kill switch during partial release",
        distractor_focus: "metric naming conventions for product experiments",
        exact_query: "staged release safety guard",
        alias_query: "partial deployment kill switch for a risky feature",
        keywords: ["feature-flag", "rollout", "rollback"],
        tags: ["release", "safety", "operations"],
        linked: true,
    },
    GoldFamilySpec {
        slug: "cdc-stream",
        canonical_term: "logical replication slot lag",
        alias_phrase: "change-data-capture stream delay",
        primary_focus: "replication slots can fall behind and increase retained WAL volume",
        support_focus: "operators monitor slot lag before downstream consumers drift too far",
        distractor_focus: "vacuum freeze policy for long-lived tables",
        exact_query: "logical replication slot lag",
        alias_query: "change-data-capture stream delay in Postgres",
        keywords: ["postgres", "replication", "slot-lag"],
        tags: ["database", "replication", "operations"],
        linked: true,
    },
    GoldFamilySpec {
        slug: "pod-eviction",
        canonical_term: "voluntary eviction guardrail",
        alias_phrase: "pod disruption budget protection",
        primary_focus: "cluster maintenance should not evict too many replicas at once",
        support_focus: "node drains respect availability budgets during rolling operations",
        distractor_focus: "CPU and memory requests for batch workers",
        exact_query: "voluntary eviction guardrail",
        alias_query: "how do we stop a node drain from removing too many pods",
        keywords: ["kubernetes", "eviction", "pdb"],
        tags: ["kubernetes", "availability", "operations"],
        linked: true,
    },
    GoldFamilySpec {
        slug: "multipart-upload",
        canonical_term: "multipart upload resume",
        alias_phrase: "chunked object transfer restart",
        primary_focus: "large object uploads should continue from the completed parts after a failure",
        support_focus: "retry logic keeps successful parts and only resends missing chunks",
        distractor_focus: "lifecycle expiration rules for stale buckets",
        exact_query: "multipart upload resume",
        alias_query: "resume a chunked object transfer after the network drops",
        keywords: ["s3", "multipart", "retry"],
        tags: ["storage", "transfer", "resilience"],
        linked: false,
    },
    GoldFamilySpec {
        slug: "api-compat",
        canonical_term: "contract-safe API evolution",
        alias_phrase: "OpenAPI additive compatibility",
        primary_focus: "schema updates should stay additive so older clients keep working",
        support_focus: "new fields and optional enums roll out without breaking callers",
        distractor_focus: "example snippets inside documentation pages",
        exact_query: "contract-safe API evolution",
        alias_query: "OpenAPI additive compatibility for an existing client",
        keywords: ["openapi", "schema", "compatibility"],
        tags: ["api", "schema", "compatibility"],
        linked: false,
    },
    GoldFamilySpec {
        slug: "cache-stampede",
        canonical_term: "thundering herd prevention",
        alias_phrase: "cache stampede control",
        primary_focus: "only one worker should recompute an expired value while others wait",
        support_focus: "single-flight coordination reduces repeated cache regeneration work",
        distractor_focus: "picking an LRU or LFU eviction policy",
        exact_query: "thundering herd prevention",
        alias_query: "cache stampede control for a hot key",
        keywords: ["cache", "stampede", "single-flight"],
        tags: ["cache", "performance", "concurrency"],
        linked: false,
    },
    GoldFamilySpec {
        slug: "alert-dedup",
        canonical_term: "incident noise suppression",
        alias_phrase: "alert deduplication",
        primary_focus: "repeat pages from the same failure should collapse into one ongoing incident",
        support_focus: "grouping rules cut down repeated pager notifications",
        distractor_focus: "handoff timing between on-call rotations",
        exact_query: "incident noise suppression",
        alias_query: "alert deduplication for the same outage",
        keywords: ["alerts", "deduplication", "on-call"],
        tags: ["observability", "incident-response", "operations"],
        linked: false,
    },
    GoldFamilySpec {
        slug: "refresh-rotation",
        canonical_term: "refresh token rotation reuse detection",
        alias_phrase: "credential renewal replay check",
        primary_focus: "a rotated refresh token should be invalidated if an old token is replayed",
        support_focus: "security systems flag reuse when a stale credential appears again",
        distractor_focus: "scope descriptions on the consent screen",
        exact_query: "refresh token rotation reuse detection",
        alias_query: "detect replay of an older refresh credential",
        keywords: ["oauth", "refresh-token", "rotation"],
        tags: ["security", "authentication", "token-management"],
        linked: false,
    },
    GoldFamilySpec {
        slug: "type-two-dimension",
        canonical_term: "historical attribute versioning",
        alias_phrase: "slowly changing dimension type two",
        primary_focus: "analytics systems keep prior attribute values by writing a new versioned row",
        support_focus: "effective dates track when a warehouse dimension changed",
        distractor_focus: "partition pruning for time-range scans",
        exact_query: "historical attribute versioning",
        alias_query: "slowly changing dimension type two rows in a warehouse",
        keywords: ["warehouse", "dimension", "history"],
        tags: ["analytics", "data-warehouse", "modeling"],
        linked: false,
    },
];

fn eval_lock() -> &'static Mutex<()> {
    static EVAL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    EVAL_LOCK.get_or_init(|| Mutex::new(()))
}

fn evals_enabled(test_name: &str) -> bool {
    if env::var(RETRIEVAL_EVAL_ENV).ok().as_deref() == Some("1") {
        true
    } else {
        eprintln!("skipping {test_name} because {RETRIEVAL_EVAL_ENV}=1 is not set");
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

fn initialize_real_encoder() -> encoder::InitializedEncoder {
    let model = env::var("MEMORY_BANK_FASTEMBED_MODEL")
        .unwrap_or_else(|_| DEFAULT_FASTEMBED_MODEL.to_string());
    encoder::initialize(
        EncoderProviderConfig::FastEmbed { model },
        &default_models_dir(),
    )
    .expect("initialize real fastembed encoder")
}

async fn seed_corpus(notes: &[SeedNote], encoder: &EncoderClient) -> SeededCorpus {
    assert!(!notes.is_empty(), "eval corpus must contain notes");

    let mut chunks = notes.chunks(NOTE_BATCH_SIZE);
    let first_chunk = chunks.next().expect("first note chunk");
    let first_embeddings = encode_note_chunk(encoder, first_chunk).await;
    let dimension = first_embeddings
        .first()
        .map(std::vec::Vec::len)
        .expect("non-empty first embedding batch");
    let db = MemoryDb::open_in_memory_for_tests(dimension)
        .await
        .expect("open eval db");

    let mut id_by_slug = HashMap::with_capacity(notes.len());
    let mut slug_by_content = HashMap::with_capacity(notes.len());
    let mut slug_by_id = HashMap::with_capacity(notes.len());

    insert_note_chunk(
        &db,
        first_chunk,
        &first_embeddings,
        &mut id_by_slug,
        &mut slug_by_content,
        &mut slug_by_id,
    )
    .await;

    for chunk in chunks {
        let embeddings = encode_note_chunk(encoder, chunk).await;
        insert_note_chunk(
            &db,
            chunk,
            &embeddings,
            &mut id_by_slug,
            &mut slug_by_content,
            &mut slug_by_id,
        )
        .await;
    }

    let mut unique_links = BTreeSet::new();
    for note in notes {
        for target in &note.linked_to {
            let left = note.slug.as_str().min(target.as_str()).to_string();
            let right = note.slug.as_str().max(target.as_str()).to_string();
            unique_links.insert((left, right));
        }
    }

    if !unique_links.is_empty() {
        let mut tx = db.begin().await.expect("begin link tx");
        for (left, right) in unique_links {
            let left_id = *id_by_slug
                .get(&left)
                .unwrap_or_else(|| panic!("missing left link slug {left}"));
            let right_id = *id_by_slug
                .get(&right)
                .unwrap_or_else(|| panic!("missing right link slug {right}"));
            db.insert_bidirectional_links(&mut tx, left_id, right_id)
                .await
                .expect("insert eval link");
        }
        tx.commit().await.expect("commit link tx");
    }

    SeededCorpus {
        db,
        slug_by_content,
        slug_by_id,
        note_count: notes.len(),
    }
}

async fn encode_note_chunk(encoder: &EncoderClient, chunk: &[SeedNote]) -> Vec<Vec<f32>> {
    let payloads: Vec<EmbeddingInput<'_>> = chunk.iter().map(SeedNote::embedding_input).collect();
    encoder
        .encode_memories(&payloads)
        .await
        .expect("encode note chunk")
}

async fn insert_note_chunk(
    db: &MemoryDb,
    chunk: &[SeedNote],
    embeddings: &[Vec<f32>],
    id_by_slug: &mut HashMap<String, i64>,
    slug_by_content: &mut HashMap<String, String>,
    slug_by_id: &mut HashMap<i64, String>,
) {
    let mut tx = db.begin().await.expect("begin note tx");
    for (note, embedding) in chunk.iter().zip(embeddings) {
        let keywords = serde_json::to_string(&note.keywords).expect("keywords json");
        let tags = serde_json::to_string(&note.tags).expect("tags json");
        let id = db
            .insert_memory(
                &mut *tx,
                &note.content,
                FIXED_TIMESTAMP,
                &note.context,
                &keywords,
                &tags,
            )
            .await
            .expect("insert eval note");
        db.insert_embedding(&mut *tx, id, &crate::encoder::embedding_to_bytes(embedding))
            .await
            .expect("insert eval embedding");
        id_by_slug.insert(note.slug.clone(), id);
        slug_by_content.insert(note.content.clone(), note.slug.clone());
        slug_by_id.insert(id, note.slug.clone());
    }
    tx.commit().await.expect("commit note tx");
}

async fn run_matrix_row(
    label: impl Into<String>,
    corpus: &EvalCorpus,
    encoder: &EncoderClient,
    nearest_neighbor_count: i32,
) -> MatrixRowReport {
    let seeded = seed_corpus(&corpus.notes, encoder).await;
    let suite_started_at = Instant::now();
    let mut queries = Vec::with_capacity(corpus.queries.len());

    for query_case in &corpus.queries {
        let query_started_at = Instant::now();
        let outcome = retrieve_with_encoder(
            &seeded.db,
            encoder,
            nearest_neighbor_count,
            &query_case.query,
        )
        .await
        .expect("run retrieval eval query");
        let duration_ms = query_started_at.elapsed().as_secs_f64() * 1000.0;
        let returned_slugs: Vec<String> = outcome
            .notes
            .iter()
            .map(|note| {
                seeded
                    .slug_by_content
                    .get(&note.content)
                    .cloned()
                    .unwrap_or_else(|| format!("unmapped:{}", note.content))
            })
            .collect();
        let knn_returned_slugs: Vec<String> = outcome
            .knn_ids
            .iter()
            .map(|id| {
                seeded
                    .slug_by_id
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| format!("unmapped-id:{id}"))
            })
            .collect();
        let returned_set: BTreeSet<&str> = returned_slugs.iter().map(String::as_str).collect();
        let hits = query_case
            .expected_slugs
            .iter()
            .filter(|slug| returned_set.contains(slug.as_str()))
            .count();
        let recall = hits as f64 / query_case.expected_slugs.len() as f64;
        let reciprocal_rank = query_case
            .expected_slugs
            .iter()
            .filter_map(|slug| {
                returned_slugs
                    .iter()
                    .position(|returned| returned == slug)
                    .map(|index| 1.0 / (index + 1) as f64)
            })
            .fold(0.0, f64::max);
        let knn_reciprocal_rank = query_case
            .expected_slugs
            .iter()
            .filter_map(|slug| {
                knn_returned_slugs
                    .iter()
                    .position(|returned| returned == slug)
                    .map(|index| 1.0 / (index + 1) as f64)
            })
            .fold(0.0, f64::max);
        let linked_hit = query_case
            .expected_linked_slug
            .as_ref()
            .map(|slug| returned_set.contains(slug.as_str()));
        let inflation = if outcome.knn_matches == 0 {
            0.0
        } else {
            outcome.expanded_matches as f64 / outcome.knn_matches as f64
        };

        queries.push(QueryEvalReport {
            name: query_case.name.clone(),
            query: query_case.query.clone(),
            expected_slugs: query_case.expected_slugs.clone(),
            returned_slugs,
            knn_returned_slugs,
            recall,
            reciprocal_rank,
            knn_reciprocal_rank,
            linked_hit,
            duration_ms,
            knn_matches: outcome.knn_matches,
            expanded_matches: outcome.expanded_matches,
            returned_count: outcome.notes.len(),
            link_expansion_inflation: inflation,
        });
    }

    MatrixRowReport {
        label: label.into(),
        note_count: seeded.note_count,
        query_count: queries.len(),
        nearest_neighbor_count,
        mean_recall: mean(queries.iter().map(|query| query.recall)),
        mean_reciprocal_rank: mean(queries.iter().map(|query| query.reciprocal_rank)),
        mean_knn_reciprocal_rank: mean(queries.iter().map(|query| query.knn_reciprocal_rank)),
        linked_hit_rate: linked_hit_rate(&queries),
        query_p50_ms: percentile(
            queries.iter().map(|query| query.duration_ms).collect(),
            0.50,
        ),
        query_p95_ms: percentile(
            queries.iter().map(|query| query.duration_ms).collect(),
            0.95,
        ),
        total_runtime_ms: suite_started_at.elapsed().as_secs_f64() * 1000.0,
        avg_link_expansion_inflation: mean(
            queries.iter().map(|query| query.link_expansion_inflation),
        ),
        queries,
    }
}

fn build_gold_corpus() -> EvalCorpus {
    let mut notes = Vec::with_capacity(GOLD_FAMILY_SPECS.len() * 3);
    let mut queries = Vec::with_capacity(GOLD_FAMILY_SPECS.len() * 2);

    for spec in GOLD_FAMILY_SPECS {
        let core_slug = format!("{}-core", spec.slug);
        let companion_slug = format!("{}-companion", spec.slug);
        let distractor_slug = format!("{}-distractor", spec.slug);

        notes.push(SeedNote {
            slug: core_slug.clone(),
            content: format!(
                "{} is the phrase we use when {}.",
                spec.canonical_term, spec.primary_focus
            ),
            context: format!(
                "Team shorthand also calls this \"{}\" when discussing {}.",
                spec.alias_phrase, spec.support_focus
            ),
            keywords: vec![
                spec.keywords[0].to_string(),
                spec.keywords[1].to_string(),
                spec.keywords[2].to_string(),
                spec.alias_phrase.to_string(),
            ],
            tags: spec.tags.iter().map(|tag| (*tag).to_string()).collect(),
            linked_to: if spec.linked {
                vec![companion_slug.clone()]
            } else {
                Vec::new()
            },
        });
        notes.push(SeedNote {
            slug: companion_slug.clone(),
            content: format!(
                "A companion note for {} focuses on {}.",
                spec.canonical_term, spec.support_focus
            ),
            context: format!(
                "Operators often describe this companion note with the phrase \"{}\".",
                spec.alias_phrase
            ),
            keywords: vec![
                spec.keywords[0].to_string(),
                spec.keywords[1].to_string(),
                spec.alias_phrase.to_string(),
            ],
            tags: vec![
                spec.tags[0].to_string(),
                spec.tags[1].to_string(),
                "companion-note".to_string(),
            ],
            linked_to: if spec.linked {
                vec![core_slug.clone()]
            } else {
                Vec::new()
            },
        });
        notes.push(SeedNote {
            slug: distractor_slug.clone(),
            content: format!(
                "{} is adjacent work in the same area, but it is not the same as {}.",
                spec.distractor_focus, spec.canonical_term
            ),
            context: format!(
                "This note shares the broad tags for {} without answering the same retrieval need.",
                spec.canonical_term
            ),
            keywords: vec![
                spec.tags[0].to_string(),
                spec.tags[1].to_string(),
                "adjacent".to_string(),
            ],
            tags: vec![
                spec.tags[0].to_string(),
                spec.tags[1].to_string(),
                "adjacent-note".to_string(),
            ],
            linked_to: Vec::new(),
        });

        let mut expected_exact = vec![core_slug.clone()];
        let mut expected_alias = vec![core_slug.clone()];
        let expected_linked = if spec.linked {
            expected_exact.push(companion_slug.clone());
            expected_alias.push(companion_slug.clone());
            Some(companion_slug)
        } else {
            None
        };

        queries.push(QueryCase {
            name: format!("{}-exact", spec.slug),
            query: spec.exact_query.to_string(),
            expected_slugs: expected_exact,
            expected_linked_slug: expected_linked.clone(),
        });
        queries.push(QueryCase {
            name: format!("{}-alias", spec.slug),
            query: spec.alias_query.to_string(),
            expected_slugs: expected_alias,
            expected_linked_slug: expected_linked,
        });
    }

    EvalCorpus { notes, queries }
}

async fn run_gold_eval(encoder_model: String, encoder: &EncoderClient) -> GoldEvalReport {
    let corpus = build_gold_corpus();
    let note_count = corpus.notes.len();
    let mut rows = Vec::new();

    for nearest_neighbor_count in [5, 10, 20] {
        rows.push(
            run_matrix_row(
                format!("gold@k={nearest_neighbor_count}"),
                &corpus,
                encoder,
                nearest_neighbor_count,
            )
            .await,
        );
    }

    GoldEvalReport {
        suite: "gold",
        encoder_model,
        note_count,
        family_count: GOLD_FAMILY_SPECS.len(),
        thresholds: GoldThresholds {
            min_recall_at_5: 0.80,
            min_recall_at_10: 0.90,
            min_mrr_at_10: 0.75,
            min_linked_hit_rate_at_10: 0.80,
        },
        rows,
    }
}

fn linked_hit_rate(queries: &[QueryEvalReport]) -> Option<f64> {
    let linked: Vec<bool> = queries
        .iter()
        .filter_map(|query| query.linked_hit)
        .collect();
    if linked.is_empty() {
        None
    } else {
        Some(mean(linked.iter().map(|hit| u8::from(*hit) as f64)))
    }
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let values: Vec<f64> = values.collect();
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn percentile(mut values: Vec<f64>, quantile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    let index = (((values.len() - 1) as f64) * quantile).round() as usize;
    values[index]
}

#[tokio::test]
#[ignore]
async fn real_encoder_gold_retrieval_eval() {
    if !evals_enabled("real_encoder_gold_retrieval_eval") {
        return;
    }

    let _guard = eval_lock().lock().await;
    let initialized = initialize_real_encoder();
    let report = run_gold_eval(initialized.model_id.clone(), &initialized.client).await;
    let pretty = serde_json::to_string_pretty(&report).expect("serialize gold report");
    println!("{pretty}");

    let row_at_5 = report
        .rows
        .iter()
        .find(|row| row.nearest_neighbor_count == 5)
        .expect("gold k=5 row");
    let row_at_10 = report
        .rows
        .iter()
        .find(|row| row.nearest_neighbor_count == 10)
        .expect("gold k=10 row");

    assert!(
        row_at_5.mean_recall >= report.thresholds.min_recall_at_5,
        "{pretty}"
    );
    assert!(
        row_at_10.mean_recall >= report.thresholds.min_recall_at_10,
        "{pretty}"
    );
    assert!(
        row_at_10.mean_reciprocal_rank >= report.thresholds.min_mrr_at_10,
        "{pretty}"
    );
    assert!(
        row_at_10.linked_hit_rate.unwrap_or(0.0) >= report.thresholds.min_linked_hit_rate_at_10,
        "{pretty}"
    );
}
