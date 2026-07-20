//! Synthetic per-operation benchmark — Part 2: a selectable catalog of read operations.
//!
//! Measures one or more Cypher read operations in isolation against a FalkorDB endpoint. For each
//! selected operation it pre-generates a seeded corpus of parameterized queries (see
//! [`catalog`]), then, under each plan-cache condition, captures on every invocation the paired
//! *server time* (FalkorDB's reported internal execution time) and *total time* (end-to-end client
//! round-trip), summarizes them with severe-outlier removal, and derives the expression
//! *compilation cost* (uncached − cached). One JSON block is written per operation.
//!
//! Note that per-operation latency distributions can be right-skewed (e.g. high-degree seed nodes
//! for expansions), so the summary trims only *severe* outliers (beyond 3×IQR) and both cache
//! modes cycle the same corpus in the same order, keeping the cached-vs-uncached medians comparable
//! on a matched workload.

pub mod catalog;
pub mod config;
pub mod dataset;
pub mod op_runner;
pub mod provenance;
pub mod report;
pub mod stats;

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::falkor::falkor_endpoint_to_redis_url;
use crate::queries_repository::QueryType;
use crate::query::Query;
use crate::synthetic::catalog::{spec, DatasetHandle, DatasetRequirement, OperationSpec, CORPUS_SIZE};
use crate::synthetic::dataset::DatasetSpec;
use crate::synthetic::op_runner::{run_and_drain, OpSample};
use crate::synthetic::report::{DatasetInfo, Meta, OperationReport, Report};
use clap::ValueEnum;
use falkordb::{ConnectionStrategy, FalkorClientBuilder};
use futures::StreamExt;
use rand::rngs::StdRng;
use rand::SeedableRng;
use serde::de::{self, Deserializer};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

/// The default graph key the probe targets when `--graph` isn't given.
pub const DEFAULT_GRAPH: &str = "falkor";

/// How many `:User` ids to sample from the live graph to seed operation corpora (Part 2 path).
const DATASET_SAMPLE_SIZE: usize = 512;

/// `UNWIND` batch size used when the generator bulk-loads a synthetic dataset.
const DATASET_LOAD_BATCH: usize = 1000;

/// The set of operations the probe can measure. `OpName` is the single source of truth: it's a
/// clap `ValueEnum` (so `--op`, `--help` and shell completion list exactly these), and every
/// variant maps to one [`catalog::OperationSpec`] via [`catalog::spec`] (an exhaustive match, so a
/// new variant won't compile until it has a corpus). Read primitives target the benchmark's
/// `:User {id, age}` / `(:User)-[:Friend]->(:User)` schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum OpName {
    /// `RETURN $i` — a pure round-trip / server parse+exec baseline that needs no dataset.
    ReturnConst,
    /// Point lookup on the `:User(id)` index.
    MatchByIndex,
    /// Full `:User` label scan with a non-indexable predicate.
    MatchByLabelScan,
    /// 1-hop `:Friend` expansion from a seed node.
    #[value(name = "expand_1_hop")]
    Expand1Hop,
    /// Fixed 5-hop `:Friend` expansion from a seed node.
    #[value(name = "expand_hops_5")]
    ExpandHops5,
    /// Count a seed node's 1-hop `:Friend` neighbours.
    AggregateCount,
    /// Group a seed node's neighbours by age with counts.
    AggregateGroup,
    /// Bounded shortest `:Friend` path between two seed nodes.
    ShortestPath,
    /// Project scalar properties of an indexed node.
    PropertyProjection,
}

impl OpName {
    /// Every operation, in declaration order (the catalog's canonical order).
    pub fn all() -> &'static [OpName] {
        OpName::value_variants()
    }

    /// Every read operation (all of them, for now — writes arrive in Part 5). Used by
    /// `--all-reads`.
    pub fn all_reads() -> Vec<OpName> {
        OpName::all()
            .iter()
            .copied()
            .filter(|op| spec(*op).kind == QueryType::Read)
            .collect()
    }

    /// The stable string id used in reports and on the CLI.
    pub fn as_str(self) -> &'static str {
        match self {
            OpName::ReturnConst => "return_const",
            OpName::MatchByIndex => "match_by_index",
            OpName::MatchByLabelScan => "match_by_label_scan",
            OpName::Expand1Hop => "expand_1_hop",
            OpName::ExpandHops5 => "expand_hops_5",
            OpName::AggregateCount => "aggregate_count",
            OpName::AggregateGroup => "aggregate_group",
            OpName::ShortestPath => "shortest_path",
            OpName::PropertyProjection => "property_projection",
        }
    }

    /// Whether this operation reads or writes (selects `RO_QUERY` vs `QUERY`).
    pub fn kind(self) -> QueryType {
        spec(self).kind
    }

    /// A one-line description for `list-ops` and the report.
    pub fn description(self) -> &'static str {
        spec(self).description
    }

    /// A stable per-operation salt mixed into the RNG seed so two ops with the same corpus shape
    /// don't draw identical parameter sequences from one `--seed`. Fixed constants (not the
    /// declaration index) so reordering the enum can't shift an op's corpus.
    pub fn salt(self) -> u64 {
        match self {
            OpName::ReturnConst => 0x5259_5f43_4f4e_5354,
            OpName::MatchByIndex => 0x4d54_4348_5f49_4458,
            OpName::MatchByLabelScan => 0x4c41_4245_4c5f_5343,
            OpName::Expand1Hop => 0x4558_5031_484f_5000,
            OpName::ExpandHops5 => 0x4558_5035_484f_5053,
            OpName::AggregateCount => 0x4147_475f_434e_5400,
            OpName::AggregateGroup => 0x4147_475f_4752_5000,
            OpName::ShortestPath => 0x5348_5254_5f50_5448,
            OpName::PropertyProjection => 0x5052_4f50_5f50_524a,
        }
    }

    /// Parse an op from its canonical [`Self::as_str`] name (the CLI/config spelling).
    pub fn from_cli_str(s: &str) -> Option<OpName> {
        OpName::all().iter().copied().find(|op| op.as_str() == s)
    }
}

