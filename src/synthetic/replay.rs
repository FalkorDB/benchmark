//! Backs `synthetic run --recording`: measure a recorded workload (see
//! [`crate::synthetic::recording`]) against a FalkorDB endpoint.
//!
//! Unlike `synthetic run --generate` (which regenerates the graph and re-derives the commands every
//! run) and the Criterion baseline (whose iteration count adapts to observed latency), this **loads
//! the recorded graph** and measures the **recorded command stream** — the same graph and the same
//! commands on every version, so two versions' reports are genuinely comparable. It runs an untimed
//! single-flight **reference pass** (capturing each result-gated command's result shape; a
//! result-N/A op skips the full capture — design §3.4), then measures each op through the shared
//! closed-loop engine across the configured **concurrency sweep + cache modes** — overlaying each
//! op's recorded [`RecordedBudget`](crate::synthetic::catalog::RecordedBudget) on the run's global knobs, exactly as a generated run overlays
//! the catalog's per-op budget — and, at each op's highest concurrency, **verifies results are
//! unchanged under concurrency**.
//!
//! The measured latency itself is still subject to environment noise; the *hard* guarantees are
//! integrity (the bundle's `workload_hash` is verified on load), graph fidelity (drop + load +
//! count-verify), and result correctness (a per-op result-**value** digest + the concurrency check),
//! leaving latency to be compared advisorily by the [`crate::synthetic::baseline`] guard.
//!
//! **Write bundles** (recording format v2, Phase 7 §4.1 latency tier): a bundle whose ops are
//! writes is measured via `GRAPH.QUERY` at **C=1 only**, with the base graph **reset (drop +
//! load + count-verify) before every measured cell** (op × cache mode) so mutation drift stays
//! bounded to one cell's invocations. Nothing is asserted about results or mutation counters —
//! outcomes are state/value-dependent (§10), so the latency tier records `result_digest: None`
//! and leaves correctness to the deferred oracle tier. The replay ends with an **error-safe final
//! restore**: the recorded base is reloaded (on success *and* failure) and its node/edge content
//! digests are verified against the pristine post-load capture, so a write replay never silently
//! leaves a mutated graph behind — if the restore itself fails, that failure is surfaced
//! (combined with the measurement error when both fail).

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::falkor::falkor_endpoint_to_redis_url;
use crate::queries_repository::QueryType;
use crate::synthetic::catalog::DEFAULT_RESET_EVERY;
use crate::synthetic::dataset::{self, DatasetSpec};
use crate::synthetic::op_runner::{capture_result, run_and_drain, ResultShape};
use crate::synthetic::recording::{self, Bundle};
use crate::synthetic::writes::{verify_mutation, ExpectedOutcome};
use crate::synthetic::report::{
    DatasetInfo, LevelReport, Meta, OperationReport, Report, ServerInfo, SCHEMA_VERSION,
};
use crate::synthetic::{
    measure_op, normalize_concurrency, open_graph, provenance, redact_endpoint,
    validate_op_config, write_report, CacheMode, CacheSelection, Config, MeasureTarget, OpKey,
};
use falkordb::AsyncGraph;
use futures::StreamExt;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
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
/// concurrency (reads) or resetting the base graph per measured cell (writes — see the module docs).
/// Builds the [`Report`].
pub async fn run(config: &ReplayConfig) -> BenchmarkResult<Report> {
    if config.samples == 0 {
        return Err(OtherError("run --recording --samples must be greater than 0".to_string()));
    }
    let concurrency = normalize_concurrency(&config.concurrency)?;
    let bundle = recording::load(&config.recording_dir)?;
    // A bundle is single-kind: reads replay through RO_QUERY exactly as before; a write bundle
    // (format v2) takes the Phase 7 write path. Its invariants — no mixed kinds, no --no-load,
    // C=1 only, nothing result-gated, nothing capability-gated — are guarded up front, offline,
    // before any connection.
    let has_writes = bundle.commands.iter().any(|(op, _)| op.kind() == QueryType::Write);
    if has_writes {
        validate_write_replay(&bundle, config, &concurrency)?;
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

    // Phase 7 §3.5: capture the pristine base-graph content (node + edge digests) right after the
    // initial load, so the error-safe final restore below can verify the graph it leaves behind is
    // EXACTLY the recorded base — content-verified, not just count-verified.
    let pristine = if has_writes {
        Some(capture_graph_content(&mut graph, config).await?)
    } else {
        None
    };

    // Per-op replay policy from the recorded manifest (keyed by the op's unique name):
    // `result_gated` + the per-op `budget` (design §3.4). A result-N/A op — a shape whose result
    // set isn't byte-stable (LIMIT-without-ORDER, top-k, float scores — design §3.2 / Decision 4)
    // — is still loaded, replayed, and timed, but its result is neither captured in full, nor
    // cross-concurrency-verified, nor digested, so a benign result difference never fails the A/B
    // non-divergence gate.
    let op_entries: std::collections::HashMap<&str, &recording::OpEntry> = bundle
        .manifest
        .ops
        .iter()
        .map(|e| (e.name.as_str(), e))
        .collect();
    // `load()` builds `bundle.commands` from `manifest.ops`, so every replayed name has an entry
    // by construction — verify anyway so a future load-path change fails gracefully, not deep in
    // the measurement loops.
    for (op, _) in &bundle.commands {
        if !op_entries.contains_key(op.name()) {
            return Err(OtherError(format!(
                "op '{}' replayed without a manifest entry (corrupt bundle)",
                op.name()
            )));
        }
    }
    let entry_for = |name: &str| {
        *op_entries
            .get(name)
            .expect("every replayed op name was validated against the manifest above")
    };

    // Engine config for the recorded workload (`reset_every` only frames catalog write scratch,
    // which recorded ops never use — recorded writes measure with `write: None`, resetting via
    // whole-graph reloads instead).
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

    // Validate every op's resolved budget overlay up front — before the reference pass uses its
    // timeouts — so a malformed manifest (zeroed samples, a non-positive timeout, an empty sweep)
    // fails closed with an error naming the op instead of a confusing driver error mid-capture.
    for (op, _) in &bundle.commands {
        let op_config = engine_config.with_recorded_budget(&entry_for(op.name()).budget);
        validate_op_config(op.name(), &op_config)?;
    }

    // Capability probe (design Phase 6 §3.5), before the reference pass: when any op's manifest
    // entry names a required procedure, ask the engine's registry once and **skip** every op whose
    // procedure is absent — recorded in the report with the reason, but never executed (a missing
    // procedure would otherwise fail the whole replay). Ops without a capability never probe.
    let any_capability =
        bundle.commands.iter().any(|(op, _)| entry_for(op.name()).capability.is_some());
    let available = if any_capability {
        // `probe_procedures` errors already name the capability probe — bubble them up as-is.
        Some(probe_procedures(&mut graph, config.server_timeout_ms, client_deadline).await?)
    } else {
        None
    };
    let skip_reason = |name: &str| -> Option<String> {
        let procedure = entry_for(name).capability.as_deref()?;
        let available = available.as_ref()?;
        (!available.contains(&procedure.to_lowercase()))
            .then(|| format!("engine lacks procedure '{}' (capability probe)", procedure))
    };
    let mut skipped: BTreeMap<String, String> = BTreeMap::new();
    for (op, _) in &bundle.commands {
        if let Some(reason) = skip_reason(op.name()) {
            info!("skipping op '{}': {}", op.name(), reason);
            skipped.insert(op.name().to_string(), reason);
        }
    }

    let run_token = rand::random_range(0..=u64::MAX);
    let uid_alloc = AtomicU64::new(0);

    let mut operations = BTreeMap::new();
    // Skipped ops get an empty-levels entry carrying the skip reason (BTreeMap renders by key),
    // so the report keeps the full recorded op set and the diff/regression guards can tell
    // "skipped" from "not recorded".
    for (name, reason) in &skipped {
        operations.insert(
            name.clone(),
            crate::synthetic::report::OperationReport {
                levels: Vec::new(),
                result_digest: None,
                policy: None,
                skipped: Some(reason.clone()),
            },
        );
    }
    // The reference pass and the measurement loop both run inside an immediately-awaited block so
    // a write replay can run its error-safe final restore on success AND failure (§3.5) before any
    // error propagates — including a failed write fail-fast probe, where earlier ops' probes (or
    // the failing command's own partial execution) may already have mutated the graph.
    let measured: BenchmarkResult<()> = async {
        // Reference pass (untimed, single-flight): capture each result's shape (cardinality +
        // order-independent value digest) for every **result-gated** command — the correctness oracle,
        // which also primes the plan cache. A result-N/A op's shapes are never verified or digested, so
        // it skips the full capture (a heavy shape would pay corpus × per-call latency for nothing —
        // design §3.4) and only its first command runs once, to fail fast on a broken recorded command.
        // Captures run under the op's budgeted timeouts so a budgeted heavy op can't trip the global
        // deadline before measurement starts. Reads return scalars, so a single connection is safe.
        let mut reference: Vec<(OpKey, Arc<Vec<String>>, Vec<ResultShape>)> =
            Vec::with_capacity(bundle.commands.len());
        for (op, cyphers) in &bundle.commands {
            if cyphers.is_empty() {
                return Err(OtherError(format!("op '{}' has no recorded commands", op.name())));
            }
            if skipped.contains_key(op.name()) {
                continue; // capability-skipped: never executed, not even the fail-fast probe
            }
            let entry = entry_for(op.name());
            let op_st = entry.budget.server_timeout_ms.unwrap_or(config.server_timeout_ms);
            let op_deadline = entry
                .budget
                .client_deadline_ms
                .map(Duration::from_millis)
                .unwrap_or(client_deadline);
            if op.kind() == QueryType::Write {
                // Fail-fast probe for a write op, via GRAPH.QUERY (`RO_QUERY` rejects writes
                // server-side): run the first recorded command once, untimed, so a broken command
                // fails here instead of mid-measurement. Its mutation is erased by the first
                // measured cell's base reset. No shapes: the latency tier asserts nothing (§4.1).
                run_and_drain(&mut graph, QueryType::Write, &cyphers[0], op_st, op_deadline)
                    .await
                    .map_err(|e| OtherError(format!("probing write '{}': {}", op.name(), e)))?;
                reference.push((op.clone(), Arc::new(cyphers.clone()), Vec::new()));
                continue;
            }
            let to_capture: &[String] = if entry.result_gated { cyphers } else { &cyphers[..1] };
            let mut shapes = Vec::with_capacity(to_capture.len());
            for c in to_capture {
                shapes.push(
                    capture_result(&mut graph, c, op_st, op_deadline)
                        .await
                        .map_err(|e| OtherError(format!("capturing '{}': {}", op.name(), e)))?,
                );
            }
            if !entry.result_gated {
                // The probe shape is discarded: downstream code treats a shapeless op as result-N/A.
                shapes.clear();
            }
            reference.push((op.clone(), Arc::new(cyphers.clone()), shapes));
        }
        // Setup connection done; drop it so it isn't an idle extra connection during the sweep.
        drop(graph);

        // §6.3 correctness pass: when the bundle carries an outcome oracle, re-verify every
        // recorded outcome BEFORE any latency measurement — restore the pristine base, run the
        // command once, and require the engine's mutation counters to EQUAL the recorded stats
        // (`ExpectedOutcome::exactly`). A mismatch is a hard replay error naming the op, seq and
        // command: the engine no longer effects the recorded outcome, so it is doing *different
        // work* — measuring its latency anyway would poison the A/B trend silently. (Divergence
        // must scream — and an oracle-bearing op that got skipped fails closed rather than
        // silently bypassing the tier.) Untimed, single-flight, per-invocation restore — sample
        // latencies stay clean because restores run between invocations, never inside one.
        for (op, cyphers) in &bundle.commands {
            let Some(expected) = bundle.oracle.get(op.name()) else { continue };
            if let Some(reason) = skipped.get(op.name()) {
                // Fail closed: unreachable through load() (write bundles can never be
                // capability-gated, and only write ops carry oracles), but a skip must never
                // bypass the correctness tier.
                return Err(OtherError(format!(
                    "op '{}' carries {} recorded oracle outcome(s) but was skipped ({}) — a \
                     skip cannot bypass the correctness tier",
                    op.name(),
                    expected.len(),
                    reason
                )));
            }
            let entry = entry_for(op.name());
            let op_st = entry.budget.server_timeout_ms.unwrap_or(config.server_timeout_ms);
            let op_deadline = entry
                .budget
                .client_deadline_ms
                .map(Duration::from_millis)
                .unwrap_or(client_deadline);
            for (seq, want) in expected.iter().enumerate() {
                restore_base(config, &bundle, &graph_name, &dataset_spec).await?;
                let mut g = open_graph(&config.endpoint, &graph_name).await?;
                let sample =
                    run_and_drain(&mut g, QueryType::Write, &cyphers[seq], op_st, op_deadline)
                        .await
                        .map_err(|e| {
                            OtherError(format!(
                                "oracle verify: op '{}' seq {}: {}",
                                op.name(),
                                seq,
                                e
                            ))
                        })?;
                verify_mutation(ExpectedOutcome::exactly(*want), &sample.mutations).map_err(
                    |e| {
                        OtherError(format!(
                            "oracle mismatch for op '{}' seq {} (command: {}): {} — the engine \
                             diverged from the recorded outcome, so its write latencies would \
                             measure different work",
                            op.name(),
                            seq,
                            cyphers[seq],
                            e
                        ))
                    },
                )?;
            }
        }
        if !bundle.oracle.is_empty() {
            let outcomes: usize = bundle.oracle.values().map(Vec::len).sum();
            info!(
                "oracle verified: {} recorded outcome(s) across {} op(s) reproduced exactly",
                outcomes,
                bundle.oracle.len()
            );
        }

        for (op, corpus, shapes) in &reference {
            let entry = entry_for(op.name());
            let is_gated = entry.result_gated;
            // Overlay this op's recorded budget on the run's global config (the same per-op
            // overlay a generated run applies from the catalog spec) — already validated before
            // the reference pass, so the resolved knobs are known-good here.
            let op_config = engine_config.with_recorded_budget(&entry.budget);
            let op_concurrency = normalize_concurrency(&op_config.concurrency)?;
            let op_deadline = Duration::from_millis(op_config.client_deadline_ms);
            if op.kind() == QueryType::Write {
                let op_report = measure_write_op(
                    config,
                    &bundle,
                    &graph_name,
                    &dataset_spec,
                    op,
                    &op_config,
                    &op_concurrency,
                    Arc::clone(corpus),
                    run_token,
                    &uid_alloc,
                    op_deadline,
                )
                .await
                .map_err(|e| {
                    OtherError(format!("measuring write op '{}': {}", op.name(), e))
                })?;
                operations.insert(op.name().to_string(), op_report);
                continue;
            }
            let op_max_c = op_concurrency.iter().copied().max().unwrap_or(1);
            // Verify results are IDENTICAL at the op's highest concurrency (untimed) before
            // trusting the measured latencies — a concurrent path that returns different/wrong
            // results is a hard fail. Skipped for result-N/A ops, whose results aren't required
            // to be stable.
            if op_max_c > 1 && is_gated {
                verify_concurrent(
                    &config.endpoint,
                    &graph_name,
                    corpus,
                    shapes,
                    op_max_c,
                    op_config.server_timeout_ms,
                    op_deadline,
                )
                .await
                .map_err(|e| {
                    OtherError(format!(
                        "op '{}' returned different results at concurrency {}: {}",
                        op.name(),
                        op_max_c,
                        e
                    ))
                })?;
            }
            let mut op_report = measure_op(
                &op_config,
                &op_concurrency,
                MeasureTarget::read(),
                Arc::clone(corpus),
                run_token,
                &uid_alloc,
                op_deadline,
            )
            .await
            .map_err(|e| OtherError(format!("measuring op '{}': {}", op.name(), e)))?;
            // Gate the result only for byte-stable shapes; a result-N/A op reports `None` so the
            // diff guard renders it N/A instead of comparing a non-deterministic digest.
            op_report.result_digest =
                is_gated.then(|| op_result_digest(op.name(), shapes));
            // Persist the op's effective measurement policy when its recorded budget overrode any
            // global knob (design §3.4): budgets are deliberately outside the workload_hash, so
            // this block is what lets the diff/regression/baseline guards refuse to compare two
            // runs that measured the same workload under different per-op conditions.
            op_report.policy =
                (!entry.budget.is_inherit()).then(|| op_config.resolved_policy(&op_concurrency));
            operations.insert(op.name().to_string(), op_report);
        }
        Ok(())
    }
    .await;

    // Phase 7 §3.5: a write replay must leave the endpoint's graph exactly as recorded — final
    // restore + content verification on success AND failure.
    let measured = if let Some(pristine) = &pristine {
        let restored =
            restore_and_verify(&bundle, &graph_name, &dataset_spec, config, pristine).await;
        reconcile_measure_and_restore(measured, restored, &graph_name)
    } else {
        measured
    };
    measured?;

    // The corpus size is what the bundle actually recorded per op (not the compile-time constant) —
    // over every recorded op, including capability-skipped ones (it describes the bundle, not the
    // subset this engine could execute).
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

/// Guard the Phase 7 write-replay invariants for a bundle containing write ops — **offline**,
/// before any connection is attempted, so a bad bundle/flag combination fails closed:
///
/// - **single-kind**: recorded bundles never mix reads and writes (one global sweep can't express
///   swept reads + C=1 writes — design §4); the record path enforces this, but budgets and flags
///   are outside `workload_hash`, so replay re-checks everything hand-editable;
/// - **`--no-load` forbidden**: write measurement is defined from the recorded base graph
///   (per-cell resets reload it — §3.3);
/// - **C=1 only**: every write op's *effective* sweep (its recorded budget's, else the run's)
///   must be exactly `[1]` (§5 defers concurrent writes);
/// - **never result-gated**: the latency tier asserts nothing (§4.1) — a gated write would imply
///   a correctness capture the write path deliberately skips;
/// - **never capability-gated**: write shapes are algorithm-free plain Cypher (§4.1), and
///   `capability` is outside `workload_hash` — a crafted capability would otherwise let the probe
///   silently skip an op, shrinking the all-ten coverage while the replay still "succeeds".
fn validate_write_replay(
    bundle: &Bundle,
    config: &ReplayConfig,
    global_concurrency: &[usize],
) -> BenchmarkResult<()> {
    if let Some((op, _)) = bundle.commands.iter().find(|(op, _)| op.kind() != QueryType::Write) {
        return Err(OtherError(format!(
            "recorded bundle mixes write ops with read op '{}' — a bundle must be single-kind \
             (record reads and writes as separate bundles)",
            op.name()
        )));
    }
    if !config.load {
        return Err(OtherError(
            "--no-load is not supported for a write bundle: write replay must start from the \
             recorded base graph, which its per-cell resets reload (design §3.3/§3.5)"
                .to_string(),
        ));
    }
    for (op, _) in &bundle.commands {
        let entry = bundle.manifest.ops.iter().find(|e| e.name == op.name());
        let sweep: &[usize] = entry
            .and_then(|e| e.budget.concurrency.as_deref())
            .unwrap_or(global_concurrency);
        if sweep != [1] {
            return Err(OtherError(format!(
                "write op '{}' resolves to concurrency sweep {:?} — write replay is C=1 only \
                 (design §5; budgets are outside workload_hash, so this is enforced at replay)",
                op.name(),
                sweep
            )));
        }
        if entry.is_some_and(|e| e.result_gated) {
            return Err(OtherError(format!(
                "write op '{}' is marked result-gated — the write latency tier asserts nothing \
                 (design §4.1), so a write op can never gate on a result digest",
                op.name()
            )));
        }
        if let Some(capability) = entry.and_then(|e| e.capability.as_deref()) {
            return Err(OtherError(format!(
                "write op '{}' declares capability '{}' — write ops are plain Cypher and never \
                 capability-gated (design §4.1; capability is outside workload_hash, so this is \
                 enforced at replay: a crafted capability must not skip-shrink the write coverage)",
                op.name(),
                capability
            )));
        }
    }
    Ok(())
}

/// Measure one recorded **write** op (Phase 7 §4.1): the base graph is **reset (drop + reload +
/// count-verify) before every measured cell** (one cell per requested cache mode; C=1 enforced by
/// [`validate_write_replay`]), bounding mutation drift to a single cell's invocations. Each cell
/// runs [`measure_op`] with the op's config narrowed to that one cache mode, and the per-mode
/// single-level results are merged into one C=1 [`LevelReport`] (cached + uncached + the derived
/// compilation cost) so a write op's report shape matches a read op's.
#[allow(clippy::too_many_arguments)]
async fn measure_write_op(
    config: &ReplayConfig,
    bundle: &Bundle,
    graph_name: &str,
    dataset_spec: &DatasetSpec,
    op: &OpKey,
    op_config: &Config,
    op_concurrency: &[usize],
    corpus: Arc<Vec<String>>,
    run_token: u64,
    uid_alloc: &AtomicU64,
    op_deadline: Duration,
) -> BenchmarkResult<OperationReport> {
    let mut cached = None;
    let mut uncached = None;
    for &mode in op_config.cache.modes() {
        // Per-cell base reset (§4.1's periodic reset — one restore per measured cell).
        restore_base(config, bundle, graph_name, dataset_spec).await?;
        let cell_config = Config {
            cache: match mode {
                CacheMode::Cached => CacheSelection::Cached,
                CacheMode::Uncached => CacheSelection::Uncached,
            },
            ..op_config.clone()
        };
        let cell_report = measure_op(
            &cell_config,
            op_concurrency,
            MeasureTarget {
                kind: QueryType::Write,
                write: None,
            },
            Arc::clone(&corpus),
            run_token,
            uid_alloc,
            op_deadline,
        )
        .await?;
        let level = cell_report.levels.into_iter().next().ok_or_else(|| {
            OtherError(format!("write op '{}' produced no level report", op.name()))
        })?;
        debug_assert_eq!(level.concurrency, 1, "write replay is C=1 only (validate_write_replay)");
        match mode {
            CacheMode::Cached => cached = level.cached,
            CacheMode::Uncached => uncached = level.uncached,
        }
    }
    let compilation_ms_median = match (&cached, &uncached) {
        (Some(cm), Some(um)) => Some(um.metrics.server_ms.median - cm.metrics.server_ms.median),
        _ => None,
    };
    Ok(OperationReport {
        levels: vec![LevelReport {
            // The op's effective sweep — `[1]` today, enforced by `validate_write_replay`, and
            // derived (not hardcoded) so the report stays honest if §5's C>1 decision ever lands.
            concurrency: op_concurrency.first().copied().unwrap_or(1),
            cached,
            uncached,
            compilation_ms_median,
        }],
        // The latency tier asserts nothing about results (§4.1): no digest, ever.
        result_digest: None,
        // The effective per-op policy is persisted unconditionally — whether WRITE_BUDGET
        // overrode the globals or an inherit budget ran under a global C=1 sweep — so the
        // diff/baseline guards always refuse cross-policy comparisons of write cells.
        policy: Some(op_config.resolved_policy(op_concurrency)),
        skipped: None,
    })
}

/// The whole-graph content queries backing the write replay's restore verification: the node and
/// edge multisets, canonicalized value-by-value by [`capture_result`] (order-independent digest;
/// node/edge canonicalization includes entity ids, which are deterministic across identical fresh
/// loads because the recorded statements replay in recorded order onto an empty graph). Both scans
/// materialize the graph client-side, so they price the verification at the recorded base's size —
/// fine for the fixture-scale bases `synthetic record` emits (10³–10⁴ entities, ≈0.1 s), by design
/// not a path for arbitrarily large graphs.
const CONTENT_QUERIES: [&str; 2] =
    ["MATCH (n) RETURN n", "MATCH (a)-[r]->(b) RETURN ID(a), r, ID(b)"];

/// Capture the graph's full content shape ([`CONTENT_QUERIES`]) under load-scale timeouts.
/// `pub` as [`restore_base`]'s verification counterpart: the §3.5 final restore compares against
/// it, the restore-primitive integration test proves restores content-identical (not merely
/// count-identical) with it, and §6.3's correctness tier captures the pristine base through it.
pub async fn capture_graph_content(
    graph: &mut AsyncGraph,
    config: &ReplayConfig,
) -> BenchmarkResult<Vec<ResultShape>> {
    // Whole-graph scans do real work — give them the same generous deadline as a bulk load.
    let deadline = Duration::from_millis(config.client_deadline_ms.max(60_000));
    let server_timeout_ms = config
        .server_timeout_ms
        .max(i64::try_from(deadline.as_millis()).unwrap_or(i64::MAX));
    let mut shapes = Vec::with_capacity(CONTENT_QUERIES.len());
    for cypher in CONTENT_QUERIES {
        shapes.push(
            capture_result(graph, cypher, server_timeout_ms, deadline)
                .await
                .map_err(|e| OtherError(format!("capturing graph content ({cypher}): {e}")))?,
        );
    }
    Ok(shapes)
}

/// Reconcile a write replay's measurement outcome with its §3.5 final-restore outcome. A restore
/// failure surfaces even when the measurement succeeded, and a **dual** failure returns a
/// combined error naming both — the caller must learn that the endpoint's graph may be left
/// polluted, not just that the measurement failed.
fn reconcile_measure_and_restore(
    measured: BenchmarkResult<()>,
    restored: BenchmarkResult<()>,
    graph_name: &str,
) -> BenchmarkResult<()> {
    match (measured, restored) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(restore)) => Err(restore),
        (Err(original), Ok(())) => Err(original),
        (Err(original), Err(restore)) => Err(OtherError(format!(
            "write replay failed AND its final restore failed — graph '{}' may be left polluted \
             (measurement error: {}; restore error: {})",
            graph_name, original, restore
        ))),
    }
}

