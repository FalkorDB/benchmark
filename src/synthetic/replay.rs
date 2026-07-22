//! Replay a recorded workload (see [`crate::synthetic::recording`]) against a FalkorDB endpoint.
//!
//! Unlike `synthetic run --generate` (which regenerates the graph and re-derives the commands every
//! run) and the Criterion baseline (whose iteration count adapts to observed latency), a replay
//! **loads the recorded graph** and measures the **recorded command stream** with a **fixed-length,
//! deterministic** runner — the same graph and the same measured sequence on every version, so two
//! versions' reports are genuinely comparable.
//!
//! The measured latency itself is still subject to environment noise; the *hard* guarantees a replay
//! provides are integrity (the bundle's `workload_hash` is verified on load), graph fidelity (drop +
//! load + count-verify), and result correctness (a per-op result-cardinality digest), leaving
//! latency to be compared advisorily by the [`crate::synthetic::baseline`] guard.

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::falkor::falkor_endpoint_to_redis_url;
use crate::queries_repository::QueryType;
use crate::synthetic::dataset::{self, DatasetSpec};
use crate::synthetic::op_runner::{run_and_drain, OpSample};
use crate::synthetic::recording::{self, Bundle};
use crate::synthetic::report::{
    DatasetInfo, LevelMetrics, LevelReport, Meta, OperationReport, Report, ServerInfo, SCHEMA_VERSION,
};
use crate::synthetic::{
    open_graph, provenance, redact_endpoint, summarize_samples, write_report, OpName,
};
use falkordb::AsyncGraph;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

/// How to replay a recorded bundle.
#[derive(Debug, Clone)]
pub struct ReplayConfig {
    /// Directory of the recorded bundle (see [`recording::load`]).
    pub recording_dir: PathBuf,
    /// FalkorDB endpoint to replay against.
    pub endpoint: String,
    /// Graph key to load into / measure against. `None` ⇒ the bundle manifest's graph.
    pub graph: Option<String>,
    /// When `true` (default), drop + load + verify the recorded graph before measuring. When
    /// `false`, skip loading but still **count-verify** the already-loaded graph (so a load-once /
    /// run-many flow can't drift onto the wrong graph).
    pub load: bool,
    /// Measured invocations per operation (deterministic cycle over the recorded commands).
    pub samples: usize,
    /// Warm-up invocations per operation, discarded.
    pub warmup: usize,
    pub server_timeout_ms: i64,
    pub client_deadline_ms: u64,
    /// Where to write the JSON report (Markdown alongside as `<out>.md`).
    pub out: String,
    /// Operator-supplied server image identity, recorded verbatim.
    pub server_image: Option<String>,
}