/// Deserialize an `OpName` from its canonical [`OpName::as_str`] name, so a `synthetic-bench.toml`
/// `operations = ["expand_1_hop", ...]` list uses the exact same spelling as the CLI (a plain
/// `rename_all = "snake_case"` derive would produce `expand1_hop`).
impl<'de> Deserialize<'de> for OpName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        OpName::from_cli_str(&s).ok_or_else(|| {
            let names: Vec<&str> = OpName::all().iter().map(|op| op.as_str()).collect();
            de::Error::custom(format!(
                "unknown operation '{}' (expected one of: {})",
                s,
                names.join(", ")
            ))
        })
    }
}

/// Which plan-cache condition to measure an operation under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ValueEnum)]
#[value(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum CacheSelection {
    /// Warm plan cache: identical query **body** every run (only the stripped `CYPHER <params>`
    /// prefix varies), so the plan is reused → execution only.
    Cached,
    /// Forced plan-cache miss every run (a unique comment makes each query body distinct) →
    /// execution + compilation.
    Uncached,
    /// Measure both and report the derived compilation cost (default).
    Both,
}

impl CacheSelection {
    fn modes(self) -> &'static [CacheMode] {
        match self {
            CacheSelection::Cached => &[CacheMode::Cached],
            CacheSelection::Uncached => &[CacheMode::Uncached],
            CacheSelection::Both => &[CacheMode::Cached, CacheMode::Uncached],
        }
    }
}

/// A single plan-cache condition (one element of a [`CacheSelection`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMode {
    Cached,
    Uncached,
}

/// Render one corpus query to a Cypher string for the given cache mode.
///
/// FalkorDB keys its plan cache on the query **body** (the text after the `CYPHER <params>` prefix
/// that [`Query::to_cypher`] emits). [`CacheMode::Cached`] uses the body verbatim, so a corpus of
/// varying parameter values still reuses one cached plan → *execution only*. [`CacheMode::Uncached`]
/// appends a unique trailing comment `/* co<run_token>-<i> */`, making every invocation a distinct
/// cache key that FalkorDB must recompile → *execution + compilation*. The per-run `run_token` keeps a
/// previous run's uncached comments from being served from cache.
fn render_cypher(
    query: &Query,
    mode: CacheMode,
    run_token: u64,
    i: usize,
) -> String {
    let base = query.to_cypher();
    match mode {
        CacheMode::Cached => base,
        CacheMode::Uncached => format!("{} /* co{:x}-{} */", base, run_token, i),
    }
}

/// Configuration for a synthetic probe run over one or more operations.
#[derive(Debug, Clone)]
pub struct Config {
    pub endpoint: String,
    /// Graph key to measure against (default [`DEFAULT_GRAPH`]).
    pub graph: String,
    /// Operations to measure, in order. Deduplicated (first occurrence wins) before running.
    pub ops: Vec<OpName>,
    pub samples: usize,
    pub warmup: usize,
    /// Seed for the per-operation parameter corpora (same seed ⇒ identical corpora).
    pub seed: u64,
    pub server_timeout_ms: i64,
    pub client_deadline_ms: u64,
    pub cache: CacheSelection,
    pub out: String,
    pub server_image: Option<String>,
    /// When `Some`, generate a reproducible synthetic dataset (Part 3) into `graph`, **replacing**
    /// its contents, before measuring. Gated behind explicit CLI consent (`--generate`).
    pub dataset: Option<DatasetSpec>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint: "falkor://127.0.0.1:6379".to_string(),
            graph: DEFAULT_GRAPH.to_string(),
            ops: vec![OpName::ReturnConst],
            samples: 1000,
            warmup: 200,
            seed: 0,
            server_timeout_ms: 5_000,
            client_deadline_ms: 6_000,
            cache: CacheSelection::Both,
            out: "synthetic-report.json".to_string(),
            server_image: None,
            dataset: None,
        }
    }
}

/// Print the available operations (for `synthetic list-ops`).
pub fn list_ops() -> String {
    let mut out = String::from("Available operations:\n");
    for op in OpName::all() {
        out.push_str(&format!("  {:<20} {}\n", op.as_str(), op.description()));
    }
    out
}

