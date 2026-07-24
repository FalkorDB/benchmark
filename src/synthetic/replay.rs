//! Backs `synthetic run --recording`: measure a recorded workload (see
//! [`crate::synthetic::recording`]) against a FalkorDB endpoint.
//!
//! Unlike `synthetic run --generate` (which regenerates the graph and re-derives the commands every
//! run) and the Criterion baseline (whose iteration count adapts to observed latency), this **loads
//! the recorded graph** and measures the **recorded command stream** — the same graph and the same
//! commands on every version, so two versions' reports are genuinely comparable. It runs an untimed
//! single-flight **reference pass** (capturing each command's result shape), then measures each op
//! through the shared closed-loop engine across the configured **concurrency sweep + cache modes**,
//! and — at the highest concurrency — **verifies results are unchanged under concurrency**.
//!
//! The measured latency itself is still subject to environment noise; the *hard* guarantees are
//! integrity (the bundle's `workload_hash` is verified on load), graph fidelity (drop + load +
//! count-verify), and result correctness (a per-op result-**value** digest + the concurrency check),
//! leaving latency to be compared advisorily by the [`crate::synthetic::baseline`] guard.

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::falkor::falkor_endpoint_to_redis_url;
use crate::queries_repository::QueryType;
use crate::synthetic::catalog::DEFAULT_RESET_EVERY;
use crate::synthetic::dataset::{self, DatasetSpec};
use crate::synthetic::op_runner::{capture_result, ResultShape};
use crate::synthetic::recording::{self, Bundle};
use crate::synthetic::report::{DatasetInfo, Meta, Report, ServerInfo, SCHEMA_VERSION};
use crate::synthetic::{
    measure_op, normalize_concurrency, open_graph, provenance, redact_endpoint, write_report,
    CacheSelection, Config, MeasureTarget, OpKey,
};
use falkordb::AsyncGraph;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
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
    /// Measured invocations per operation, per worker.
    pub samples: usize,
    /// Warm-up invocations per operation, discarded.
    pub warmup: usize,
    /// Concurrency levels to sweep (closed-loop worker counts `C`).
    pub concurrency: Vec<usize>,
    /// Plan-cache condition(s) to measure: cached, uncached, or both.
    pub cache: CacheSelection,
    pub server_timeout_ms: i64,
    pub client_deadline_ms: u64,
    /// Where to write the JSON report (Markdown alongside as `<out>.md`).
    pub out: String,
    /// Operator-supplied server image identity, recorded verbatim.
    pub server_image: Option<String>,
    /// Optional display name for this run (e.g. `pr`/`main`), recorded into the report.
    pub label: Option<String>,
}

