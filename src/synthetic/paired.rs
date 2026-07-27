//! Backs `synthetic run --recording --paired-endpoint`: measure ONE recorded bundle against TWO
//! FalkorDB endpoints **interleaved at per-op cell granularity**, so both sides of every
//! comparison see the same environment window.
//!
//! A cross-version comparison built from two sequential [`crate::synthetic::replay`] runs leaves
//! **minutes** between the moments the same op is measured on each side, so slow environment
//! drift (thermal throttling, background load, page-cache churn) lands on *different* ops in each
//! run and pollutes the per-op deltas. The paired replay instead sets both endpoints up
//! identically (each loads the recorded graph and runs its own untimed reference pass and
//! capability probe, honoring per-op budgets), then for each op, for each **cache mode ×
//! concurrency cell**, measures the cell on endpoint A and *immediately* the same cell on
//! endpoint B (A,B,A,B, … — see [`paired_schedule`]). Strict per-op attribution is preserved: an
//! op's cells are never interleaved with another op's, and commands are never mixed between ops.
//! Each endpoint's own cell subsequence is exactly the solo replay's order, so per-side behavior
//! (plan-cache state, warm-up, cache-buster uniqueness) matches a solo run.
//!
//! The output is **two complete standard [`Report`]s** (side A → `--out`, side B →
//! `--paired-out`) that work unchanged with `report --diff` / `report --regression`; pairing
//! provenance is recorded additively in [`crate::synthetic::report::Meta::paired_with`] (each
//! side names the other's redacted endpoint).
//!
//! **Scope: read bundles only.** A write bundle's replay resets the base graph before every
//! measured cell and ends with an error-safe restore + content verification (Phase 7 §3.3/§3.5);
//! interleaving two endpoints' reset/restore cycles would multiply those untimed reloads into the
//! paired window and tangle the two sides' error-recovery paths, so paired mode **fails fast on a
//! write bundle** — record reads and writes as separate bundles and pair only the reads.

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::falkor::falkor_endpoint_to_redis_url;
use crate::queries_repository::QueryType;
use crate::synthetic::catalog::DEFAULT_RESET_EVERY;
use crate::synthetic::dataset;
use crate::synthetic::op_runner::ResultShape;
use crate::synthetic::recording::{self, Bundle, OpEntry};
use crate::synthetic::replay::{
    bundle_corpus_size, capture_read_op_shapes, load_recorded_graph, op_result_digest,
    probe_procedures, replay_meta, verify_concurrent, ReplayConfig,
};
use crate::synthetic::report::{
    LevelMetrics, LevelReport, OperationReport, Report, ServerInfo, SCHEMA_VERSION,
};
use crate::synthetic::{
    measure_op, normalize_concurrency, open_graph, provenance, redact_endpoint, validate_op_config,
    write_report, CacheMode, CacheSelection, Config, MeasureTarget,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

/// How to replay one recorded bundle against two endpoints, interleaved per cell.
#[derive(Debug, Clone)]
pub struct PairedReplayConfig {
    /// Side A: the standard replay knobs (endpoint, graph, sweep, cache, out, label, …). The
    /// global measurement knobs (samples/warmup/concurrency/cache/timeouts) apply to **both**
    /// sides — only endpoint/graph/out/label differ per side.
    pub base: ReplayConfig,
    /// Side B's FalkorDB endpoint.
    pub paired_endpoint: String,
    /// Side B's graph key. `None` ⇒ the same resolution as side A (the `--graph` override or the
    /// bundle manifest's graph). Give B its own graph to pair two graphs on ONE server (an A/A
    /// self-check).
    pub paired_graph: Option<String>,
    /// Where to write side B's JSON report (Markdown alongside as `<out>.md`).
    pub paired_out: String,
    /// Optional display name for side B (e.g. `pr`), recorded into B's report.
    pub paired_label: Option<String>,
}

impl PairedReplayConfig {
    /// The [`ReplayConfig`] view of one side: A is `base` verbatim; B swaps in the paired
    /// endpoint/graph/out/label. B's `server_image` is cleared — the operator-supplied identity
    /// describes the primary endpoint only (B's identity comes from its own provenance probe).
    fn side_config(
        &self,
        side: Side,
    ) -> ReplayConfig {
        match side {
            Side::A => self.base.clone(),
            Side::B => ReplayConfig {
                endpoint: self.paired_endpoint.clone(),
                graph: self
                    .paired_graph
                    .clone()
                    .or_else(|| self.base.graph.clone()),
                out: self.paired_out.clone(),
                label: self.paired_label.clone(),
                server_image: None,
                ..self.base.clone()
            },
        }
    }
}

/// One side of a paired replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    A,
    B,
}

