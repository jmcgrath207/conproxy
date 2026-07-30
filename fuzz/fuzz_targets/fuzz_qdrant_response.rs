#![no_main]
use libfuzzer_sys::fuzz_target;
use serde::Deserialize;

/// Mirror of the private QdrantSearchResponse used in conproxy::proxy::qdrant.
/// Fuzzing the same serde shape catches panics in deserialization.
#[derive(Debug, Deserialize)]
struct QdrantSearchResponse {
    result: Vec<QdrantScoredPoint>,
    time: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct QdrantScoredPoint {
    id: QdrantPointId,
    score: f32,
    #[serde(default)]
    payload: Option<serde_json::Value>,
    #[serde(default)]
    vector: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum QdrantPointId {
    Uuid(String),
    Integer(u64),
}

fuzz_target!(|data: &[u8]| {
    let _ = serde_json::from_slice::<QdrantSearchResponse>(data);
});