/// Replay `config`'s bundle: load the recorded graph, then measure the recorded commands through the
/// closed-loop engine across the concurrency sweep + cache modes, verifying results are unchanged by
/// concurrency. Builds the [`Report`].
pub async fn run(config: &ReplayConfig) -> BenchmarkResult<Report> {
    if config.samples == 0 {
        return Err(OtherError("run --recording --samples must be greater than 0".to_string()));
    }
    let concurrency = normalize_concurrency(&config.concurrency)?;
    let bundle = recording::load(&config.recording_dir)?;
    // Fail closed on a write op: v1 recording is read-only, and the measurement path uses RO_QUERY.
    // A hand-crafted bundle naming a write op would otherwise be run as a read.
    if let Some((op, _)) = bundle
        .commands
        .iter()
        .find(|(op, _)| op.kind() == QueryType::Write)
    {
        return Err(OtherError(format!(
            "recorded op '{}' is a write op — replaying writes is not supported (v1 records reads \
             only)",
            op.name()
        )));
    }
    let dataset_spec = bundle.spec();
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
        load_recorded_graph(&mut graph, &bundle, &graph_name, &dataset_spec, config).await?;
    } else {
        // Load-once / run-many: don't reload, but confirm the right graph is present.
        dataset::verify_counts(&mut graph, &dataset_spec, config.server_timeout_ms, client_deadline)
            .await
            .map_err(|e| {
                OtherError(format!(
                    "{} — load the recording first (don't pass --no-load)",
                    e
                ))
            })?;
    }

    // Reference pass (untimed, single-flight) over every recorded command: capture each result's
    // shape (cardinality + order-independent value digest). This is the correctness oracle and also
    // primes the plan cache. Reads return scalars, so a single connection is safe.
    let st = config.server_timeout_ms;
    let mut reference: Vec<(OpKey, Arc<Vec<String>>, Vec<ResultShape>)> =
        Vec::with_capacity(bundle.commands.len());
    for (op, cyphers) in &bundle.commands {
        if cyphers.is_empty() {
            return Err(OtherError(format!("op '{}' has no recorded commands", op.name())));
        }
        let mut shapes = Vec::with_capacity(cyphers.len());
        for c in cyphers {
            shapes.push(
                capture_result(&mut graph, c, st, client_deadline)
                    .await
                    .map_err(|e| OtherError(format!("capturing '{}': {}", op.name(), e)))?,
            );
        }
        reference.push((op.clone(), Arc::new(cyphers.clone()), shapes));
    }
    // Setup connection done; drop it so it isn't an idle extra connection during the sweep.
    drop(graph);

    // Engine config for the recorded workload (writes/reset are irrelevant — recorded ops are reads).
    let engine_config = Config {
        endpoint: config.endpoint.clone(),
        graph: graph_name.clone(),
        // `ops` is unused by the measurement path (`measure_op` replays the passed-in corpus, not
        // the config's op list) — leave it empty rather than lossily mapping string-keyed `OpKey`s
        // back to the `OpName` enum this field holds.
        ops: Vec::new(),
        samples: config.samples,
        warmup: config.warmup,
        concurrency: concurrency.clone(),
        reset_every: DEFAULT_RESET_EVERY,
        seed: bundle.manifest.corpus_seed,
        server_timeout_ms: config.server_timeout_ms,
        client_deadline_ms: config.client_deadline_ms,
        cache: config.cache,
        out: config.out.clone(),
        server_image: config.server_image.clone(),
        label: config.label.clone(),
        dataset: None,
    };
    let run_token = rand::random_range(0..=u64::MAX);
    let uid_alloc = AtomicU64::new(0);
    let max_c = concurrency.iter().copied().max().unwrap_or(1);

    // Per-op result-gating policy from the recorded manifest (keyed by the op's unique name). A
    // result-N/A op — a shape whose result set isn't byte-stable (LIMIT-without-ORDER, top-k,
    // float scores — design §3.2 / Decision 4) — is still loaded, replayed, and timed, but its
    // result is neither cross-concurrency-verified nor digested, so a benign result difference
    // never fails the A/B non-divergence gate. Unknown names default to gated (the safe default).
    let result_gated: std::collections::HashMap<&str, bool> = bundle
        .manifest
        .ops
        .iter()
        .map(|e| (e.name.as_str(), e.result_gated))
        .collect();

    let mut operations = BTreeMap::new();
    for (op, corpus, shapes) in &reference {
        let is_gated = result_gated.get(op.name()).copied().unwrap_or(true);
        // Verify results are IDENTICAL at the highest concurrency (untimed) before trusting the
        // measured latencies — a concurrent path that returns different/wrong results is a hard
        // fail. Skipped for result-N/A ops, whose results aren't required to be stable.
        if max_c > 1 && is_gated {
            verify_concurrent(
                &config.endpoint,
                &graph_name,
                corpus,
                shapes,
                max_c,
                st,
                client_deadline,
            )
            .await
            .map_err(|e| {
                OtherError(format!(
                    "op '{}' returned different results at concurrency {}: {}",
                    op.name(),
                    max_c,
                    e
                ))
            })?;
        }
        let mut op_report = measure_op(
            &engine_config,
            &concurrency,
            MeasureTarget::read(),
            Arc::clone(corpus),
            run_token,
            &uid_alloc,
            client_deadline,
        )
        .await?;
        // Gate the result only for byte-stable shapes; a result-N/A op reports `None` so the diff
        // guard renders it N/A instead of comparing a non-deterministic digest.
        op_report.result_digest =
            is_gated.then(|| op_result_digest(op.name(), shapes));
        operations.insert(op.name().to_string(), op_report);
    }

    // The corpus size is what the bundle actually recorded per op (not the compile-time constant).
    let corpus_size = reference
        .iter()
        .map(|(_, corpus, _)| corpus.len())
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
            concurrency: concurrency.clone(),
            seed: bundle.manifest.corpus_seed,
            corpus_size,
            server_timeout_ms: config.server_timeout_ms,
            client_deadline_ms: config.client_deadline_ms,
            connection: "pool(size=1) per worker".to_string(),
            started_at_epoch_secs,
            server,
            host: crate::synthetic::host::collect(),
            // The bundle's workload_hash attests the graph *and* the commands, so the guard compares
            // replays of the same bundle safely.
            dataset: Some(DatasetInfo {
                seed: dataset_spec.seed,
                nodes: dataset_spec.nodes,
                edges: dataset_spec.edges,
                workload_hash: bundle.manifest.workload_hash.clone(),
            }),
            label: config.label.clone(),
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