/// The cell grid one op is measured across: its effective concurrency sweep × cache modes
/// (per-op budget overlays applied).
#[derive(Debug, Clone)]
pub(crate) struct OpCells {
    pub concurrency: Vec<usize>,
    pub modes: Vec<CacheMode>,
}

/// One scheduled measurement: op `op_index`'s cell (`concurrency`, `mode`) on `side`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScheduledCell {
    pub op_index: usize,
    pub concurrency: usize,
    pub mode: CacheMode,
    pub side: Side,
}

/// The paired measurement order (pure — the whole interleaving policy lives here): ops run **one
/// at a time** in the given order (never interleaved with each other); within an op, cells follow
/// the solo replay's order (concurrency outer, cache mode inner); and every cell is measured on
/// side A then **immediately** on side B, so both sides of each per-op cell share the same
/// environment window.
pub(crate) fn paired_schedule(ops: &[OpCells]) -> Vec<ScheduledCell> {
    let mut cells = Vec::new();
    for (op_index, op) in ops.iter().enumerate() {
        for &concurrency in &op.concurrency {
            for &mode in &op.modes {
                for side in [Side::A, Side::B] {
                    cells.push(ScheduledCell {
                        op_index,
                        concurrency,
                        mode,
                        side,
                    });
                }
            }
        }
    }
    cells
}

/// The default `--paired-out` for a given `--out`: insert `-b` before the extension
/// (`synthetic-report.json` → `synthetic-report-b.json`), or append `-b` when there is none.
pub fn default_paired_out(out: &str) -> String {
    let path = std::path::Path::new(out);
    match (
        path.file_stem().and_then(|s| s.to_str()),
        path.extension().and_then(|e| e.to_str()),
    ) {
        (Some(stem), Some(ext)) => path
            .with_file_name(format!("{stem}-b.{ext}"))
            .to_string_lossy()
            .into_owned(),
        _ => format!("{out}-b"),
    }
}

/// Everything one side carries out of setup and into the interleaved measurement phase.
struct SideSetup {
    config: ReplayConfig,
    graph_name: String,
    server: ServerInfo,
    /// This side's capability-skip reasons (op name → reason), from its own procedure registry.
    skipped: BTreeMap<String, String>,
    /// Per op name: the reference shapes captured on THIS side — each side digests its own
    /// results, so `report --diff` genuinely compares the two engines. Absent for a
    /// capability-skipped op (never executed).
    reference: BTreeMap<String, Vec<ResultShape>>,
}

/// Set up one side exactly as the solo replay does before measuring: open a connection, collect
/// provenance (best-effort), load (or count-verify) the recorded graph, probe capabilities when
/// any op requires one, and run the untimed reference pass. The connection is dropped on return
/// so it never idles alongside the measurement workers.
async fn setup_side(
    config: ReplayConfig,
    bundle: &Bundle,
    op_entries: &HashMap<&str, &OpEntry>,
) -> BenchmarkResult<SideSetup> {
    let graph_name = resolve_graph(&config, bundle);
    let client_deadline = Duration::from_millis(config.client_deadline_ms);
    let mut graph = open_graph(&config.endpoint, &graph_name).await?;

    let redis_url = falkor_endpoint_to_redis_url(Some(&config.endpoint));
    let server = match provenance::collect(&redis_url, config.server_image.clone()).await {
        Ok(info) => info,
        Err(e) => fallback_server_info(&config, &e.to_string()),
    };

    if config.load {
        load_recorded_graph(&mut graph, bundle, &graph_name, &bundle.spec(), &config).await?;
    } else {
        dataset::verify_counts(
            &mut graph,
            &bundle.spec(),
            config.server_timeout_ms,
            client_deadline,
        )
        .await
        .map_err(no_load_hint)?;
    }

    // Capability probe (one registry query per side): each side skips the ops ITS engine lacks;
    // the caller then unions the two skip sets so a cell is only ever measured on both sides.
    let any_capability = bundle
        .commands
        .iter()
        .any(|(op, _)| op_entries[op.name()].capability.is_some());
    let available: Option<BTreeSet<String>> = if any_capability {
        Some(probe_procedures(&mut graph, config.server_timeout_ms, client_deadline).await?)
    } else {
        None
    };
    let mut skipped = BTreeMap::new();
    if let Some(available) = available.as_ref() {
        for (op, _) in &bundle.commands {
            let Some(procedure) = op_entries[op.name()].capability.as_deref() else {
                continue;
            };
            if !available.contains(&procedure.to_lowercase()) {
                let reason = format!("engine lacks procedure '{}' (capability probe)", procedure);
                let side = redact_endpoint(&config.endpoint);
                info!("paired side {side}: skipping op '{}': {reason}", op.name());
                skipped.insert(op.name().to_string(), reason);
            }
        }
    }

    // Reference pass (untimed, single-flight, per-op budgeted timeouts) — this side's own result
    // shapes. Capability-skipped ops are never executed, not even the fail-fast probe.
    let mut reference = BTreeMap::new();
    for (op, cyphers) in &bundle.commands {
        if skipped.contains_key(op.name()) {
            continue;
        }
        let entry = op_entries[op.name()];
        let op_st = entry
            .budget
            .server_timeout_ms
            .unwrap_or(config.server_timeout_ms);
        let op_deadline = entry
            .budget
            .client_deadline_ms
            .map(Duration::from_millis)
            .unwrap_or(client_deadline);
        let shapes = capture_read_op_shapes(
            &mut graph,
            op.name(),
            cyphers,
            entry.result_gated,
            op_st,
            op_deadline,
        )
        .await?;
        reference.insert(op.name().to_string(), shapes);
    }

    Ok(SideSetup {
        config,
        graph_name,
        server,
        skipped,
        reference,
    })
}