/// Strip any password from an endpoint before it is recorded in the report or printed, so
/// credentials passed in a `falkor://user:pass@host` URL never leak into `synthetic-report.json`.
/// If the endpoint can't be parsed as a URL it's replaced with a placeholder (rather than echoed
/// verbatim, which could still contain credentials).
fn redact_endpoint(endpoint: &str) -> String {
    match url::Url::parse(endpoint) {
        Ok(mut url) => {
            if url.password().is_some() {
                let _ = url.set_password(None);
            }
            url.to_string()
        }
        Err(_) => "<unparseable-endpoint>".to_string(),
    }
}

/// Open a single-connection [`falkordb::AsyncGraph`] handle for `endpoint` on graph `graph_name`.
///
/// Uses a one-socket pool so latency is honest single-flight (the client otherwise defaults to 8
/// multiplexed sockets). A malformed `endpoint` is reported as an error with credentials redacted.
pub async fn open_graph(
    endpoint: &str,
    graph_name: &str,
) -> BenchmarkResult<falkordb::AsyncGraph> {
    let connection_info = endpoint.try_into().map_err(|e| {
        OtherError(format!(
            "invalid endpoint '{}': {:?}",
            redact_endpoint(endpoint),
            e
        ))
    })?;

    let client = FalkorClientBuilder::new_async()
        .with_connection_info(connection_info)
        .with_connection_strategy(ConnectionStrategy::Pooled {
            size: nonzero::nonzero!(1u8),
        })
        .build()
        .await?;
    Ok(client.select_graph(graph_name))
}

/// Run the probe: connect (single connection), collect server provenance, sample the graph to seed
/// corpora, then measure each selected operation under each cache mode and build the [`Report`].
/// Writing the report to disk is the caller's responsibility (see [`run_and_report`]).
pub async fn run(config: &Config) -> BenchmarkResult<Report> {
    if config.samples == 0 {
        return Err(OtherError("samples must be greater than 0".to_string()));
    }
    let ops = dedup_ops(&config.ops);
    if ops.is_empty() {
        return Err(OtherError(
            "no operations selected — pass --op <name> (repeatable/comma-separated) or --all-reads"
                .to_string(),
        ));
    }

    let started_at_epoch_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // A single dedicated connection: honest single-flight latency.
    let mut graph = open_graph(&config.endpoint, &config.graph).await?;

    // Server provenance (best-effort: log and continue on failure).
    let redis_url = falkor_endpoint_to_redis_url(Some(&config.endpoint));
    let server = match provenance::collect(&redis_url, config.server_image.clone()).await {
        Ok(info) => info,
        Err(e) => {
            warn!("could not collect server provenance: {}", e);
            crate::synthetic::report::ServerInfo {
                server_image: config.server_image.clone(),
                ..Default::default()
            }
        }
    };
    if server.is_placeholder() {
        warn!(
            "FalkorDB module version is the {} dev placeholder — use a tagged image for version comparisons",
            crate::synthetic::report::ServerInfo::PLACEHOLDER_VER
        );
    }

    let client_deadline = Duration::from_millis(config.client_deadline_ms);

    // Dataset: either generate a reproducible one (Part 3) or sample the live graph (Part 2).
    let dataset = if let Some(spec) = &config.dataset {
        // Generation replaces the target graph, so it's gated behind explicit CLI consent upstream.
        let load_deadline = Duration::from_millis(config.client_deadline_ms.max(60_000));
        info!(
            "generating synthetic dataset (seed {}, nodes {}, edges {}) into graph '{}'",
            spec.seed, spec.nodes, spec.edges, config.graph
        );
        dataset::generate_and_load(
            &mut graph,
            spec,
            DATASET_LOAD_BATCH,
            load_deadline,
            config.server_timeout_ms,
        )
        .await?
    } else {
        ensure_graph_exists(&mut graph, config, client_deadline).await?;
        // Only sample the graph if some selected op needs seed ids (return_const / label scan
        // don't), so a return_const-only run doesn't fail on a graph that has no :User data.
        let needs_dataset = ops
            .iter()
            .any(|op| spec(*op).requirement != DatasetRequirement::None);
        if needs_dataset {
            probe_dataset(&mut graph, config, client_deadline).await?
        } else {
            DatasetHandle::default()
        }
    };

    // A fresh OS-random run_token per run keeps uncached-mode comments globally unique, so a small
    // run's uncached queries can never be served from a previous run's plan cache.
    let run_token = rand::random_range(0..=u64::MAX);

    let mut operations = BTreeMap::new();
    // Capture each op's cached query body (in execution order) so the corpus_hash reflects the
    // exact workload, not just the op names.
    let mut op_bodies: Vec<(OpName, String)> = Vec::with_capacity(ops.len());
    for op in ops {
        let op_spec = spec(op);
        // Seed each op's corpus deterministically (same --seed ⇒ byte-identical corpus).
        let mut rng = StdRng::seed_from_u64(config.seed ^ op.salt());
        let corpus = op_spec.build_corpus(&mut rng, &dataset, 0, 1)?;
        op_bodies.push((op, corpus[0].text.clone()));
        let op_report =
            measure_op(&mut graph, config, &op_spec, &corpus, run_token, client_deadline).await?;
        operations.insert(op.as_str().to_string(), op_report);
    }

    // Record dataset provenance + the workload's corpus_hash only when we generated the data (we
    // can't fingerprint an externally-supplied graph, so comparing hashes would be misleading).
    let dataset_info = config.dataset.as_ref().map(|spec| DatasetInfo {
        seed: spec.seed,
        nodes: spec.nodes,
        edges: spec.edges,
        corpus_hash: dataset::corpus_hash(spec, config.seed, CORPUS_SIZE, &op_bodies, &dataset),
    });

    Ok(Report {
        meta: Meta {
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            endpoint: redact_endpoint(&config.endpoint),
            graph: config.graph.clone(),
            samples: config.samples,
            warmup: config.warmup,
            seed: config.seed,
            corpus_size: CORPUS_SIZE,
            server_timeout_ms: config.server_timeout_ms,
            client_deadline_ms: config.client_deadline_ms,
            connection: "pool(size=1)".to_string(),
            started_at_epoch_secs,
            server,
            dataset: dataset_info,
        },
        operations,
    })
}

