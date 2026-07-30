//! Proto <-> internal type conversions.
//!
//! `From`/`Into` impls between generated proto types and `types.rs` structs.

use super::proto;
use crate::proxy::types::{
    CacheStatus as InternalCacheStatus, CachedResponse, QueryRequest as InternalQueryRequest,
    QueryResponse as InternalQueryResponse, SearchResult as InternalSearchResult,
};

// =============================================================================
// QueryRequest conversions
// =============================================================================

impl From<proto::QueryRequest> for InternalQueryRequest {
    fn from(p: proto::QueryRequest) -> Self {
        Self {
            query: p.query,
            top_k: if p.top_k > 0 {
                Some(p.top_k as usize)
            } else {
                None
            },
            priority: if p.priority > 0 {
                Some(p.priority as u8)
            } else {
                None
            },
            upstream_id: if p.upstream_id.is_empty() {
                None
            } else {
                Some(p.upstream_id)
            },
            upstream_type: if p.upstream_type.is_empty() {
                None
            } else {
                Some(p.upstream_type)
            },
        }
    }
}

impl From<&InternalQueryRequest> for proto::QueryRequest {
    fn from(r: &InternalQueryRequest) -> Self {
        Self {
            query: r.query.clone(),
            top_k: r.top_k.unwrap_or(0) as u32,
            priority: r.priority.unwrap_or(0) as u32,
            upstream_id: r.upstream_id.clone().unwrap_or_default(),
            upstream_type: r.upstream_type.clone().unwrap_or_default(),
            context: String::new(),
            request_id: String::new(),
        }
    }
}

// =============================================================================
// SearchResult conversions
// =============================================================================

impl From<&InternalSearchResult> for proto::SearchResult {
    fn from(r: &InternalSearchResult) -> Self {
        let metadata_json = r
            .metadata
            .as_ref()
            .map(|m| serde_json::to_vec(m).unwrap_or_default())
            .unwrap_or_default();

        Self {
            id: r.id.clone(),
            score: r.score,
            content: r.content.clone(),
            metadata_json,
            upstream_id: r.upstream_id.clone().unwrap_or_default(),
        }
    }
}

impl From<proto::SearchResult> for InternalSearchResult {
    fn from(p: proto::SearchResult) -> Self {
        let metadata = if p.metadata_json.is_empty() {
            None
        } else {
            serde_json::from_slice(&p.metadata_json).ok()
        };

        Self {
            id: p.id,
            score: p.score,
            content: p.content,
            metadata,
            upstream_id: if p.upstream_id.is_empty() {
                None
            } else {
                Some(p.upstream_id)
            },
        }
    }
}

// =============================================================================
// CacheStatus conversions
// =============================================================================

impl From<InternalCacheStatus> for proto::CacheStatus {
    fn from(s: InternalCacheStatus) -> Self {
        match s {
            InternalCacheStatus::Hit => proto::CacheStatus::Hit,
            InternalCacheStatus::Miss => proto::CacheStatus::Miss,
            InternalCacheStatus::Stale => proto::CacheStatus::Stale,
            InternalCacheStatus::Frozen => proto::CacheStatus::Frozen,
        }
    }
}

impl From<proto::CacheStatus> for InternalCacheStatus {
    fn from(s: proto::CacheStatus) -> Self {
        match s {
            proto::CacheStatus::Hit => InternalCacheStatus::Hit,
            proto::CacheStatus::Miss => InternalCacheStatus::Miss,
            proto::CacheStatus::Stale => InternalCacheStatus::Stale,
            proto::CacheStatus::Frozen => InternalCacheStatus::Frozen,
            proto::CacheStatus::Unspecified => InternalCacheStatus::Miss,
        }
    }
}

// =============================================================================
// QueryResponse conversions
// =============================================================================