/// Replay `config`'s bundle against both endpoints, interleaved per cell (module docs), and build
/// the two [`Report`]s — `(side A, side B)`. Writing them is the caller's responsibility (see
/// [`run_and_report`]).
pub async fn run(config: &PairedReplayConfig) -> BenchmarkResult<(Report, Report)> {
    if config.base.samples == 0 {
        return Err(OtherError(
            "run --recording --samples must be greater than 0".to_string(),
        ));
    }
    if config.base.require_oracle {
        return Err(OtherError(
            "--require-oracle cannot be combined with --paired-endpoint: paired replay measures \
             read bundles only, and reads carry no outcome oracle"
                .to_string(),
        ));
    }
    let concurrency = normalize_concurrency(&config.base.concurrency)?;
    let bundle = recording::load(&config.base.recording_dir)?;

    // Scope guard (offline, before any connection): paired mode measures READ bundles only. A
    // write bundle's per-cell base resets + error-safe final restore (Phase 7) don't interleave
    // safely — see the module docs.
    if let Some((op, _)) = bundle
        .commands
        .iter()
        .find(|(op, _)| op.kind() == QueryType::Write)
    {
        return Err(OtherError(format!(
            "--paired-endpoint supports read bundles only, but op '{}' is a write — a write \
             replay resets/restores the base graph around every measured cell, which does not \
             interleave safely across two endpoints. Measure the write bundle with two solo \
             `synthetic run --recording` runs instead",
            op.name()
        )));
    }

    let config_a = config.side_config(Side::A);
    let config_b = config.side_config(Side::B);
    // Two sides must be two distinct measurement targets: the same endpoint + graph would make
    // the "pair" one graph measured twice, silently reloaded mid-flight by the second setup.
    if config_a.endpoint == config_b.endpoint
        && resolve_graph(&config_a, &bundle) == resolve_graph(&config_b, &bundle)
    {
        return Err(OtherError(format!(
            "--paired-endpoint resolves to the same endpoint AND graph as the primary side \
             ({} / '{}') — point it at a second server, or give side B its own graph with \
             --paired-graph for an A/A self-check on one server",
            redact_endpoint(&config_a.endpoint),
            resolve_graph(&config_a, &bundle),
        )));
    }

    // Manifest entry lookup, validated up front (mirrors the solo replay) so a corrupt bundle
    // fails before either endpoint is touched.
    let op_entries: HashMap<&str, &OpEntry> = bundle
        .manifest
        .ops
        .iter()
        .map(|e| (e.name.as_str(), e))
        .collect();
    validate_bundle_ops(&bundle, &op_entries)?;

    let started_at_epoch_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Per-side engine configs (differ only in endpoint/graph/out/label) + offline validation of
    // every op's resolved budget overlay, once — budgets and global knobs are shared, so a
    // malformed manifest fails closed before either endpoint is touched.
    let engine_config_a = engine_config(&config_a, &bundle, &concurrency);
    let engine_config_b = engine_config(&config_b, &bundle, &concurrency);
    for (op, _) in &bundle.commands {
        let entry = op_entries[op.name()];
        validate_op_config(
            op.name(),
            &engine_config_a.with_recorded_budget(&entry.budget),
        )?;
    }

    // Set both sides up identically, A first (setup is untimed; measurement is what interleaves).
    let a = Box::pin(setup_side(config_a, &bundle, &op_entries)).await?;
    let b = Box::pin(setup_side(config_b, &bundle, &op_entries)).await?;

    // Union skip semantics: a cell is measured on BOTH sides or on neither (a half-measured op
    // would break the pairing this mode exists for). Each side's report entry records its own
    // probe's reason when it has one, else names the peer that lacked the capability.
    let mut skipped_union: BTreeSet<String> = a.skipped.keys().cloned().collect();
    skipped_union.extend(b.skipped.keys().cloned());

    let mut operations_a: BTreeMap<String, OperationReport> = BTreeMap::new();
    let mut operations_b: BTreeMap<String, OperationReport> = BTreeMap::new();
    for name in &skipped_union {
        operations_a.insert(
            name.clone(),
            skipped_report(skip_reason(
                a.skipped.get(name),
                &b.config.endpoint,
                b.skipped.get(name),
            )),
        );
        operations_b.insert(
            name.clone(),
            skipped_report(skip_reason(
                b.skipped.get(name),
                &a.config.endpoint,
                a.skipped.get(name),
            )),
        );
    }

    // The measured ops (present on both sides), in recorded order, with the shared cell grid the
    // pure schedule below is built from.
    let mut measured: Vec<MeasuredOp<'_>> = Vec::new();
    let mut grid: Vec<OpCells> = Vec::new();
    for (op, cyphers) in &bundle.commands {
        if skipped_union.contains(op.name()) {
            continue;
        }
        let entry = op_entries[op.name()];
        let op_config_a = engine_config_a.with_recorded_budget(&entry.budget);
        let op_config_b = engine_config_b.with_recorded_budget(&entry.budget);
        let op_concurrency = normalize_concurrency(&op_config_a.concurrency)?;
        let op_deadline = Duration::from_millis(op_config_a.client_deadline_ms);
        grid.push(OpCells {
            concurrency: op_concurrency.clone(),
            modes: op_config_a.cache.modes().to_vec(),
        });
        measured.push(MeasuredOp {
            name: op.name(),
            corpus: Arc::new(cyphers.clone()),
            entry,
            op_config_a,
            op_config_b,
            op_concurrency,
            op_deadline,
        });
    }

    // Concurrency-correctness pass (untimed, per side, mirroring the solo replay): results must
    // be IDENTICAL at each op's highest concurrency before any latency is trusted.
    for m in &measured {
        let op_max_c = m.op_concurrency.iter().copied().max().unwrap_or(1);
        if op_max_c <= 1 || !m.entry.result_gated {
            continue;
        }
        for side in [&a, &b] {
            let shapes = &side.reference[m.name];
            verify_concurrent(
                &side.config.endpoint,
                &side.graph_name,
                &m.corpus,
                shapes,
                op_max_c,
                m.op_config_a.server_timeout_ms,
                m.op_deadline,
            )
            .await
            .map_err(|e| {
                let side_ep = redact_endpoint(&side.config.endpoint);
                let msg = format!(
                    "op '{}' returned different results at concurrency {op_max_c} on {side_ep}: {e}",
                    m.name
                );
                OtherError(msg)
            })?;
        }
    }

    // The interleaved measurement itself: consume the pure schedule cell by cell — A then
    // immediately B for every (op, concurrency, cache-mode) cell.
    let run_token = rand::random_range(0..=u64::MAX);
    let uid_alloc = AtomicU64::new(0);
    let mut acc_a: Vec<OpAccumulator> = vec![BTreeMap::new(); measured.len()];
    let mut acc_b: Vec<OpAccumulator> = vec![BTreeMap::new(); measured.len()];
    for cell in paired_schedule(&grid) {
        let m = &measured[cell.op_index];
        let op_config = match cell.side {
            Side::A => &m.op_config_a,
            Side::B => &m.op_config_b,
        };
        let cell_config = Config {
            cache: match cell.mode {
                CacheMode::Cached => CacheSelection::Cached,
                CacheMode::Uncached => CacheSelection::Uncached,
            },
            ..op_config.clone()
        };
        let cell_report = measure_op(
            &cell_config,
            &[cell.concurrency],
            MeasureTarget::read(),
            Arc::clone(&m.corpus),
            run_token,
            &uid_alloc,
            m.op_deadline,
        )
        .await
        .map_err(|e| {
            let side_ep = redact_endpoint(&cell_config.endpoint);
            let msg = format!(
                "measuring op '{}' (C={}, {:?}) on {side_ep}: {e}",
                m.name, cell.concurrency, cell.mode
            );
            OtherError(msg)
        })?;
        let level = cell_report
            .levels
            .into_iter()
            .next()
            .ok_or_else(|| OtherError(format!("op '{}' produced no level report", m.name)))?;
        let acc = match cell.side {
            Side::A => &mut acc_a[cell.op_index],
            Side::B => &mut acc_b[cell.op_index],
        };
        let slot = acc.entry(cell.concurrency).or_insert((None, None));
        match cell.mode {
            CacheMode::Cached => slot.0 = level.cached,
            CacheMode::Uncached => slot.1 = level.uncached,
        }
    }

    // Assemble each side's per-op report from its accumulated cells + its OWN reference digests.
    for (idx, m) in measured.iter().enumerate() {
        let insert = |acc: &mut OpAccumulator,
                      setup: &SideSetup,
                      ops: &mut BTreeMap<String, OperationReport>| {
            let shapes = &setup.reference[m.name];
            ops.insert(
                m.name.to_string(),
                OperationReport {
                    levels: assemble_levels(&m.op_concurrency, std::mem::take(acc)),
                    result_digest: m
                        .entry
                        .result_gated
                        .then(|| op_result_digest(m.name, shapes)),
                    policy: (!m.entry.budget.is_inherit())
                        .then(|| m.op_config_a.resolved_policy(&m.op_concurrency)),
                    skipped: None,
                },
            );
        };
        insert(&mut acc_a[idx], &a, &mut operations_a);
        insert(&mut acc_b[idx], &b, &mut operations_b);
    }

    let corpus_size = bundle_corpus_size(&bundle);
    let build = |setup: &SideSetup, peer: &SideSetup, operations| -> Report {
        Report {
            schema_version: SCHEMA_VERSION,
            meta: replay_meta(
                &setup.config,
                &bundle,
                setup.graph_name.clone(),
                concurrency.clone(),
                corpus_size,
                started_at_epoch_secs,
                setup.server.clone(),
                Some(redact_endpoint(&peer.config.endpoint)),
            ),
            operations,
        }
    };
    let report_a = build(&a, &b, operations_a);
    let report_b = build(&b, &a, operations_b);
    Ok((report_a, report_b))
}

