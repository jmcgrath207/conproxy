//! Hit-rate benchmark for the conproxy cache thesis.
//!
//! Measures exact-match hit rates against the real `CacheStore` on synthetic
//! agentic and Zipf traces, plus semantic-mode hit rates against the real
//! `SemanticCache` tier driven by a deterministic hashing embedder.
//! See `docs/strategy-assessment.md` §3.
//!
//! v2 scope:
//! - measured: exact hit rate (real cache), semantic hit rate + false-hit
//!   rate (real `SemanticCache`, synthetic hash embedder), τ frontier
//! - modeled: latency / cost / agent task-time savings (params, not measured)
//! - deferred: TTL expiry (fast replay = effectively infinite TTL), ONNX/API
//!   embedder fidelity, stale rate (needs CDC live mode)
//!
//! The synthetic embedder is a bag-of-words hashing trick: paraphrases that
//! share words land close (cosine ~0.7–1.0), unrelated clusters land far.
//! It measures the semantic *decision machinery* (threshold, LRU, false-hit
//! accounting), not production embedder quality.
//!
//! Verdict: FAIL-CORE (exit 2) if the agentic workload misses the exact
//! hit-rate gate; FAIL-TRUST (exit 3) if semantic mode runs and no τ clears
//! the false-hit gate with positive uplift. Otherwise PASS.

// Bench binary: pointer/index arithmetic and vocab indexing dominate; RNG and
// Zipf sampling are noise-free by construction (deterministic seed).
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_arguments
)]

use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

#[cfg(feature = "embed-api")]
use conproxy::proxy::semantic_cache::SemanticCache;
use conproxy::proxy::types::QueryHash;
use conproxy::proxy::{CacheStatus, CacheStore, QueryResponse, SearchResult};

// ---------------------------------------------------------------- rng

/// xorshift64* — deterministic, dependency-free, reproducible across hosts.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in [0, n).
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    fn bernoulli(&mut self, p: f64) -> bool {
        self.f64() < p
    }
}

// ---------------------------------------------------------------- trace

#[derive(Clone)]
struct TraceEvent {
    query: String,
    cluster: u64,
}

const VOCAB: [&str; 50] = [
    "login", "password", "reset", "account", "billing", "payment", "refund", "order", "shipping",
    "invoice", "error", "crash", "install", "update", "delete", "backup", "restore", "sync",
    "export", "import", "api", "token", "webhook", "quota", "limit", "latency", "timeout",
    "config", "deploy", "rollback", "cache", "index", "search", "vector", "embed", "model",
    "agent", "retry", "session", "auth", "scope", "filter", "tenant", "region", "cluster", "node",
    "shard", "replica", "snapshot", "migrate",
];

const SYNONYMS: [(&str, &str); 10] = [
    ("login", "signin"),
    ("password", "passcode"),
    ("reset", "restore"),
    ("error", "failure"),
    ("payment", "billing"),
    ("refund", "chargeback"),
    ("order", "purchase"),
    ("account", "profile"),
    ("delete", "remove"),
    ("update", "modify"),
];

/// Deterministic 3-word text for a pool/rank id (mixed radix over VOCAB).
/// Words are emitted in sorted order so the (text → meaning) mapping is
/// canonical: two ids that select the same word multiset produce the SAME
/// text. Without this, bag-of-words paraphrases would make distinct clusters
/// textually identical and corrupt false-hit ground truth.
fn id_text(id: u64) -> String {
    let mut w = [
        VOCAB[(id % 50) as usize],
        VOCAB[((id / 50) % 50) as usize],
        VOCAB[((id / 2500) % 50) as usize],
    ];
    w.sort_unstable();
    format!("{} {} {}", w[0], w[1], w[2])
}

/// Cluster identity = the canonical (pre-paraphrase) text. A paraphrase keeps
/// the cluster of the text it was derived from; identical texts always share
/// a cluster.
fn cluster_of(text: &str) -> u64 {
    let h = blake3::hash(text.as_bytes());
    u64::from_le_bytes(h.as_bytes()[..8].try_into().unwrap_or([0u8; 8]))
}

/// Meaning-preserving surface transform: word shuffle, synonym swap, or
/// filler template. Same cluster, different cache key.
fn paraphrase(rng: &mut Rng, text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    match rng.below(3) {
        0 => {
            let mut w = words.clone();
            // Fisher–Yates over 3 words.
            for i in (1..w.len()).rev() {
                let j = rng.below(i + 1);
                w.swap(i, j);
            }
            w.join(" ")
        }
        1 => {
            let mut out = text.to_string();
            for (a, b) in SYNONYMS {
                if out.contains(a) {
                    out = out.replacen(a, b, 1);
                    break;
                }
            }
            out
        }
        _ => {
            const TEMPLATES: [&str; 3] = ["how to {}", "help with {}", "please {}"];
            let t = TEMPLATES[rng.below(TEMPLATES.len())];
            t.replacen("{}", text, 1)
        }
    }
}

fn maybe_paraphrase(rng: &mut Rng, text: &str, rate: f64) -> String {
    if rate > 0.0 && rng.bernoulli(rate) {
        paraphrase(rng, text)
    } else {
        text.to_string()
    }
}

/// Zipf popularity over `unique` queries: p(rank) ∝ rank^-s.
/// `pool_texts`: real query texts (MS MARCO/ORCAS) replacing synthetic id_text;
/// rank wraps modulo pool length.
fn gen_zipf(
    rng: &mut Rng,
    unique: usize,
    s: f64,
    queries: usize,
    para_rate: f64,
    pool_texts: Option<&[String]>,
) -> Vec<TraceEvent> {
    let mut prefix = Vec::with_capacity(unique);
    let mut acc = 0.0f64;
    for rank in 1..=unique {
        acc += (rank as f64).powf(-s);
        prefix.push(acc);
    }
    let total = acc;
    (0..queries)
        .map(|_| {
            let x = rng.f64() * total;
            let rank = prefix.partition_point(|&c| c < x); // first prefix >= x
            let rank = rank.min(unique - 1) as u64;
            let base = match pool_texts {
                Some(p) => p[(rank as usize) % p.len()].clone(),
                None => id_text(rank),
            };
            TraceEvent {
                cluster: cluster_of(&base),
                query: maybe_paraphrase(rng, &base, para_rate),
            }
        })
        .collect()
}

/// Agentic workload: `tasks` tasks × `agents` agents × `calls` retrieval
/// calls. Each task draws a shared working set from a pool of P sub-queries;
/// agents re-query within their own loop with prob `requery` and collide
/// with sibling agents via the shared working set.
#[allow(clippy::too_many_arguments)]
fn gen_agentic(
    rng: &mut Rng,
    tasks: usize,
    calls: usize,
    requery: f64,
    agents: usize,
    pool: usize,
    para_rate: f64,
    pool_texts: Option<&[String]>,
) -> Vec<TraceEvent> {
    let mut events = Vec::with_capacity(tasks * agents * calls);
    for _task in 0..tasks {
        // Shared working set: ~calls*(1-requery)+2 distinct pool entries.
        let k = ((calls as f64) * (1.0 - requery) + 2.0) as usize;
        let working: Vec<u64> = (0..k).map(|_| rng.below(pool) as u64).collect();
        for _agent in 0..agents {
            let mut emitted: Vec<u64> = Vec::with_capacity(calls);
            for _ in 0..calls {
                let id = if !emitted.is_empty() && rng.bernoulli(requery) {
                    emitted[rng.below(emitted.len())]
                } else {
                    working[rng.below(working.len())]
                };
                emitted.push(id);
                let base = match pool_texts {
                    Some(p) => p[(id as usize) % p.len()].clone(),
                    None => id_text(id),
                };
                events.push(TraceEvent {
                    cluster: cluster_of(&base),
                    query: maybe_paraphrase(rng, &base, para_rate),
                });
            }
        }
    }
    events
}

/// JSONL replay: {"query": "...", "cluster_id": 7}. cluster_id optional
/// (defaults to a hash of the query → every unique text its own cluster).
fn load_replay(path: &Path) -> Result<Vec<TraceEvent>, String> {
    let raw = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut events = Vec::new();
    for (lineno, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| format!("{}:{}: {e}", path.display(), lineno + 1))?;
        let query = v
            .get("query")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("{}:{}: missing \"query\"", path.display(), lineno + 1))?;
        let cluster = v
            .get("cluster_id")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_else(|| {
                let h = blake3::hash(query.as_bytes());
                u64::from_le_bytes(h.as_bytes()[..8].try_into().unwrap_or([0u8; 8]))
            });
        events.push(TraceEvent {
            query: query.to_string(),
            cluster,
        });
    }
    Ok(events)
}

/// Real query-text pool (MS MARCO / ORCAS adapter). One query per line; TSV
/// rows take the SECOND tab field (MS MARCO `qid\tquery`, ORCAS
/// `qid\tquery\t...`). Feeds zipf/agentic generators in place of synthetic
/// id_text; pool wraps modulo when unique/pool params exceed file size.
fn load_queries_file(path: &Path) -> Result<Vec<String>, String> {
    let raw = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let text = if line.contains('\t') {
            line.split('\t').nth(1).unwrap_or(line).trim()
        } else {
            line
        };
        if !text.is_empty() {
            out.push(text.to_string());
        }
    }
    if out.is_empty() {
        return Err(format!("{}: no queries found", path.display()));
    }
    Ok(out)
}

// ---------------------------------------------------------------- embed

/// Synthetic embedder dimension. First `KNOWN_WORDS.len()` coordinates are a
/// dedicated orthogonal basis (one per known word); the tail absorbs unknown
/// words via hashing. Deterministic, collision-free for bench vocabulary.
#[cfg(feature = "embed-api")]
const SEM_DIM: usize = 128;

