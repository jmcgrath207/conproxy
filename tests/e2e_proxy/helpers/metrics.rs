use super::client::E2eClient;
use super::report::{SectionMetrics, TestReport};

/// Returned by `snapshot_and_reset` for downstream assertions.
#[derive(Clone, Copy)]
pub struct SectionSnapshot {
    pub requests: u64,
    pub hits: u64,
    #[allow(dead_code)]
    pub misses: u64,
    pub errors: u64,
    pub hit_rate: f64,
}

/// Snapshot metrics for the current section, then reset for the next section.
/// Returns the snapshot so callers can assert on expected thresholds.
pub fn snapshot_and_reset(
    client: &E2eClient,
    section_name: &str,
    report: &mut TestReport,
) -> SectionSnapshot {
    let (status, metrics) = client.metrics();
    if status != 200 {
        return SectionSnapshot {
            requests: 0,
            hits: 0,
            misses: 0,
            errors: 0,
            hit_rate: 0.0,
        };
    }

    let proxy = &metrics["proxy"];
    let requests = proxy["requests_total"].as_u64().unwrap_or(0);
    let hits = proxy["cache_hits"].as_u64().unwrap_or(0);
    let misses = proxy["cache_misses"].as_u64().unwrap_or(0);
    let errors = proxy["upstream_failures"].as_u64().unwrap_or(0);
    let hit_rate_f64 = proxy["cache_hit_rate"].as_f64().unwrap_or(0.0);

    report.record_section(SectionMetrics {
        section: section_name.to_string(),
        description: TestReport::section_description(section_name),
        requests,
        hits,
        misses,
        errors,
        hit_rate: format!("{:.2}", hit_rate_f64),
    });

    // Reset for next section
    client.admin_metrics_reset();

    SectionSnapshot {
        requests,
        hits,
        misses,
        errors,
        hit_rate: hit_rate_f64,
    }
}