/// [`run`], then print both console summaries and write both JSON + Markdown reports
/// (A → `base.out`, B → `paired_out`).
pub async fn run_and_report(config: &PairedReplayConfig) -> BenchmarkResult<()> {
    let (report_a, report_b) = run(config).await?;
    println!("{}", report_a.to_console());
    println!("{}", report_b.to_console());
    write_report(&report_a, &config.base.out).await?;
    write_report(&report_b, &config.paired_out).await
}

/// One measured op's shared measurement context (both sides).
struct MeasuredOp<'b> {
    name: &'b str,
    corpus: Arc<Vec<String>>,
    entry: &'b OpEntry,
    op_config_a: Config,
    op_config_b: Config,
    /// The op's effective (normalized) sweep — identical on both sides by construction.
    op_concurrency: Vec<usize>,
    op_deadline: Duration,
}

/// One op's accumulated per-concurrency `(cached, uncached)` metrics on one side.
type OpAccumulator = BTreeMap<usize, (Option<LevelMetrics>, Option<LevelMetrics>)>;

/// Side-resolved graph name (the `--graph`/`--paired-graph` override or the bundle's).
fn resolve_graph(
    config: &ReplayConfig,
    bundle: &Bundle,
) -> String {
    config
        .graph
        .clone()
        .unwrap_or_else(|| bundle.manifest.graph.clone())
}