/// Known words → dedicated coordinates 0..N. VOCAB + synonym targets +
/// template fillers. Unknown words hash into the 64..128 tail (rare).
#[cfg(feature = "embed-api")]
fn known_word_index(w: &str) -> Option<usize> {
    // VOCAB occupies 0..50 in declaration order.
    let idx = VOCAB.iter().position(|&v| v == w);
    if let Some(i) = idx {
        return Some(i);
    }
    // Synonym targets + fillers occupy 50..; small linear scan is fine.
    const EXTRA: [&str; 16] = [
        "signin",
        "passcode",
        "chargeback",
        "purchase",
        "profile",
        "remove",
        "modify",
        "failure",
        "how",
        "to",
        "help",
        "with",
        "please",
        "what",
        "is",
        "the",
    ];
    EXTRA.iter().position(|&v| v == w).map(|i| 50 + i)
}

/// Bag-of-words embedder on a near-orthogonal basis: each known word gets its
/// own coordinate; unknown words hash into a spillover tail. L2-normalized.
///
/// Similarity structure by construction:
/// - word shuffle (same bag)        → cosine 1.0
/// - template paraphrase (+2 words) → cosine ≈ 0.77
/// - synonym swap (1 of 3 words)    → cosine ≈ 0.67 (or higher if the swap
///   lands on another VOCAB word — realistic synonym drift)
/// - neighbor cluster (2 of 3 shared) → cosine ≈ 0.67
/// - unrelated cluster              → cosine ≈ 0
///
/// This is an *idealized* embedder with a known similarity geometry: it
/// validates the semantic cache machinery (τ, LRU, false-hit accounting)
/// without ONNX/API deps. Real embedders blur these bands — that is why the
/// live-embedder mode remains on the roadmap.
#[cfg(feature = "embed-api")]
fn embed(text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; SEM_DIM];
    for tok in text.split_whitespace() {
        let idx = known_word_index(tok).unwrap_or_else(|| {
            let h = blake3::hash(tok.as_bytes());
            64 + (h.as_bytes()[0] as usize) % (SEM_DIM - 64)
        });
        v[idx] += 1.0;
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

/// Query hash keyed on raw query text (same basis as the exact cache key in
/// this bench). Used by the TTL virtual-clock bookkeeping in all builds, and
/// by the semantic tier under `embed-api`.
fn query_hash(text: &str) -> QueryHash {
    *blake3::hash(text.as_bytes()).as_bytes()
}

// ------------------------------------------------------ embedder backends

/// Pluggable embedder for the semantic tier.
///
/// - `Synthetic`: deterministic bag-of-words with known similarity geometry.
/// - `Onnx`: real local sentence-transformer via prod `Embedder` (feature
///   `embed`, sync API).
/// - `Api`: remote provider via prod `create_provider` (async, driven on a
///   current-thread runtime).
#[cfg(feature = "embed-api")]
enum BenchEmbedder {
    Synthetic,
    #[cfg(feature = "embed")]
    Onnx(conproxy::embedding::embedder::Embedder),
    Api {
        provider: std::sync::Arc<dyn conproxy::embedding::provider::EmbedderProvider>,
        runtime: tokio::runtime::Runtime,
    },
}

#[cfg(feature = "embed-api")]
impl BenchEmbedder {
    fn embed_one(&self, text: &str) -> Result<Vec<f32>, String> {
        match self {
            Self::Synthetic => Ok(embed(text)),
            #[cfg(feature = "embed")]
            Self::Onnx(e) => e.embed(text).map_err(|err| err.to_string()),
            Self::Api { provider, runtime } => runtime
                .block_on(provider.embed(text))
                .map_err(|err| err.to_string()),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Synthetic => "synthetic",
            #[cfg(feature = "embed")]
            Self::Onnx(_) => "onnx",
            Self::Api { .. } => "api",
        }
    }
}

/// Memoizing wrapper: embed each unique text once, reuse across τ/cache
/// sweep points and workloads. Keeps ONNX/API cost proportional to unique
/// texts, not events.
#[cfg(feature = "embed-api")]
struct MemoEmbedder {
    inner: BenchEmbedder,
    memo: HashMap<String, Vec<f32>>,
}

#[cfg(feature = "embed-api")]
impl MemoEmbedder {
    fn new(inner: BenchEmbedder) -> Self {
        Self {
            inner,
            memo: HashMap::new(),
        }
    }

    fn embed_memo(&mut self, text: &str) -> Result<&Vec<f32>, String> {
        if !self.memo.contains_key(text) {
            let v = self.inner.embed_one(text)?;
            self.memo.insert(text.to_string(), v);
        }
        self.memo
            .get(text)
            .ok_or_else(|| "memo insert failed".to_string())
    }
}

/// Build the semantic-tier embedder from CLI args.
#[cfg(feature = "embed-api")]
fn build_embedder(args: &Args) -> Result<BenchEmbedder, String> {
    let kind = args.embedder.as_deref().unwrap_or("synthetic");
    match kind {
        "synthetic" => Ok(BenchEmbedder::Synthetic),
        "onnx" => build_onnx_embedder(args),
        "api" => build_api_embedder(args),
        other => Err(format!(
            "unknown --embedder {other:?} (expected synthetic|onnx|api)"
        )),
    }
}

#[cfg(feature = "embed")]
fn build_onnx_embedder(args: &Args) -> Result<BenchEmbedder, String> {
    use conproxy::embedding::embedder::Embedder;
    use conproxy::embedding::models::ModelManager;

    let name = args.embed_model.as_deref().unwrap_or("all-MiniLM-L6-v2");
    if !ModelManager::is_installed(name) {
        return Err(format!(
            "ONNX model {name:?} not installed under {} \
             (need model.onnx + tokenizer.json)",
            ModelManager::models_dir().display()
        ));
    }
    Embedder::new(
        ModelManager::model_path(name),
        ModelManager::tokenizer_path(name),
    )
    .map(BenchEmbedder::Onnx)
    .map_err(|e| format!("ONNX embedder init: {e}"))
}

#[cfg(all(feature = "embed-api", not(feature = "embed")))]
fn build_onnx_embedder(_args: &Args) -> Result<BenchEmbedder, String> {
    Err("--embedder onnx requires `--features embed` (ONNX runtime)".to_string())
}

#[cfg(feature = "embed-api")]
fn build_api_embedder(args: &Args) -> Result<BenchEmbedder, String> {
    use conproxy::embedding::provider::{create_provider, ProviderConfig, ProviderType};

    let provider_name = args.embed_provider.as_deref().unwrap_or("openai");
    if provider_name == "mock" {
        return build_mock_api_embedder();
    }
    let (ptype, default_model, default_key_var) = match provider_name {
        "openai" => (
            ProviderType::OpenAi,
            "text-embedding-3-small",
            "OPENAI_API_KEY",
        ),
        "cohere" => (ProviderType::Cohere, "embed-english-v3.0", "COHERE_API_KEY"),
        "huggingface" => (
            ProviderType::HuggingFace,
            "sentence-transformers/all-MiniLM-L6-v2",
            "HF_API_KEY",
        ),
        other => {
            return Err(format!(
                "unknown --embed-provider {other:?} (expected openai|cohere|huggingface|mock)"
            ))
        }
    };
    let model = args
        .embed_model
        .clone()
        .unwrap_or_else(|| default_model.to_string());
    let key_var = args
        .embed_api_key_var
        .clone()
        .unwrap_or_else(|| default_key_var.to_string());
    if std::env::var(&key_var).is_err() {
        return Err(format!(
            "--embedder api needs ${key_var} set (or --embed-api-key-var NAME)"
        ));
    }
    let config = ProviderConfig {
        provider: ptype,
        model_name: model,
        api_key: Some(format!("${{{key_var}}}")),
        base_url: None,
        request_timeout: Duration::from_secs(30),
    };
    let provider = create_provider(&config).map_err(|e| format!("provider init: {e}"))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    Ok(BenchEmbedder::Api { provider, runtime })
}

/// Live wire test for the API embedder path without real keys: serve the
/// OpenAI `/v1/embeddings` wire format on 127.0.0.1 (ephemeral port) backed
/// by the synthetic bag-of-words embedder, then point the prod OpenAI
/// provider at it. Validates HTTP transport, request/response JSON, auth
/// header plumbing, and the current-thread runtime integration end-to-end.
/// Known similarity geometry is preserved (same `embed` fn), so semantic
/// sweep results should match `--embedder synthetic`.
#[cfg(feature = "embed-api")]
fn build_mock_api_embedder() -> Result<BenchEmbedder, String> {
    use conproxy::embedding::provider::{create_provider, ProviderConfig, ProviderType};

    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("mock bind: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("mock nonblocking: {e}"))?;
    let addr = listener
        .local_addr()
        .map_err(|e| format!("mock addr: {e}"))?;
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("mock server runtime: {e}");
                return;
            }
        };
        rt.block_on(async move {
            async fn handle(
                axum::Json(body): axum::Json<serde_json::Value>,
            ) -> axum::Json<serde_json::Value> {
                let text = body
                    .get("input")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                let embedding = embed(text);
                axum::Json(serde_json::json!({
                    "object": "list",
                    "data": [{"object": "embedding", "index": 0, "embedding": embedding}],
                    "model": body.get("model").cloned().unwrap_or(serde_json::Value::Null),
                    "usage": {"prompt_tokens": 0, "total_tokens": 0},
                }))
            }
            let app = axum::Router::new().route("/v1/embeddings", axum::routing::post(handle));
            let serve = async {
                let listener = tokio::net::TcpListener::from_std(listener)?;
                axum::serve(listener, app).await
            };
            if let Err(e) = serve.await {
                eprintln!("mock server: {e}");
            }
        });
    });
    eprintln!("mock OpenAI embeddings server: http://{addr}/v1");
    let config = ProviderConfig {
        provider: ProviderType::OpenAi,
        model_name: "mock-embed".to_string(),
        api_key: Some("mock-key".to_string()),
        base_url: Some(format!("http://{addr}/v1")),
        request_timeout: Duration::from_secs(30),
    };
    let provider = create_provider(&config).map_err(|e| format!("provider init: {e}"))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    Ok(BenchEmbedder::Api { provider, runtime })
}

// ---------------------------------------------------------------- harness

struct CostModel {
    t_lookup_ms: f64,
    t_embed_ms: f64,
    t_backend_ms: f64,
    embed_usd_per_mtok: f64,
    ru_usd_per_m: f64,
    tokens_per_query: f64,
}