impl From<&InternalQueryResponse> for proto::QueryResponse {
    fn from(r: &InternalQueryResponse) -> Self {
        Self {
            results: r.results.iter().map(proto::SearchResult::from).collect(),
            cache_status: proto::CacheStatus::from(r.cache_status) as i32,
            took_ms: r.took_ms,
            generated_at: r.generated_at.unwrap_or(0),
        }
    }
}

impl From<proto::QueryResponse> for InternalQueryResponse {
    fn from(p: proto::QueryResponse) -> Self {
        let cache_status =
            proto::CacheStatus::try_from(p.cache_status).unwrap_or(proto::CacheStatus::Unspecified);

        Self {
            results: p
                .results
                .into_iter()
                .map(InternalSearchResult::from)
                .collect(),
            cache_status: InternalCacheStatus::from(cache_status),
            took_ms: p.took_ms,
            generated_at: if p.generated_at > 0 {
                Some(p.generated_at)
            } else {
                None
            },
            miss_reason: None,
        }
    }
}

// =============================================================================
// CachedResponse -> proto::QueryResponse
// =============================================================================

impl From<&CachedResponse> for proto::QueryResponse {
    fn from(r: &CachedResponse) -> Self {
        match r {
            CachedResponse::Cached {
                entry,
                cache_status,
                took_ms,
            } => Self {
                results: entry
                    .response
                    .results
                    .iter()
                    .map(proto::SearchResult::from)
                    .collect(),
                cache_status: proto::CacheStatus::from(*cache_status) as i32,
                took_ms: *took_ms,
                generated_at: entry.response.generated_at.unwrap_or(0),
            },
            CachedResponse::Fresh(response) => proto::QueryResponse::from(response),
            CachedResponse::SharedFresh { response, took_ms } => Self {
                results: response
                    .results
                    .iter()
                    .map(proto::SearchResult::from)
                    .collect(),
                cache_status: proto::CacheStatus::from(response.cache_status) as i32,
                took_ms: *took_ms,
                generated_at: response.generated_at.unwrap_or(0),
            },
        }
    }
}

// =============================================================================
// BatchRequest conversion (proto uses simple strings, internal uses BatchQuery)
// =============================================================================

impl From<proto::BatchQueryRequest> for crate::proxy::batch::BatchRequest {
    fn from(p: proto::BatchQueryRequest) -> Self {
        Self {
            queries: p
                .queries
                .into_iter()
                .enumerate()
                .map(|(i, q)| crate::proxy::batch::BatchQuery {
                    id: format!("q{}", i),
                    query: q,
                    top_k: None,
                    priority: None,
                })
                .collect(),
            fail_fast: p.fail_fast,
            timeout_ms: None,
        }
    }
}

/// Convert a BatchResponse to proto, mapping BatchQueryResult to QueryResponse.
pub(crate) fn batch_response_to_proto(
    r: &crate::proxy::batch::BatchResponse,
) -> proto::BatchQueryResponse {
    let results = r
        .results
        .iter()
        .map(|(k, v)| {
            let proto_resp = if let Some(ref resp) = v.response {
                proto::QueryResponse::from(resp)
            } else {
                proto::QueryResponse {
                    results: vec![],
                    cache_status: proto::CacheStatus::Miss as i32,
                    took_ms: v.took_ms,
                    generated_at: 0,
                }
            };
            (k.clone(), proto_resp)
        })
        .collect();

    proto::BatchQueryResponse {
        results,
        took_ms: r.took_ms,
        success_count: r.success_count as u32,
        error_count: r.error_count as u32,
        complete: r.complete,
    }
}

// =============================================================================
// FederatedStats conversion
// =============================================================================

impl From<&crate::proxy::federated::FederatedStats> for proto::FederatedStats {
    fn from(s: &crate::proxy::federated::FederatedStats) -> Self {
        Self {
            local_count: s.local_count as u32,
            remote_count: s.remote_count as u32,
            merged_count: s.merged_count as u32,
            local_latency_ms: s.local_latency_ms,
            remote_latency_ms: s.remote_latency_ms.unwrap_or(0),
            fallback_reason: s.fallback_reason.clone().unwrap_or_default(),
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic
)]
mod tests {
    use super::*;

