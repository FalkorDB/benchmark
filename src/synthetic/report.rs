//! The synthetic-benchmark report: run metadata, server provenance, per-operation stats, and
//! JSON + console rendering.

use crate::synthetic::stats::Summary;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Serde default for [`Meta::graph`]: the graph a pre-Part-2 report was measured against.
fn default_graph() -> String {
    crate::synthetic::DEFAULT_GRAPH.to_string()
}

/// Server build/provenance captured from `INFO server` + `MODULE LIST`, plus the operator-supplied
/// image reference. FalkorDB does not expose a graph-module git SHA to clients, so the reproducible
/// identity is `module_graph_ver` (real for tagged releases, a `999999` placeholder on `:edge`)
/// together with `server_image` when provided. The version is the numeric encoding
/// `major*10000 + minor*100 + patch` (e.g. `42001` → `4.20.1`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerInfo {
    /// FalkorDB graph-module version (e.g. `42001` for v4.20.1), or `None` if it couldn't be read.
    pub module_graph_ver: Option<u64>,
    /// The server's query plan-cache size (`GRAPH.CONFIG GET CACHE_SIZE`), for context on the
    /// cached-vs-uncached comparison. `None` if it couldn't be read.
    pub cache_size: Option<u64>,
    pub redis_version: Option<String>,
    pub redis_build_id: Option<String>,
    pub redis_git_sha1: Option<String>,
    pub run_id: Option<String>,
    pub os: Option<String>,
    pub arch_bits: Option<String>,
    /// Operator-supplied image reference (e.g. `falkordb/falkordb:v4.2.1@sha256:…`), recorded
    /// verbatim from `--server-image` / `FALKOR_SERVER_IMAGE`.
    pub server_image: Option<String>,
}

impl ServerInfo {
    /// The FalkorDB dev-placeholder module version used by non-release images such as `:edge`.
    pub const PLACEHOLDER_VER: u64 = 999_999;

    /// Whether the module version is the dev placeholder (⇒ not a tagged release; version
    /// comparisons are meaningless).
    pub fn is_placeholder(&self) -> bool {
        self.module_graph_ver == Some(Self::PLACEHOLDER_VER)
    }
}

/// Run-level metadata: how and against what the probe ran.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub tool_version: String,
    pub endpoint: String,
    /// Graph key the probe measured against. Defaults to [`crate::synthetic::DEFAULT_GRAPH`] when
    /// deserializing a pre-Part-2 report (which lacked this field but always measured the default
    /// graph), so an old report round-trips to its true graph rather than an empty string.
    #[serde(default = "default_graph")]
    pub graph: String,
    pub samples: usize,
    pub warmup: usize,
    /// Concurrency levels swept (the closed-loop worker counts `C`). `#[serde(default)]` for
    /// backward compatibility with pre-Part-4 reports (which always measured single-connection).
    #[serde(default)]
    pub concurrency: Vec<usize>,
    /// Seed used to generate the per-operation parameter corpora (for reproducibility).
    /// `#[serde(default)]` for backward compatibility with pre-Part-2 reports.
    #[serde(default)]
    pub seed: u64,
    /// Number of distinct parameterizations pre-generated per operation. `#[serde(default)]` for
    /// backward compatibility with pre-Part-2 reports.
    #[serde(default)]
    pub corpus_size: usize,
    pub server_timeout_ms: i64,
    pub client_deadline_ms: u64,
    /// Connection strategy, e.g. `"pool(size=1)"`.
    pub connection: String,
    pub started_at_epoch_secs: u64,
    pub server: ServerInfo,
    /// Present only when the probe generated its own reproducible dataset (Part 3). Absent for an
    /// externally-provided graph, whose contents we can't fingerprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset: Option<DatasetInfo>,
}

/// Provenance for a generated synthetic dataset: its knobs and the `corpus_hash` that identifies
/// the whole workload (dataset + selected operations + query bodies), so runs are only compared
/// when they match.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetInfo {
    pub seed: u64,
    pub nodes: usize,
    pub edges: usize,
    /// Algorithm-tagged hash (`sha256:…`) of the full workload; equal iff comparable.
    pub corpus_hash: String,
}

