//! Markdown rendering for distill entries.
//!
//! `render_entry_md` turns a gRPC `DistillEntry` into a human-readable
//! markdown document suitable for LLM ingestion, search-result export, or
//! a readable audit log. The output is deterministic and hash-stable for
//! the same input.
//!
//! Date math uses Howard Hinnant's civil-from-days algorithm and operates on
//! `i64` / `u64` values bounded well within `i64::MAX` for any real wall-clock
//! timestamp, so the arithmetic never overflows in practice. The
//! `arithmetic_side_effects` lint is allowed at module level to keep the
//! date helpers readable.

#![allow(clippy::arithmetic_side_effects)]

use crate::proxy::grpc::proto::DistillEntry;

/// Render a single `DistillEntry` as a markdown document.
///
/// The output has three sections:
///
/// 1. **Header** — query text (or `<empty>` for legacy entries).
/// 2. **Metadata** — context, upstream, hash, insertion time (ISO 8601), TTL-extension count.
/// 3. **Response** — pretty-printed JSON of the cached `QueryResponse` payload.
///    If JSON parsing fails (malformed response), the raw bytes are emitted
///    wrapped in a fenced block with a warning so the file is still parseable.
///
/// The `embedding` field is **not** rendered by default. Embedding vectors
/// are large, opaque, and rarely useful in human-facing markdown. Callers
/// that want the embedding should use the `json` export path instead.
pub fn render_entry_md(entry: &DistillEntry) -> String {
    let mut out = String::new();

    // 1. Header
    let query_display = if entry.query.is_empty() {
        "<empty>".to_string()
    } else {
        entry.query.clone()
    };
    out.push_str(&format!("# {}\n\n", query_display));

    // 2. Metadata block
    out.push_str("## Metadata\n\n");
    out.push_str(&format!("- **Context:** `{}`\n", entry.context_id));
    out.push_str(&format!("- **Upstream:** `{}`\n", entry.upstream_id));
    out.push_str(&format!("- **Hash:** `{}`\n", entry.hash_hex));
    out.push_str(&format!(
        "- **Cached at:** `{}`\n",
        format_cached_at_ms(entry.cached_at_ms)
    ));
    if entry.extended_count > 0 {
        out.push_str(&format!("- **TTL extensions:** {}\n", entry.extended_count));
    }
    out.push('\n');

    // 3. Response payload
    out.push_str("## Response\n\n");
    match serde_json::from_slice::<serde_json::Value>(&entry.response_json) {
        Ok(value) => match serde_json::to_string_pretty(&value) {
            Ok(pretty) => {
                out.push_str("```json\n");
                out.push_str(&pretty);
                out.push_str("\n```\n");
            }
            Err(_) => {
                out.push_str("```\n");
                out.push_str("(response could not be pretty-printed)\n");
                out.push_str("```\n");
            }
        },
        Err(_) => {
            out.push_str("```\n");
            out.push_str("(response JSON unparseable; raw bytes below)\n\n");
            // Render raw bytes as a UTF-8 lossy string for readability.
            out.push_str(&String::from_utf8_lossy(&entry.response_json));
            out.push_str("\n```\n");
        }
    }

    out
}

/// Format `cached_at_ms` (Unix epoch milliseconds) as an ISO 8601 UTC string.
///
/// Returns `<invalid>` for timestamps outside the representable range.
fn format_cached_at_ms(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let nsec = ((ms % 1000) * 1_000_000) as u32;
    match chrono_like_format(secs, nsec) {
        Some(s) => s,
        None => "<invalid>".to_string(),
    }
}

/// Minimal ISO 8601 formatter (UTC) without pulling in a date crate.
///
/// Format: `YYYY-MM-DDTHH:MM:SS.nnnnnnnnnZ`
fn chrono_like_format(secs: i64, nsec: u32) -> Option<String> {
    // Days from 1970-01-01 to given seconds-since-epoch.
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400) as u32;
    let (year, month, day) = days_to_ymd(days)?;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    Some(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z",
        year, month, day, hour, minute, second, nsec
    ))
}