    #[test]
    fn test_query_request_round_trip() {
        let internal = InternalQueryRequest {
            query: "test query".to_string(),
            top_k: Some(5),
            priority: Some(2),
            upstream_id: Some("es-1".to_string()),
            upstream_type: Some("elasticsearch".to_string()),
        };

        let proto_req = proto::QueryRequest::from(&internal);
        assert_eq!(proto_req.query, "test query");
        assert_eq!(proto_req.top_k, 5);
        assert_eq!(proto_req.priority, 2);
        assert_eq!(proto_req.upstream_id, "es-1");
        assert_eq!(proto_req.upstream_type, "elasticsearch");

        let back = InternalQueryRequest::from(proto_req);
        assert_eq!(back.query, internal.query);
        assert_eq!(back.top_k, internal.top_k);
        assert_eq!(back.priority, internal.priority);
        assert_eq!(back.upstream_id, internal.upstream_id);
        assert_eq!(back.upstream_type, internal.upstream_type);
    }

    #[test]
    fn test_query_request_defaults() {
        let proto_req = proto::QueryRequest {
            query: "test".to_string(),
            top_k: 0,
            priority: 0,
            upstream_id: String::new(),
            upstream_type: String::new(),
            context: String::new(),
            request_id: String::new(),
        };

        let internal = InternalQueryRequest::from(proto_req);
        assert_eq!(internal.top_k, None);
        assert_eq!(internal.priority, None);
        assert_eq!(internal.upstream_id, None);
        assert_eq!(internal.upstream_type, None);
    }

    #[test]
    fn test_search_result_round_trip() {
        let internal = InternalSearchResult {
            id: "doc-1".to_string(),
            score: 0.95,
            content: "test content".to_string(),
            metadata: Some(serde_json::json!({"key": "value"})),
            upstream_id: Some("es-1".to_string()),
        };

        let proto_result = proto::SearchResult::from(&internal);
        assert_eq!(proto_result.id, "doc-1");
        assert_eq!(proto_result.score, 0.95);
        assert_eq!(proto_result.content, "test content");
        assert!(!proto_result.metadata_json.is_empty());
        assert_eq!(proto_result.upstream_id, "es-1");

        let back = InternalSearchResult::from(proto_result);
        assert_eq!(back.id, internal.id);
        assert_eq!(back.score, internal.score);
        assert_eq!(back.content, internal.content);
        assert_eq!(back.metadata, internal.metadata);
        assert_eq!(back.upstream_id, internal.upstream_id);
    }

    #[test]
    fn test_search_result_no_metadata() {
        let internal = InternalSearchResult {
            id: "doc-2".to_string(),
            score: 0.5,
            content: "content".to_string(),
            metadata: None,
            upstream_id: None,
        };

        let proto_result = proto::SearchResult::from(&internal);
        assert!(proto_result.metadata_json.is_empty());
        assert!(proto_result.upstream_id.is_empty());

        let back = InternalSearchResult::from(proto_result);
        assert_eq!(back.metadata, None);
        assert_eq!(back.upstream_id, None);
    }

    #[test]
    fn test_cache_status_round_trip() {
        for status in [
            InternalCacheStatus::Hit,
            InternalCacheStatus::Miss,
            InternalCacheStatus::Stale,
            InternalCacheStatus::Frozen,
        ] {
            let proto_status = proto::CacheStatus::from(status);
            let back = InternalCacheStatus::from(proto_status);
            assert_eq!(back, status);
        }
    }

    #[test]
    fn test_cache_status_unspecified_defaults_to_miss() {
        let back = InternalCacheStatus::from(proto::CacheStatus::Unspecified);
        assert_eq!(back, InternalCacheStatus::Miss);
    }

