//! Synthetic per-operation benchmark — Part 1: a single-operation latency probe.
//!
//! Measures one Cypher operation in isolation against a FalkorDB endpoint, capturing on every
//! invocation the paired *server time* (FalkorDB's reported internal execution time) and *total
//! time* (end-to-end client round-trip), then summarizes them with severe-outlier removal and
//! writes a JSON report + console summary. This is the foundation the rest of the epic builds on.

pub mod op_runner;
pub mod provenance;
pub mod report;
pub mod stats;

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::falkor::falkor_endpoint_to_redis_url;
use crate::query::{Query, QueryBuilder};
use crate::queries_repository::QueryType;
use crate::synthetic::op_runner::{run_and_drain, OpSample};
use crate::synthetic::report::{Meta, OperationReport, Report};
use clap::ValueEnum;
use falkordb::{ConnectionStrategy, FalkorClientBuilder};
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

/// The set of operations the probe can run. Part 1 ships a single dataset-free baseline; later
/// parts extend this enum and the catalog together (it stays the single source of truth so
/// `--help`, shell completion and the catalog never drift).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum OpName {
    /// `RETURN $i` — a pure round-trip / server parse+exec baseline that needs no dataset.
    ReturnConst,
}

impl OpName {
    /// The stable string id used in reports and on the CLI.
    pub fn as_str(self) -> &'static str {
        match self {
            OpName::ReturnConst => "return_const",
        }
    }

    /// Whether this operation reads or writes (selects `RO_QUERY` vs `QUERY`).
    pub fn kind(self) -> QueryType {
        match self {
            OpName::ReturnConst => QueryType::Read,
        }
    }

    /// A one-line description for `list-ops`.
    pub fn description(self) -> &'static str {
        match self {
            OpName::ReturnConst => "RETURN $i — pure round-trip baseline (no dataset required)",
        }
    }

    /// Build the query for invocation `i` (parameterized; content varies so we don't measure a
    /// single trivially-cached literal).
    pub fn build_query(
        self,
        i: usize,
    ) -> Query {
        match self {
            OpName::ReturnConst => QueryBuilder::new()
                .text("RETURN $i AS x")
                .param("i", i as i32)
                .build(),
        }
    }

    /// Render the Cypher string for invocation `i` in the given cache mode.
    ///
    /// In [`CacheMode::Uncached`] a unique trailing comment (`/* co<i> */`) is appended so every
    /// invocation is a distinct plan-cache key — forcing FalkorDB to recompile the query each time
    /// (verified via the response's `cached_execution` flag), which exposes expression-compilation
    /// cost. In [`CacheMode::Cached`] the text is identical every time, so after warm-up the plan
    /// is reused and only execution is measured.
    pub fn render_cypher(
        self,
        i: usize,
        mode: CacheMode,
    ) -> String {
        let base = self.build_query(i).to_cypher();
        match mode {
            CacheMode::Cached => base,
            CacheMode::Uncached => format!("{} /* co{} */", base, i),
        }
    }
}

/// Which plan-cache condition to measure an operation under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum CacheSelection {
    /// Warm plan cache: identical query text, plan reused → execution only.
    Cached,
    /// Forced plan-cache miss every run (unique query text) → execution + compilation.
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

/// Configuration for a single-operation probe run.
#[derive(Debug, Clone)]
pub struct Config {
    pub endpoint: String,
    pub op: OpName,
    pub samples: usize,
    pub warmup: usize,
    pub server_timeout_ms: i64,
    pub client_deadline_ms: u64,
    pub cache: CacheSelection,
    pub out: String,
    pub server_image: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint: "falkor://127.0.0.1:6379".to_string(),
            op: OpName::ReturnConst,
            samples: 1000,
            warmup: 200,
            server_timeout_ms: 5_000,
            client_deadline_ms: 6_000,
            cache: CacheSelection::Both,
            out: "synthetic-report.json".to_string(),
            server_image: None,
        }
    }
}

