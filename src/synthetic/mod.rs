//! Synthetic per-operation benchmark — a selectable catalog of read operations over a controlled
//! dataset.
//!
//! Measures one or more Cypher read operations in isolation against a FalkorDB endpoint. The graph
//! is either sampled from the live endpoint or **generated reproducibly** from a seed (see
//! [`dataset`]). For each selected operation it pre-generates a seeded corpus of parameterized
//! queries (see [`catalog`]), then, under each plan-cache condition, captures on every invocation
//! the paired *server time* (FalkorDB's reported internal execution time) and *total time*
//! (end-to-end client round-trip), summarizes them with severe-outlier removal, and derives the
//! expression *compilation cost* (uncached − cached). One JSON block is written per operation; when
//! the dataset was generated, a `corpus_hash` fingerprints the whole workload for comparability.
//!
//! Note that per-operation latency distributions can be right-skewed (e.g. high-degree seed nodes
//! for expansions), so the summary trims only *severe* outliers (beyond 3×IQR) and both cache
//! modes cycle the same corpus in the same order, keeping the cached-vs-uncached medians comparable
//! on a matched workload.

pub mod catalog;
pub mod config;
pub mod baseline;
pub mod dataset;
pub mod engine;
pub mod host;
pub mod op_runner;
pub mod provenance;
pub mod recording;
pub mod replay;
pub mod report;
pub mod stats;
pub mod writes;

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::falkor::falkor_endpoint_to_redis_url;
use crate::queries_repository::QueryType;
use crate::query::Query;
use crate::synthetic::catalog::{
    spec, DatasetHandle, DatasetRequirement, OperationSpec, CORPUS_SIZE,
};
use crate::synthetic::dataset::DatasetSpec;
use crate::synthetic::engine::{run_closed_loop, OpInvoker};
use crate::synthetic::op_runner::{run_and_drain, OpSample};
use crate::synthetic::report::{
    DatasetInfo, LevelMetrics, LevelReport, Meta, OperationReport, Report,
};
use crate::synthetic::writes::{verify_mutation, WritePlan, WriteScratch};
use clap::ValueEnum;
use falkordb::{AsyncGraph, ConnectionStrategy, FalkorClientBuilder};
use futures::StreamExt;
use rand::rngs::StdRng;
use rand::SeedableRng;
use serde::de::{self, Deserializer};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

/// The default concurrency sweep (closed-loop worker counts `C`) when none is configured: the
/// canonical latency-vs-throughput curve from `1` to `32` workers.
pub const DEFAULT_CONCURRENCY: &[usize] = &[1, 2, 4, 8, 16, 32];

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
    /// (write) `CREATE` a fresh scratch node each invocation.
    CreateNode,
    /// (write) `MERGE` a fresh scratch node each invocation — always misses, so always creates.
    MergeMiss,
    /// (write) `CREATE` a fresh edge between two of this worker's scratch nodes.
    CreateEdge,
    /// (write) `SET` a property on a pre-created scratch node each invocation.
    SetProperty,
    /// (write) `DELETE` a pre-created scratch node each invocation.
    DeleteNode,
    /// (write) `MERGE` an existing scratch node each invocation — always hits, so never creates.
    MergeHit,
}