    #[test]
    fn test_query_response_round_trip() {
        let internal = InternalQueryResponse {
            results: vec![InternalSearchResult {
                id: "doc-1".to_string(),
                score: 0.9,
                content: "hello".to_string(),
                metadata: None,
                upstream_id: None,
            }],
            cache_status: InternalCacheStatus::Hit,
            took_ms: 42,
            generated_at: Some(1234567890),
            miss_reason: None,
        };

        let proto_resp = proto::QueryResponse::from(&internal);
        assert_eq!(proto_resp.results.len(), 1);
        assert_eq!(proto_resp.took_ms, 42);
        assert_eq!(proto_resp.generated_at, 1234567890);
        assert_eq!(proto_resp.cache_status, proto::CacheStatus::Hit as i32);

        let back = InternalQueryResponse::from(proto_resp);
        assert_eq!(back.results.len(), 1);
        assert_eq!(back.cache_status, InternalCacheStatus::Hit);
        assert_eq!(back.took_ms, 42);
        assert_eq!(back.generated_at, Some(1234567890));
    }

    #[test]
    fn test_query_response_no_generated_at() {
        let internal = InternalQueryResponse {
            results: vec![],
            cache_status: InternalCacheStatus::Miss,
            took_ms: 10,
            generated_at: None,
            miss_reason: None,
        };

        let proto_resp = proto::QueryResponse::from(&internal);
        assert_eq!(proto_resp.generated_at, 0);

        let back = InternalQueryResponse::from(proto_resp);
        assert_eq!(back.generated_at, None);
    }

    #[test]
    fn test_batch_request_conversion() {
        let proto_req = proto::BatchQueryRequest {
            queries: vec!["q1".to_string(), "q2".to_string()],
            fail_fast: true,
            context: "ctx-1".to_string(),
        };

        let internal = crate::proxy::batch::BatchRequest::from(proto_req);
        assert_eq!(internal.queries.len(), 2);
        assert_eq!(internal.queries[0].query, "q1");
        assert_eq!(internal.queries[1].query, "q2");
        assert!(internal.fail_fast);
    }

    #[test]
    fn test_batch_request_no_fail_fast() {
        let proto_req = proto::BatchQueryRequest {
            queries: vec!["q1".to_string()],
            fail_fast: false,
            context: String::new(),
        };

        let internal = crate::proxy::batch::BatchRequest::from(proto_req);
        assert!(!internal.fail_fast);
    }

    #[test]
    fn test_cached_response_fresh_to_proto() {
        let response = InternalQueryResponse {
            results: vec![InternalSearchResult {
                id: "doc-1".to_string(),
                score: 0.8,
                content: "hello".to_string(),
                metadata: None,
                upstream_id: None,
            }],
            cache_status: InternalCacheStatus::Miss,
            took_ms: 15,
            generated_at: Some(999),
            miss_reason: None,
        };

        let cached = CachedResponse::Fresh(response);
        let proto_resp = proto::QueryResponse::from(&cached);
        assert_eq!(proto_resp.results.len(), 1);
        assert_eq!(proto_resp.results[0].id, "doc-1");
        assert_eq!(proto_resp.took_ms, 15);
        assert_eq!(proto_resp.generated_at, 999);
    }

    #[test]
    fn test_cached_response_cached_to_proto() {
        use std::sync::Arc;
        use std::time::{Instant, SystemTime};

        let entry = Arc::new(crate::proxy::types::CacheEntry {
            response: InternalQueryResponse {
                results: vec![InternalSearchResult {
                    id: "doc-2".to_string(),
                    score: 0.9,
                    content: "world".to_string(),
                    metadata: Some(serde_json::json!({"k": "v"})),
                    upstream_id: Some("es-1".to_string()),
                }],
                cache_status: InternalCacheStatus::Hit,
                took_ms: 5,
                generated_at: Some(12345),
                miss_reason: None,
            },
            upstream_id: "es-1".to_string(),
            cached_at: Instant::now(),
            cached_at_wall: Some(SystemTime::now()),
            extended_count: 0,
            schema: Default::default(),
            content_hash: None,
            query_text: None,
            context_id: "default".to_string(),
            freq: std::sync::atomic::AtomicU8::new(0),
        });

        let cached = CachedResponse::from_cache(entry, InternalCacheStatus::Hit, 42);
        let proto_resp = proto::QueryResponse::from(&cached);
        assert_eq!(proto_resp.results.len(), 1);
        assert_eq!(proto_resp.results[0].id, "doc-2");
        assert_eq!(proto_resp.cache_status, proto::CacheStatus::Hit as i32);
        assert_eq!(proto_resp.took_ms, 42);
        assert_eq!(proto_resp.generated_at, 12345);
    }