impl CostModel {
    fn saved_per_hit_ms(&self) -> f64 {
        self.t_embed_ms + self.t_backend_ms
    }

    /// USD saved per 1k cache hits: embed tokens + one managed-DB read unit.
    fn usd_per_1k_hits(&self) -> f64 {
        let embed = self.embed_usd_per_mtok * self.tokens_per_query / 1000.0;
        embed + self.ru_usd_per_m / 1000.0
    }
}

struct SweepPoint {
    cache_size: usize,
    /// Exact-tier TTL in virtual seconds. `None` = infinite (fast-replay default).
    ttl_secs: Option<u64>,
    queries: usize,
    exact_hits: usize,
    /// Hits lost because the entry aged past the virtual TTL.
    expired_misses: usize,
    /// Hits served from an entry inserted before its cluster last mutated
    /// (stale-content hits; exact key still matched).
    stale_hits: usize,
    /// Misses caused by what-if CDC invalidation (`--cdc-delay`): the entry
    /// was stale but already invalidated, so the request re-fetched fresh.
    cdc_healed: usize,
    /// What-if CDC propagation delay (virtual seconds) for this run.
    cdc_delay_secs: Option<f64>,
    exact_hit_rate: f64,
    /// Misses whose cluster was already cached — embedding-free upper bound
    /// on semantic uplift (assumes a perfect embedder).
    semantic_ceiling_rate: f64,
    mean_latency_ms: f64,
    mean_latency_no_cache_ms: f64,
    latency_saved_pct: f64,
    usd_saved_per_1k_queries: f64,
}

fn make_response() -> QueryResponse {
    QueryResponse {
        results: vec![SearchResult {
            id: "bench-doc".to_string(),
            score: 1.0,
            content: "hitrate bench payload".to_string(),
            metadata: None,
            upstream_id: Some("bench".to_string()),
        }],
        cache_status: CacheStatus::Miss,
        took_ms: 1,
        generated_at: None,
        miss_reason: None,
    }
}

/// Replay events against the real `CacheStore`.
///
/// TTL: `CacheStore` timestamps entries with wall-clock `Instant`, which
/// cannot be virtualized. When `ttl` is set the harness keeps its own
/// virtual clock (`dt_secs` per event) and per-entry virtual insert times;
/// a hit older than the TTL counts as an expired miss and the entry is
/// refreshed (re-inserted), mirroring prod miss→fetch→insert behavior.
/// The semantic ceiling is TTL-blind (cluster-set membership only).
///
/// Stale model (`mutation_rate > 0`): after each event, a random already-seen
/// cluster mutates with that probability (bias toward popular clusters —
/// mutation picks uniformly over event history). Each cached entry records
/// its cluster's version at insert; a hit on an older version counts as a
/// stale hit. The entry is NOT refreshed — without CDC it stays stale until
/// TTL expiry heals it. With infinite TTL this is the no-invalidation worst
/// case; the gap between TTL values shows how TTL bounds staleness.
///
/// What-if CDC (`cdc_delay`): each mutation also enqueues an invalidation
/// firing at `mutation_time + cdc_delay`. When it fires, all entries of that
/// cluster with an older version are considered deleted (tracked via
/// `healed_version`). A hit on an invalidated entry is a `cdc_healed` miss
/// that re-fetches fresh — the difference between TTL-only staleness and
/// CDC-bounded staleness. (Prod note: conproxy's CDC today is the *outbound*
/// cache-mutation stream for peers; upstream-change invalidation would
/// arrive via the evict API or a future upstream watcher — this model
/// quantifies what that would buy.)
#[allow(clippy::too_many_arguments)]
fn run_sweep(
    events: &[TraceEvent],
    cache_size: usize,
    ttl: Option<Duration>,
    dt_secs: f64,
    mutation_rate: f64,
    cdc_delay: Option<f64>,
    rng: &mut Rng,
    model: &CostModel,
) -> SweepPoint {
    // Fast replay: no wall-clock sleeps, so the store's own TTL is set huge
    // and expiry is decided by the harness virtual clock instead.
    let store = CacheStore::new(
        Duration::from_secs(86_400),
        Duration::from_secs(604_800),
        cache_size,
    );
    let ttl_secs = ttl.map(|t| t.as_secs_f64());
    let mutations = mutation_rate > 0.0;
    let mut insert_vtime: HashMap<QueryHash, f64> = HashMap::new();
    let mut insert_version: HashMap<QueryHash, u64> = HashMap::new();
    let mut cluster_version: HashMap<u64, u64> = HashMap::new();
    // What-if CDC: per-cluster highest invalidated version + pending queue
    // (fire_time, cluster), appended in time order.
    let mut healed_version: HashMap<u64, u64> = HashMap::new();
    let mut pending_inv: std::collections::VecDeque<(f64, u64)> = std::collections::VecDeque::new();
    let mut seen_clusters: Vec<u64> = Vec::new();
    let mut vnow = 0.0_f64;
    let mut cached_clusters: HashSet<u64> = HashSet::new();
    let mut exact = 0usize;
    let mut expired = 0usize;
    let mut stale = 0usize;
    let mut healed = 0usize;
    let mut ceiling = 0usize;
    let resp = make_response();
    for ev in events {
        vnow += dt_secs;
        // Fire due what-if CDC invalidations.
        while let Some(&(fire, _)) = pending_inv.front() {
            if fire > vnow {
                break;
            }
            if let Some((_, c)) = pending_inv.pop_front() {
                let cv = cluster_version.get(&c).copied().unwrap_or(0);
                let hv = healed_version.entry(c).or_insert(0);
                if cv > *hv {
                    *hv = cv;
                }
            }
        }
        // Hash needed for virtual-TTL and/or stale bookkeeping — skip otherwise.
        let qh = if ttl_secs.is_some() || mutations {
            Some(query_hash(&ev.query))
        } else {
            None
        };
        if store.get(&ev.query).is_some() {
            let is_expired = match (ttl_secs, qh.and_then(|h| insert_vtime.get(&h))) {
                (Some(t), Some(&t0)) => vnow - t0 > t,
                _ => false,
            };
            if is_expired {
                expired += 1;
                // Fall through to miss path: refresh entry + virtual time.
                store.insert(&ev.query, resp.clone(), "bench".to_string());
                if let Some(h) = qh {
                    insert_vtime.insert(h, vnow);
                    if mutations {
                        insert_version
                            .insert(h, cluster_version.get(&ev.cluster).copied().unwrap_or(0));
                    }
                }
            } else {
                let versions = (
                    qh.and_then(|h| insert_version.get(&h)).copied(),
                    cluster_version.get(&ev.cluster).copied(),
                    healed_version.get(&ev.cluster).copied(),
                );
                let cdc_invalidated = cdc_delay.is_some()
                    && match versions {
                        (Some(iv), _, Some(hv)) => iv < hv,
                        _ => false,
                    };
                if cdc_invalidated {
                    // Entry already invalidated by what-if CDC: miss,
                    // re-fetch, store fresh version.
                    healed += 1;
                    store.insert(&ev.query, resp.clone(), "bench".to_string());
                    if let Some(h) = qh {
                        insert_vtime.insert(h, vnow);
                        insert_version
                            .insert(h, cluster_version.get(&ev.cluster).copied().unwrap_or(0));
                    }
                } else {
                    let is_stale = mutations
                        && match versions {
                            (Some(iv), Some(cv), _) => iv < cv,
                            _ => false,
                        };
                    if is_stale {
                        stale += 1;
                    }
                    exact += 1;
                }
            }
        } else {
            if cached_clusters.contains(&ev.cluster) {
                ceiling += 1;
            }
            store.insert(&ev.query, resp.clone(), "bench".to_string());
            if let Some(h) = qh {
                insert_vtime.insert(h, vnow);
                if mutations {
                    insert_version
                        .insert(h, cluster_version.get(&ev.cluster).copied().unwrap_or(0));
                }
            }
            cached_clusters.insert(ev.cluster);
        }
        if mutations {
            seen_clusters.push(ev.cluster);
            if rng.bernoulli(mutation_rate) {
                let c = seen_clusters[rng.below(seen_clusters.len())];
                *cluster_version.entry(c).or_insert(0) += 1;
                if let Some(d) = cdc_delay {
                    pending_inv.push_back((vnow + d, c));
                }
            }
        }
    }
    let q = events.len();
    let h = exact as f64 / q as f64;
    let c = ceiling as f64 / q as f64;
    let mean = model.t_lookup_ms + (1.0 - h) * model.saved_per_hit_ms();
    let no_cache = model.t_lookup_ms + model.saved_per_hit_ms();
    SweepPoint {
        cache_size,
        ttl_secs: ttl.map(|t| t.as_secs()),
        queries: q,
        exact_hits: exact,
        expired_misses: expired,
        stale_hits: stale,
        cdc_healed: healed,
        cdc_delay_secs: cdc_delay,
        exact_hit_rate: h,
        semantic_ceiling_rate: c,
        mean_latency_ms: mean,
        mean_latency_no_cache_ms: no_cache,
        latency_saved_pct: 100.0 * (no_cache - mean) / no_cache,
        usd_saved_per_1k_queries: h * model.usd_per_1k_hits(),
    }
}

// ---------------------------------------------------------------- semantic

/// One semantic-mode sweep point (single τ, single cache size).
struct SemPoint {
    cache_size: usize,
    tau: f64,
    queries: usize,
    exact_hits: usize,
    semantic_hits: usize,
    /// Semantic hits whose matched entry shares the ground-truth cluster.
    semantic_correct: usize,
    /// Semantic hits served from the wrong cluster — correctness violations.
    semantic_false: usize,
    exact_hit_rate: f64,
    semantic_hit_rate: f64,
    combined_hit_rate: f64,
    /// False hits / all semantic hits. 0 when no semantic hits.
    false_hit_rate: f64,
    /// combined − exact: what semantic mode actually buys.
    uplift: f64,
}