/// Replay `config`'s bundle and build a [`Report`] (a single C=1 cached level per op).
pub async fn run(config: &ReplayConfig) -> BenchmarkResult<Report> {
    if config.samples == 0 {
        return Err(OtherError("replay --samples must be greater than 0".to_string()));
    }
    let bundle = recording::load(&config.recording_dir)?;
    let spec = bundle.spec();
    let graph_name = config
        .graph
        .clone()
        .unwrap_or_else(|| bundle.manifest.graph.clone());

    let started_at_epoch_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let client_deadline = Duration::from_millis(config.client_deadline_ms);
    let mut graph = open_graph(&config.endpoint, &graph_name).await?;

    // Server provenance (best-effort: log and continue on failure).
    let redis_url = falkor_endpoint_to_redis_url(Some(&config.endpoint));
    let server = match provenance::collect(&redis_url, config.server_image.clone()).await {
        Ok(info) => info,
        Err(e) => {
            warn!("could not collect server provenance: {}", e);
            ServerInfo {
                server_image: config.server_image.clone(),
                ..Default::default()
            }
        }
    };

    if config.load {
        load_recorded_graph(&mut graph, &bundle, &graph_name, &spec, config).await?;
    } else {
        // Load-once / run-many: don't reload, but confirm the right graph is present.
        dataset::verify_counts(&mut graph, &spec, config.server_timeout_ms, client_deadline)
            .await
            .map_err(|e| {
                OtherError(format!(
                    "{} — load the recording first (don't pass --no-load)",
                    e
                ))
            })?;
    }

    let mut operations = BTreeMap::new();
    for (op, cyphers) in &bundle.commands {
        let op_report = measure_op(&mut graph, *op, cyphers, config, client_deadline).await?;
        operations.insert(op.as_str().to_string(), op_report);
    }

    // The corpus size is what the bundle actually recorded per op (not the compile-time constant),
    // so a report reflects the replayed commands even if the recorder used a different count.
    let corpus_size = bundle
        .commands
        .iter()
        .map(|(_, cyphers)| cyphers.len())
        .max()
        .unwrap_or(0);

    Ok(Report {
        schema_version: SCHEMA_VERSION,
        meta: Meta {
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            endpoint: redact_endpoint(&config.endpoint),
            graph: graph_name,
            samples: config.samples,
            warmup: config.warmup,
            concurrency: vec![1],
            seed: bundle.manifest.corpus_seed,
            corpus_size,
            server_timeout_ms: config.server_timeout_ms,
            client_deadline_ms: config.client_deadline_ms,
            connection: "pool(size=1)".to_string(),
            started_at_epoch_secs,
            server,
            host: crate::synthetic::host::collect(),
            // The bundle's workload_hash is the comparison key: it attests the graph *and* the
            // commands, so the existing guard compares replays of the same bundle safely.
            dataset: Some(DatasetInfo {
                seed: spec.seed,
                nodes: spec.nodes,
                edges: spec.edges,
                corpus_hash: bundle.manifest.workload_hash.clone(),
            }),
        },
        operations,
    })
}

/// Replay, print the console summary, and write the JSON + Markdown report.
pub async fn run_and_report(config: &ReplayConfig) -> BenchmarkResult<()> {
    let report = run(config).await?;
    println!("{}", report.to_console());
    write_report(&report, &config.out).await
}

/// Drop `graph`, execute the bundle's recorded load statements, and verify the node/edge counts.
async fn load_recorded_graph(
    graph: &mut AsyncGraph,
    bundle: &Bundle,
    graph_name: &str,
    spec: &DatasetSpec,
    config: &ReplayConfig,
) -> BenchmarkResult<()> {
    // Bulk loads do real server-side work, so give them a generous deadline and a matching
    // server-side timeout (mirroring `synthetic run --generate`).
    let load_deadline = Duration::from_millis(config.client_deadline_ms.max(60_000));
    let load_server_timeout_ms = config
        .server_timeout_ms
        .max(i64::try_from(load_deadline.as_millis()).unwrap_or(i64::MAX));

    // Log the graph actually being loaded (the resolved target, which `--graph` can override — not
    // necessarily the bundle's recorded graph name).
    info!(
        "loading recorded graph ({} statements) into '{}'",
        bundle.graph_statements.len(),
        graph_name
    );

    // Drop + load the recorded statements + verify counts — the exact path `--generate` uses.
    dataset::load_dataset(
        graph,
        bundle.graph_statements.iter().cloned(),
        spec,
        load_deadline,
        load_server_timeout_ms,
    )
    .await
}