/// Latency and cache-health stats for one (operation, cache-mode) measurement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricSet {
    pub server_ms: Summary,
    pub total_ms: Summary,
    /// Paired `total − server` per invocation (everything outside the DB's internal timer).
    pub non_internal_ms: Summary,
    /// Fraction of retained invocations *with a known cache stat* that reported an un-cached
    /// execution plan (denominator excludes `cached_unknown`).
    pub cached_false_rate: f64,
    /// Count of retained invocations whose response omitted the cache statistic.
    pub cached_unknown: usize,
}

/// One (operation, cache-mode) measurement at a single concurrency level: the latency stats plus
/// the **achieved** throughput the closed-loop engine sustained at that level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LevelMetrics {
    /// Achieved throughput (completed measured invocations ÷ wall-clock window), operations/sec.
    pub throughput_ops_per_sec: f64,
    /// Latency + cache-health stats over the pooled (outlier-filtered) samples for this level.
    pub metrics: MetricSet,
}

/// Stats for one operation at one concurrency level `C`, across cache modes.
///
/// `cached` measures with the plan cache warm (execution only); `uncached` forces a plan-cache
/// miss on every invocation (unique query text), so it also pays expression **compilation** each
/// time. `compilation_ms_median` is the derived per-op compilation cost when both were measured.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LevelReport {
    /// Number of concurrent closed-loop workers (`C`) this level ran with.
    pub concurrency: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached: Option<LevelMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uncached: Option<LevelMetrics>,
    /// `uncached.server_ms.median − cached.server_ms.median` (exposes compilation slowness without
    /// hiding execution). Present only when both modes were measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compilation_ms_median: Option<f64>,
}

/// Stats for one operation across the whole concurrency sweep.
///
/// Each entry of `levels` is the same operation measured at one concurrency `C`; together they
/// trace the latency-vs-throughput curve. A single-element sweep (`concurrency = [1]`) reproduces
/// the pre-Part-4 single-connection measurement (plus its achieved throughput).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationReport {
    pub levels: Vec<LevelReport>,
}