/// Deduplicate the selected ops, preserving first-occurrence order (so a repeated `--op` or an
/// overlap between `--op` and `--all-reads` doesn't silently overwrite a report entry).
fn dedup_ops(ops: &[OpName]) -> Vec<OpName> {
    let mut seen = std::collections::HashSet::new();
    ops.iter().copied().filter(|op| seen.insert(*op)).collect()
}

/// Ensure the target graph key exists: a read (`RO_QUERY`) against a never-written graph fails with
/// "Invalid graph operation on empty key". Probe with a read first; only when the error is exactly
/// that empty-key condition, re-run the same trivial `RETURN 1` over the writable `GRAPH.QUERY`
/// command (not `RO_QUERY`) — which instantiates the empty graph key even though the query itself
/// mutates nothing. So a read-only replica whose graph already exists still works via the read
/// path, and any other error (auth/network) is surfaced rather than masked. Both are bounded by the
/// client deadline.
async fn ensure_graph_exists(
    graph: &mut falkordb::AsyncGraph,
    config: &Config,
    client_deadline: Duration,
) -> BenchmarkResult<()> {
    let name = &config.graph;
    let probe = tokio::time::timeout(
        client_deadline,
        graph
            .ro_query("RETURN 1")
            .with_timeout(config.server_timeout_ms)
            .execute(),
    )
    .await
    .map_err(|e| OtherError(format!("graph '{}' readiness probe timed out: {}", name, e)))?;
    match probe {
        Ok(_) => Ok(()),
        Err(e) => {
            let msg = format!("{:?}", e);
            if is_empty_graph_key(&msg) {
                tokio::time::timeout(
                    client_deadline,
                    graph
                        .query("RETURN 1")
                        .with_timeout(config.server_timeout_ms)
                        .execute(),
                )
                .await
                .map_err(|e| {
                    OtherError(format!("graph '{}' instantiation timed out: {}", name, e))
                })?
                .map_err(|e| {
                    OtherError(format!("failed to instantiate graph '{}': {:?}", name, e))
                })?;
                Ok(())
            } else {
                Err(OtherError(format!(
                    "graph '{}' readiness probe failed: {}",
                    name, msg
                )))
            }
        }
    }
}

/// Sample up to [`DATASET_SAMPLE_SIZE`] existing `:User` ids (ascending, for a stable reproducible
/// sample) to seed operation corpora. A graph with no `:User` nodes yields an empty handle; ops
/// that need seed ids then fail with a clear message (ops that don't, like `return_const` or the
/// label scan, still run).
async fn probe_dataset(
    graph: &mut falkordb::AsyncGraph,
    config: &Config,
    client_deadline: Duration,
) -> BenchmarkResult<DatasetHandle> {
    let cypher = format!(
        "MATCH (n:User) WHERE n.id >= {} AND n.id <= {} RETURN n.id AS id ORDER BY id LIMIT {}",
        i32::MIN,
        i32::MAX,
        DATASET_SAMPLE_SIZE
    );
    let collect = async {
        let mut result = graph
            .ro_query(&cypher)
            .with_timeout(config.server_timeout_ms)
            .execute()
            .await
            .map_err(|e| OtherError(format!("dataset probe failed: {:?}", e)))?;
        let mut node_ids = Vec::new();
        while let Some(row) = result.data.next().await {
            let row = row.map_err(|e| OtherError(format!("dataset probe row error: {:?}", e)))?;
            // ids are read as i64 then narrowed; out-of-`i32`-range ids are skipped (QueryParam is
            // i32) rather than clamped, so we never target a wrong/nonexistent node.
            let id: i64 = row
                .try_get_at(0)
                .map_err(|e| OtherError(format!("dataset probe decode error: {:?}", e)))?;
            if let Ok(id) = i32::try_from(id) {
                node_ids.push(id);
            }
        }
        Ok::<Vec<i32>, crate::error::BenchmarkError>(node_ids)
    };
    let node_ids = tokio::time::timeout(client_deadline, collect)
        .await
        .map_err(|e| OtherError(format!("dataset probe timed out: {}", e)))??;
    Ok(DatasetHandle {
        node_ids,
        ..Default::default()
    })
}