/// The write replay's final restore (§3.5): reload the recorded base graph, then verify its
/// **content** — not just its counts — matches the pristine post-load capture, so the replay
/// provably leaves the endpoint's graph exactly as recorded.
async fn restore_and_verify(
    bundle: &Bundle,
    graph_name: &str,
    dataset_spec: &DatasetSpec,
    config: &ReplayConfig,
    pristine: &[ResultShape],
) -> BenchmarkResult<()> {
    let mut graph = open_graph(&config.endpoint, graph_name).await?;
    load_recorded_graph(&mut graph, bundle, graph_name, dataset_spec, config).await?;
    let restored = capture_graph_content(&mut graph, config).await?;
    if restored != pristine {
        return Err(OtherError(format!(
            "final restore left graph '{}' content-diverged from the recorded base (node/edge \
             digests differ) — the recorded dataset did not reload reproducibly",
            graph_name
        )));
    }
    Ok(())
}

/// Ask the engine which procedures it registers (`dbms.procedures()`), returning their
/// **lowercased** names — the capability probe (design Phase 6 §3.5). One query per replay,
/// mirroring the A/B driver's `detect_algorithm_capabilities` (matching is case-insensitive so a
/// registry spelling change can't fake a missing capability).
async fn probe_procedures(
    graph: &mut AsyncGraph,
    server_timeout_ms: i64,
    client_deadline: Duration,
) -> BenchmarkResult<BTreeSet<String>> {
    let cypher = "CALL dbms.procedures() YIELD name RETURN name";
    let fut = async {
        let query_result = graph
            .ro_query(cypher)
            .with_timeout(server_timeout_ms)
            .execute()
            .await
            .map_err(|e| OtherError(format!("capability probe: procedure registry query failed: {:?}", e)))?;
        let mut names = BTreeSet::new();
        let mut data = query_result.data;
        while let Some(row) = data.next().await {
            let row = row
                .map_err(|e| OtherError(format!("capability probe: registry row decode error: {:?}", e)))?;
            let name: String = row.try_get_at(0).map_err(|e| {
                OtherError(format!(
                    "capability probe: registry returned a non-string name: {:?}",
                    e
                ))
            })?;
            names.insert(name.to_lowercase());
        }
        Ok::<BTreeSet<String>, crate::error::BenchmarkError>(names)
    };
    tokio::time::timeout(client_deadline, fut)
        .await
        .map_err(|e| {
            OtherError(format!(
                "capability probe: client deadline ({} ms) exceeded: {}",
                client_deadline.as_millis(),
                e
            ))
        })?
}