impl OperationReport {
    /// The level with the highest achieved throughput (across either cache mode), i.e. the sweep's
    /// saturation "knee". `None` when no level recorded a throughput.
    fn knee_concurrency(&self) -> Option<usize> {
        self.levels
            .iter()
            .filter_map(|lvl| {
                let t = level_peak_throughput(lvl)?;
                Some((lvl.concurrency, t))
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(c, _)| c)
    }
}

/// The higher of a level's cached/uncached achieved throughput, if any mode was measured.
fn level_peak_throughput(level: &LevelReport) -> Option<f64> {
    let cached = level.cached.as_ref().map(|m| m.throughput_ops_per_sec);
    let uncached = level.uncached.as_ref().map(|m| m.throughput_ops_per_sec);
    match (cached, uncached) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// The current synthetic-report schema version. Bumped when the on-disk shape changes
/// incompatibly (Part 4 introduced `operations[].levels[]` and per-metric `p95`).
pub const SCHEMA_VERSION: u32 = 2;

/// Serde default for [`Report::schema_version`]: a report written before the field existed is a
/// pre-Part-4 (v1) report.
fn default_schema_version() -> u32 {
    1
}

/// The full report written to `synthetic-report.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    /// On-disk schema version (see [`SCHEMA_VERSION`]); lets later tooling (baseline comparison)
    /// detect an incompatible older report instead of silently misreading it.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub meta: Meta,
    pub operations: BTreeMap<String, OperationReport>,
}

impl Report {
    /// Serialize to pretty JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Render a compact human-readable summary (one block per operation).
    pub fn to_console(&self) -> String {
        let mut out = String::new();
        let concurrency = if self.meta.concurrency.is_empty() {
            "1".to_string()
        } else {
            self.meta
                .concurrency
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
                .join(",")
        };
        out.push_str(&format!(
            "synthetic benchmark — endpoint {}  graph {}  samples {}  warmup {}  concurrency [{}]  seed {}  connection {}\n",
            self.meta.endpoint,
            self.meta.graph,
            self.meta.samples,
            self.meta.warmup,
            concurrency,
            self.meta.seed,
            self.meta.connection
        ));
        let v = self
            .meta
            .server
            .module_graph_ver
            .map(crate::synthetic::provenance::decode_module_version)
            .unwrap_or_else(|| "unknown".to_string());
        let cache_size = self
            .meta
            .server
            .cache_size
            .map(|c| c.to_string())
            .unwrap_or_else(|| "?".to_string());
        out.push_str(&format!(
            "server — falkordb module ver {}{}  redis {}  CACHE_SIZE {}\n",
            v,
            if self.meta.server.is_placeholder() {
                " (dev placeholder — use a tagged image for comparisons)"
            } else {
                ""
            },
            self.meta.server.redis_version.as_deref().unwrap_or("?"),
            cache_size,
        ));
        if let Some(img) = &self.meta.server.server_image {
            out.push_str(&format!("server image: {}\n", img));
        }
        if let Some(d) = &self.meta.dataset {
            out.push_str(&format!(
                "dataset — seed {}  nodes {}  edges {}  corpus_hash {}\n",
                d.seed, d.nodes, d.edges, d.corpus_hash
            ));
        }
        for (name, op) in &self.operations {
            out.push_str(&format!("\n{}\n", name));
            render_op_levels(&mut out, op);
        }
        out
    }
}

/// Render one operation's concurrency sweep as a latency-vs-throughput table per cache mode,
/// followed by the derived per-level compilation cost. The highest-throughput level (the
/// saturation "knee") is flagged.
fn render_op_levels(
    out: &mut String,
    op: &OperationReport,
) {
    let knee = op.knee_concurrency();
    for (mode_label, pick) in [
        (
            "cached — plan reused, execution only",
            (|l: &LevelReport| l.cached.as_ref()) as fn(&LevelReport) -> Option<&LevelMetrics>,
        ),
        (
            "uncached — plan-cache miss each run, execution + compilation",
            (|l: &LevelReport| l.uncached.as_ref()) as fn(&LevelReport) -> Option<&LevelMetrics>,
        ),
    ] {
        if !op.levels.iter().any(|l| pick(l).is_some()) {
            continue;
        }
        out.push_str(&format!("  [{}]\n", mode_label));
        out.push_str(
            "    C    throughput(ops/s)   server p50/p90/p95/p99             total p50/p90/p95/p99              miss%\n",
        );
        for level in &op.levels {
            let Some(m) = pick(level) else { continue };
            let knee_mark = if knee == Some(level.concurrency) {
                "  <- knee"
            } else {
                ""
            };
            out.push_str(&format!(
                "  {:>3}   {:>15.0}   {:>7.3} /{:>6.3} /{:>6.3} /{:>6.3}   {:>7.3} /{:>6.3} /{:>6.3} /{:>6.3}   {:>5.1}{}\n",
                level.concurrency,
                m.throughput_ops_per_sec,
                m.metrics.server_ms.median,
                m.metrics.server_ms.p90,
                m.metrics.server_ms.p95,
                m.metrics.server_ms.p99,
                m.metrics.total_ms.median,
                m.metrics.total_ms.p90,
                m.metrics.total_ms.p95,
                m.metrics.total_ms.p99,
                m.metrics.cached_false_rate * 100.0,
                knee_mark,
            ));
        }
    }
    if op.levels.iter().any(|l| l.compilation_ms_median.is_some()) {
        out.push_str("  compilation_ms (median uncached-cached server time) by level:\n");
        for level in &op.levels {
            if let Some(comp) = level.compilation_ms_median {
                out.push_str(&format!("    C={:<4} {:.3}\n", level.concurrency, comp));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_summary() -> Summary {
        crate::synthetic::stats::summarize(&[1.0, 2.0, 3.0, 4.0, 5.0]).unwrap()
    }

    fn sample_metric_set() -> MetricSet {
        MetricSet {
            server_ms: sample_summary(),
            total_ms: sample_summary(),
            non_internal_ms: sample_summary(),
            cached_false_rate: 0.0,
            cached_unknown: 0,
        }
    }

    fn sample_level_metrics(throughput: f64) -> LevelMetrics {
        LevelMetrics {
            throughput_ops_per_sec: throughput,
            metrics: sample_metric_set(),
        }
    }

    fn sample_report() -> Report {
        let mut operations = BTreeMap::new();
        operations.insert(
            "return_const".to_string(),
            OperationReport {
                levels: vec![
                    LevelReport {
                        concurrency: 1,
                        cached: Some(sample_level_metrics(2_950.0)),
                        uncached: Some(sample_level_metrics(2_800.0)),
                        compilation_ms_median: Some(0.05),
                    },
                    LevelReport {
                        concurrency: 8,
                        cached: Some(sample_level_metrics(30_400.0)),
                        uncached: Some(sample_level_metrics(28_000.0)),
                        compilation_ms_median: Some(0.06),
                    },
                ],
            },
        );
        Report {
            schema_version: SCHEMA_VERSION,
            meta: Meta {
                tool_version: "0.1.0".to_string(),
                endpoint: "falkor://127.0.0.1:6379".to_string(),
                graph: "falkor".to_string(),
                samples: 1000,
                warmup: 200,
                concurrency: vec![1, 8],
                seed: 0,
                corpus_size: 256,
                server_timeout_ms: 5000,
                client_deadline_ms: 6000,
                connection: "pool(size=1) per worker".to_string(),
                started_at_epoch_secs: 42,
                server: ServerInfo {
                    module_graph_ver: Some(42001),
                    cache_size: Some(25),
                    redis_version: Some("8.6.3".to_string()),
                    ..Default::default()
                },
                dataset: None,
            },
            operations,
        }
    }

    #[test]
    fn json_round_trips() {
        let report = sample_report();
        let json = report.to_json().unwrap();
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(back.schema_version, SCHEMA_VERSION);
        assert_eq!(back.meta.samples, 1000);
        assert_eq!(back.meta.concurrency, vec![1, 8]);
        assert_eq!(back.operations.len(), 1);
        let op = back.operations.get("return_const").unwrap();
        assert_eq!(op.levels.len(), 2);
        let l0 = &op.levels[0];
        assert_eq!(l0.concurrency, 1);
        assert!(l0.cached.is_some());
        assert!(l0.uncached.is_some());
        assert_eq!(l0.compilation_ms_median, Some(0.05));
        assert_eq!(l0.cached.as_ref().unwrap().throughput_ops_per_sec, 2_950.0);
        // p95 is present on every summary (Part 4 addition).
        let _ = l0.cached.as_ref().unwrap().metrics.server_ms.p95;
        assert_eq!(back.meta.server.module_graph_ver, Some(42001));
        assert_eq!(back.meta.server.cache_size, Some(25));
    }

    #[test]
    fn pre_part4_report_deserializes_with_defaults() {
        // A report written before Part 4 has no schema_version/concurrency fields; `#[serde(
        // default)]` must let its metadata deserialize rather than erroring. (The operation shape
        // changed to `levels[]`, so only the metadata is exercised here.)
        let old = r#"{
            "meta": {
                "tool_version": "0.1.0",
                "endpoint": "falkor://127.0.0.1:6379",
                "samples": 1000, "warmup": 200,
                "server_timeout_ms": 5000, "client_deadline_ms": 6000,
                "connection": "pool(size=1)", "started_at_epoch_secs": 42,
                "server": {}
            },
            "operations": {}
        }"#;
        let report: Report = serde_json::from_str(old).expect("old report should deserialize");
        // Missing schema_version reconstructs to the pre-Part-4 version 1.
        assert_eq!(report.schema_version, 1);
        // A pre-Part-2 report always measured the default graph, so `graph` reconstructs to it.
        assert_eq!(report.meta.graph, "falkor");
        assert_eq!(report.meta.seed, 0);
        assert_eq!(report.meta.corpus_size, 0);
        assert!(report.meta.concurrency.is_empty());
        assert_eq!(report.meta.samples, 1000);
    }

    #[test]
    fn console_contains_key_fields() {
        let mut r = sample_report();
        r.meta.server.server_image = Some("falkordb/falkordb:v4.2.1@sha256:deadbeef".to_string());
        let out = r.to_console();
        assert!(out.contains("return_const"));
        // The latency-vs-throughput table headers and both cache-mode sections.
        assert!(out.contains("throughput(ops/s)"));
        assert!(out.contains("server p50/p90/p95/p99"));
        assert!(out.contains("total p50/p90/p95/p99"));
        assert!(out.contains("cached — plan reused"));
        assert!(out.contains("uncached — plan-cache miss"));
        assert!(out.contains("compilation_ms"));
        // Concurrency sweep is echoed in the header, and the knee is flagged.
        assert!(out.contains("concurrency [1,8]"));
        assert!(out.contains("<- knee"));
        assert!(out.contains("4.20.1"));
        assert!(out.contains("CACHE_SIZE 25"));
        // The operator-supplied image identity is echoed when present.
        assert!(out.contains("server image: falkordb/falkordb:v4.2.1@sha256:deadbeef"));
    }

    #[test]
    fn console_renders_single_cache_modes_and_defaults() {
        // Empty meta.concurrency (a pre-Part-4-style report) renders the implicit single level, and
        // single-cache-mode ops render only their measured table (exercising the mode-skip and the
        // knee/peak-throughput paths for one-sided levels).
        let mut r = sample_report();
        r.meta.concurrency = vec![];
        let mut ops = BTreeMap::new();
        ops.insert(
            "cached_only".to_string(),
            OperationReport {
                levels: vec![
                    LevelReport {
                        concurrency: 1,
                        cached: Some(sample_level_metrics(100.0)),
                        uncached: None,
                        compilation_ms_median: None,
                    },
                    LevelReport {
                        concurrency: 4,
                        cached: Some(sample_level_metrics(400.0)),
                        uncached: None,
                        compilation_ms_median: None,
                    },
                ],
            },
        );
        ops.insert(
            "uncached_only".to_string(),
            OperationReport {
                levels: vec![LevelReport {
                    concurrency: 1,
                    cached: None,
                    uncached: Some(sample_level_metrics(50.0)),
                    compilation_ms_median: None,
                }],
            },
        );
        ops.insert(
            "mixed".to_string(),
            OperationReport {
                levels: vec![
                    LevelReport {
                        concurrency: 1,
                        cached: Some(sample_level_metrics(100.0)),
                        uncached: Some(sample_level_metrics(90.0)),
                        compilation_ms_median: Some(0.02),
                    },
                    // A level measured in cached mode only: no uncached row, no compilation.
                    LevelReport {
                        concurrency: 4,
                        cached: Some(sample_level_metrics(300.0)),
                        uncached: None,
                        compilation_ms_median: None,
                    },
                ],
            },
        );
        r.operations = ops;
        let out = r.to_console();

        // An empty concurrency sweep prints the implicit single level.
        assert!(out.contains("concurrency [1]"));
        assert!(out.contains("cached_only"));
        assert!(out.contains("uncached_only"));
        // The knee is flagged on the highest-throughput level across the report.
        assert!(out.contains("<- knee"));
        // The mixed op's compilation block is present (from the level that has both modes).
        assert!(out.contains("compilation_ms"));
    }

    #[test]
    fn console_shows_dataset_block_when_generated() {
        let mut r = sample_report();
        r.meta.dataset = Some(DatasetInfo {
            seed: 42,
            nodes: 1000,
            edges: 5000,
            corpus_hash: "sha256:abc123".to_string(),
        });
        let out = r.to_console();
        assert!(
            out.contains("dataset — seed 42  nodes 1000  edges 5000  corpus_hash sha256:abc123")
        );
        // Absent by default (externally-supplied graph).
        assert!(!sample_report().to_console().contains("dataset —"));
    }

    #[test]
    fn placeholder_version_is_flagged() {
        let mut r = sample_report();
        r.meta.server.module_graph_ver = Some(ServerInfo::PLACEHOLDER_VER);
        assert!(r.meta.server.is_placeholder());
        assert!(r.to_console().contains("dev placeholder"));
    }

    #[test]
    fn non_placeholder_version_not_flagged() {
        assert!(!sample_report().meta.server.is_placeholder());
    }
}
