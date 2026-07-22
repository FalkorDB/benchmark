//! One measured invocation of an operation, and the loop that collects many of them.
//!
//! [`run_and_drain`] times a single query end-to-end (the wall-clock around
//! `execute().await` *and* draining every row) and reads FalkorDB's own reported internal
//! execution time from the response. A missing server-time statistic is an error rather than a
//! silent `NaN`, so it can never poison the summary.

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::queries_repository::QueryType;
use crate::synthetic::writes::MutationStats;
use falkordb::{AsyncGraph, FalkorValue};
use futures::StreamExt;
use sha2::{Digest, Sha256};
use std::hint::black_box;
use std::time::{Duration, Instant};
use tokio::time::error::Elapsed;

/// A single paired latency measurement for one operation invocation.
#[derive(Debug, Clone, Copy)]
pub struct OpSample {
    /// FalkorDB's reported internal execution time, in milliseconds.
    pub server_ms: f64,
    /// End-to-end client wall-clock (send → response received → all rows drained), in milliseconds.
    pub total_ms: f64,
    /// Number of rows drained from the result.
    pub rows: usize,
    /// Whether the server reported a cached execution plan (`None` if the stat was absent).
    pub cached: Option<bool>,
    /// The mutation counters FalkorDB reported (all zero for reads), so a write worker can verify
    /// each sample actually effected its intended change (see [`crate::synthetic::writes`]).
    pub mutations: MutationStats,
}

/// Execute one query against `graph`, timing the full round-trip and reading the server time.
///
/// `cypher` is the already-rendered query string (the caller controls its exact text, e.g. to
/// force a plan-cache miss). `server_timeout_ms` is the FalkorDB-side per-query guard;
/// `client_deadline` bounds the **entire** operation — execute *and* row draining — so a stuck
/// socket or a slow stream can't hang the probe. Reads use `GRAPH.RO_QUERY`, writes `GRAPH.QUERY`.
pub async fn run_and_drain(
    graph: &mut AsyncGraph,
    kind: QueryType,
    cypher: &str,
    server_timeout_ms: i64,
    client_deadline: Duration,
) -> BenchmarkResult<OpSample> {
    let started = Instant::now();

    // The whole operation (execute + drain) runs under one deadline.
    let measured = async {
        let query_result = match kind {
            QueryType::Read => {
                graph
                    .ro_query(cypher)
                    .with_timeout(server_timeout_ms)
                    .execute()
                    .await
            }
            QueryType::Write => {
                graph
                    .query(cypher)
                    .with_timeout(server_timeout_ms)
                    .execute()
                    .await
            }
        }
        .map_err(|e| OtherError(format!("query '{}' failed: {:?}", cypher, e)))?;

        let cached = query_result.get_cached_execution();
        // Read the mutation counters (absent ⇒ 0, e.g. for reads) before draining the stream, so a
        // write worker can verify the sample actually did what the operation intends.
        let mutations = MutationStats {
            nodes_created: query_result.get_nodes_created().unwrap_or(0),
            nodes_deleted: query_result.get_nodes_deleted().unwrap_or(0),
            relationships_created: query_result.get_relationship_created().unwrap_or(0),
            properties_set: query_result.get_properties_set().unwrap_or(0),
        };
        // A missing server-time stat is a hard error — never fold it into the numbers as NaN/0.
        let server_ms = validate_server_ms(query_result.get_internal_execution_time(), cypher)?;

        // Drain every row so `total_ms` reflects full client-side consumption and any row-decode
        // error surfaces here rather than being silently skipped.
        let mut rows = 0usize;
        let mut data = query_result.data;
        while let Some(row) = data.next().await {
            let row =
                row.map_err(|e| OtherError(format!("row decode error for '{}': {:?}", cypher, e)))?;
            let _ = black_box(row);
            rows += 1;
        }
        Ok::<(f64, usize, Option<bool>, MutationStats), crate::error::BenchmarkError>((
            server_ms, rows, cached, mutations,
        ))
    };

    let (server_ms, rows, cached, mutations) = tokio::time::timeout(client_deadline, measured)
        .await
        .map_err(|e: Elapsed| {
            OtherError(format!(
                "client deadline ({} ms) exceeded for query '{}': {}",
                client_deadline.as_millis(),
                cypher,
                e
            ))
        })??;

    let total_ms = started.elapsed().as_secs_f64() * 1_000.0;
    Ok(OpSample {
        server_ms,
        total_ms,
        rows,
        cached,
        mutations,
    })
}

/// A canonical, order-independent fingerprint of a read query's result set: the row count plus a
/// SHA-256 over the **sorted** canonical rendering of every row. Used by the untimed correctness
/// passes (reference + concurrency-invariance check) — never on the timed measurement path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultShape {
    /// Number of rows returned.
    pub rows: usize,
    /// `sha256:…` over the sorted per-row canonical strings (order-independent).
    pub value_digest: String,
}