/// Replay against real `CacheStore` + real `SemanticCache` (prod order:
/// exact first, then semantic). Embeddings come from the shared memoized
/// embedder (synthetic, ONNX, or API).
#[cfg(feature = "embed-api")]
fn run_sem_sweep(
    events: &[TraceEvent],
    cache_size: usize,
    tau: f64,
    max_q: usize,
    memo: &mut MemoEmbedder,
) -> Result<SemPoint, String> {
    let store = CacheStore::new(
        Duration::from_secs(86_400),
        Duration::from_secs(604_800),
        cache_size,
    );
    let sem = SemanticCache::new(tau as f32, cache_size);
    // Ground truth: query hash → cluster, so we can grade every semantic hit.
    let mut hash_cluster: HashMap<QueryHash, u64> = HashMap::new();
    let mut exact = 0usize;
    let mut sem_hits = 0usize;
    let mut semantic_correct = 0usize;
    let mut semantic_false = 0usize;
    let resp = make_response();
    for ev in events.iter().take(max_q) {
        if store.get(&ev.query).is_some() {
            exact += 1;
            continue;
        }
        let emb = memo.embed_memo(&ev.query)?.clone();
        if let Some(h) = sem.lookup(&emb) {
            sem_hits += 1;
            match hash_cluster.get(&h) {
                Some(&c) if c == ev.cluster => semantic_correct += 1,
                _ => semantic_false += 1,
            }
            continue;
        }
        let h = query_hash(&ev.query);
        store.insert(&ev.query, resp.clone(), "bench".to_string());
        sem.insert(h, emb);
        hash_cluster.insert(h, ev.cluster);
    }
    let q = events.len().min(max_q);
    let eh = exact as f64 / q as f64;
    let sh = sem_hits as f64 / q as f64;
    Ok(SemPoint {
        cache_size,
        tau,
        queries: q,
        exact_hits: exact,
        semantic_hits: sem_hits,
        semantic_correct,
        semantic_false,
        exact_hit_rate: eh,
        semantic_hit_rate: sh,
        combined_hit_rate: eh + sh,
        false_hit_rate: semantic_false as f64 / sem_hits.max(1) as f64,
        uplift: sh,
    })
}

/// `--probe`: embed a fixed panel of texts, print the pairwise cosine
/// matrix, exit. Debug tool for checking whether an embedder separates
/// paraphrases from unrelated queries on this workload's text shape.
#[cfg(feature = "embed-api")]
fn probe_embedder(memo: &mut MemoEmbedder) -> ExitCode {
    let texts = [
        "login alpha bravo charlie",
        "bravo charlie login alpha",
        "how do I reset my password",
        "password reset instructions",
        "kubernetes pod crash loop back-off",
        "best italian restaurants nearby",
        // sentence-transformers model-card reference pair: real
        // all-MiniLM-L6-v2 gives cosine ~0.68 for these.
        "This is an example sentence",
        "Each sentence is converted",
    ];
    let mut embs = Vec::new();
    for t in &texts {
        match memo.embed_memo(t) {
            Ok(v) => embs.push(v.clone()),
            Err(e) => {
                eprintln!("error embedding {t:?}: {e}");
                return ExitCode::from(1);
            }
        }
    }
    println!("embedder: {} (dim {})", memo.inner.name(), embs[0].len());
    println!("\npairwise cosine:");
    print!("{:>42}", "");
    for (j, _) in texts.iter().enumerate() {
        print!("  t{j}    ");
    }
    println!();
    for (i, a) in embs.iter().enumerate() {
        print!("t{i} {:>38}", texts[i]);
        for b in &embs {
            let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
            let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
            let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
            let cos = if na > 0.0 && nb > 0.0 {
                dot / (na * nb)
            } else {
                0.0
            };
            print!("  {cos:+.3} ");
        }
        println!();
    }
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------- args

#[derive(Default)]
struct Args {
    workload: Option<String>,
    seed: Option<u64>,
    queries: Option<usize>,
    unique: Option<usize>,
    zipf_s: Option<f64>,
    tasks: Option<usize>,
    calls: Option<usize>,
    requery: Option<f64>,
    agents: Option<usize>,
    pool: Option<usize>,
    paraphrase_rate: Option<f64>,
    trace: Option<PathBuf>,
    queries_file: Option<PathBuf>,
    mutation_rate: Option<f64>,
    /// What-if CDC: invalidate mutated-cluster entries this many virtual
    /// seconds after each mutation (None = TTL-only healing, worst case).
    cdc_delay: Option<f64>,
    cache_sizes: Vec<usize>,
    ttl_values: Vec<u64>,
    virtual_qps: Option<f64>,
    t_embed_ms: Option<f64>,
    t_backend_ms: Option<f64>,
    t_lookup_ms: Option<f64>,
    embed_usd_per_mtok: Option<f64>,
    ru_usd_per_m: Option<f64>,
    tokens_per_query: Option<f64>,
    gate_exact: Option<f64>,
    gate_false_hit: Option<f64>,
    semantic: bool,
    embedder: Option<String>,
    embed_provider: Option<String>,
    embed_model: Option<String>,
    embed_api_key_var: Option<String>,
    taus: Vec<f64>,
    sem_cache_sizes: Vec<usize>,
    sem_max_queries: Option<usize>,
    probe: bool,
    no_fail: bool,
    results_dir: Option<PathBuf>,
    /// Live mode: replay against a running proxy HTTP base URL.
    live_url: Option<String>,
    /// Seed qdrant before replay (requires --features embed).
    live_seed: Option<String>,
    live_collection: Option<String>,
    live_top_k: Option<u32>,
    live_docs: Option<usize>,
    live_mutate: Option<f64>,
    live_evict: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        let mut take = |name: &str| -> Result<String, String> {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} requires a value"))
        };
        match a.as_str() {
            "--workload" => args.workload = Some(take("--workload")?),
            "--seed" => args.seed = Some(parse_num(&take("--seed")?, "--seed")?),
            "--queries" => args.queries = Some(parse_num(&take("--queries")?, "--queries")?),
            "--unique" => args.unique = Some(parse_num(&take("--unique")?, "--unique")?),
            "--zipf-s" => args.zipf_s = Some(parse_num(&take("--zipf-s")?, "--zipf-s")?),
            "--tasks" => args.tasks = Some(parse_num(&take("--tasks")?, "--tasks")?),
            "--calls" => args.calls = Some(parse_num(&take("--calls")?, "--calls")?),
            "--requery" => args.requery = Some(parse_num(&take("--requery")?, "--requery")?),
            "--agents" => args.agents = Some(parse_num(&take("--agents")?, "--agents")?),
            "--pool" => args.pool = Some(parse_num(&take("--pool")?, "--pool")?),
            "--paraphrase-rate" => {
                args.paraphrase_rate =
                    Some(parse_num(&take("--paraphrase-rate")?, "--paraphrase-rate")?);
            }
            "--trace" => args.trace = Some(PathBuf::from(take("--trace")?)),
            "--queries-file" => {
                args.queries_file = Some(PathBuf::from(take("--queries-file")?));
            }
            "--mutation-rate" => {
                args.mutation_rate = Some(parse_num(&take("--mutation-rate")?, "--mutation-rate")?);
            }
            "--cdc-delay" => {
                args.cdc_delay = Some(parse_num(&take("--cdc-delay")?, "--cdc-delay")?);
            }
            "--cache-size" => args
                .cache_sizes
                .push(parse_num(&take("--cache-size")?, "--cache-size")?),
            "--ttl" => args.ttl_values.push(parse_num(&take("--ttl")?, "--ttl")?),
            "--virtual-qps" => {
                args.virtual_qps = Some(parse_num(&take("--virtual-qps")?, "--virtual-qps")?)
            }
            "--t-embed-ms" => {
                args.t_embed_ms = Some(parse_num(&take("--t-embed-ms")?, "--t-embed-ms")?)
            }
            "--t-backend-ms" => {
                args.t_backend_ms = Some(parse_num(&take("--t-backend-ms")?, "--t-backend-ms")?)
            }
            "--t-lookup-ms" => {
                args.t_lookup_ms = Some(parse_num(&take("--t-lookup-ms")?, "--t-lookup-ms")?)
            }
            "--embed-usd-per-mtok" => {
                args.embed_usd_per_mtok = Some(parse_num(
                    &take("--embed-usd-per-mtok")?,
                    "--embed-usd-per-mtok",
                )?);
            }
            "--ru-usd-per-m" => {
                args.ru_usd_per_m = Some(parse_num(&take("--ru-usd-per-m")?, "--ru-usd-per-m")?)
            }
            "--tokens-per-query" => {
                args.tokens_per_query = Some(parse_num(
                    &take("--tokens-per-query")?,
                    "--tokens-per-query",
                )?);
            }
            "--gate-exact" => {
                args.gate_exact = Some(parse_num(&take("--gate-exact")?, "--gate-exact")?)
            }
            "--gate-false-hit" => {
                args.gate_false_hit =
                    Some(parse_num(&take("--gate-false-hit")?, "--gate-false-hit")?);
            }
            "--semantic" => args.semantic = true,
            "--embedder" => args.embedder = Some(take("--embedder")?),
            "--embed-provider" => args.embed_provider = Some(take("--embed-provider")?),
            "--embed-model" => args.embed_model = Some(take("--embed-model")?),
            "--embed-api-key-var" => {
                args.embed_api_key_var = Some(take("--embed-api-key-var")?);
            }
            "--tau" => args.taus.push(parse_num(&take("--tau")?, "--tau")?),
            "--sem-cache-size" => args
                .sem_cache_sizes
                .push(parse_num(&take("--sem-cache-size")?, "--sem-cache-size")?),
            "--sem-max-queries" => {
                args.sem_max_queries =
                    Some(parse_num(&take("--sem-max-queries")?, "--sem-max-queries")?);
            }
            "--live" => args.live_url = Some(take("--live")?),
            "--live-seed" => args.live_seed = Some(take("--live-seed")?),
            "--live-collection" => args.live_collection = Some(take("--live-collection")?),
            "--live-top-k" => {
                args.live_top_k = Some(parse_num(&take("--live-top-k")?, "--live-top-k")?);
            }
            "--live-docs" => {
                args.live_docs = Some(parse_num(&take("--live-docs")?, "--live-docs")?);
            }
            "--live-mutate" => {
                args.live_mutate = Some(parse_num(&take("--live-mutate")?, "--live-mutate")?);
            }
            "--live-evict" => args.live_evict = true,
            "--no-fail" => args.no_fail = true,
            "--probe" => args.probe = true,
            "--results-dir" => args.results_dir = Some(PathBuf::from(take("--results-dir")?)),
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown arg: {other} (try --help)")),
        }
    }
    Ok(args)
}