/// Whether a query error string is FalkorDB's "graph key does not exist yet" condition.
fn is_empty_graph_key(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("empty key") || m.contains("invalid graph operation")
}

/// Measure one operation under every requested cache mode and derive its compilation cost.
async fn measure_op(
    graph: &mut falkordb::AsyncGraph,
    config: &Config,
    op_spec: &OperationSpec,
    corpus: &[Query],
    run_token: u64,
    client_deadline: Duration,
) -> BenchmarkResult<OperationReport> {
    let mut cached_set: Option<crate::synthetic::report::MetricSet> = None;
    let mut uncached_set: Option<crate::synthetic::report::MetricSet> = None;
    for &mode in config.cache.modes() {
        let set = measure_mode(graph, config, op_spec, corpus, mode, run_token, client_deadline).await?;
        match mode {
            CacheMode::Cached => cached_set = Some(set),
            CacheMode::Uncached => uncached_set = Some(set),
        }
    }

    // Derived expression-compilation cost: how much slower an uncached (recompiled) run's server
    // time is than a cached (plan-reused) one. Both modes cycle the same corpus in the same order,
    // so the medians compare a matched workload.
    let compilation_ms_median = match (&cached_set, &uncached_set) {
        (Some(c), Some(u)) => Some(u.server_ms.median - c.server_ms.median),
        _ => None,
    };

    Ok(OperationReport {
        cached: cached_set,
        uncached: uncached_set,
        compilation_ms_median,
    })
}

/// Warm up then measure `config.samples` invocations of one operation in one cache mode, cycling
/// through the pre-generated `corpus` (so parameter values vary while the body stays constant).
async fn measure_mode(
    graph: &mut falkordb::AsyncGraph,
    config: &Config,
    op_spec: &OperationSpec,
    corpus: &[Query],
    mode: CacheMode,
    run_token: u64,
    client_deadline: Duration,
) -> BenchmarkResult<crate::synthetic::report::MetricSet> {
    let kind = op_spec.kind;

    // Prime the plan cache once even when warmup==0, so a cached-mode measurement never pays
    // first-touch compilation on its first sample. (No help for uncached, whose every query is
    // unique by design.)
    if config.warmup == 0 && mode == CacheMode::Cached {
        let cypher = render_cypher(&corpus[0], mode, run_token, 0);
        let _ = run_and_drain(graph, kind, &cypher, config.server_timeout_ms, client_deadline)
            .await?;
    }

    // Warm-up (discarded) primes the plan cache (cached mode) and the connection.
    for i in 0..config.warmup {
        let cypher = render_cypher(&corpus[i % corpus.len()], mode, run_token, i);
        let _ = run_and_drain(graph, kind, &cypher, config.server_timeout_ms, client_deadline)
            .await?;
    }

    let mut samples: Vec<OpSample> = Vec::with_capacity(config.samples);
    for i in 0..config.samples {
        // Continue the uncached comment counter past warm-up so every key stays unique.
        let idx = config.warmup + i;
        let cypher = render_cypher(&corpus[idx % corpus.len()], mode, run_token, idx);
        let sample =
            run_and_drain(graph, kind, &cypher, config.server_timeout_ms, client_deadline).await?;
        samples.push(sample);
    }

    summarize_samples(&samples)
}

/// Summarize a set of paired samples into a [`MetricSet`].
///
/// Outlier removal is *paired*: a sample is dropped if it is a severe outlier in **either**
/// `server_ms` or `total_ms`, and all three summaries (server, total, and the paired residual) are
/// computed over that single shared retained cohort. This keeps their sample counts identical and
/// preserves the invariant that, since every raw pair has `total >= server`, the retained
/// aggregates do too. Cache-health stats are computed over the same retained cohort.
fn summarize_samples(samples: &[OpSample]) -> BenchmarkResult<crate::synthetic::report::MetricSet> {
    let server: Vec<f64> = samples.iter().map(|s| s.server_ms).collect();
    let total: Vec<f64> = samples.iter().map(|s| s.total_ms).collect();

    let server_fence = stats::severe_fence(&server);
    let total_fence = stats::severe_fence(&total);
    let within = |v: f64, fence: Option<(f64, f64)>| match fence {
        Some((lo, hi)) => v >= lo && v <= hi,
        None => true,
    };

    let kept: Vec<&OpSample> = samples
        .iter()
        .filter(|s| within(s.server_ms, server_fence) && within(s.total_ms, total_fence))
        .collect();
    let removed = samples.len() - kept.len();

    let kept_server: Vec<f64> = kept.iter().map(|s| s.server_ms).collect();
    let kept_total: Vec<f64> = kept.iter().map(|s| s.total_ms).collect();
    let kept_residual: Vec<f64> = kept.iter().map(|s| s.total_ms - s.server_ms).collect();

    let server_ms = stats::summarize_kept(&kept_server, removed)
        .ok_or_else(|| OtherError("no server_ms samples to summarize".to_string()))?;
    let total_ms = stats::summarize_kept(&kept_total, removed)
        .ok_or_else(|| OtherError("no total_ms samples to summarize".to_string()))?;
    let non_internal_ms = stats::summarize_kept(&kept_residual, removed)
        .ok_or_else(|| OtherError("no non_internal_ms samples to summarize".to_string()))?;

    // Cache health over the same retained cohort (not the raw samples).
    let cached_unknown = kept.iter().filter(|s| s.cached.is_none()).count();
    let known: Vec<bool> = kept.iter().filter_map(|s| s.cached).collect();
    let cached_false_rate = if known.is_empty() {
        0.0
    } else {
        known.iter().filter(|&&c| !c).count() as f64 / known.len() as f64
    };

    Ok(crate::synthetic::report::MetricSet {
        server_ms,
        total_ms,
        non_internal_ms,
        cached_false_rate,
        cached_unknown,
    })
}