/// Print the available operations (for `synthetic list-ops`).
pub fn list_ops() -> String {
    let mut out = String::from("Available operations:\n");
    for op in OpName::value_variants() {
        out.push_str(&format!("  {:<16} {}\n", op.as_str(), op.description()));
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

/// Run the probe: connect (single connection), collect server provenance, warm up, measure, then
/// build the [`Report`]. Writing the report to disk is the caller's responsibility (see
/// [`run_and_report`]).
pub async fn run(config: &Config) -> BenchmarkResult<Report> {
    if config.samples == 0 {
        return Err(OtherError("samples must be greater than 0".to_string()));
    }

    let started_at_epoch_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let connection_info = config
        .endpoint
        .as_str()
        .try_into()
        .map_err(|e| {
            OtherError(format!(
                "invalid endpoint '{}': {:?}",
                redact_endpoint(&config.endpoint),
                e
            ))
        })?;

    // A single dedicated connection: honest single-flight latency (the client otherwise defaults
    // to 8 multiplexed sockets).
    let client = FalkorClientBuilder::new_async()
        .with_connection_info(connection_info)
        .with_connection_strategy(ConnectionStrategy::Pooled {
            size: nonzero::nonzero!(1u8),
        })
        .build()
        .await?;
    let mut graph = client.select_graph("falkor");

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

    // Ensure the graph key exists: a read (`RO_QUERY`) against a never-written graph fails with
    // "Invalid graph operation on empty key". Probe with a read first and only write to
    // instantiate the graph when the error is exactly that empty-key condition — so a read-only
    // replica whose graph already exists still works, and any other error (auth/network) is
    // surfaced rather than masked. Both are bounded by the client deadline.
    let probe = tokio::time::timeout(
        client_deadline,
        graph
            .ro_query("RETURN 1")
            .with_timeout(config.server_timeout_ms)
            .execute(),
    )
    .await
    .map_err(|e| OtherError(format!("graph 'falkor' readiness probe timed out: {}", e)))?;
    match probe {
        Ok(_) => {}
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
                .map_err(|e| OtherError(format!("graph 'falkor' instantiation timed out: {}", e)))?
                .map_err(|e| {
                    OtherError(format!("failed to instantiate graph 'falkor': {:?}", e))
                })?;
            } else {
                return Err(OtherError(format!(
                    "graph 'falkor' readiness probe failed: {}",
                    msg
                )));
            }
        }
    }

    // Measure the operation under each requested plan-cache condition.
    let mut cached_set: Option<crate::synthetic::report::MetricSet> = None;
    let mut uncached_set: Option<crate::synthetic::report::MetricSet> = None;
    for &mode in config.cache.modes() {
        let set = measure_mode(&mut graph, config, mode, client_deadline).await?;
        match mode {
            CacheMode::Cached => cached_set = Some(set),
            CacheMode::Uncached => uncached_set = Some(set),
        }
    }

    // Derived expression-compilation cost: how much slower an uncached (recompiled) run's server
    // time is than a cached (plan-reused) one.
    let compilation_ms_median = match (&cached_set, &uncached_set) {
        (Some(c), Some(u)) => Some(u.server_ms.median - c.server_ms.median),
        _ => None,
    };

    let op_report = OperationReport {
        cached: cached_set,
        uncached: uncached_set,
        compilation_ms_median,
    };
    let mut operations = BTreeMap::new();
    operations.insert(config.op.as_str().to_string(), op_report);

    Ok(Report {
        meta: Meta {
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            endpoint: redact_endpoint(&config.endpoint),
            samples: config.samples,
            warmup: config.warmup,
            server_timeout_ms: config.server_timeout_ms,
            client_deadline_ms: config.client_deadline_ms,
            connection: "pool(size=1)".to_string(),
            started_at_epoch_secs,
            server,
        },
        operations,
    })
}

/// Whether a query error string is FalkorDB's "graph key does not exist yet" condition.
fn is_empty_graph_key(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("empty key") || m.contains("invalid graph operation")
}

/// Warm up then measure `config.samples` invocations of `config.op` in one cache mode.
async fn measure_mode(
    graph: &mut falkordb::AsyncGraph,
    config: &Config,
    mode: CacheMode,
    client_deadline: Duration,
) -> BenchmarkResult<crate::synthetic::report::MetricSet> {
    // Warm-up (discarded) primes the plan cache (cached mode) and the connection.
    for i in 0..config.warmup {
        let cypher = config.op.render_cypher(i, mode);
        let _ = run_and_drain(
            graph,
            config.op.kind(),
            &cypher,
            config.server_timeout_ms,
            client_deadline,
        )
        .await?;
    }

    let mut samples: Vec<OpSample> = Vec::with_capacity(config.samples);
    for i in 0..config.samples {
        let cypher = config.op.render_cypher(config.warmup + i, mode);
        let sample = run_and_drain(
            graph,
            config.op.kind(),
            &cypher,
            config.server_timeout_ms,
            client_deadline,
        )
        .await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_name_maps_are_consistent() {
        assert_eq!(OpName::ReturnConst.as_str(), "return_const");
        assert_eq!(OpName::ReturnConst.kind(), QueryType::Read);
        assert!(!OpName::ReturnConst.description().is_empty());
    }

    #[test]
    fn build_query_is_parameterized_and_varies() {
        let q0 = OpName::ReturnConst.build_query(0);
        let q1 = OpName::ReturnConst.build_query(1);
        assert!(q0.text.contains("$i"));
        assert_eq!(q0.params.len(), 1);
        // Different invocation index ⇒ different parameter value.
        assert_ne!(q0.params.get("i"), q1.params.get("i"));
    }

    #[test]
    fn uncached_render_is_unique_per_invocation_cached_is_stable() {
        // Cached mode: no per-invocation uniqueness token, so FalkorDB caches by the parameterized
        // query body (it strips the leading `CYPHER <params>` prefix from the cache key), and the
        // plan is reused across invocations.
        let c0 = OpName::ReturnConst.render_cypher(0, CacheMode::Cached);
        let c1 = OpName::ReturnConst.render_cypher(1, CacheMode::Cached);
        assert!(!c0.contains("/* co"));
        assert!(!c1.contains("/* co"));
        assert!(c0.contains("RETURN $i AS x"));
        assert!(c1.contains("RETURN $i AS x"));

        // Uncached mode: a unique trailing token per `i` ⇒ distinct plan-cache key each run.
        let u0 = OpName::ReturnConst.render_cypher(0, CacheMode::Uncached);
        let u1 = OpName::ReturnConst.render_cypher(1, CacheMode::Uncached);
        assert_ne!(u0, u1);
        assert!(u0.contains("/* co0 */"));
        assert!(u1.contains("/* co1 */"));
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