fn parse_num<T: std::str::FromStr>(v: &str, name: &str) -> Result<T, String> {
    v.parse::<T>()
        .map_err(|_| format!("{name}: invalid value {v:?}"))
}

fn print_help() {
    println!(
        "hitrate_bench — cache hit-rate benchmark (docs/strategy-assessment.md §3)\n\
         \n\
         USAGE:\n  \
         hitrate_bench [OPTIONS]\n\
         \n\
         WORKLOAD:\n  \
         --workload zipf|agentic|replay|suite   (default suite = zipf + agentic)\n  \
         --queries N          zipf: total queries          (default 100000)\n  \
         --unique N           zipf: unique queries         (default 10000)\n  \
         --zipf-s S           zipf: exponent               (default 1.0)\n  \
         --tasks N            agentic: tasks               (default 500)\n  \
         --calls N            agentic: calls/agent/task    (default 20)\n  \
         --requery P          agentic: re-query prob       (default 0.4)\n  \
         --agents N           agentic: agents/task         (default 4)\n  \
         --pool N             agentic: shared pool size    (default 200)\n  \
         --paraphrase-rate P  near-dup transform rate      (default 0.3 agentic, 0.1 zipf)\n  \
         --trace FILE         replay: JSONL {{query, cluster_id?}}\n\
         --queries-file FILE  zipf/agentic: real query texts as pool (MS MARCO/ORCAS\n  \
                              adapter; one query/line, TSV takes 2nd field; wraps modulo)\n  \
         --mutation-rate P    per-event doc mutation prob → stale-hit accounting\n  \
                              (default 0; exact sweep only; stale entry heals at TTL expiry)\n  \
         --cdc-delay SECS   what-if CDC: invalidate mutated entries SECS after mutation\n  \
                              (virtual clock; shows staleness window vs TTL-only healing)\n\
         \n\
         CACHE / MODEL:\n  \
         --cache-size N       repeatable; default 100, 1000, 10000\n  \
         --ttl SECS           repeatable exact-tier TTL (virtual time); default infinite\n  \
         --virtual-qps F      virtual arrival rate for TTL clock (default 10.0)\n  \
         --t-lookup-ms F      (default 1)   --t-embed-ms F    (default 30)\n  \
         --t-backend-ms F     (default 20)  --tokens-per-query F (default 15)\n  \
         --embed-usd-per-mtok F (default 0.13)  --ru-usd-per-m F (default 16)\n\
         \n\
         SEMANTIC MODE (v2):\n  \
         --semantic           enable real SemanticCache tier sweep\n  \
         --embedder E         synthetic (default) | onnx (needs --features embed) | api\n  \
         --embed-provider P   api embedder: openai | cohere | huggingface | mock\n  \
                              (mock = local /v1/embeddings server, no keys; wire-path test)\n  \
         --embed-model M      onnx model name (default all-MiniLM-L6-v2)\n  \
         --embed-api-key-var V  env var holding the api key (provider default if unset)\n  \
         --tau F              repeatable cosine threshold; default 0.70 0.75 0.80 0.85 0.90 0.95\n  \
         --sem-cache-size N   repeatable; default 1000 (linear scan cost scales with this)\n  \
         --sem-max-queries N  cap events per semantic run (default all)\n  \
         --gate-false-hit F   max tolerable false-hit rate (default 0.01)\n\
         \n\
         LIVE MODE (v4 — real proxy wire):\n  \
         --live URL           replay workloads against a running proxy (HTTP base,\n  \
                              e.g. http://127.0.0.1:8099); measures REAL hit rate +\n  \
                              wall latency; replaces the in-memory sweep\n  \
         --live-seed URL      seed qdrant first (needs --features embed): one doc per\n  \
                              unique query text, MiniLM vectors, ' v1' content markers\n  \
         --live-collection C  qdrant collection (default conproxy_hitrate)\n  \
         --live-top-k N       top_k per query (default 5)\n  \
         --live-docs N        cap seeded docs (default 2000)\n  \
         --live-mutate P      per-event doc mutation prob (payload version bump in\n  \
                              qdrant) → stale-content accounting; requires --live-seed\n  \
         --live-evict         after each mutation POST /cache/evict (simulated\n  \
                              external CDC invalidation)\n\
         \n\
         OUTPUT / GATES:\n  \
         --results-dir DIR    write summary.json + SUMMARY.md + frontier.json + MANIFEST.md\n  \
         --gate-exact F       agentic exact hit-rate gate  (default 0.40)\n  \
         --no-fail            always exit 0 (report only)\n  \
         --seed N             RNG seed                     (default 42)\n\
         \n\
         Exit codes: 0 = PASS, 2 = FAIL-CORE (agentic gate missed),\n\
         3 = FAIL-TRUST (no τ clears false-hit gate with uplift), 1 = usage error."
    );
}

// ---------------------------------------------------------------- report

struct WorkloadReport {
    name: String,
    description: String,
    sweep: Vec<SweepPoint>,
    /// Semantic τ sweep; empty when `--semantic` not passed.
    sem: Vec<SemPoint>,
}