/// Run the probe, print the console summary, and write the JSON report to `config.out`.
pub async fn run_and_report(config: &Config) -> BenchmarkResult<()> {
    if config.samples == 0 {
        return Err(OtherError("--samples must be greater than 0".to_string()));
    }
    let report = run(config).await?;
    println!("{}", report.to_console());
    let json = report.to_json()?;
    tokio::fs::write(&config.out, json).await?;
    info!("wrote {}", config.out);
    println!("report written to {}", config.out);
    Ok(())
}

/// Execute a parsed `synthetic` subcommand. This keeps `main.rs` a thin shell: it loads the
/// optional `synthetic-bench.toml`, merges it with the CLI overrides into a [`Config`] and runs the
/// probe, or prints the operation catalog.
pub async fn run_command(command: crate::cli::SyntheticCommands) -> BenchmarkResult<()> {
    match command {
        crate::cli::SyntheticCommands::Run {
            config: config_path,
            endpoint,
            graph,
            ops,
            all_reads,
            samples,
            warmup,
            seed,
            cache,
            server_timeout_ms,
            client_deadline_ms,
            out,
            server_image,
            generate,
            nodes,
            edges,
        } => {
            let overrides = config::CliOverrides {
                endpoint,
                graph,
                ops,
                all_reads,
                samples,
                warmup,
                seed,
                cache,
                server_timeout_ms,
                client_deadline_ms,
                out,
                server_image,
                generate,
                nodes,
                edges,
            };
            let file = config::FileConfig::load(config_path.as_deref())?;
            let config = config::resolve(overrides, file)?;
            run_and_report(&config).await
        }
        crate::cli::SyntheticCommands::ListOps => {
            print!("{}", list_ops());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::QueryBuilder;

    #[test]
    fn op_name_maps_are_consistent() {
        assert_eq!(OpName::ReturnConst.as_str(), "return_const");
        assert_eq!(OpName::ReturnConst.kind(), QueryType::Read);
        assert!(!OpName::ReturnConst.description().is_empty());
    }

    #[test]
    fn clap_value_names_match_as_str() {
        // The CLI value (used by --op, --help and shell completion) must equal `as_str()` so the
        // catalog, reports and CLI never disagree (e.g. `expand_1_hop`, not `expand1_hop`).
        for op in OpName::all() {
            let cli = op
                .to_possible_value()
                .expect("every op is selectable")
                .get_name()
                .to_string();
            assert_eq!(cli, op.as_str(), "clap name vs as_str mismatch");
        }
    }

    #[test]
    fn every_op_builds_valid_parameterized_cypher() {
        use crate::synthetic::catalog::{spec, DatasetHandle, CORPUS_SIZE};
        // A dataset with enough ids for every op (incl. shortest_path's TwoIds).
        let dataset = DatasetHandle {
            node_ids: (1..=50).collect(),
            ..Default::default()
        };
        for op in OpName::all() {
            let s = spec(*op);
            let mut rng = StdRng::seed_from_u64(7 ^ op.salt());
            let corpus = s
                .build_corpus(&mut rng, &dataset, 0, 1)
                .unwrap_or_else(|e| panic!("corpus for {} should build: {}", op.as_str(), e));
            assert_eq!(corpus.len(), CORPUS_SIZE, "op {}", op.as_str());
            // Every corpus entry shares one identical body (required for cache correctness), is
            // parameterized (no inlined literals beyond the body), and the rendered Cypher never
            // returns a whole node/edge (scalar projections only).
            let body0 = &corpus[0].text;
            for q in &corpus {
                assert_eq!(&q.text, body0, "op {} bodies must match", op.as_str());
                assert!(q.text.contains("RETURN"), "op {} must RETURN", op.as_str());
            }
        }
    }

    #[test]
    fn corpus_is_deterministic_in_seed() {
        use crate::synthetic::catalog::{spec, DatasetHandle};
        let dataset = DatasetHandle {
            node_ids: (1..=100).collect(),
            ..Default::default()
        };
        let build = |seed: u64| {
            let s = spec(OpName::MatchByIndex);
            let mut rng = StdRng::seed_from_u64(seed);
            s.build_corpus(&mut rng, &dataset, 0, 1).unwrap()
        };
        let params = |c: &[Query]| -> Vec<String> { c.iter().map(|q| q.to_cypher()).collect() };
        // Same seed ⇒ byte-identical corpus; different seed ⇒ different corpus.
        assert_eq!(params(&build(42)), params(&build(42)));
        assert_ne!(params(&build(42)), params(&build(43)));
    }

    #[test]
    fn ops_needing_seeds_error_on_empty_dataset() {
        use crate::synthetic::catalog::{spec, DatasetHandle};
        let empty = DatasetHandle::default();
        // return_const and the label scan need no ids; the rest do.
        assert!(spec(OpName::ReturnConst)
            .build_corpus(&mut StdRng::seed_from_u64(1), &empty, 0, 1)
            .is_ok());
        assert!(spec(OpName::MatchByLabelScan)
            .build_corpus(&mut StdRng::seed_from_u64(1), &empty, 0, 1)
            .is_ok());
        assert!(spec(OpName::MatchByIndex)
            .build_corpus(&mut StdRng::seed_from_u64(1), &empty, 0, 1)
            .is_err());
        // shortest_path needs two ids: one is not enough.
        let one = DatasetHandle {
            node_ids: vec![1],
            ..Default::default()
        };
        assert!(spec(OpName::ShortestPath)
            .build_corpus(&mut StdRng::seed_from_u64(1), &one, 0, 1)
            .is_err());
    }

    #[test]
    fn all_reads_covers_the_catalog_and_dedups() {
        let reads = OpName::all_reads();
        assert_eq!(reads.len(), OpName::all().len(), "all ops are reads in Part 2");
        // dedup_ops keeps first occurrence and drops duplicates / overlaps.
        let deduped = dedup_ops(&[
            OpName::MatchByIndex,
            OpName::Expand1Hop,
            OpName::MatchByIndex,
        ]);
        assert_eq!(deduped, vec![OpName::MatchByIndex, OpName::Expand1Hop]);
    }

    #[test]
    fn render_cypher_cached_stable_uncached_unique() {
        let q = QueryBuilder::new()
            .text("MATCH (n:User {id: $id}) RETURN n.id")
            .param("id", 7)
            .build();
        // Cached: body verbatim (plan reused), no uniqueness token.
        let c0 = render_cypher(&q, CacheMode::Cached, 0xABCD, 0);
        let c1 = render_cypher(&q, CacheMode::Cached, 0xABCD, 1);
        assert_eq!(c0, c1);
        assert!(!c0.contains("/* co"));
        assert!(c0.contains("RETURN n.id"));
        // Uncached: a unique per-invocation comment ⇒ distinct cache key; the run_token keeps it apart
        // from other runs.
        let u0 = render_cypher(&q, CacheMode::Uncached, 0xABCD, 0);
        let u1 = render_cypher(&q, CacheMode::Uncached, 0xABCD, 1);
        assert_ne!(u0, u1);
        assert!(u0.contains("/* coabcd-0 */"));
        assert!(u1.contains("/* coabcd-1 */"));
    }

    #[test]
    fn cache_selection_expands_to_modes() {
        assert_eq!(CacheSelection::Cached.modes(), &[CacheMode::Cached]);
        assert_eq!(CacheSelection::Uncached.modes(), &[CacheMode::Uncached]);
        assert_eq!(
            CacheSelection::Both.modes(),
            &[CacheMode::Cached, CacheMode::Uncached]
        );
    }

    #[test]
    fn empty_graph_key_detection() {
        assert!(is_empty_graph_key("Invalid graph operation on empty key"));
        assert!(is_empty_graph_key(
            "RedisError(\"Invalid graph operation on empty key\")"
        ));
        assert!(!is_empty_graph_key("Password authentication failed"));
        assert!(!is_empty_graph_key("connection refused"));
    }

    #[tokio::test]
    async fn run_rejects_zero_samples() {
        // Guarded before any network use, so this needs no server.
        let config = Config {
            samples: 0,
            ..Config::default()
        };
        assert!(run(&config).await.is_err());
        assert!(run_and_report(&config).await.is_err());
    }

    #[tokio::test]
    async fn run_rejects_malformed_endpoint() {
        // A connection string that fails to parse errors at `try_into`, before any network use,
        // so this needs no server.
        let config = Config {
            endpoint: "falkor://host:notaport".to_string(),
            samples: 10,
            ..Config::default()
        };
        assert!(run(&config).await.is_err());
    }

    #[tokio::test]
    async fn run_command_list_ops_needs_no_server() {
        // The catalog path is pure output.
        assert!(run_command(crate::cli::SyntheticCommands::ListOps)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn run_command_run_maps_args_and_validates() {
        // Exercises the CLI→Config mapping without a server: samples==0 is rejected up front.
        let command = crate::cli::SyntheticCommands::Run {
            config: None,
            endpoint: Some("falkor://127.0.0.1:6379".to_string()),
            graph: Some("falkor".to_string()),
            ops: vec![OpName::ReturnConst],
            all_reads: false,
            samples: Some(0),
            warmup: Some(0),
            seed: Some(0),
            cache: Some(CacheSelection::Both),
            server_timeout_ms: Some(5_000),
            client_deadline_ms: Some(6_000),
            out: Some("unused.json".to_string()),
            server_image: None,
            generate: false,
            nodes: None,
            edges: None,
        };
        assert!(run_command(command).await.is_err());
    }

    #[tokio::test]
    async fn run_command_run_requires_an_op() {
        // Neither --op nor --all-reads nor a config ⇒ a clear error before any network use.
        // (Uses a config path that doesn't exist so no ambient synthetic-bench.toml is picked up.)
        let command = crate::cli::SyntheticCommands::Run {
            config: Some("/nonexistent/synthetic-bench.toml".to_string()),
            endpoint: Some("falkor://127.0.0.1:6379".to_string()),
            graph: Some("falkor".to_string()),
            ops: vec![],
            all_reads: false,
            samples: Some(100),
            warmup: Some(0),
            seed: Some(0),
            cache: Some(CacheSelection::Both),
            server_timeout_ms: Some(5_000),
            client_deadline_ms: Some(6_000),
            out: Some("unused.json".to_string()),
            server_image: None,
            generate: false,
            nodes: None,
            edges: None,
        };
        assert!(run_command(command).await.is_err());
    }

    #[test]
    fn list_ops_mentions_each_op() {
        let listing = list_ops();
        for op in OpName::value_variants() {
            assert!(listing.contains(op.as_str()));
        }
    }

    #[test]
    fn summarize_samples_computes_paired_residual_and_cache() {
        let samples = vec![
            OpSample { server_ms: 0.10, total_ms: 0.40, rows: 1, cached: Some(true) },
            OpSample { server_ms: 0.12, total_ms: 0.45, rows: 1, cached: Some(false) },
            OpSample { server_ms: 0.11, total_ms: 0.42, rows: 1, cached: None },
            OpSample { server_ms: 0.13, total_ms: 0.44, rows: 1, cached: Some(true) },
        ];
        let r = summarize_samples(&samples).unwrap();
        assert_eq!(r.server_ms.n, 4);
        assert_eq!(r.cached_unknown, 1);
        // 1 of 3 known-cache samples was false.
        assert!((r.cached_false_rate - 1.0 / 3.0).abs() < 1e-9);
        // non_internal median should be positive (total > server).
        assert!(r.non_internal_ms.median > 0.0);
    }

    #[test]
    fn paired_summaries_share_one_retained_cohort() {
        // 20 well-behaved pairs (total = server + 0.3) plus one pair whose TOTAL is a severe
        // outlier. That pair must be dropped from *all three* summaries, so their `n` matches and
        // total.median stays >= server.median.
        let mut samples: Vec<OpSample> = (0..20)
            .map(|i| {
                let s = 0.10 + i as f64 * 0.001;
                OpSample { server_ms: s, total_ms: s + 0.3, rows: 1, cached: Some(true) }
            })
            .collect();
        samples.push(OpSample { server_ms: 0.11, total_ms: 500.0, rows: 1, cached: Some(true) });
        let r = summarize_samples(&samples).unwrap();
        assert_eq!(r.server_ms.n, r.total_ms.n);
        assert_eq!(r.total_ms.n, r.non_internal_ms.n);
        assert_eq!(r.total_ms.removed, 1);
        assert!(r.total_ms.median >= r.server_ms.median);
    }

    #[test]
    fn summarize_samples_tiny_all_unknown_cache() {
        // A tiny sample set: `severe_fence` returns None (nothing removed → the `within` no-fence
        // path), and with every `cached` unknown the false-rate collapses to 0.0.
        let samples = vec![
            OpSample { server_ms: 0.10, total_ms: 0.40, rows: 1, cached: None },
            OpSample { server_ms: 0.12, total_ms: 0.45, rows: 1, cached: None },
            OpSample { server_ms: 0.11, total_ms: 0.42, rows: 1, cached: None },
        ];
        let r = summarize_samples(&samples).unwrap();
        assert_eq!(r.server_ms.n, 3);
        assert_eq!(r.server_ms.removed, 0);
        assert_eq!(r.cached_unknown, 3);
        assert_eq!(r.cached_false_rate, 0.0);
    }

    #[test]
    fn redact_endpoint_strips_password() {
        assert_eq!(
            redact_endpoint("falkor://user:secret@host:6379"),
            "falkor://user@host:6379"
        );
        // No credentials → unchanged (modulo url normalization).
        assert!(redact_endpoint("falkor://127.0.0.1:6379").contains("127.0.0.1:6379"));
        assert!(!redact_endpoint("falkor://user:secret@host:6379").contains("secret"));
        // Unparseable input is replaced with a placeholder, never echoed verbatim.
        assert_eq!(redact_endpoint("not a url"), "<unparseable-endpoint>");
    }
}
