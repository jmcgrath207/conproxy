#![no_main]
use libfuzzer_sys::fuzz_target;
use serde::Deserialize;

/// Mirror of the private EsSearchResponse used in conproxy::proxy::elasticsearch.
/// Fuzzing the same serde shape catches panics in deserialization.
#[derive(Debug, Deserialize)]
struct EsSearchResponse {
    hits: EsHitsContainer,
}

#[derive(Debug, Deserialize)]
struct EsHitsContainer {
    total: EsHitsTotal,
    max_score: Option<f32>,
    hits: Vec<EsHit>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum EsHitsTotal {
    Object { value: u64 },
    Integer(u64),
}

#[derive(Debug, Deserialize)]
struct EsHit {
    #[serde(rename = "_id")]
    id: String,
    #[serde(rename = "_score")]
    score: Option<f32>,
    #[serde(rename = "_source")]
    source: Option<serde_json::Value>,
}

fuzz_target!(|data: &[u8]| {
    let _ = serde_json::from_slice::<EsSearchResponse>(data);
});
