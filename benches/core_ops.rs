//! Core operations micro-benchmarks.
//!
//! Covers hot paths in the proxy:
//! - Cache insert/lookup/eviction
//! - Query normalization + hashing
//! - Slugify (filename generation)
//! - Request (de)serialization (serde_json + bincode)
//!
//! Run with:
//!   cargo bench --bench core_ops
//!   cargo bench --bench core_ops -- --save-baseline main
//!   cargo bench --bench core_ops -- --baseline main
//!
//! Regression threshold: 15% slower than baseline → investigate.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;
use std::time::Duration;

use conproxy::proxy::{
    slugify, CacheStatus, CacheStore, QueryRequest, QueryResponse, SearchResult,
};

fn make_response() -> QueryResponse {
    let results = (0..10)
        .map(|i| SearchResult {
            id: format!("doc-{i}"),
            score: 1.0 - (i as f32) * 0.01,
            content: format!("Content for document {i} with some sample text payload"),
            metadata: None,
            upstream_id: Some("upstream-1".to_string()),
        })
        .collect();
    QueryResponse {
        results,
        cache_status: CacheStatus::Miss,
        took_ms: 5,
        generated_at: None,
        miss_reason: None,
    }
}

fn bench_cache_insert(c: &mut Criterion) {
    let store = CacheStore::new(Duration::from_secs(60), Duration::from_secs(300), 100_000);

    let mut counter = 0u64;
    c.bench_function("cache_insert", |b| {
        b.iter(|| {
            counter += 1;
            let query = format!("query-{counter}");
            let resp = make_response();
            black_box(store.insert(&query, resp, "upstream-1".to_string()));
        });
    });
}

fn bench_cache_lookup(c: &mut Criterion) {
    let store = CacheStore::new(Duration::from_secs(60), Duration::from_secs(300), 100_000);

    // Pre-populate with 1000 entries
    for i in 0..1000 {
        store.insert(&format!("key-{i}"), make_response(), "u".to_string());
    }

    let mut counter = 0u64;
    c.bench_function("cache_lookup_hit", |b| {
        b.iter(|| {
            counter = (counter + 1) % 1000;
            black_box(store.get(&format!("key-{counter}")));
        });
    });
}

fn bench_cache_lookup_miss(c: &mut Criterion) {
    let store = CacheStore::new(Duration::from_secs(60), Duration::from_secs(300), 100_000);

    // Pre-populate so miss path is exercised (no key matches)
    for i in 0..1000 {
        store.insert(&format!("key-{i}"), make_response(), "u".to_string());
    }

    let mut counter = 0u64;
    c.bench_function("cache_lookup_miss", |b| {
        b.iter(|| {
            counter += 1;
            black_box(store.get(&format!("nope-{counter}")));
        });
    });
}

fn bench_cache_eviction_pressure(c: &mut Criterion) {
    // Small max_entries to force evictions on every insert
    let store = CacheStore::new(Duration::from_secs(60), Duration::from_secs(300), 100);

    let mut counter = 0u64;
    c.bench_function("cache_eviction_pressure", |b| {
        b.iter(|| {
            counter += 1;
            let query = format!("evict-{counter}");
            let resp = make_response();
            black_box(store.insert(&query, resp, "u".to_string()));
        });
    });
}

fn bench_slugify(c: &mut Criterion) {
    let inputs = [
        "Hello, World!",
        "  multi   space  ",
        "foo/bar\\baz",
        "café au lait",
        "trailing/   ",
        "already-clean-slug",
    ];

    let mut idx = 0usize;
    c.bench_function("slugify", |b| {
        b.iter(|| {
            let input = inputs[idx % inputs.len()];
            idx += 1;
            black_box(slugify(input));
        });
    });
}

fn bench_query_serialize(c: &mut Criterion) {
    let req = QueryRequest {
        query: "a search query that has some length to it".to_string(),
        top_k: Some(10),
        priority: Some(1),
        upstream_id: Some("upstream-1".to_string()),
        upstream_type: None,
    };

    c.bench_function("query_serialize_json", |b| {
        b.iter(|| {
            let s = serde_json::to_string(black_box(&req)).unwrap();
            black_box(s);
        });
    });

    c.bench_function("query_deserialize_json", |b| {
        let json = serde_json::to_string(&req).unwrap();
        b.iter(|| {
            let r: QueryRequest = serde_json::from_str(black_box(&json)).unwrap();
            black_box(r);
        });
    });
}

fn bench_response_serialize(c: &mut Criterion) {
    let resp = make_response();

    c.bench_function("response_serialize_json", |b| {
        b.iter(|| {
            let s = serde_json::to_string(black_box(&resp)).unwrap();
            black_box(s);
        });
    });

    c.bench_function("response_deserialize_json", |b| {
        let json = serde_json::to_string(&resp).unwrap();
        b.iter(|| {
            let r: QueryResponse = serde_json::from_str(black_box(&json)).unwrap();
            black_box(r);
        });
    });
}

fn bench_query_hash(c: &mut Criterion) {
    let inputs: Vec<String> = (0..256)
        .map(|i| format!("query number {i} with payload"))
        .collect();

    let mut idx = 0usize;
    c.bench_function("query_hash", |b| {
        b.iter(|| {
            let s = &inputs[idx % inputs.len()];
            idx += 1;
            black_box(CacheStore::hash_query(s));
        });
    });
}

fn bench_normalize_query(c: &mut Criterion) {
    let inputs = [
        "  hello   world  ",
        "café au lait",
        "Trailing whitespace   ",
        "Tab\t\tSeparated",
    ];

    let mut idx = 0usize;
    c.bench_function("normalize_query", |b| {
        b.iter(|| {
            let s = inputs[idx % inputs.len()];
            idx += 1;
            black_box(CacheStore::normalize_query(s));
        });
    });
}

fn bench_cache_throughput(c: &mut Criterion) {
    // Throughput-oriented bench: measure ops/sec at scale
    let mut group = c.benchmark_group("cache_throughput");
    for size in [100usize, 1_000, 10_000].iter() {
        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &n| {
            let store = CacheStore::new(Duration::from_secs(60), Duration::from_secs(300), n + 100);
            b.iter(|| {
                for i in 0..n {
                    store.insert(&format!("k-{i}"), make_response(), "u".to_string());
                }
                black_box(store.len());
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_cache_insert,
    bench_cache_lookup,
    bench_cache_lookup_miss,
    bench_cache_eviction_pressure,
    bench_slugify,
    bench_query_serialize,
    bench_response_serialize,
    bench_query_hash,
    bench_normalize_query,
    bench_cache_throughput,
);
criterion_main!(benches);