/// The skip reason recorded on a side's report for a union-skipped op: its own probe's reason
/// when it has one, else a pointer at the peer that lacked the capability. Pure, so both the
/// wiring and the message are unit-testable.
fn skip_reason(
    own_reason: Option<&String>,
    peer_endpoint: &str,
    peer_reason: Option<&String>,
) -> String {
    own_reason.cloned().unwrap_or_else(|| {
        format!(
            "paired endpoint {} skipped this op ({}) — paired cells run on both sides or neither",
            redact_endpoint(peer_endpoint),
            peer_reason.map(String::as_str).unwrap_or("no reason recorded")
        )
    })
}

/// Validate the bundle's op listing offline (before any connection is opened): every recorded
/// command stream must have a manifest entry and at least one command — either failure means a
/// corrupt bundle. Mirrors the solo replay's guards.
fn validate_bundle_ops(
    bundle: &Bundle,
    op_entries: &HashMap<&str, &OpEntry>,
) -> BenchmarkResult<()> {
    for (op, cyphers) in &bundle.commands {
        if !op_entries.contains_key(op.name()) {
            return Err(OtherError(format!(
                "op '{}' replayed without a manifest entry (corrupt bundle)",
                op.name()
            )));
        }
        if cyphers.is_empty() {
            return Err(OtherError(format!(
                "op '{}' has no recorded commands",
                op.name()
            )));
        }
    }
    Ok(())
}