    #[test]
    fn test_batch_response_to_proto() {
        use std::collections::HashMap;

        let mut results = HashMap::new();
        results.insert(
            "q1".to_string(),
            crate::proxy::batch::BatchQueryResult {
                success: true,
                response: Some(InternalQueryResponse {
                    results: vec![InternalSearchResult {
                        id: "r1".to_string(),
                        score: 0.7,
                        content: "result".to_string(),
                        metadata: None,
                        upstream_id: None,
                    }],
                    cache_status: InternalCacheStatus::Hit,
                    took_ms: 10,
                    generated_at: None,
                    miss_reason: None,
                }),
                error: None,
                took_ms: 10,
            },
        );
        results.insert(
            "q2".to_string(),
            crate::proxy::batch::BatchQueryResult {
                success: false,
                response: None,
                error: Some("upstream failed".to_string()),
                took_ms: 5,
            },
        );

        let batch_resp = crate::proxy::batch::BatchResponse {
            results,
            took_ms: 15,
            success_count: 1,
            error_count: 1,
            complete: true,
        };

        let proto_resp = super::batch_response_to_proto(&batch_resp);
        assert_eq!(proto_resp.took_ms, 15);
        assert_eq!(proto_resp.success_count, 1);
        assert_eq!(proto_resp.error_count, 1);
        assert!(proto_resp.complete);
        assert_eq!(proto_resp.results.len(), 2);

        // Check successful query
        let q1 = &proto_resp.results["q1"];
        assert_eq!(q1.results.len(), 1);
        assert_eq!(q1.results[0].id, "r1");

        // Check failed query (empty results, miss status)
        let q2 = &proto_resp.results["q2"];
        assert!(q2.results.is_empty());
        assert_eq!(q2.cache_status, proto::CacheStatus::Miss as i32);
    }

    #[test]
    fn test_federated_stats_to_proto() {
        let stats = crate::proxy::federated::FederatedStats {
            local_count: 5,
            remote_count: 3,
            merged_count: 7,
            duplicates_removed: 1,
            remote_queried: true,
            fallback_reason: Some("low_confidence".to_string()),
            local_latency_ms: 10,
            remote_latency_ms: Some(50),
        };

        let proto_stats = proto::FederatedStats::from(&stats);
        assert_eq!(proto_stats.local_count, 5);
        assert_eq!(proto_stats.remote_count, 3);
        assert_eq!(proto_stats.merged_count, 7);
        assert_eq!(proto_stats.local_latency_ms, 10);
        assert_eq!(proto_stats.remote_latency_ms, 50);
        assert_eq!(proto_stats.fallback_reason, "low_confidence");
    }

    #[test]
    fn test_federated_stats_no_remote() {
        let stats = crate::proxy::federated::FederatedStats {
            local_count: 5,
            remote_count: 0,
            merged_count: 5,
            duplicates_removed: 0,
            remote_queried: false,
            fallback_reason: None,
            local_latency_ms: 10,
            remote_latency_ms: None,
        };

        let proto_stats = proto::FederatedStats::from(&stats);
        assert_eq!(proto_stats.remote_latency_ms, 0);
        assert_eq!(proto_stats.fallback_reason, "");
    }
}
