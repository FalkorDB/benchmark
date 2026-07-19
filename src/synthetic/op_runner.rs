//! One measured invocation of an operation, and the loop that collects many of them.
//!
//! [`run_and_drain`] times a single query end-to-end (the wall-clock around
//! `execute().await` *and* draining every row) and reads FalkorDB's own reported internal
//! execution time from the response. A missing server-time statistic is an error rather than a
//! silent `NaN`, so it can never poison the summary.

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::query::Query;
use crate::queries_repository::QueryType;
use falkordb::AsyncGraph;
use futures::StreamExt;
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
}

/// Execute one query against `graph`, timing the full round-trip and reading the server time.
///
/// `server_timeout_ms` is the FalkorDB-side per-query guard; `client_deadline` is a Tokio-side
/// deadline so a stuck socket can't hang the probe. Reads use `GRAPH.RO_QUERY`, writes use
/// `GRAPH.QUERY`.
pub async fn run_and_drain(
    graph: &mut AsyncGraph,
    kind: QueryType,
    query: &Query,
    server_timeout_ms: i64,
    client_deadline: Duration,
) -> BenchmarkResult<OpSample> {
    let cypher = query.to_cypher();
    let started = Instant::now();

    // `ro_query` borrows the query string, so it must outlive the builder.
    let exec = async {
        match kind {
            QueryType::Read => {
                graph
                    .ro_query(cypher.as_str())
                    .with_timeout(server_timeout_ms)
                    .execute()
                    .await
            }
            QueryType::Write => {
                graph
                    .query(cypher.as_str())
                    .with_timeout(server_timeout_ms)
                    .execute()
                    .await
            }
        }
    };

    let query_result = tokio::time::timeout(client_deadline, exec)
        .await
        .map_err(|e: Elapsed| {
            OtherError(format!(
                "client deadline ({} ms) exceeded for query '{}': {}",
                client_deadline.as_millis(),
                cypher,
                e
            ))
        })?
        .map_err(|e| OtherError(format!("query '{}' failed: {:?}", cypher, e)))?;

    let cached = query_result.get_cached_execution();
    // A missing server-time stat is a hard error — never fold it into the numbers as NaN/0.
    let server_ms = query_result.get_internal_execution_time().ok_or_else(|| {
        OtherError(format!(
            "response for '{}' had no internal execution time statistic",
            cypher
        ))
    })?;

    // Drain every row so `total_ms` reflects full client-side consumption and any row-decode
    // error surfaces here rather than being silently skipped.
    let mut rows = 0usize;
    let mut data = query_result.data;
    while let Some(row) = data.next().await {
        let row = row.map_err(|e| OtherError(format!("row decode error for '{}': {:?}", cypher, e)))?;
        let _ = black_box(row);
        rows += 1;
    }

    let total_ms = started.elapsed().as_secs_f64() * 1_000.0;
    Ok(OpSample {
        server_ms,
        total_ms,
        rows,
        cached,
    })
}