impl OpName {
    /// Every operation, in declaration order (the catalog's canonical order).
    pub fn all() -> &'static [OpName] {
        OpName::value_variants()
    }

    /// Every read operation. Used by `--all-reads` (write ops are opt-in via explicit `--op`).
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
            OpName::CreateNode => "create_node",
            OpName::MergeMiss => "merge_miss",
            OpName::CreateEdge => "create_edge",
            OpName::SetProperty => "set_property",
            OpName::DeleteNode => "delete_node",
            OpName::MergeHit => "merge_hit",
        }
    }

    /// Whether this operation reads or writes (selects `RO_QUERY` vs `QUERY`).
    pub fn kind(self) -> QueryType {
        spec(self).kind
    }

    /// Parse an [`OpName`] from its stable [`as_str`](Self::as_str) tag (e.g. reading a recorded
    /// bundle's manifest/command files). `None` for an unknown tag.
    pub fn from_tag(tag: &str) -> Option<OpName> {
        OpName::all().iter().copied().find(|op| op.as_str() == tag)
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
            OpName::CreateNode => 0x4352_4541_5445_4e44,
            OpName::MergeMiss => 0x4d45_5247_455f_4d53,
            OpName::CreateEdge => 0x4352_545f_4544_4745,
            OpName::SetProperty => 0x5345_545f_5052_4f50,
            OpName::DeleteNode => 0x4445_4c5f_4e4f_4445,
            OpName::MergeHit => 0x4d45_5247_4548_4954,
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
/// appends a unique trailing comment `/* co<run_token>-<uid> */`, making every invocation a distinct
/// cache key that FalkorDB must recompile → *execution + compilation*. The per-run `run_token` keeps
/// a previous run's uncached comments from being served from cache; `uid` is a **run-global** unique
/// invocation id (disjoint per worker and per concurrency level), so no two invocations anywhere in
/// the sweep ever collide.
fn render_cypher(
    query: &Query,
    mode: CacheMode,
    run_token: u64,
    uid: u64,
) -> String {
    let base = query.to_cypher();
    match mode {
        CacheMode::Cached => base,
        CacheMode::Uncached => format!("{} /* co{:x}-{} */", base, run_token, uid),
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
    /// Concurrency levels to sweep (closed-loop worker counts `C`); each op is measured once per
    /// level, tracing its latency-vs-throughput curve. Non-empty, each ≥ 1, deduped + sorted.
    pub concurrency: Vec<usize>,
    /// Reset cadence for write operations: every `reset_every` ops, each worker's scratch is reset
    /// (untimed) to bound drift to one sawtooth window. Ignored by read ops.
    pub reset_every: usize,
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
            concurrency: DEFAULT_CONCURRENCY.to_vec(),
            reset_every: crate::synthetic::catalog::DEFAULT_RESET_EVERY,
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
pub(crate) fn redact_endpoint(endpoint: &str) -> String {
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
    let concurrency = normalize_concurrency(&config.concurrency)?;
    let ops = dedup_ops(&config.ops);
    if ops.is_empty() {
        return Err(OtherError(
            "no operations selected — pass --op <name> (repeatable/comma-separated) or --all-reads"
                .to_string(),
        ));
    }
    // Write ops need a positive reset cadence AND a per-worker key band that fits `i32` (FalkorDB
    // params) at the widest concurrency level. Validate both up-front — before opening any
    // connection or collecting provenance — so an invalid write config fails fast instead of after
    // connection/provenance setup.
    if ops.iter().any(|op| spec(*op).write.is_some()) {
        if config.reset_every == 0 {
            return Err(OtherError(
                "reset_every must be >= 1 for write operations".to_string(),
            ));
        }
        // The band bound grows with the worker id, so the largest level bounds every smaller one;
        // reuse `WriteScratch::new`'s checked i32 arithmetic (its run_token doesn't affect the
        // bound). `normalize_concurrency` guarantees a non-empty, ≥1 sweep, so `max_worker ≥ 0`.
        let max_worker = concurrency.last().copied().unwrap_or(1).saturating_sub(1);
        WriteScratch::new(0, max_worker, config.reset_every)?;
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
        // Bulk-load batches do real server-side work, so give them a generous deadline *and* a
        // matching server-side per-query timeout (the default measurement timeout — often 5s — is
        // too small for a large UNWIND batch and would trip before the client deadline).
        let load_deadline = Duration::from_millis(config.client_deadline_ms.max(60_000));
        let load_server_timeout_ms = config
            .server_timeout_ms
            .max(i64::try_from(load_deadline.as_millis()).unwrap_or(i64::MAX));
        info!(
            "generating synthetic dataset (seed {}, nodes {}, edges {}) into graph '{}'",
            spec.seed, spec.nodes, spec.edges, config.graph
        );
        dataset::generate_and_load(
            &mut graph,
            spec,
            DATASET_LOAD_BATCH,
            load_deadline,
            load_server_timeout_ms,
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

    // The setup connection's work (provenance, dataset generation/probe) is done; drop it so it
    // doesn't sit idle as a `C + 1`-th connection while the workers open their own pools.
    drop(graph);

    // Run-global allocator of disjoint invocation-id ranges: every worker claims a block so its
    // uncached query text can never collide with another worker's or another level's.
    let uid_alloc = AtomicU64::new(0);

    let mut operations = BTreeMap::new();
    // Capture each op's corpus fingerprint (in execution order) so the corpus_hash reflects the
    // exact rendered workload — parameter values included — not just the op names.
    let mut op_fingerprints: Vec<(OpName, String)> = Vec::with_capacity(ops.len());
    for op in ops {
        let op_spec = spec(op);
        // Seed each op's corpus deterministically (same --seed ⇒ byte-identical corpus).
        let mut rng = StdRng::seed_from_u64(config.seed ^ op.salt());
        let corpus = Arc::new(op_spec.build_corpus(&mut rng, &dataset, 0, 1)?);
        // Fingerprint reads by their rendered corpus; writes by their stable plan tag + reset
        // cadence (their queries are rendered per-invocation from scratch, not from the corpus).
        let fingerprint = match &op_spec.write {
            Some(plan) => format!("write:{}:reset_every={}", plan.plan_tag, config.reset_every),
            None => dataset::corpus_fingerprint(&corpus),
        };
        op_fingerprints.push((op, fingerprint));
        let op_report = measure_op(
            config,
            &concurrency,
            &op_spec,
            Arc::clone(&corpus),
            run_token,
            &uid_alloc,
            client_deadline,
        )
        .await?;
        operations.insert(op.as_str().to_string(), op_report);
    }

    // Record dataset provenance + the workload's corpus_hash only when we generated the data (we
    // can't fingerprint an externally-supplied graph, so comparing hashes would be misleading).
    let dataset_info = config.dataset.as_ref().map(|spec| DatasetInfo {
        seed: spec.seed,
        nodes: spec.nodes,
        edges: spec.edges,
        corpus_hash: dataset::corpus_hash(
            spec,
            config.seed,
            CORPUS_SIZE,
            &op_fingerprints,
            &dataset,
        ),
    });

    Ok(Report {
        schema_version: crate::synthetic::report::SCHEMA_VERSION,
        meta: Meta {
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            endpoint: redact_endpoint(&config.endpoint),
            graph: config.graph.clone(),
            samples: config.samples,
            warmup: config.warmup,
            concurrency: concurrency.clone(),
            seed: config.seed,
            corpus_size: CORPUS_SIZE,
            server_timeout_ms: config.server_timeout_ms,
            client_deadline_ms: config.client_deadline_ms,
            connection: "pool(size=1) per worker".to_string(),
            started_at_epoch_secs,
            server,
            host: host::collect(),
            dataset: dataset_info,
        },
        operations,
    })
}

/// Validate and canonicalize the configured concurrency sweep: non-empty, every level ≥ 1,
/// deduplicated and sorted ascending (so the curve reads low → high and each `C` runs once).
fn normalize_concurrency(concurrency: &[usize]) -> BenchmarkResult<Vec<usize>> {
    if concurrency.is_empty() {
        return Err(OtherError(
            "concurrency must list at least one level (e.g. --concurrency 1,4,16)".to_string(),
        ));
    }
    if concurrency.contains(&0) {
        return Err(OtherError(
            "concurrency levels must be >= 1 (0 workers can't measure anything)".to_string(),
        ));
    }
    let mut levels: Vec<usize> = concurrency.to_vec();
    levels.sort_unstable();
    levels.dedup();
    Ok(levels)
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
pub(crate) fn is_empty_graph_key(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("empty key") || m.contains("invalid graph operation")
}

/// Where a [`GraphWorker`] gets each invocation's query: a read cycles a shared corpus; a write
/// renders a fresh query from its own [`WriteScratch`] and runs the untimed reset/verification.
enum WorkerSource {
    /// A read op: cycle the shared corpus from a decorrelating offset.
    Read {
        corpus: Arc<Vec<Query>>,
        corpus_offset: usize,
    },
    /// A write op (Part 5): the plan's per-invocation render + the worker's isolated scratch.
    Write {
        plan: WritePlan,
        scratch: WriteScratch,
    },
}

/// A closed-loop worker for one operation, cache mode and connection: owns a single-socket
/// `AsyncGraph` and drives one query at a time via [`run_and_drain`]. Reads cycle a shared corpus
/// from a decorrelating `corpus_offset`; writes render from a per-worker [`WriteScratch`], run an
/// untimed reset at each window boundary, and verify each sample's mutation. It claims a disjoint
/// `uid_base` so its uncached query text is globally unique across the whole sweep.
struct GraphWorker {
    graph: AsyncGraph,
    kind: QueryType,
    mode: CacheMode,
    run_token: u64,
    uid_base: u64,
    server_timeout_ms: i64,
    client_deadline: Duration,
    /// Generous server-timeout/deadline for the untimed write hooks (a reset can delete a whole
    /// window of nodes, which mustn't trip the tight measurement deadline).
    hook_server_timeout_ms: i64,
    hook_deadline: Duration,
    source: WorkerSource,
}

impl OpInvoker for GraphWorker {
    async fn invoke(
        &mut self,
        seq: u64,
    ) -> BenchmarkResult<OpSample> {
        let uid = self.uid_base + seq;
        let mode = self.mode;
        let run_token = self.run_token;
        let kind = self.kind;
        let server_timeout_ms = self.server_timeout_ms;
        let client_deadline = self.client_deadline;
        // `&self.source` and `&mut self.graph` borrow disjoint fields, so both are live together.
        match &self.source {
            WorkerSource::Read {
                corpus,
                corpus_offset,
            } => {
                let idx = (corpus_offset + seq as usize) % corpus.len();
                let cypher = render_cypher(&corpus[idx], mode, run_token, uid);
                run_and_drain(&mut self.graph, kind, &cypher, server_timeout_ms, client_deadline)
                    .await
            }
            WorkerSource::Write { plan, scratch } => {
                let plan = *plan;
                let scratch = scratch.clone();
                // Undo the previous window's drift *before* reusing its key band — untimed, so it
                // never lands in a sample. `seq` is the global (warm-up + measured) counter, so the
                // cadence bounds warm-up accumulation too.
                if scratch.schedule().should_reset(seq) {
                    for q in (plan.reset)(&scratch)? {
                        let c = q.to_cypher();
                        run_and_drain(
                            &mut self.graph,
                            kind,
                            &c,
                            self.hook_server_timeout_ms,
                            self.hook_deadline,
                        )
                        .await?;
                    }
                }
                let base = (plan.render)(&scratch, seq)?;
                let cypher = render_cypher(&base, mode, run_token, uid);
                let sample = run_and_drain(
                    &mut self.graph,
                    kind,
                    &cypher,
                    server_timeout_ms,
                    client_deadline,
                )
                .await?;
                // The op must actually effect its intended mutation — a silent no-op is an error.
                verify_mutation(plan.expected, &sample.mutations)?;
                Ok(sample)
            }
        }
    }
}

/// Measure one operation across the concurrency sweep, under every requested cache mode, and derive
/// its per-level compilation cost.
#[allow(clippy::too_many_arguments)]
async fn measure_op(
    config: &Config,
    concurrency: &[usize],
    op_spec: &OperationSpec,
    corpus: Arc<Vec<Query>>,
    run_token: u64,
    uid_alloc: &AtomicU64,
    client_deadline: Duration,
) -> BenchmarkResult<OperationReport> {
    let mut levels = Vec::with_capacity(concurrency.len());
    for &c in concurrency {
        let mut cached: Option<LevelMetrics> = None;
        let mut uncached: Option<LevelMetrics> = None;
        for &mode in config.cache.modes() {
            let metrics = measure_level(
                config,
                c,
                op_spec,
                &corpus,
                mode,
                run_token,
                uid_alloc,
                client_deadline,
            )
            .await?;
            match mode {
                CacheMode::Cached => cached = Some(metrics),
                CacheMode::Uncached => uncached = Some(metrics),
            }
        }

        // Derived expression-compilation cost: how much slower an uncached (recompiled) run's
        // server time is than a cached (plan-reused) one, at this concurrency level.
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

    Ok(OperationReport {
        levels,
        result_digest: None,
    })
}

/// Run a list of untimed write statements (setup/reset/cleanup) to completion on `graph`.
async fn run_write_stmts(
    graph: &mut AsyncGraph,
    stmts: Vec<Query>,
    server_timeout_ms: i64,
    deadline: Duration,
) -> BenchmarkResult<()> {
    for q in stmts {
        let cypher = q.to_cypher();
        run_and_drain(graph, QueryType::Write, &cypher, server_timeout_ms, deadline).await?;
    }
    Ok(())
}

/// Open a fresh connection and drop a write run's scratch (its `cleanup` statements). Kept off the
/// measurement connections so a level that errored still gets cleaned up, and so — unlike the
/// per-op work — a cleanup failure is a **surfaced** error the caller can propagate on the success
/// path rather than silent scratch pollution.
async fn run_scratch_cleanup(
    endpoint: &str,
    graph: &str,
    plan: WritePlan,
    scratch: &WriteScratch,
    server_timeout_ms: i64,
    deadline: Duration,
) -> BenchmarkResult<()> {
    let mut g = open_graph(endpoint, graph).await?;
    let stmts = (plan.cleanup)(scratch)?;
    run_write_stmts(&mut g, stmts, server_timeout_ms, deadline).await
}

/// Measure one operation at one concurrency level `C` in one cache mode via the closed-loop engine:
/// open `C` single-socket connections (one per worker), drive them to completion, and summarize the
/// pooled samples plus the achieved throughput.
///
/// For write ops each worker gets an isolated [`WriteScratch`] and its untimed setup runs before the
/// window. The run's scratch is dropped afterward on a fresh connection whether or not the level
/// errored, so a failed write level never leaks scratch into the next one. A cleanup failure on the
/// **success** path is surfaced (leftover scratch must never silently pollute the graph); on the
/// **failure** path cleanup stays best-effort so it can't mask the level's original error.
#[allow(clippy::too_many_arguments)]
async fn measure_level(
    config: &Config,
    concurrency: usize,
    op_spec: &OperationSpec,
    corpus: &Arc<Vec<Query>>,
    mode: CacheMode,
    run_token: u64,
    uid_alloc: &AtomicU64,
    client_deadline: Duration,
) -> BenchmarkResult<LevelMetrics> {
    // Prime the plan cache once even when warmup==0, so a cached-mode measurement never pays
    // first-touch compilation on its first sample. (No help for uncached, whose every query is
    // unique by design.) An untimed warm-up invocation does exactly that.
    let effective_warmup = if config.warmup == 0 && mode == CacheMode::Cached {
        1
    } else {
        config.warmup
    };

    // Each worker claims a disjoint id block wide enough for its warm-up + measured invocations, so
    // no two workers (here or at any other level) ever render the same uncached query text.
    let block = (effective_warmup + config.samples) as u64;

    // The untimed write hooks (setup/reset/cleanup) can touch a whole window of nodes, so give them
    // a generous deadline independent of the tight per-op measurement deadline.
    let hook_deadline = client_deadline.max(Duration::from_millis(60_000));
    let hook_server_timeout_ms = config
        .server_timeout_ms
        .max(i64::try_from(hook_deadline.as_millis()).unwrap_or(i64::MAX));

    // For a write op, capture the cleanup handle up-front — *before* any worker's setup mutates the
    // graph — so a mid-loop connection/setup failure still drops whatever scratch earlier workers
    // created. The run label is shared, so worker 0's scratch cleans the whole run.
    let write_cleanup: Option<(WritePlan, WriteScratch)> = match &op_spec.write {
        Some(plan) => Some((*plan, WriteScratch::new(run_token, 0, config.reset_every)?)),
        None => None,
    };

    // Build the workers (opening a connection and running each write op's untimed setup). Collected
    // into a Result so a partway failure routes through the scratch cleanup below instead of leaking
    // the nodes earlier workers already created.
    let build_workers = async {
        let mut workers = Vec::with_capacity(concurrency);
        for w in 0..concurrency {
            let mut graph = open_graph(&config.endpoint, &config.graph).await?;
            let uid_base = uid_alloc.fetch_add(block, Ordering::Relaxed);
            let source = match &op_spec.write {
                Some(plan) => {
                    let scratch = WriteScratch::new(run_token, w, config.reset_every)?;
                    // Untimed setup on this worker's own connection before the measurement window.
                    run_write_stmts(
                        &mut graph,
                        (plan.setup)(&scratch)?,
                        hook_server_timeout_ms,
                        hook_deadline,
                    )
                    .await?;
                    WorkerSource::Write {
                        plan: *plan,
                        scratch,
                    }
                }
                None => WorkerSource::Read {
                    corpus: Arc::clone(corpus),
                    corpus_offset: w % corpus.len(),
                },
            };
            workers.push(GraphWorker {
                graph,
                kind: op_spec.kind,
                mode,
                run_token,
                uid_base,
                server_timeout_ms: config.server_timeout_ms,
                client_deadline,
                hook_server_timeout_ms,
                hook_deadline,
                source,
            });
        }
        Ok::<_, crate::error::BenchmarkError>(workers)
    }
    .await;

    // Run the closed loop only if every worker built; otherwise carry the build/setup error forward
    // so the single cleanup path below runs for both outcomes (a partially set-up populated band
    // must never leak). The build/setup error is what we ultimately propagate.
    let run = match build_workers {
        Ok(workers) => run_closed_loop(workers, effective_warmup, config.samples).await,
        Err(e) => Err(e),
    };

    // Drop the run's scratch on a fresh connection, whether or not the level succeeded, so a write
    // level can't leave scratch behind for the next level/op. On the **success** path a cleanup
    // failure is surfaced (leftover scratch must never silently pollute the graph); on the
    // **failure** path cleanup stays best-effort so it can't mask the level's original error.
    let cleanup = match write_cleanup {
        Some((plan, scratch)) => {
            run_scratch_cleanup(
                &config.endpoint,
                &config.graph,
                plan,
                &scratch,
                hook_server_timeout_ms,
                hook_deadline,
            )
            .await
        }
        None => Ok(()),
    };

    let run = run?; // a level error wins (cleanup already ran best-effort above)
    cleanup?; // otherwise surface any cleanup failure so scratch can't leak silently
    let metrics = summarize_samples(&run.samples)?;
    Ok(LevelMetrics {
        throughput_ops_per_sec: run.throughput_ops_per_sec(),
        metrics,
    })
}

/// Summarize a set of paired samples into a [`MetricSet`].
///
/// Outlier removal is *paired*: a sample is dropped if it is a severe outlier in **either**
/// `server_ms` or `total_ms`, and all three summaries (server, total, and the paired residual) are
/// computed over that single shared retained cohort. This keeps their sample counts identical and
/// preserves the invariant that, since every raw pair has `total >= server`, the retained
/// aggregates do too. Cache-health stats are computed over the same retained cohort.
pub(crate) fn summarize_samples(
    samples: &[OpSample]
) -> BenchmarkResult<crate::synthetic::report::MetricSet> {
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
    write_report(&report, &config.out).await
}

/// Print nothing extra, but write the JSON report to `out` **and** the pasteable Markdown alongside
/// it (`<out>.md`). Shared by [`run_and_report`] and `synthetic replay`.
pub(crate) async fn write_report(
    report: &crate::synthetic::report::Report,
    out: &str,
) -> BenchmarkResult<()> {
    let json = report.to_json()?;
    tokio::fs::write(out, json).await?;
    info!("wrote {}", out);
    println!("report written to {}", out);
    // A PR-pasteable Markdown report alongside the JSON: `<out>.md` (replacing a `.json` suffix).
    let md_path = markdown_path(out);
    tokio::fs::write(&md_path, report.to_markdown()).await?;
    info!("wrote {}", md_path);
    println!("markdown written to {}", md_path);
    Ok(())
}

/// The Markdown report path for a JSON `out` path: swap a trailing `.json` for `.md`, else append
/// `.md` (e.g. `synthetic-report.json` → `synthetic-report.md`, `report` → `report.md`).
fn markdown_path(out: &str) -> String {
    match out.strip_suffix(".json") {
        Some(stem) => format!("{stem}.md"),
        None => format!("{out}.md"),
    }
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
            concurrency,
            reset_every,
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
                concurrency,
                reset_every,
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
        crate::cli::SyntheticCommands::Record {
            config: config_path,
            graph,
            ops,
            all_reads,
            seed,
            nodes,
            edges,
            out_dir,
        } => {
            // Reuse the run-config resolution (with generate=true) to validate + resolve the
            // dataset knobs, graph and read-op selection, then record OFFLINE (no server).
            let overrides = config::CliOverrides {
                endpoint: None,
                graph,
                ops,
                all_reads,
                samples: None,
                warmup: None,
                concurrency: Vec::new(),
                reset_every: None,
                seed,
                cache: None,
                server_timeout_ms: None,
                client_deadline_ms: None,
                out: None,
                server_image: None,
                generate: true,
                nodes,
                edges,
            };
            let file = config::FileConfig::load(config_path.as_deref())?;
            let resolved = config::resolve(overrides, file)?;
            let spec = resolved.dataset.ok_or_else(|| {
                OtherError("record requires --nodes/--edges (or a config) to generate a dataset".to_string())
            })?;
            let manifest = recording::record(
                &spec,
                &resolved.graph,
                &resolved.ops,
                resolved.seed,
                DATASET_LOAD_BATCH,
                std::path::Path::new(&out_dir),
            )?;
            println!(
                "recorded {} op(s) into {} (workload_hash {})",
                manifest.ops.len(),
                out_dir,
                manifest.workload_hash
            );
            Ok(())
        }
        crate::cli::SyntheticCommands::Replay {
            recording,
            endpoint,
            graph,
            no_load,
            samples,
            warmup,
            server_timeout_ms,
            client_deadline_ms,
            out,
            server_image,
        } => {
            let replay_config = replay::ReplayConfig {
                recording_dir: std::path::PathBuf::from(recording),
                endpoint: endpoint.unwrap_or_else(|| "falkor://127.0.0.1:6379".to_string()),
                graph,
                load: !no_load,
                samples: samples.unwrap_or(1000),
                warmup: warmup.unwrap_or(200),
                server_timeout_ms: server_timeout_ms.unwrap_or(5_000),
                client_deadline_ms: client_deadline_ms.unwrap_or(6_000),
                out: out.unwrap_or_else(|| "synthetic-report.json".to_string()),
                server_image,
            };
            replay::run_and_report(&replay_config).await
        }
        crate::cli::SyntheticCommands::BaselineGuard { baseline, current } => {
            baseline_guard(&baseline, &current)
        }
    }
}

/// Guard a version comparison: load the saved baseline and current run reports, compare their
/// workload identity, and **abort** (return an error ⇒ non-zero exit) when the workloads differ, so
/// `synthetic-compare` never compares latencies across mismatched benchmarks. Advisory notes (a
/// version/image change, an identical or placeholder version) are printed but do not abort.
fn baseline_guard(
    baseline_path: &str,
    current_path: &str,
) -> BenchmarkResult<()> {
    let load = |path: &str| -> BenchmarkResult<crate::synthetic::report::Report> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| OtherError(format!("could not read report '{}': {}", path, e)))?;
        serde_json::from_str(&text)
            .map_err(|e| OtherError(format!("invalid synthetic report '{}': {}", path, e)))
    };
    let baseline = baseline::BaselineKey::from_report(&load(baseline_path)?);
    let current = baseline::BaselineKey::from_report(&load(current_path)?);

    match baseline::guard(&baseline, &current) {
        baseline::GuardOutcome::Proceed { warnings } => {
            for w in &warnings {
                eprintln!("⚠ {}", w);
            }
            println!("baseline guard: OK — same workload, safe to compare");
            Ok(())
        }
        baseline::GuardOutcome::Abort { reason } => {
            Err(OtherError(format!("baseline guard: ABORT — {}", reason)))
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
            // Write ops render their measured query per-invocation from a WriteScratch, not from a
            // corpus (the corpus is a stub), so the read-corpus invariants below don't apply.
            if s.write.is_some() {
                continue;
            }
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
        // `--all-reads` selects exactly the read-kind ops; write ops are opt-in via `--op`.
        assert!(
            reads.iter().all(|op| op.kind() == QueryType::Read),
            "all_reads must contain only reads"
        );
        assert!(
            !reads.contains(&OpName::CreateNode) && !reads.contains(&OpName::MergeMiss),
            "write ops must be excluded from --all-reads"
        );
        assert_eq!(
            reads.len(),
            OpName::all()
                .iter()
                .filter(|op| op.kind() == QueryType::Read)
                .count(),
            "all_reads covers every read op"
        );
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
    fn normalize_concurrency_sorts_dedups_and_validates() {
        // Deduped + sorted ascending so the curve reads low → high.
        assert_eq!(
            normalize_concurrency(&[8, 1, 4, 1, 8]).unwrap(),
            vec![1, 4, 8]
        );
        assert_eq!(
            normalize_concurrency(DEFAULT_CONCURRENCY).unwrap(),
            vec![1, 2, 4, 8, 16, 32]
        );
        // Empty and zero-containing sweeps are rejected.
        assert!(normalize_concurrency(&[]).is_err());
        assert!(normalize_concurrency(&[1, 0, 4]).is_err());
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
        // Use an empty temp config (not `config: None`) so the test never auto-detects an ambient
        // `synthetic-bench.toml` from the working directory.
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let cfg_path = std::env::temp_dir().join(format!(
            "syn-maps-{}-{}.toml",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&cfg_path, "# empty\n").unwrap();
        let command = crate::cli::SyntheticCommands::Run {
            config: Some(cfg_path.to_string_lossy().into_owned()),
            endpoint: Some("falkor://127.0.0.1:6379".to_string()),
            graph: Some("falkor".to_string()),
            ops: vec![OpName::ReturnConst],
            all_reads: false,
            samples: Some(0),
            warmup: Some(0),
            concurrency: vec![],
            reset_every: None,
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
        let _ = std::fs::remove_file(&cfg_path);
    }

    #[tokio::test]
    async fn run_command_run_requires_an_op() {
        // Neither --op nor --all-reads nor any config `operations` ⇒ a clear error before any
        // network use. Point --config at a real but empty config file so the file *loads* fine and
        // the failure comes from the no-operations validation (not a missing-file read error). The
        // filename mixes pid + a process-unique counter so parallel tests can't collide.
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir();
        let cfg_path = dir.join(format!(
            "syn-noops-{}-{}.toml",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&cfg_path, "# empty config, no operations\n").unwrap();
        let command = crate::cli::SyntheticCommands::Run {
            config: Some(cfg_path.to_string_lossy().into_owned()),
            endpoint: Some("falkor://127.0.0.1:6379".to_string()),
            graph: Some("falkor".to_string()),
            ops: vec![],
            all_reads: false,
            samples: Some(100),
            warmup: Some(0),
            concurrency: vec![],
            reset_every: None,
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
        let err = run_command(command).await.expect_err("no ops ⇒ error");
        assert!(
            format!("{err:?}").contains("no operations selected"),
            "expected a no-operations error, got: {err:?}"
        );
        let _ = std::fs::remove_file(&cfg_path);
    }

    #[test]
    fn list_ops_mentions_each_op() {
        let listing = list_ops();
        for op in OpName::value_variants() {
            assert!(listing.contains(op.as_str()));
        }
    }

    #[tokio::test]
    async fn baseline_guard_command_gates_on_corpus_hash() {
        // Hermetic: writes minimal report JSONs and drives the `baseline-guard` subcommand (which
        // only reads files + applies the guard — no server).
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir();
        let write_report = |hash: &str, ver: u64| -> String {
            let p = dir.join(format!(
                "bg-{}-{}.json",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            let json = format!(
                r#"{{"meta":{{"tool_version":"0.1.0","endpoint":"x","samples":1,"warmup":0,"server_timeout_ms":5000,"client_deadline_ms":6000,"connection":"c","started_at_epoch_secs":0,"server":{{"module_graph_ver":{ver}}},"dataset":{{"seed":1,"nodes":10,"edges":20,"corpus_hash":"{hash}"}}}},"operations":{{}}}}"#
            );
            std::fs::write(&p, json).unwrap();
            p.to_string_lossy().into_owned()
        };
        let base = write_report("sha256:same", 42001);
        // Same workload + same version ⇒ guard proceeds with an advisory warning (exercises the
        // warning-print path).
        let ok = write_report("sha256:same", 42001);
        assert!(run_command(crate::cli::SyntheticCommands::BaselineGuard {
            baseline: base.clone(),
            current: ok.clone(),
        })
        .await
        .is_ok());
        // Different workload ⇒ guard aborts.
        let bad = write_report("sha256:different", 42002);
        let err = run_command(crate::cli::SyntheticCommands::BaselineGuard {
            baseline: base.clone(),
            current: bad.clone(),
        })
        .await
        .expect_err("corpus_hash mismatch must abort");
        assert!(format!("{err}").contains("corpus_hash mismatch"));
        for p in [base, ok, bad] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn markdown_path_swaps_json_suffix_or_appends() {
        assert_eq!(markdown_path("synthetic-report.json"), "synthetic-report.md");
        assert_eq!(markdown_path("/tmp/out/report.json"), "/tmp/out/report.md");
        assert_eq!(markdown_path("report"), "report.md");
        // Only a trailing `.json` is swapped; anything else just gets `.md` appended.
        assert_eq!(markdown_path("weird.jsonx"), "weird.jsonx.md");
    }

    #[tokio::test]
    async fn run_rejects_zero_reset_every_for_write_ops_before_connecting() {
        // A write op with reset_every == 0 fails the fast pre-connection validation, so no
        // connection is opened — this stays hermetic (the endpoint is never touched).
        let cfg = Config {
            ops: vec![OpName::CreateNode],
            reset_every: 0,
            ..Config::default()
        };
        let err = run(&cfg).await.expect_err("zero cadence must be rejected");
        assert!(
            format!("{err:?}").contains("reset_every must be >= 1"),
            "expected a reset_every validation error, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn run_rejects_write_key_band_overflow_before_connecting() {
        // A write op whose widest concurrency level × reset_every would overflow the i32 key range
        // is rejected up-front, before any connection is opened (hermetic). Here worker 1's highest
        // key is 2·(i32::MAX) − 1, well past i32::MAX.
        let cfg = Config {
            ops: vec![OpName::CreateNode],
            reset_every: i32::MAX as usize,
            concurrency: vec![2],
            ..Config::default()
        };
        let err = run(&cfg)
            .await
            .expect_err("an overflowing key band must be rejected");
        assert!(
            format!("{err:?}").contains("overflows i32"),
            "expected an i32 key-band overflow error, got: {err:?}"
        );
    }

    #[test]
    fn summarize_samples_computes_paired_residual_and_cache() {
        let samples = vec![
            OpSample {
                server_ms: 0.10,
                total_ms: 0.40,
                rows: 1,
                cached: Some(true),
                mutations: crate::synthetic::writes::MutationStats::default(),
            },
            OpSample {
                server_ms: 0.12,
                total_ms: 0.45,
                rows: 1,
                cached: Some(false),
                mutations: crate::synthetic::writes::MutationStats::default(),
            },
            OpSample {
                server_ms: 0.11,
                total_ms: 0.42,
                rows: 1,
                cached: None,
                mutations: crate::synthetic::writes::MutationStats::default(),
            },
            OpSample {
                server_ms: 0.13,
                total_ms: 0.44,
                rows: 1,
                cached: Some(true),
                mutations: crate::synthetic::writes::MutationStats::default(),
            },
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
                OpSample {
                    server_ms: s,
                    total_ms: s + 0.3,
                    rows: 1,
                    cached: Some(true),
                    mutations: crate::synthetic::writes::MutationStats::default(),
                }
            })
            .collect();
        samples.push(OpSample {
            server_ms: 0.11,
            total_ms: 500.0,
            rows: 1,
            cached: Some(true),
            mutations: crate::synthetic::writes::MutationStats::default(),
        });
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
            OpSample {
                server_ms: 0.10,
                total_ms: 0.40,
                rows: 1,
                cached: None,
                mutations: crate::synthetic::writes::MutationStats::default(),
            },
            OpSample {
                server_ms: 0.12,
                total_ms: 0.45,
                rows: 1,
                cached: None,
                mutations: crate::synthetic::writes::MutationStats::default(),
            },
            OpSample {
                server_ms: 0.11,
                total_ms: 0.42,
                rows: 1,
                cached: None,
                mutations: crate::synthetic::writes::MutationStats::default(),
            },
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
