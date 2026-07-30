//! Cascade + scope micro-benchmarks.
//!
//! Covers hot paths in:
//! - `cascade::fuse_rrf` — RRF dedup + merge across upstreams
//! - `scope::ScopeFilter::best_sim` — lexical Jaccard scoring
//! - `scope::ScopeFilter::filter_results` — full filter pipeline
//!
//! Run with:
//!   cargo bench --bench cascade_scope
//!   cargo bench --bench cascade_scope -- --save-baseline main
//!   cargo bench --bench cascade_scope -- --baseline main
//!
//! Setup vs measurement isolation: `fuse_rrf` consumes owned `Vec<SearchResult>`
//! lists. We use `iter_batched` (BatchSize::SmallInput) so the per-iter clone
//! cost does not contaminate the measurement of the RRF path itself.
//! `Throughput::Elements(total_results)` reports elements/s on the timed inner
//! closure only.

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;

use conproxy::config::{ProxyScopeConfig, WeightedPhrase};
use conproxy::proxy::cascade::fuse_rrf;
use conproxy::proxy::{ScopeFilter, SearchResult};

fn make_result(id: usize, content: &str) -> SearchResult {
    SearchResult {
        id: format!("doc-{id}"),
        score: 0.9 - (id as f32) * 0.01,
        content: content.to_string(),
        metadata: None,
        upstream_id: None,
    }
}

fn make_upstream_results(upstream_id: &str, n: usize, base: &str) -> (String, Vec<SearchResult>) {
    let results = (0..n)
        .map(|i| {
            let unique_word = format!("{base}_{i}");
            let content = format!(
                "This document discusses {unique_word} in the context of \
                 vector search optimization and retrieval-augmented generation. \
                 The approach uses {base} techniques for improved relevance."
            );
            make_result(i, &content)
        })
        .collect();
    (upstream_id.to_string(), results)
}

// ---------------------------------------------------------------------------
// fuse_rrf
// ---------------------------------------------------------------------------

type FuseLists = Vec<(String, Vec<SearchResult>)>;
type FuseScenario = (&'static str, FuseLists, u32, usize, u64);

fn bench_fuse_rrf(c: &mut Criterion) {
    // Setup is done once per batch (BatchSize::SmallInput ≈ 1 clone per iter);
    // the timed closure is the RRF path only.
    let mut group = c.benchmark_group("fuse_rrf");

    // (label, lists, k, max_results, total_elements)
    let scenarios: &[FuseScenario] = &[
        (
            "3upstreams_k60",
            vec![
                make_upstream_results("es", 10, "elasticsearch"),
                make_upstream_results("qdrant", 10, "qdrant"),
                make_upstream_results("meili", 10, "meilisearch"),
            ],
            60,
            10,
            30,
        ),
        (
            "dedup_2upstreams",
            {
                let shared = "Shared document about caching strategies for RAG systems";
                vec![
                    (
                        "es".to_string(),
                        (0..10).map(|i| make_result(i, shared)).collect(),
                    ),
                    (
                        "qdrant".to_string(),
                        (10..20).map(|i| make_result(i, shared)).collect(),
                    ),
                ]
            },
            60,
            10,
            20,
        ),
        (
            "2upstreams_50each",
            vec![
                make_upstream_results("es", 50, "elasticsearch"),
                make_upstream_results("qdrant", 50, "qdrant"),
            ],
            60,
            20,
            100,
        ),
    ];

    for (label, lists, k, max_results, total_elems) in scenarios {
        group.throughput(Throughput::Elements(*total_elems));
        group.bench_with_input(BenchmarkId::from_parameter(label), lists, |b, lists| {
            b.iter_batched(
                || lists.clone(),
                |l| black_box(fuse_rrf(l, *k, *max_results)),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// scope best_sim (lexical Jaccard)
// ---------------------------------------------------------------------------

fn make_scope_filter(phrases: Vec<&str>) -> ScopeFilter {
    let config = ProxyScopeConfig {
        weighted_phrases: phrases.into_iter().map(WeightedPhrase::new).collect(),
        seeds: vec![],
        mode: Some("filter".to_string()),
        min_seed_similarity: Some(0.25),
        seed_weight: Some(0.3),
        query_prefix: None,
        lexical_weight: None,
        embed_band: None,
    };
    ScopeFilter::from_config(&config)
}

fn bench_scope_best_sim_short(c: &mut Criterion) {
    let filter = make_scope_filter(vec!["vector search", "cache optimization"]);
    let content = "Vector search techniques for cache optimization in RAG systems";

    c.bench_function("scope_best_sim_short_content", |b| {
        b.iter(|| black_box(filter.best_sim(content)));
    });
}

fn bench_scope_best_sim_long(c: &mut Criterion) {
    let filter = make_scope_filter(vec![
        "vector search optimization",
        "retrieval augmented generation",
        "cache invalidation strategies",
    ]);
    let content = "This is a lengthy document about vector search optimization \
        techniques in the context of retrieval augmented generation systems. \
        We discuss cache invalidation strategies and their impact on performance \
        across distributed systems with multiple replicas and shards. The key \
        insight is that proper scoping of search results improves relevance \
        while reducing latency for end users.";

    c.bench_function("scope_best_sim_long_content", |b| {
        b.iter(|| black_box(filter.best_sim(content)));
    });
}

fn bench_scope_best_sim_no_match(c: &mut Criterion) {
    let filter = make_scope_filter(vec!["quantum computing", "blockchain"]);

    c.bench_function("scope_best_sim_no_match", |b| {
        b.iter(|| black_box(filter.best_sim("Vector search and cache optimization")));
    });
}

// ---------------------------------------------------------------------------
// scope filter_results (full pipeline)
// ---------------------------------------------------------------------------

fn bench_scope_filter_results(c: &mut Criterion) {
    let filter = make_scope_filter(vec![
        "vector search",
        "cache optimization",
        "retrieval augmented generation",
    ]);
    let results: Vec<SearchResult> = (0..20)
        .map(|i| {
            let (content, score) = if i % 3 == 0 {
                (
                    format!("Vector search optimization for cache #{i} in RAG systems"),
                    0.95,
                )
            } else {
                (
                    format!("Unrelated document about cooking recipes #{i}"),
                    0.85,
                )
            };
            SearchResult {
                id: format!("doc-{i}"),
                score,
                content,
                metadata: None,
                upstream_id: None,
            }
        })
        .collect();

    c.bench_function("scope_filter_results_20items", |b| {
        // Setup is cheap (1 Vec clone) but b.iter_batched keeps scope_filter
        // setup and filter pipeline timing on separate clocks. Stick with
        // plain b.iter for now — input is fixed and clone is small.
        b.iter(|| black_box(filter.filter_results(results.clone())));
    });
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

criterion_group!(
    benches,
    bench_fuse_rrf,
    bench_scope_best_sim_short,
    bench_scope_best_sim_long,
    bench_scope_best_sim_no_match,
    bench_scope_filter_results,
);

criterion_main!(benches);