/// Restore the recorded pristine base into `graph_name` — drop + reload the bundle's recorded
/// statements + verify counts (the exact `--generate` load path), on a fresh short-lived
/// connection (the measurement workers hold their own).
///
/// This is the Phase 7 §3.3 **restore primitive**: the §4.1 latency tier calls it once per
/// measured cell (a periodic reset bounding mutation drift to one cell's invocations), and the
/// §6.3 correctness tier will call it **before each measured invocation** (per-invocation pristine
/// state at C=1, so accumulated-state effects — MERGE create-vs-match, delete no-ops — can't skew
/// the recorded outcome). Restores always run *between* invocations/cells, never inside a timed
/// sample, so their cost lands in wall-clock only — sample latencies stay clean.
pub async fn restore_base(
    config: &ReplayConfig,
    bundle: &Bundle,
    graph_name: &str,
    spec: &DatasetSpec,
) -> BenchmarkResult<()> {
    let mut conn = open_graph(&config.endpoint, graph_name).await?;
    load_recorded_graph(&mut conn, bundle, graph_name, spec, config).await
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
    use crate::synthetic::catalog::RecordedBudget;
    use crate::synthetic::recording::{DatasetKnobs, Manifest, OpEntry};

    fn shape(rows: usize, digest: &str) -> ResultShape {
        ResultShape {
            rows,
            value_digest: format!("sha256:{digest}"),
        }
    }

    fn replay_config(load: bool, concurrency: Vec<usize>) -> ReplayConfig {
        ReplayConfig {
            recording_dir: PathBuf::from("/nonexistent/recording"),
            // Nothing should ever connect in these tests — a guard regression that reaches the
            // network fails loudly on this closed port instead of hanging.
            endpoint: "falkor://127.0.0.1:1".to_string(),
            graph: None,
            load,
            samples: 5,
            warmup: 0,
            concurrency,
            cache: CacheSelection::Cached,
            server_timeout_ms: 5_000,
            client_deadline_ms: 6_000,
            out: "unused.json".to_string(),
            server_image: None,
            label: None,
        }
    }

    /// A minimal in-memory bundle: one op per `(name, kind, result_gated, budget)` row, each with
    /// a single recorded command. (The workload hash is never checked here — these bundles feed
    /// [`validate_write_replay`] directly, mimicking what a hand-crafted-but-hash-valid bundle
    /// could smuggle past `recording::load`.)
    fn bundle_of(rows: Vec<(&str, QueryType, bool, RecordedBudget)>) -> Bundle {
        let ops = rows
            .iter()
            .map(|(name, kind, gated, budget)| OpEntry {
                name: name.to_string(),
                kind: *kind,
                result_gated: *gated,
                budget: budget.clone(),
                capability: None,
                oracle: None,
                count: 1,
            })
            .collect();
        let commands = rows
            .iter()
            .map(|(name, kind, _, _)| {
                (OpKey::dynamic(name.to_string(), *kind), vec!["CREATE (n)".to_string()])
            })
            .collect();
        Bundle {
            manifest: Manifest {
                format_version: 2,
                generator_version: "synthbench/v5".to_string(),
                tool_version: "test".to_string(),
                dataset: DatasetKnobs {
                    seed: 7,
                    nodes: 10,
                    edges: 20,
                },
                graph: "g".to_string(),
                corpus_seed: 7,
                batch_size: 8,
                ops,
                workload_hash: "sha256:unchecked".to_string(),
                created_at_epoch_secs: 0,
            },
            graph_statements: Vec::new(),
            commands,
            oracle: std::collections::BTreeMap::new(),
        }
    }

    fn write_budget() -> RecordedBudget {
        RecordedBudget {
            concurrency: Some(vec![1]),
            ..RecordedBudget::default()
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
        let mut config = replay_config(true, vec![1]);
        config.samples = 0;
        let err = run(&config).await.unwrap_err();
        assert!(format!("{err}").contains("samples must be greater than 0"), "got: {err}");
    }

    #[test]
    fn validate_write_replay_rejects_a_mixed_bundle() {
        // Single-kind invariant (§4): the record path already refuses mixed bundles, but the
        // replay re-checks because a hand-crafted manifest with a correct v2 hash can mix kinds.
        let bundle = bundle_of(vec![
            ("w", QueryType::Write, false, write_budget()),
            ("r", QueryType::Read, true, RecordedBudget::default()),
        ]);
        let err = validate_write_replay(&bundle, &replay_config(true, vec![1]), &[1]).unwrap_err();
        assert!(format!("{err}").contains("mixes write ops with read op 'r'"), "got: {err}");
    }

    #[test]
    fn validate_write_replay_rejects_no_load() {
        // §3.3/§3.5: write measurement is defined from the recorded base graph.
        let bundle = bundle_of(vec![("w", QueryType::Write, false, write_budget())]);
        let err = validate_write_replay(&bundle, &replay_config(false, vec![1]), &[1]).unwrap_err();
        assert!(
            format!("{err}").contains("--no-load is not supported for a write bundle"),
            "got: {err}"
        );
    }

    #[test]
    fn validate_write_replay_rejects_a_non_c1_sweep() {
        // §5: C=1 only — budgets are outside workload_hash, so a tampered budget passes the load
        // hash gate and MUST be caught here, whether the sweep comes from the budget…
        let tampered = RecordedBudget {
            concurrency: Some(vec![8]),
            ..RecordedBudget::default()
        };
        let bundle = bundle_of(vec![("w", QueryType::Write, false, tampered)]);
        let err = validate_write_replay(&bundle, &replay_config(true, vec![1]), &[1]).unwrap_err();
        assert!(format!("{err}").contains("write replay is C=1 only"), "got: {err}");
        assert!(format!("{err}").contains("[8]"), "must name the offending sweep: {err}");

        // …or from the run's global sweep via an inherit budget.
        let bundle = bundle_of(vec![("w", QueryType::Write, false, RecordedBudget::default())]);
        let err = validate_write_replay(&bundle, &replay_config(true, vec![1, 8]), &[1, 8])
            .unwrap_err();
        assert!(format!("{err}").contains("write replay is C=1 only"), "got: {err}");
    }

    #[test]
    fn validate_write_replay_rejects_a_result_gated_write() {
        // §4.1: the latency tier asserts nothing — result_gated isn't hashed, so a tampered
        // manifest could otherwise send a write down the RO_QUERY capture path.
        let bundle = bundle_of(vec![("w", QueryType::Write, true, write_budget())]);
        let err = validate_write_replay(&bundle, &replay_config(true, vec![1]), &[1]).unwrap_err();
        assert!(format!("{err}").contains("marked result-gated"), "got: {err}");
    }

    #[test]
    fn validate_write_replay_accepts_a_c1_write_bundle() {
        // The recorded WRITE_BUDGET pins C=[1]; an inherit budget under a C=1 global sweep is
        // equally valid.
        let bundle = bundle_of(vec![
            ("w1", QueryType::Write, false, write_budget()),
            ("w2", QueryType::Write, false, RecordedBudget::default()),
        ]);
        validate_write_replay(&bundle, &replay_config(true, vec![1]), &[1]).unwrap();
    }

    #[test]
    fn validate_write_replay_rejects_a_capability_on_a_write_op() {
        // §4.1: writes are plain Cypher — and `capability` is outside workload_hash, so a crafted
        // capability naming a procedure the engine lacks would otherwise make the probe skip the
        // op, silently shrinking the all-ten coverage while the replay still "succeeds".
        let mut bundle = bundle_of(vec![("w", QueryType::Write, false, write_budget())]);
        bundle.manifest.ops[0].capability = Some("algo.nonexistent".to_string());
        let err = validate_write_replay(&bundle, &replay_config(true, vec![1]), &[1]).unwrap_err();
        assert!(format!("{err}").contains("never capability-gated"), "got: {err}");
        assert!(format!("{err}").contains("algo.nonexistent"), "must name the capability: {err}");
    }

    #[tokio::test]
    async fn run_rejects_a_tampered_capability_on_a_write_bundle() {
        // The full disk-level attack: `capability` is not folded into workload_hash, so adding a
        // nonexistent capability to a recorded write bundle survives `recording::load`'s hash
        // gate. The offline write guard must then fail the replay closed — before this guard the
        // replay "succeeded" with the op capability-skipped.
        use crate::synthetic::recording::{record_rendered, temp_bundle_dir, RecordedOp};

        let dir = temp_bundle_dir("replay-cap-tamper");
        let spec = DatasetSpec {
            seed: 7,
            nodes: 10,
            edges: 20,
        };
        let ops = vec![RecordedOp {
            key: OpKey::dynamic("w_op", QueryType::Write),
            result_gated: false,
            budget: write_budget(),
            capability: None,
            commands: vec!["CREATE (n:X)".to_string()],
        }];
        record_rendered(&spec, "g", &ops, 7, 64, &dir).expect("record a legit write bundle");

        let manifest_path = dir.join("manifest.json");
        let mut manifest: Manifest =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        manifest.ops[0].capability = Some("algo.nonexistent".to_string());
        std::fs::write(&manifest_path, serde_json::to_string(&manifest).unwrap()).unwrap();
        recording::load(&dir)
            .expect("capability is unhashed, so the tamper must survive the load hash gate");

        let mut config = replay_config(true, vec![1]);
        config.recording_dir = dir.clone();
        let err = run(&config).await.unwrap_err();
        assert!(format!("{err}").contains("never capability-gated"), "got: {err}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reconcile_measure_and_restore_combines_a_dual_failure() {
        // §3.5: when the measurement AND the final restore both fail, the caller must see both —
        // the restore failure means the endpoint's graph may be left polluted, which the
        // measurement error alone would hide.
        let err = reconcile_measure_and_restore(
            Err(OtherError("probe exploded".to_string())),
            Err(OtherError("content diverged".to_string())),
            "g7",
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("may be left polluted"), "got: {msg}");
        assert!(msg.contains("g7"), "must name the graph: {msg}");
        assert!(msg.contains("probe exploded"), "must surface the measurement error: {msg}");
        assert!(msg.contains("content diverged"), "must surface the restore error: {msg}");
    }

    #[test]
    fn reconcile_measure_and_restore_passes_single_outcomes_through() {
        assert!(reconcile_measure_and_restore(Ok(()), Ok(()), "g").is_ok());
        let err = reconcile_measure_and_restore(
            Ok(()),
            Err(OtherError("restore only".to_string())),
            "g",
        )
        .unwrap_err();
        assert!(format!("{err}").contains("restore only"), "got: {err}");
        let err = reconcile_measure_and_restore(
            Err(OtherError("measure only".to_string())),
            Ok(()),
            "g",
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("measure only"), "got: {msg}");
        assert!(!msg.contains("polluted"), "a successful restore is not a dual failure: {msg}");
    }
}