/// The provenance-collection fallback: warn and report only the operator-supplied image (the
/// exact behavior of the solo replay when a side's `INFO`/`GRAPH.INFO` probes fail).
fn fallback_server_info(config: &ReplayConfig, err: &str) -> ServerInfo {
    warn!(
        "could not collect server provenance for {}: {}",
        redact_endpoint(&config.endpoint),
        err
    );
    ServerInfo {
        server_image: config.server_image.clone(),
        ..Default::default()
    }
}

/// Decorate a `--no-load` count-verification failure with the fix (load the recording first).
fn no_load_hint(e: crate::error::BenchmarkError) -> crate::error::BenchmarkError {
    OtherError(format!(
        "{} — load the recording first (don't pass --no-load)",
        e
    ))
}

/// The shared closed-loop engine config for one side (mirrors the solo replay's, minus the op
/// list, which the measurement path never reads).
fn engine_config(
    config: &ReplayConfig,
    bundle: &Bundle,
    concurrency: &[usize],
) -> Config {
    Config {
        endpoint: config.endpoint.clone(),
        graph: resolve_graph(config, bundle),
        ops: Vec::new(),
        samples: config.samples,
        warmup: config.warmup,
        concurrency: concurrency.to_vec(),
        reset_every: DEFAULT_RESET_EVERY,
        seed: bundle.manifest.corpus_seed,
        server_timeout_ms: config.server_timeout_ms,
        client_deadline_ms: config.client_deadline_ms,
        cache: config.cache,
        out: config.out.clone(),
        server_image: config.server_image.clone(),
        label: config.label.clone(),
        dataset: None,
    }
}

/// A skipped op's report entry: no levels, no digest, no policy — just the reason.
fn skipped_report(reason: String) -> OperationReport {
    OperationReport {
        levels: Vec::new(),
        result_digest: None,
        policy: None,
        skipped: Some(reason),
    }
}