/// Best valid semantic operating point: max combined HR subject to the
/// false-hit gate. `None` when no τ qualifies (or semantic not run).
fn best_valid_tau(sem: &[SemPoint], gate_false: f64) -> Option<&SemPoint> {
    sem.iter()
        .filter(|p| p.false_hit_rate <= gate_false && p.semantic_hits > 0)
        .max_by(|a, b| {
            a.combined_hit_rate
                .partial_cmp(&b.combined_hit_rate)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

fn summary_md(
    reports: &[WorkloadReport],
    model: &CostModel,
    gate: f64,
    gate_false: f64,
    verdict: &str,
    agentic_best: f64,
    embedder_name: &str,
    mutation_active: bool,
) -> String {
    let mut md = String::from("# Hit-Rate Benchmark\n\n");
    let _ = writeln!(
        md,
        "**Verdict: {verdict}** (agentic exact hit-rate gate: {:.0}%, best: {:.1}%)\n",
        gate * 100.0,
        agentic_best * 100.0
    );
    let _ = writeln!(
        md,
        "Model: lookup {} ms, embed {} ms, backend {} ms, ${}/Mtok embed, ${}/M RU.\n",
        model.t_lookup_ms,
        model.t_embed_ms,
        model.t_backend_ms,
        model.embed_usd_per_mtok,
        model.ru_usd_per_m
    );
    for r in reports {
        let _ = writeln!(md, "## {} — {}\n", r.name, r.description);
        let ttl_active = r.sweep.iter().any(|p| p.ttl_secs.is_some());
        let cdc_active = r.sweep.iter().any(|p| p.cdc_delay_secs.is_some());
        let mut header = String::from("| cache size |");
        let mut sep = String::from("|---|");
        if ttl_active {
            header.push_str(" TTL |");
            sep.push_str("---|");
        }
        header.push_str(" queries | exact HR |");
        sep.push_str("---|---|---|");
        if ttl_active {
            header.push_str(" expired |");
            sep.push_str("---|");
        }
        if mutation_active {
            header.push_str(" stale |");
            sep.push_str("---|");
        }
        if cdc_active {
            header.push_str(" cdc-healed |");
            sep.push_str("---|");
        }
        header.push_str(
            " semantic ceiling | mean ms (no cache) | mean ms | lat saved | $/1k saved |",
        );
        sep.push_str("---|---|---|---|---|");
        let _ = writeln!(md, "{header}");
        let _ = writeln!(md, "{sep}");
        for p in &r.sweep {
            let mut row = format!("| {} |", p.cache_size);
            if ttl_active {
                let ttl = p
                    .ttl_secs
                    .map(|t| format!("{t}s"))
                    .unwrap_or_else(|| "∞".to_string());
                row.push_str(&format!(" {ttl} |"));
            }
            row.push_str(&format!(
                " {} | {:.1}% |",
                p.queries,
                p.exact_hit_rate * 100.0
            ));
            if ttl_active {
                row.push_str(&format!(" {} |", p.expired_misses));
            }
            if mutation_active {
                row.push_str(&format!(" {} |", p.stale_hits));
            }
            if cdc_active {
                row.push_str(&format!(" {} |", p.cdc_healed));
            }
            row.push_str(&format!(
                " {:.1}% | {:.1} | {:.2} | {:.1}% | ${:.4} |",
                p.semantic_ceiling_rate * 100.0,
                p.mean_latency_no_cache_ms,
                p.mean_latency_ms,
                p.latency_saved_pct,
                p.usd_saved_per_1k_queries
            ));
            let _ = writeln!(md, "{row}");
        }
        md.push('\n');
        if !r.sem.is_empty() {
            let _ = writeln!(
                md,
                "### Semantic mode (real SemanticCache, {embedder_name} embedder)\n"
            );
            let _ = writeln!(
                md,
                "| cache | τ | queries | exact HR | semantic HR | combined | false-hit | uplift |"
            );
            let _ = writeln!(md, "|---|---|---|---|---|---|---|---|");
            for p in &r.sem {
                let _ = writeln!(
                    md,
                    "| {} | {:.2} | {} | {:.1}% | {:.1}% | {:.1}% | {:.2}% | +{:.1}pp |",
                    p.cache_size,
                    p.tau,
                    p.queries,
                    p.exact_hit_rate * 100.0,
                    p.semantic_hit_rate * 100.0,
                    p.combined_hit_rate * 100.0,
                    p.false_hit_rate * 100.0,
                    p.uplift * 100.0
                );
            }
            match best_valid_tau(&r.sem, gate_false) {
                Some(p) => {
                    let _ = writeln!(
                        md,
                        "\nBest valid τ: **{:.2}** → combined {:.1}% (false-hit {:.2}% ≤ gate {:.2}%).\n",
                        p.tau,
                        p.combined_hit_rate * 100.0,
                        p.false_hit_rate * 100.0,
                        gate_false * 100.0
                    );
                }
                None => {
                    let _ = writeln!(
                        md,
                        "\n**No τ clears the false-hit gate ({:.2}%) with any uplift — FAIL-TRUST.**\n",
                        gate_false * 100.0
                    );
                }
            }
        }
    }
    let _ = writeln!(
        md,
        "Notes:\n\
         - exact HR: measured against the real CacheStore.\n\
         - semantic ceiling: misses whose cluster was already cached — embedding-free\n  \
         upper bound (perfect embedder). Compare against measured semantic HR below.\n\
         - semantic HR / false-hit: measured against the real SemanticCache with the\n  \
         {embedder_name} embedder. Synthetic salads + small vocab make neighbor space\n  \
         artificially dense for real embedders — interpret false-hit with workload\n  \
         realism in mind (--queries-file with real query logs is the de-risking path).\n\
         - TTL: virtual clock (see --virtual-qps). Stale: cluster mutated after insert\n  \
         (see --mutation-rate); stale entry heals only at TTL expiry — no-CDC worst case.\n"
    );
    md
}

fn sem_point_json(p: &SemPoint) -> serde_json::Value {
    serde_json::json!({
        "cache_size": p.cache_size,
        "tau": p.tau,
        "queries": p.queries,
        "exact_hits": p.exact_hits,
        "semantic_hits": p.semantic_hits,
        "semantic_correct": p.semantic_correct,
        "semantic_false": p.semantic_false,
        "exact_hit_rate": p.exact_hit_rate,
        "semantic_hit_rate": p.semantic_hit_rate,
        "combined_hit_rate": p.combined_hit_rate,
        "false_hit_rate": p.false_hit_rate,
        "uplift": p.uplift,
    })
}

/// τ frontier per workload: every sweep point + the best valid operating τ.
fn frontier_json(reports: &[WorkloadReport], gate_false: f64) -> serde_json::Value {
    let workloads: Vec<serde_json::Value> = reports
        .iter()
        .filter(|r| !r.sem.is_empty())
        .map(|r| {
            let best = best_valid_tau(&r.sem, gate_false);
            serde_json::json!({
                "workload": r.name,
                "gate_false_hit": gate_false,
                "frontier": r.sem.iter().map(sem_point_json).collect::<Vec<_>>(),
                "best_valid": best.map(sem_point_json),
            })
        })
        .collect();
    serde_json::json!({
        "schema_version": 1,
        "tool": "hitrate_bench",
        "kind": "semantic_frontier",
        "workloads": workloads,
    })
}

// Arg count is high because the summary needs the full run context; grouping
// into a struct would just move the noise. Module-level allow covers clippy.
fn summary_json(
    reports: &[WorkloadReport],
    model: &CostModel,
    seed: u64,
    gate: f64,
    gate_false: f64,
    verdict: &str,
    agentic_best: f64,
    embedder_name: &str,
) -> serde_json::Value {
    let workloads: Vec<serde_json::Value> = reports
        .iter()
        .map(|r| {
            let sweep: Vec<serde_json::Value> = r
                .sweep
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "cache_size": p.cache_size,
                        "ttl_secs": p.ttl_secs,
                        "queries": p.queries,
                        "exact_hits": p.exact_hits,
                        "expired_misses": p.expired_misses,
                        "stale_hits": p.stale_hits,
                        "stale_rate": p.stale_hits as f64 / p.exact_hits.max(1) as f64,
                        "cdc_healed": p.cdc_healed,
                        "cdc_delay_secs": p.cdc_delay_secs,
                        "exact_hit_rate": p.exact_hit_rate,
                        "semantic_ceiling_rate": p.semantic_ceiling_rate,
                        "mean_latency_ms": p.mean_latency_ms,
                        "mean_latency_no_cache_ms": p.mean_latency_no_cache_ms,
                        "latency_saved_pct": p.latency_saved_pct,
                        "usd_saved_per_1k_queries": p.usd_saved_per_1k_queries,
                    })
                })
                .collect();
            serde_json::json!({
                "name": r.name,
                "description": r.description,
                "sweep": sweep,
                "semantic": r.sem.iter().map(sem_point_json).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({
        "schema_version": 1,
        "tool": "hitrate_bench",
        "seed": seed,
        "verdict": verdict,
        "gates": {
            "agentic_exact_hit_rate": gate,
            "agentic_exact_best": agentic_best,
            "false_hit_rate": gate_false,
        },
        "model": {
            "t_lookup_ms": model.t_lookup_ms,
            "t_embed_ms": model.t_embed_ms,
            "t_backend_ms": model.t_backend_ms,
            "embed_usd_per_mtok": model.embed_usd_per_mtok,
            "ru_usd_per_m": model.ru_usd_per_m,
            "tokens_per_query": model.tokens_per_query,
        },
        "embedder": embedder_name,
        "notes": [
            "exact hit rate measured against real CacheStore",
            "semantic ceiling = embedding-free upper bound (perfect embedder assumption)",
            "semantic hit/false-hit measured against real SemanticCache with the named embedder",
            "synthetic word-salad workloads are artificially dense for real embedders",
            "TTL via harness virtual clock; stale = cluster mutated after insert, heals at TTL expiry (no-CDC worst case)",
        ],
        "workloads": workloads,
    })
}

// ---------------------------------------------------------------- main

// ---------------------------------------------------------------- live mode

/// One live workload replay result (real proxy wire, no simulated cache).
struct LivePoint {
    workload: String,
    description: String,
    queries: usize,
    hits: usize,
    misses: usize,
    /// Proxy-reported CacheStatus::Stale (served stale-while-revalidate).
    stale_status: usize,
    /// Hit whose returned content version lags the latest known version
    /// (only tracked when --live-mutate > 0; docs carry " v{N}" markers).
    stale_content: usize,
    mutated: usize,
    evictions: usize,
    errors: usize,
    hit_ms: Vec<f64>,
    miss_ms: Vec<f64>,
}

impl LivePoint {
    fn hit_rate(&self) -> f64 {
        if self.queries == 0 {
            return 0.0;
        }
        (self.hits + self.stale_status) as f64 / self.queries as f64
    }
}

fn percentile_ms(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Parse the trailing " v{N}" version marker from seeded doc content.
fn parse_doc_version(content: &str) -> Option<u64> {
    let (head, tail) = content.rsplit_once(" v")?;
    if head.is_empty() {
        return None;
    }
    tail.trim().parse().ok()
}

/// Seed qdrant with one doc per unique query text, MiniLM-embedded so the
/// proxy's own ONNX embedder searches the same vector space. Content carries
/// a " v1" marker for later stale detection. Returns doc count.
#[cfg(feature = "embed")]
fn live_seed_qdrant(
    client: &reqwest::blocking::Client,
    qdrant_url: &str,
    collection: &str,
    docs: &[String],
    memo: &mut MemoEmbedder,
) -> Result<usize, String> {
    if docs.is_empty() {
        return Err("live seed: no documents".to_string());
    }
    let dim = memo.embed_memo(&docs[0])?.len();
    // Fresh collection (ignore delete failure — may not exist).
    let _ = client
        .delete(format!("{qdrant_url}/collections/{collection}"))
        .send();
    let create = serde_json::json!({"vectors": {"size": dim, "distance": "Cosine"}});
    let resp = client
        .put(format!("{qdrant_url}/collections/{collection}"))
        .json(&create)
        .send()
        .map_err(|e| format!("qdrant create collection: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "qdrant create collection: HTTP {}",
            resp.status().as_u16()
        ));
    }
    for (chunk_idx, chunk) in docs.chunks(64).enumerate() {
        let base = chunk_idx * 64;
        let mut points = Vec::with_capacity(chunk.len());
        for (off, text) in chunk.iter().enumerate() {
            points.push(serde_json::json!({
                "id": base + off,
                "vector": memo.embed_memo(text)?.clone(),
                "payload": {"content": format!("{text} v1"), "version": 1},
            }));
        }
        let body = serde_json::json!({"points": points});
        let resp = client
            .put(format!("{qdrant_url}/collections/{collection}/points"))
            .json(&body)
            .send()
            .map_err(|e| format!("qdrant upsert: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("qdrant upsert: HTTP {}", resp.status().as_u16()));
        }
        if (chunk_idx + 1) % 8 == 0 || base + chunk.len() == docs.len() {
            eprintln!("    seeded {}/{} docs", base + chunk.len(), docs.len());
        }
    }
    Ok(docs.len())
}

/// Replay one workload against the live proxy. Mutations go straight to
/// qdrant (payload " v{N}" bump); with `evict`, each mutation is followed by
/// POST /cache/evict (simulated external CDC invalidation).
#[allow(clippy::too_many_arguments)]
fn run_live(
    client: &reqwest::blocking::Client,
    name: &str,
    desc: &str,
    events: &[TraceEvent],
    proxy_url: &str,
    top_k: u32,
    mutate_rate: f64,
    evict: bool,
    qdrant_url: Option<&str>,
    collection: &str,
    doc_texts: &[String],
    latest_version: &mut HashMap<u64, u64>,
    rng: &mut Rng,
) -> Result<LivePoint, String> {
    let mut pt = LivePoint {
        workload: name.to_string(),
        description: desc.to_string(),
        queries: events.len(),
        hits: 0,
        misses: 0,
        stale_status: 0,
        stale_content: 0,
        mutated: 0,
        evictions: 0,
        errors: 0,
        hit_ms: Vec::new(),
        miss_ms: Vec::new(),
    };
    let query_url = format!("{proxy_url}/query");
    let total = events.len();
    for (ev_idx, ev) in events.iter().enumerate() {
        if ev_idx > 0 && ev_idx % 1000 == 0 {
            eprintln!("    {name}: {ev_idx}/{total} queries replayed");
        }
        let body = serde_json::json!({"query": ev.query, "top_k": top_k});
        let t0 = std::time::Instant::now();
        let resp = client
            .post(&query_url)
            .json(&body)
            .send()
            .map_err(|e| format!("proxy /query: {e}"))?;
        let wall_ms = t0.elapsed().as_secs_f64() * 1000.0;
        if !resp.status().is_success() {
            pt.errors += 1;
            continue;
        }
        let json: serde_json::Value = resp
            .json()
            .map_err(|e| format!("proxy /query decode: {e}"))?;
        let status = json
            .get("cache_status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("miss");
        match status {
            "hit" | "frozen" => {
                pt.hits += 1;
                pt.hit_ms.push(wall_ms);
                // Stale-content check: returned doc version vs latest known.
                if let Some(first) = json.get("results").and_then(|r| r.get(0)) {
                    let doc_id = first
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|s| s.parse::<u64>().ok());
                    let version = first
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .and_then(parse_doc_version);
                    if let (Some(id), Some(v)) = (doc_id, version) {
                        if v < *latest_version.get(&id).unwrap_or(&1) {
                            pt.stale_content += 1;
                        }
                    }
                }
            }
            "stale" => {
                pt.stale_status += 1;
                pt.hit_ms.push(wall_ms);
            }
            _ => {
                pt.misses += 1;
                pt.miss_ms.push(wall_ms);
            }
        }
        // Mutation driver: bump a random doc version in qdrant.
        if mutate_rate > 0.0 && !doc_texts.is_empty() && rng.f64() < mutate_rate {
            if let Some(qurl) = qdrant_url {
                let idx64 = rng.next_u64() % doc_texts.len() as u64;
                let new_v = latest_version.get(&idx64).copied().unwrap_or(1) + 1;
                let payload = serde_json::json!({
                    "payload": {"content": format!("{} v{}", doc_texts[idx64 as usize], new_v), "version": new_v},
                    "points": [idx64],
                });
                let ok = client
                    .post(format!("{qurl}/collections/{collection}/points/payload"))
                    .json(&payload)
                    .send()
                    .map(|r| r.status().is_success())
                    .unwrap_or(false);
                if ok {
                    latest_version.insert(idx64, new_v);
                    pt.mutated += 1;
                    if evict {
                        let evict_body = serde_json::json!({"upstream_id": "qdrant"});
                        let evicted = client
                            .post(format!("{proxy_url}/cache/evict"))
                            .json(&evict_body)
                            .send()
                            .map(|r| r.status().is_success())
                            .unwrap_or(false);
                        if evicted {
                            pt.evictions += 1;
                        }
                    }
                }
            }
        }
    }
    Ok(pt)
}

fn summary_md_live(points: &[LivePoint], gate: f64, verdict: &str) -> String {
    let mut out = String::from("# Hit-Rate Benchmark — LIVE mode\n\n");
    let _ = writeln!(
        out,
        "**Verdict: {verdict}** (agentic live hit-rate gate: {:.0}%)\n",
        gate * 100.0
    );
    out.push_str(
        "| workload | queries | hit HR | stale-status | stale-content | mutated | evicted | errors | p50 hit ms | p99 hit ms | p50 miss ms |\n",
    );
    out.push_str("|---|---|---|---|---|---|---|---|---|---|---|\n");
    for p in points {
        let mut h = p.hit_ms.clone();
        h.sort_by(f64::total_cmp);
        let mut m = p.miss_ms.clone();
        m.sort_by(f64::total_cmp);
        let _ = writeln!(
            out,
            "| {} | {} | {:.1}% | {} | {} | {} | {} | {} | {:.1} | {:.1} | {:.1} |",
            p.workload,
            p.queries,
            p.hit_rate() * 100.0,
            p.stale_status,
            p.stale_content,
            p.mutated,
            p.evictions,
            p.errors,
            percentile_ms(&h, 0.50),
            percentile_ms(&h, 0.99),
            percentile_ms(&m, 0.50),
        );
    }
    out.push_str(
        "\nHit HR counts proxy `hit`+`frozen`+`stale` statuses. `stale-content` = hit \
         whose returned doc version lags the mutation stream (needs --live-mutate). \
         `evicted` = simulated external CDC invalidations via /cache/evict.\n",
    );
    out
}

fn summary_json_live(
    points: &[LivePoint],
    seed: u64,
    gate: f64,
    verdict: &str,
) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "tool": "hitrate_bench",
        "mode": "live",
        "seed": seed,
        "gate_exact": gate,
        "verdict": verdict,
        "workloads": points.iter().map(|p| serde_json::json!({
            "name": p.workload,
            "description": p.description,
            "queries": p.queries,
            "hits": p.hits,
            "misses": p.misses,
            "stale_status": p.stale_status,
            "stale_content": p.stale_content,
            "mutated": p.mutated,
            "evicted": p.evictions,
            "errors": p.errors,
            "hit_rate": p.hit_rate(),
        })).collect::<Vec<_>>(),
    })
}

/// Live mode entry: optionally seed qdrant, replay every workload against
/// the proxy, report, exit with the same FAIL-CORE semantics as the sweep.
fn run_live_mode(
    args: &Args,
    specs: &[(String, String, Vec<TraceEvent>)],
    proxy_url: &str,
    seed: u64,
) -> ExitCode {
    let gate = args.gate_exact.unwrap_or(0.40);
    let collection = args
        .live_collection
        .clone()
        .unwrap_or_else(|| "conproxy_hitrate".to_string());
    let top_k = args.live_top_k.unwrap_or(5);
    let mutate_rate = args.live_mutate.unwrap_or(0.0);
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: http client: {e}");
            return ExitCode::from(1);
        }
    };

    // Doc corpus: unique query texts across all workloads, insertion-ordered.
    let doc_cap = args.live_docs.unwrap_or(2_000);
    let mut seen = HashSet::new();
    let mut doc_texts: Vec<String> = Vec::new();
    for (_, _, events) in specs {
        for ev in events {
            if seen.insert(ev.query.as_str()) && doc_texts.len() < doc_cap {
                doc_texts.push(ev.query.clone());
            }
        }
    }

    // Seed qdrant with MiniLM embeddings (same model the proxy uses).
    if let Some(qdrant_url) = &args.live_seed {
        #[cfg(feature = "embed")]
        {
            let mut memo = match build_onnx_embedder(args) {
                Ok(b) => MemoEmbedder::new(b),
                Err(e) => {
                    eprintln!("error: live seed embedder: {e}");
                    return ExitCode::from(1);
                }
            };
            match live_seed_qdrant(&client, qdrant_url, &collection, &doc_texts, &mut memo) {
                Ok(n) => eprintln!("live seed: {n} docs into {qdrant_url}/{collection}"),
                Err(e) => {
                    eprintln!("error: live seed: {e}");
                    return ExitCode::from(1);
                }
            }
        }
        #[cfg(not(feature = "embed"))]
        {
            let _ = qdrant_url;
            eprintln!("error: --live-seed requires `--features embed` (ONNX doc vectors)");
            return ExitCode::from(1);
        }
    } else if mutate_rate > 0.0 {
        eprintln!("error: --live-mutate requires --live-seed (mutation targets seeded docs)");
        return ExitCode::from(1);
    }

    let mut latest_version: HashMap<u64, u64> = HashMap::new();
    let mut rng = Rng::new(seed.wrapping_add(7));
    let mut points = Vec::new();
    for (name, desc, events) in specs {
        match run_live(
            &client,
            name,
            desc,
            events,
            proxy_url,
            top_k,
            mutate_rate,
            args.live_evict,
            args.live_seed.as_deref(),
            &collection,
            &doc_texts,
            &mut latest_version,
            &mut rng,
        ) {
            Ok(p) => points.push(p),
            Err(e) => {
                eprintln!("error: live replay ({name}): {e}");
                return ExitCode::from(1);
            }
        }
    }

    let agentic_best = points
        .iter()
        .find(|p| p.workload == "agentic")
        .map_or(1.0, |p| p.hit_rate());
    let fail_core = points.iter().any(|p| p.workload == "agentic") && agentic_best < gate;
    let verdict = if fail_core { "FAIL-CORE" } else { "PASS" };

    let md = summary_md_live(&points, gate, verdict);
    if let Some(dir) = &args.results_dir {
        let manifest = "summary.json — machine-readable live results\n\
                        SUMMARY.md — human-readable report\n\
                        MANIFEST.md — this file\n";
        let writes = fs::create_dir_all(dir)
            .map_err(|e| format!("create {}: {e}", dir.display()))
            .and_then(|()| {
                fs::write(
                    dir.join("summary.json"),
                    serde_json::to_string_pretty(&summary_json_live(&points, seed, gate, verdict))
                        .unwrap_or_default(),
                )
                .map_err(|e| format!("write summary.json: {e}"))
            })
            .and_then(|()| fs::write(dir.join("SUMMARY.md"), &md).map_err(|e| e.to_string()))
            .and_then(|()| fs::write(dir.join("MANIFEST.md"), manifest).map_err(|e| e.to_string()));
        if let Err(e) = writes {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
        println!("results written to {}", dir.display());
    }
    print!("{md}");
    if fail_core && !args.no_fail {
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };

    let seed = args.seed.unwrap_or(42);
    let gate = args.gate_exact.unwrap_or(0.40);
    let gate_false = args.gate_false_hit.unwrap_or(0.01);
    // Semantic mode replays through the prod SemanticCache, which is gated
    // behind `embed-api`. Fail fast with a clear message on default builds.
    if args.semantic && !cfg!(feature = "embed-api") {
        eprintln!(
            "error: --semantic requires `--features embed-api` (SemanticCache tier is feature-gated)"
        );
        return ExitCode::from(1);
    }
    #[cfg(feature = "embed-api")]
    let taus = if args.taus.is_empty() {
        vec![0.70, 0.75, 0.80, 0.85, 0.90, 0.95]
    } else {
        args.taus.clone()
    };
    #[cfg(feature = "embed-api")]
    let sem_cache_sizes = if args.sem_cache_sizes.is_empty() {
        vec![1_000]
    } else {
        args.sem_cache_sizes.clone()
    };
    #[cfg(feature = "embed-api")]
    let sem_max_q = args.sem_max_queries.unwrap_or(usize::MAX);
    #[cfg(feature = "embed-api")]
    if args.probe {
        let mut m = match build_embedder(&args) {
            Ok(b) => MemoEmbedder::new(b),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(1);
            }
        };
        return probe_embedder(&mut m);
    }
    #[cfg(feature = "embed-api")]
    let mut memo = if args.semantic {
        match build_embedder(&args) {
            Ok(b) => {
                eprintln!("semantic embedder: {}", b.name());
                Some(MemoEmbedder::new(b))
            }
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(1);
            }
        }
    } else {
        if args.embedder.is_some() {
            eprintln!("error: --embedder only applies with --semantic");
            return ExitCode::from(1);
        }
        None
    };
    let cache_sizes = if args.cache_sizes.is_empty() {
        vec![100, 1_000, 10_000]
    } else {
        args.cache_sizes.clone()
    };
    // TTL grid: no --ttl flags → single infinite-TTL run per cache size
    // (preserves v1/v2 behavior). Otherwise cache_size × ttl cross-product.
    let ttls: Vec<Option<Duration>> = if args.ttl_values.is_empty() {
        vec![None]
    } else {
        args.ttl_values
            .iter()
            .map(|&s| Some(Duration::from_secs(s)))
            .collect()
    };
    // Virtual inter-arrival: constant 1/qps spacing per event.
    let vqps = args.virtual_qps.unwrap_or(10.0);
    if vqps <= 0.0 {
        eprintln!("error: --virtual-qps must be > 0");
        return ExitCode::from(1);
    }
    let dt_secs = 1.0 / vqps;
    if args.semantic && !args.ttl_values.is_empty() {
        eprintln!(
            "note: --ttl applies to the exact sweep only; the prod SemanticCache tier has no TTL"
        );
    }
    let model = CostModel {
        t_lookup_ms: args.t_lookup_ms.unwrap_or(1.0),
        t_embed_ms: args.t_embed_ms.unwrap_or(30.0),
        t_backend_ms: args.t_backend_ms.unwrap_or(20.0),
        embed_usd_per_mtok: args.embed_usd_per_mtok.unwrap_or(0.13),
        ru_usd_per_m: args.ru_usd_per_m.unwrap_or(16.0),
        tokens_per_query: args.tokens_per_query.unwrap_or(15.0),
    };
    let workload = args.workload.as_deref().unwrap_or("suite");

    // Real query-text pool (MS MARCO/ORCAS adapter) for zipf/agentic.
    let pool_texts: Option<Vec<String>> = match &args.queries_file {
        Some(p) => match load_queries_file(p) {
            Ok(v) => {
                eprintln!("queries-file: {} texts from {}", v.len(), p.display());
                Some(v)
            }
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(1);
            }
        },
        None => None,
    };
    let pool_ref = pool_texts.as_deref();
    let mutation_rate = args.mutation_rate.unwrap_or(0.0);
    if args.semantic && mutation_rate > 0.0 {
        eprintln!(
            "note: --mutation-rate applies to the exact sweep only (semantic tier unversioned)"
        );
    }
    if args.cdc_delay.is_some() && mutation_rate == 0.0 {
        eprintln!("note: --cdc-delay has no effect without --mutation-rate");
    }
    if let Some(d) = args.cdc_delay {
        if d < 0.0 {
            eprintln!("error: --cdc-delay must be >= 0");
            return ExitCode::from(1);
        }
    }

    let mut specs: Vec<(String, String, Vec<TraceEvent>)> = Vec::new();
    let mut rng = Rng::new(seed);
    match workload {
        "zipf" | "suite" => {
            let events = gen_zipf(
                &mut rng,
                args.unique.unwrap_or(10_000),
                args.zipf_s.unwrap_or(1.0),
                args.queries.unwrap_or(100_000),
                args.paraphrase_rate.unwrap_or(0.1),
                pool_ref,
            );
            specs.push((
                "zipf".to_string(),
                format!(
                    "Zipf popularity, {} unique, s={:.2}, {} queries",
                    args.unique.unwrap_or(10_000),
                    args.zipf_s.unwrap_or(1.0),
                    args.queries.unwrap_or(100_000)
                ),
                events,
            ));
            if workload == "zipf" {
                // fall through
            }
        }
        "agentic" => {}
        "replay" => {}
        other => {
            eprintln!("error: unknown workload {other:?}");
            return ExitCode::from(1);
        }
    }
    match workload {
        "agentic" | "suite" => {
            let events = gen_agentic(
                &mut rng,
                args.tasks.unwrap_or(500),
                args.calls.unwrap_or(20),
                args.requery.unwrap_or(0.4),
                args.agents.unwrap_or(4),
                args.pool.unwrap_or(200),
                args.paraphrase_rate.unwrap_or(0.3),
                pool_ref,
            );
            specs.push((
                "agentic".to_string(),
                format!(
                    "{} tasks × {} agents × {} calls, requery={:.2}, pool={}",
                    args.tasks.unwrap_or(500),
                    args.agents.unwrap_or(4),
                    args.calls.unwrap_or(20),
                    args.requery.unwrap_or(0.4),
                    args.pool.unwrap_or(200)
                ),
                events,
            ));
        }
        "replay" => {
            let path = match &args.trace {
                Some(p) => p.clone(),
                None => {
                    eprintln!("error: --workload replay requires --trace FILE");
                    return ExitCode::from(1);
                }
            };
            match load_replay(&path) {
                Ok(events) => specs.push((
                    "replay".to_string(),
                    format!("replay of {} ({} events)", path.display(), events.len()),
                    events,
                )),
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::from(1);
                }
            }
        }
        _ => {}
    }

    if let Some(live_url) = &args.live_url {
        if args.semantic {
            eprintln!("note: --semantic sweep skipped in live mode (proxy-side tier)");
        }
        return run_live_mode(&args, &specs, live_url, seed);
    }
    if args.live_seed.is_some()
        || args.live_mutate.is_some()
        || args.live_evict
        || args.live_collection.is_some()
    {
        eprintln!("error: --live-* flags require --live URL");
        return ExitCode::from(1);
    }

    let mut reports = Vec::new();
    let mut sweep_rng = Rng::new(seed.wrapping_add(1));
    for (name, desc, events) in &specs {
        let mut sweep = Vec::new();
        for &size in &cache_sizes {
            for &ttl in &ttls {
                sweep.push(run_sweep(
                    events,
                    size,
                    ttl,
                    dt_secs,
                    mutation_rate,
                    args.cdc_delay,
                    &mut sweep_rng,
                    &model,
                ));
            }
        }
        #[cfg(feature = "embed-api")]
        let sem = if args.semantic {
            let memo = match memo.as_mut() {
                Some(m) => m,
                None => {
                    eprintln!("error: internal: semantic mode without embedder");
                    return ExitCode::from(1);
                }
            };
            let mut pts = Vec::new();
            for &size in &sem_cache_sizes {
                for &tau in &taus {
                    match run_sem_sweep(events, size, tau, sem_max_q, memo) {
                        Ok(p) => pts.push(p),
                        Err(e) => {
                            eprintln!("error: semantic sweep ({name}): {e}");
                            return ExitCode::from(1);
                        }
                    }
                }
            }
            pts
        } else {
            Vec::new()
        };
        #[cfg(not(feature = "embed-api"))]
        let sem: Vec<SemPoint> = Vec::new();
        reports.push(WorkloadReport {
            name: name.clone(),
            description: desc.clone(),
            sweep,
            sem,
        });
    }

    // Verdict: agentic workload carries the core bet; best (largest cache)
    // exact HR must clear the gate. Non-agentic-only runs report PASS.
    // Verdict: agentic workload carries the core bet; best exact HR across
    // the whole cache×TTL grid must clear the gate.
    let agentic_best = reports
        .iter()
        .find(|r| r.name == "agentic")
        .map(|r| {
            r.sweep
                .iter()
                .map(|p| p.exact_hit_rate)
                .fold(0.0_f64, f64::max)
        })
        .unwrap_or(1.0);
    let fail_core = reports.iter().any(|r| r.name == "agentic") && agentic_best < gate;
    // FAIL-TRUST: semantic mode ran and at least one workload has no valid τ.
    let fail_trust = args.semantic
        && reports
            .iter()
            .any(|r| !r.sem.is_empty() && best_valid_tau(&r.sem, gate_false).is_none());
    let verdict = if fail_core {
        "FAIL-CORE"
    } else if fail_trust {
        "FAIL-TRUST"
    } else {
        "PASS"
    };

    #[cfg(feature = "embed-api")]
    let embedder_name = memo.as_ref().map_or("n/a", |m| m.inner.name());
    #[cfg(not(feature = "embed-api"))]
    let embedder_name = "n/a";

    let md = summary_md(
        &reports,
        &model,
        gate,
        gate_false,
        verdict,
        agentic_best,
        embedder_name,
        mutation_rate > 0.0,
    );
    let json = summary_json(
        &reports,
        &model,
        seed,
        gate,
        gate_false,
        verdict,
        agentic_best,
        embedder_name,
    );

    if let Some(dir) = &args.results_dir {
        if let Err(e) = fs::create_dir_all(dir) {
            eprintln!("error: create {}: {e}", dir.display());
            return ExitCode::from(1);
        }
        let write = |name: &str, content: &str| -> Result<(), String> {
            fs::write(dir.join(name), content).map_err(|e| format!("write {name}: {e}"))
        };
        let manifest = "summary.json — machine-readable results\n\
                        SUMMARY.md — human-readable report\n\
                        frontier.json — semantic τ frontier (only with --semantic)\n\
                        MANIFEST.md — this file\n";
        let mut writes = write(
            "summary.json",
            &serde_json::to_string_pretty(&json).unwrap_or_default(),
        )
        .and_then(|()| write("SUMMARY.md", &md))
        .and_then(|()| write("MANIFEST.md", manifest));
        if writes.is_ok() && args.semantic {
            writes = write(
                "frontier.json",
                &serde_json::to_string_pretty(&frontier_json(&reports, gate_false))
                    .unwrap_or_default(),
            );
        }
        if let Err(e) = writes {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
        println!("results written to {}", dir.display());
    }

    print!("{md}");
    if fail_core && !args.no_fail {
        return ExitCode::from(2);
    }
    if fail_trust && !args.no_fail {
        return ExitCode::from(3);
    }
    ExitCode::SUCCESS
}