/// Execute a **read** query and capture its [`ResultShape`] (row count + order-independent value
/// digest), bounded by `client_deadline`. Rows are rendered to canonical strings, sorted, then
/// hashed, so a differing row *order* doesn't matter but differing *values* or *cardinality* do.
pub async fn capture_result(
    graph: &mut AsyncGraph,
    cypher: &str,
    server_timeout_ms: i64,
    client_deadline: Duration,
) -> BenchmarkResult<ResultShape> {
    let fut = async {
        let query_result = graph
            .ro_query(cypher)
            .with_timeout(server_timeout_ms)
            .execute()
            .await
            .map_err(|e| OtherError(format!("capture query '{}' failed: {:?}", cypher, e)))?;
        let mut rows: Vec<String> = Vec::new();
        let mut data = query_result.data;
        while let Some(row) = data.next().await {
            let row =
                row.map_err(|e| OtherError(format!("row decode error for '{}': {:?}", cypher, e)))?;
            rows.push(canonical_row(&row.into_values()));
        }
        Ok::<Vec<String>, crate::error::BenchmarkError>(rows)
    };
    let mut rows = tokio::time::timeout(client_deadline, fut)
        .await
        .map_err(|e: Elapsed| {
            OtherError(format!(
                "client deadline ({} ms) exceeded capturing '{}': {}",
                client_deadline.as_millis(),
                cypher,
                e
            ))
        })??;
    let count = rows.len();
    rows.sort_unstable();
    let mut h = Sha256::new();
    h.update((count as u64).to_le_bytes());
    for r in &rows {
        h.update((r.len() as u64).to_le_bytes());
        h.update(r.as_bytes());
    }
    Ok(ResultShape {
        rows: count,
        value_digest: format!("sha256:{:x}", h.finalize()),
    })
}

/// Canonicalize one row (its column values, in column order) to a stable string.
fn canonical_row(values: &[FalkorValue]) -> String {
    let mut s = String::new();
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            s.push('\u{1f}'); // unit separator — can't appear in our scalar renderings
        }
        s.push_str(&canonical_value(v));
    }
    s
}

/// A stable string for one scalar [`FalkorValue`] (the shapes our read ops return). Floats use their
/// bit pattern so equal values always render identically; other shapes fall back to `Debug`.
fn canonical_value(v: &FalkorValue) -> String {
    match v {
        FalkorValue::I64(i) => format!("i:{i}"),
        FalkorValue::F64(f) => format!("f:{:016x}", f.to_bits()),
        FalkorValue::Bool(b) => format!("b:{b}"),
        FalkorValue::String(s) => format!("s:{s}"),
        FalkorValue::None => "null".to_string(),
        other => format!("o:{other:?}"),
    }
}

/// Validate FalkorDB's reported internal execution time: it must be present, finite and
/// non-negative. A missing or garbage statistic becomes a hard error rather than poisoning the
/// summary with a `NaN`/`0`.
fn validate_server_ms(
    reported: Option<f64>,
    cypher: &str,
) -> BenchmarkResult<f64> {
    let server_ms = reported.ok_or_else(|| {
        OtherError(format!(
            "response for '{}' had no internal execution time statistic",
            cypher
        ))
    })?;
    if !server_ms.is_finite() || server_ms < 0.0 {
        return Err(OtherError(format!(
            "response for '{}' reported an invalid internal execution time: {}",
            cypher, server_ms
        )));
    }
    Ok(server_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_value_covers_every_shape() {
        assert_eq!(canonical_value(&FalkorValue::I64(7)), "i:7");
        assert_eq!(canonical_value(&FalkorValue::Bool(true)), "b:true");
        assert_eq!(canonical_value(&FalkorValue::String("x".into())), "s:x");
        assert_eq!(canonical_value(&FalkorValue::None), "null");
        // Floats use their bit pattern so equal values render identically.
        assert_eq!(
            canonical_value(&FalkorValue::F64(1.5)),
            canonical_value(&FalkorValue::F64(1.5))
        );
        assert!(canonical_value(&FalkorValue::F64(1.5)).starts_with("f:"));
        // A non-scalar falls back to Debug (tagged `o:`).
        assert!(canonical_value(&FalkorValue::Array(vec![FalkorValue::I64(1)])).starts_with("o:"));
    }

    #[test]
    fn canonical_row_joins_columns_stably() {
        let row = vec![FalkorValue::I64(3), FalkorValue::String("a".into())];
        let s = canonical_row(&row);
        assert!(s.contains("i:3") && s.contains("s:a"));
        // Column order is preserved (a different order ⇒ different string).
        let swapped = vec![FalkorValue::String("a".into()), FalkorValue::I64(3)];
        assert_ne!(canonical_row(&row), canonical_row(&swapped));
    }

    #[test]
    fn validate_server_ms_accepts_finite_non_negative() {
        assert_eq!(validate_server_ms(Some(0.0), "q").unwrap(), 0.0);
        assert_eq!(validate_server_ms(Some(1.5), "q").unwrap(), 1.5);
    }

    #[test]
    fn validate_server_ms_rejects_missing_and_invalid() {
        // Absent statistic.
        assert!(validate_server_ms(None, "q").is_err());
        // Non-finite and negative values are both rejected.
        assert!(validate_server_ms(Some(f64::NAN), "q").is_err());
        assert!(validate_server_ms(Some(f64::INFINITY), "q").is_err());
        assert!(validate_server_ms(Some(-0.001), "q").is_err());
    }
}