/// Fold one op's accumulated per-cell metrics into [`LevelReport`]s in sweep order, deriving the
/// per-level compilation cost exactly as the solo path does.
fn assemble_levels(
    op_concurrency: &[usize],
    mut acc: OpAccumulator,
) -> Vec<LevelReport> {
    let mut levels = Vec::with_capacity(op_concurrency.len());
    for &c in op_concurrency {
        let (cached, uncached) = acc.remove(&c).unwrap_or((None, None));
        let compilation_ms_median = match (&cached, &uncached) {
            (Some(cm), Some(um)) => Some(um.metrics.server_ms.median - cm.metrics.server_ms.median),
            _ => None,
        };
        levels.push(LevelReport {
            concurrency: c,
            cached,
            uncached,
            compilation_ms_median,
        });
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::catalog::RecordedBudget;
    use crate::synthetic::dataset::DatasetSpec;
    use crate::synthetic::recording::{record_rendered, temp_bundle_dir, RecordedOp};
    use crate::synthetic::OpKey;
    use std::path::PathBuf;

    fn base_replay_config() -> ReplayConfig {
        ReplayConfig {
            recording_dir: PathBuf::from("/nonexistent/recording"),
            // Nothing should ever connect in these tests — a guard regression that reaches the
            // network fails loudly on this closed port instead of hanging.
            endpoint: "falkor://127.0.0.1:1".to_string(),
            graph: None,
            load: true,
            samples: 5,
            warmup: 0,
            concurrency: vec![1],
            cache: CacheSelection::Cached,
            server_timeout_ms: 5_000,
            client_deadline_ms: 6_000,
            out: "unused.json".to_string(),
            server_image: None,
            label: None,
            require_oracle: false,
        }
    }

    fn paired_config(base: ReplayConfig) -> PairedReplayConfig {
        PairedReplayConfig {
            base,
            paired_endpoint: "falkor://127.0.0.2:1".to_string(),
            paired_graph: None,
            paired_out: "unused-b.json".to_string(),
            paired_label: None,
        }
    }

    /// Record a tiny single-op bundle of `kind` on disk (offline), returning its directory.
    fn record_one_op(
        tag: &str,
        kind: QueryType,
    ) -> PathBuf {
        let dir = temp_bundle_dir(tag);
        let spec = DatasetSpec {
            seed: 7,
            nodes: 10,
            edges: 20,
        };
        let (name, command, budget) = match kind {
            QueryType::Write => (
                "w_op",
                "CREATE (n:X)",
                RecordedBudget {
                    concurrency: Some(vec![1]),
                    ..RecordedBudget::default()
                },
            ),
            QueryType::Read => (
                "r_op",
                "MATCH (n) RETURN count(n)",
                RecordedBudget::default(),
            ),
        };
        let ops = vec![RecordedOp {
            key: OpKey::dynamic(name, kind),
            result_gated: kind == QueryType::Read,
            budget,
            capability: None,
            commands: vec![command.to_string()],
        }];
        record_rendered(&spec, "g", &ops, 7, 64, &dir).expect("record bundle");
        dir
    }

    #[test]
    fn paired_schedule_alternates_sides_and_never_interleaves_ops() {
        // Two ops with different grids: op 0 sweeps C=1,4 under both modes; op 1 is a budgeted
        // C=1 cached-only op (the shape a recorded budget produces).
        let ops = vec![
            OpCells {
                concurrency: vec![1, 4],
                modes: vec![CacheMode::Cached, CacheMode::Uncached],
            },
            OpCells {
                concurrency: vec![1],
                modes: vec![CacheMode::Cached],
            },
        ];
        let schedule = paired_schedule(&ops);

        // Every (op, C, mode) cell appears exactly twice: A then immediately B.
        assert_eq!(schedule.len(), 2 * (2 * 2 + 1));
        for pair in schedule.chunks_exact(2) {
            let (first, second) = (&pair[0], &pair[1]);
            assert_eq!(first.side, Side::A, "each cell starts on side A");
            assert_eq!(second.side, Side::B, "…and B follows immediately");
            assert_eq!(
                (first.op_index, first.concurrency, first.mode),
                (second.op_index, second.concurrency, second.mode),
                "adjacent A/B entries are the SAME cell"
            );
        }

        // Ops are contiguous blocks (never interleaved with each other), in the given order.
        let op_sequence: Vec<usize> = schedule.iter().map(|c| c.op_index).collect();
        let mut deduped = op_sequence.clone();
        deduped.dedup();
        assert_eq!(deduped, vec![0, 1], "ops run one at a time, in order");

        // Within an op, cells follow the solo replay's order: concurrency outer, mode inner.
        let op0: Vec<(usize, CacheMode)> = schedule
            .iter()
            .filter(|c| c.op_index == 0 && c.side == Side::A)
            .map(|c| (c.concurrency, c.mode))
            .collect();
        assert_eq!(
            op0,
            vec![
                (1, CacheMode::Cached),
                (1, CacheMode::Uncached),
                (4, CacheMode::Cached),
                (4, CacheMode::Uncached),
            ]
        );
    }

    #[test]
    fn paired_schedule_of_no_ops_is_empty() {
        assert!(paired_schedule(&[]).is_empty());
    }

    #[test]
    fn default_paired_out_inserts_b_before_the_extension() {
        assert_eq!(
            default_paired_out("synthetic-report.json"),
            "synthetic-report-b.json"
        );
        assert_eq!(
            default_paired_out("recordings/demo/report.json"),
            "recordings/demo/report-b.json"
        );
        // No extension: append.
        assert_eq!(default_paired_out("report"), "report-b");
    }

    #[tokio::test]
    async fn run_rejects_zero_samples() {
        let mut base = base_replay_config();
        base.samples = 0;
        let err = run(&paired_config(base)).await.unwrap_err();
        assert!(
            format!("{err}").contains("samples must be greater than 0"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn run_rejects_require_oracle() {
        // Paired replay is read-only, and reads carry no outcome oracle — the flag combination is
        // operator error, refused before any disk/server access.
        let mut base = base_replay_config();
        base.require_oracle = true;
        let err = run(&paired_config(base)).await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("--require-oracle cannot be combined"),
            "got: {msg}"
        );
        assert!(msg.contains("read bundles only"), "got: {msg}");
    }

    #[tokio::test]
    async fn run_rejects_a_write_bundle() {
        // Scope cut (module docs): write bundles reset/restore the base graph around every cell,
        // which doesn't interleave safely — fail fast, offline, naming the op.
        let dir = record_one_op("paired-write-reject", QueryType::Write);
        let mut base = base_replay_config();
        base.recording_dir = dir.clone();
        let err = run(&paired_config(base)).await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("read bundles only"), "got: {msg}");
        assert!(msg.contains("'w_op'"), "must name the write op: {msg}");
        assert!(
            msg.contains("two solo"),
            "must point at the sequential path: {msg}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn run_rejects_the_same_endpoint_and_graph_on_both_sides() {
        // A "pair" of one endpoint + one graph is a single measurement target: the second setup
        // would silently reload the graph the first side just set up.
        let dir = record_one_op("paired-same-target", QueryType::Read);
        let mut base = base_replay_config();
        base.recording_dir = dir.clone();
        let mut config = paired_config(base.clone());
        config.paired_endpoint = base.endpoint.clone();
        let err = run(&config).await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("same endpoint AND graph"), "got: {msg}");
        assert!(
            msg.contains("--paired-graph"),
            "must suggest the A/A escape hatch: {msg}"
        );

        // …while the SAME endpoint with a DISTINCT --paired-graph is the supported A/A pairing:
        // it must clear the offline guards and fail only on the (closed) connection attempt.
        config.paired_graph = Some("g_b".to_string());
        let err = run(&config).await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            !msg.contains("same endpoint AND graph"),
            "distinct graphs must pass the target guard: {msg}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn skip_reason_prefers_the_side_s_own_probe_reason() {
        let own = "engine lacks procedure 'algo.x' (capability probe)".to_string();
        let peer = "engine lacks procedure 'algo.y' (capability probe)".to_string();
        // A side with its own reason reports it verbatim…
        assert_eq!(
            skip_reason(Some(&own), "falkor://peer:6379", Some(&peer)),
            own
        );
        // …a side skipped only by union names the peer and its reason…
        let msg = skip_reason(None, "falkor://peer:6379", Some(&peer));
        assert!(msg.contains("paired endpoint"), "got: {msg}");
        assert!(msg.contains("algo.y"), "got: {msg}");
        // …and a reason-less peer (defensive) still yields a self-explanatory message.
        let msg = skip_reason(None, "falkor://peer:6379", None);
        assert!(msg.contains("no reason recorded"), "got: {msg}");
    }

    #[test]
    fn validate_bundle_ops_rejects_corrupt_bundles() {
        let dir = record_one_op("paired-validate-ops", QueryType::Read);
        let mut bundle = recording::load(&dir).expect("load");
        std::fs::remove_dir_all(&dir).ok();

        // The recorded bundle itself is valid.
        let entries: HashMap<&str, &OpEntry> =
            bundle.manifest.ops.iter().map(|e| (e.name.as_str(), e)).collect();
        assert!(validate_bundle_ops(&bundle, &entries).is_ok());

        // An op with a command stream but no manifest entry is corrupt…
        let mut phantom = bundle.clone();
        phantom
            .commands
            .push((OpKey::dynamic("phantom", QueryType::Read), vec!["RETURN 1".to_string()]));
        let entries: HashMap<&str, &OpEntry> =
            phantom.manifest.ops.iter().map(|e| (e.name.as_str(), e)).collect();
        let msg = format!("{}", validate_bundle_ops(&phantom, &entries).unwrap_err());
        assert!(msg.contains("phantom") && msg.contains("manifest"), "got: {msg}");

        // …and so is an op with an empty command stream.
        bundle.commands[0].1.clear();
        let entries: HashMap<&str, &OpEntry> =
            bundle.manifest.ops.iter().map(|e| (e.name.as_str(), e)).collect();
        let msg = format!("{}", validate_bundle_ops(&bundle, &entries).unwrap_err());
        assert!(msg.contains("no recorded commands"), "got: {msg}");
    }

    #[test]
    fn fallback_server_info_reports_only_the_operator_supplied_image() {
        let config = ReplayConfig {
            server_image: Some("falkordb/falkordb:test".to_string()),
            ..base_replay_config()
        };
        let info = fallback_server_info(&config, "connection refused");
        assert_eq!(info.server_image.as_deref(), Some("falkordb/falkordb:test"));
        assert_eq!(info.module_graph_ver, None, "everything else stays unknown");
    }

    #[test]
    fn no_load_hint_names_the_fix() {
        let msg = format!("{}", no_load_hint(OtherError("node count mismatch".to_string())));
        assert!(msg.contains("node count mismatch"), "got: {msg}");
        assert!(msg.contains("don't pass --no-load"), "got: {msg}");
    }

    #[tokio::test]
    async fn run_rejects_an_invalid_recorded_budget_offline() {
        // A per-op budget that zeroes samples must fail closed BEFORE either endpoint is touched
        // (the recorded budget overlays the global config exactly as in the solo replay).
        let dir = temp_bundle_dir("paired-bad-budget");
        let spec = DatasetSpec {
            seed: 7,
            nodes: 10,
            edges: 20,
        };
        let ops = vec![RecordedOp {
            key: OpKey::dynamic("zero_samples", QueryType::Read),
            result_gated: true,
            budget: RecordedBudget {
                samples: Some(0),
                ..RecordedBudget::default()
            },
            capability: None,
            commands: vec!["RETURN 1".to_string()],
        }];
        record_rendered(&spec, "g", &ops, 7, 64, &dir).expect("record bundle");
        let mut base = base_replay_config();
        base.recording_dir = dir.clone();
        let mut config = paired_config(base);
        config.paired_graph = Some("g_b".to_string());
        let msg = format!("{}", run(&config).await.unwrap_err());
        std::fs::remove_dir_all(&dir).ok();
        assert!(
            msg.contains("zero_samples") && msg.contains("samples"),
            "the offline budget validation must name the op: {msg}"
        );
    }
}