/// Convert days-since-1970-01-01 to (year, month, day).
fn days_to_ymd(days: i64) -> Option<(i32, u32, u32)> {
    // Civil-from-days algorithm by Howard Hinnant (public domain).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i32 + (era * 400) as i32;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    Some((y, m, d))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    fn make_entry(query: &str, context: &str, upstream: &str) -> DistillEntry {
        DistillEntry {
            query: query.to_string(),
            context_id: context.to_string(),
            upstream_id: upstream.to_string(),
            cached_at_ms: 1_700_000_000_000,
            extended_count: 0,
            response_json: br#"{"results":[],"took_ms":5}"#.to_vec(),
            hash_hex: "abcd1234".repeat(8),
            embedding: vec![],
        }
    }

    #[test]
    fn render_basic_entry() {
        let entry = make_entry("rust async", "production", "qdrant-main");
        let md = render_entry_md(&entry);
        assert!(md.contains("# rust async"));
        assert!(md.contains("- **Context:** `production`"));
        assert!(md.contains("- **Upstream:** `qdrant-main`"));
        assert!(md.contains(
            "- **Hash:** `abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234`"
        ));
        assert!(md.contains("## Metadata"));
        assert!(md.contains("## Response"));
        assert!(md.contains("```json"));
        assert!(md.contains("\"took_ms\": 5"));
    }

    #[test]
    fn render_golden_fixture_shape() {
        let entry = make_entry("golden query", "docs", "meili-main");
        let md = render_entry_md(&entry);
        let expected = "\
# golden query

## Metadata

- **Context:** `docs`
- **Upstream:** `meili-main`
- **Hash:** `abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234`
- **Cached at:** `2023-11-14T22:13:20.000000000Z`

## Response

```json
{
  \"results\": [],
  \"took_ms\": 5
}
```
";
        assert_eq!(md, expected);
    }

    #[test]
    fn render_empty_query_uses_placeholder() {
        let entry = make_entry("", "default", "up");
        let md = render_entry_md(&entry);
        assert!(md.contains("# <empty>"));
    }

    #[test]
    fn render_extended_count_omitted_when_zero() {
        let entry = make_entry("q", "ctx", "up");
        let md = render_entry_md(&entry);
        assert!(!md.contains("TTL extensions"));
    }

    #[test]
    fn render_extended_count_shown_when_nonzero() {
        let mut entry = make_entry("q", "ctx", "up");
        entry.extended_count = 3;
        let md = render_entry_md(&entry);
        assert!(md.contains("- **TTL extensions:** 3"));
    }

    #[test]
    fn render_unparseable_json_shows_raw() {
        let mut entry = make_entry("q", "ctx", "up");
        entry.response_json = b"not json {{{".to_vec();
        let md = render_entry_md(&entry);
        assert!(md.contains("response JSON unparseable"));
        assert!(md.contains("not json {{{"));
    }

    #[test]
    fn render_cached_at_iso_format() {
        // 2023-11-14T22:13:20.000000000Z
        let entry = make_entry("q", "ctx", "up");
        let md = render_entry_md(&entry);
        assert!(md.contains("2023-11-14T22:13:20.000000000Z"));
    }

    #[test]
    fn render_does_not_include_embedding() {
        let mut entry = make_entry("q", "ctx", "up");
        entry.embedding = vec![0.1, 0.2, 0.3, 0.4];
        let md = render_entry_md(&entry);
        assert!(!md.contains("0.1"));
        assert!(!md.contains("embedding"));
    }

    #[test]
    fn render_pretty_prints_nested_json() {
        let mut entry = make_entry("q", "ctx", "up");
        entry.response_json = br#"{"results":[{"id":"a","score":0.9}]}"#.to_vec();
        let md = render_entry_md(&entry);
        assert!(md.contains("\"id\": \"a\""));
        assert!(md.contains("\"score\": 0.9"));
    }

    #[test]
    fn chrono_like_format_epoch_zero() {
        // 1970-01-01T00:00:00.000000000Z
        let s = chrono_like_format(0, 0).unwrap();
        assert_eq!(s, "1970-01-01T00:00:00.000000000Z");
    }

    #[test]
    fn chrono_like_format_y2k() {
        // 2000-01-01T00:00:00.000000000Z
        let s = chrono_like_format(946_684_800, 0).unwrap();
        assert_eq!(s, "2000-01-01T00:00:00.000000000Z");
    }

    #[test]
    fn chrono_like_format_known_timestamp() {
        // 2024-01-15T12:50:45.123456789Z
        let s = chrono_like_format(1_705_323_045, 123_456_789).unwrap();
        assert_eq!(s, "2024-01-15T12:50:45.123456789Z");
    }

    #[test]
    fn days_to_ymd_epoch() {
        assert_eq!(days_to_ymd(0), Some((1970, 1, 1)));
    }

    #[test]
    fn days_to_ymd_y2k() {
        // 2000-01-01 is day 10957 from 1970-01-01
        assert_eq!(days_to_ymd(10_957), Some((2000, 1, 1)));
    }

    #[test]
    fn days_to_ymd_leap_day() {
        // 2024-02-29
        let days = chrono_days_from_ymd(2024, 2, 29);
        assert_eq!(days_to_ymd(days), Some((2024, 2, 29)));
    }

    fn chrono_days_from_ymd(y: i32, m: u32, d: u32) -> i64 {
        // Days-from-civil (inverse of days_to_ymd).
        let y = if m <= 2 { y - 1 } else { y };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = (y - era * 400) as u32;
        let m = m as i32;
        let d = d as i32;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy as u32;
        era as i64 * 146_097 + doe as i64 - 719_468
    }
}