/// Verify the recorded commands return the **same results** when run concurrently: spin up `workers`
/// connections, run every command on each, and assert each command's [`ResultShape`] equals the
/// single-flight reference. Untimed — a pure correctness check that concurrency didn't change
/// results. Any mismatch (or error) fails the whole run.
async fn verify_concurrent(
    endpoint: &str,
    graph_name: &str,
    cyphers: &Arc<Vec<String>>,
    expected: &[ResultShape],
    workers: usize,
    server_timeout_ms: i64,
    client_deadline: Duration,
) -> BenchmarkResult<()> {
    let expected: Arc<Vec<ResultShape>> = Arc::new(expected.to_vec());
    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let endpoint = endpoint.to_string();
        let graph_name = graph_name.to_string();
        let cyphers = Arc::clone(cyphers);
        let expected = Arc::clone(&expected);
        handles.push(tokio::spawn(async move {
            let mut graph = open_graph(&endpoint, &graph_name).await?;
            for (i, cypher) in cyphers.iter().enumerate() {
                let shape = capture_result(&mut graph, cypher, server_timeout_ms, client_deadline).await?;
                if shape != expected[i] {
                    return Err(OtherError(format!(
                        "command #{i} returned {:?}, expected {:?}",
                        shape, expected[i]
                    )));
                }
            }
            Ok::<(), crate::error::BenchmarkError>(())
        }));
    }
    for h in handles {
        h.await
            .map_err(|e| OtherError(format!("concurrent verify task panicked: {e}")))??;
    }
    Ok(())
}

/// A `sha256:…` digest over an operation's per-command result **values** (order-independent within a
/// row set), in command order. Deterministic given the same graph + recorded commands, and
/// length-framed so it can't alias a different op's digest. Two versions returning different results
/// for the same recorded command produce different digests.
fn op_result_digest(
    name: &str,
    shapes: &[ResultShape],
) -> String {
    let mut h = Sha256::new();
    let name = name.as_bytes();
    h.update((name.len() as u64).to_le_bytes());
    h.update(name);
    h.update((shapes.len() as u64).to_le_bytes());
    for s in shapes {
        h.update((s.rows as u64).to_le_bytes());
        let d = s.value_digest.as_bytes();
        h.update((d.len() as u64).to_le_bytes());
        h.update(d);
    }
    format!("sha256:{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(rows: usize, digest: &str) -> ResultShape {
        ResultShape {
            rows,
            value_digest: format!("sha256:{digest}"),
        }
    }

    #[test]
    fn op_result_digest_is_deterministic_and_sensitive() {
        let base = vec![shape(1, "aa"), shape(3, "bb")];
        let a = op_result_digest("match_by_index", &base);
        assert_eq!(a, op_result_digest("match_by_index", &base));
        // A different value digest changes it (even at the same cardinality).
        assert_ne!(a, op_result_digest("match_by_index", &[shape(1, "aa"), shape(3, "cc")]));
        // A different cardinality changes it.
        assert_ne!(a, op_result_digest("match_by_index", &[shape(2, "aa"), shape(3, "bb")]));
        // The op name is part of the digest.
        assert_ne!(a, op_result_digest("expand_1hop", &base));
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
            concurrency: vec![1],
            cache: CacheSelection::Cached,
            server_timeout_ms: 5_000,
            client_deadline_ms: 6_000,
            out: "unused.json".to_string(),
            server_image: None,
            label: None,
        };
        let err = run(&config).await.unwrap_err();
        assert!(format!("{err}").contains("samples must be greater than 0"), "got: {err}");
    }
}