/// Measure one operation: prime the plan cache, capture a result-cardinality digest, run a fixed
/// warm-up then a fixed measured cycle over the recorded commands, and summarize.
async fn measure_op(
    graph: &mut AsyncGraph,
    op: OpName,
    cyphers: &[String],
    config: &ReplayConfig,
    client_deadline: Duration,
) -> BenchmarkResult<OperationReport> {
    if cyphers.is_empty() {
        return Err(OtherError(format!("op '{}' has no recorded commands", op.as_str())));
    }
    let st = config.server_timeout_ms;

    // Untimed cardinality pass over every recorded command (in order) → the result digest that the
    // guard uses as a correctness gate. This also primes the plan cache for *every* command (so the
    // measured cycle below never pays a first-time compile), which is why there's no separate prime.
    let mut cardinalities = Vec::with_capacity(cyphers.len());
    for cypher in cyphers {
        let sample = run_and_drain(graph, QueryType::Read, cypher, st, client_deadline)
            .await
            .map_err(|e| OtherError(format!("verifying '{}': {}", op.as_str(), e)))?;
        cardinalities.push(sample.rows);
    }
    let result_digest = cardinality_digest(op, &cardinalities);

    // Warm-up (discarded), then the fixed-length measured cycle.
    for i in 0..config.warmup {
        let cypher = &cyphers[i % cyphers.len()];
        run_and_drain(graph, QueryType::Read, cypher, st, client_deadline)
            .await
            .map_err(|e| OtherError(format!("warm-up '{}': {}", op.as_str(), e)))?;
    }

    let mut samples: Vec<OpSample> = Vec::with_capacity(config.samples);
    let start = Instant::now();
    for i in 0..config.samples {
        let cypher = &cyphers[i % cyphers.len()];
        let sample = run_and_drain(graph, QueryType::Read, cypher, st, client_deadline)
            .await
            .map_err(|e| OtherError(format!("measuring '{}': {}", op.as_str(), e)))?;
        samples.push(sample);
    }
    let elapsed = start.elapsed().as_secs_f64();
    let throughput_ops_per_sec = if elapsed > 0.0 {
        samples.len() as f64 / elapsed
    } else {
        0.0
    };

    let metrics = summarize_samples(&samples)?;
    Ok(OperationReport {
        levels: vec![LevelReport {
            concurrency: 1,
            cached: Some(LevelMetrics {
                throughput_ops_per_sec,
                metrics,
            }),
            uncached: None,
            compilation_ms_median: None,
        }],
        result_digest: Some(result_digest),
    })
}

/// A `sha256:…` digest over an operation's per-command result cardinality (row counts), in command
/// order. Deterministic given the same graph + recorded commands, and length-framed so it can't
/// alias a different op's digest.
fn cardinality_digest(
    op: OpName,
    rows: &[usize],
) -> String {
    let mut h = Sha256::new();
    let name = op.as_str().as_bytes();
    h.update((name.len() as u64).to_le_bytes());
    h.update(name);
    h.update((rows.len() as u64).to_le_bytes());
    for &r in rows {
        h.update((r as u64).to_le_bytes());
    }
    format!("sha256:{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cardinality_digest_is_deterministic_and_sensitive() {
        let a = cardinality_digest(OpName::MatchByIndex, &[1, 1, 1]);
        let b = cardinality_digest(OpName::MatchByIndex, &[1, 1, 1]);
        assert_eq!(a, b);
        // A different cardinality vector changes the digest.
        assert_ne!(a, cardinality_digest(OpName::MatchByIndex, &[1, 0, 1]));
        // The op name is part of the digest (same rows, different op).
        assert_ne!(a, cardinality_digest(OpName::Expand1Hop, &[1, 1, 1]));
        assert!(a.starts_with("sha256:"));
    }

    #[tokio::test]
    async fn run_rejects_zero_samples() {
        // Guarded before any disk/server access, so this stays hermetic.
        let config = ReplayConfig {
            recording_dir: PathBuf::from("/nonexistent/recording"),
            endpoint: "falkor://127.0.0.1:6379".to_string(),
            graph: None,
            load: true,
            samples: 0,
            warmup: 0,
            server_timeout_ms: 5_000,
            client_deadline_ms: 6_000,
            out: "unused.json".to_string(),
            server_image: None,
        };
        let err = run(&config).await.unwrap_err();
        assert!(format!("{err}").contains("samples must be greater than 0"), "got: {err}");
    }
}
