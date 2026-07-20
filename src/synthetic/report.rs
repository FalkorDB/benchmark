//! The synthetic-benchmark report: run metadata, server provenance, per-operation stats, and
//! JSON + console rendering.

use crate::synthetic::stats::Summary;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    /// Graph key the probe measured against. `#[serde(default)]` so reports written before Part 2
    /// (which lacked this field) still deserialize.
    #[serde(default)]
    pub graph: String,
    pub samples: usize,
    pub warmup: usize,
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

/// Stats for one operation across cache modes.
///
/// `cached` measures with the plan cache warm (execution only); `uncached` forces a plan-cache
/// miss on every invocation (unique query text), so it also pays expression **compilation** each
/// time. `compilation_ms_median` is the derived per-op compilation cost when both were measured.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached: Option<MetricSet>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uncached: Option<MetricSet>,
    /// `uncached.server_ms.median − cached.server_ms.median` (exposes compilation slowness without
    /// hiding execution). Present only when both modes were measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compilation_ms_median: Option<f64>,
}

/// The full report written to `synthetic-report.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
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
        out.push_str(&format!(
            "synthetic benchmark — endpoint {}  graph {}  samples {}  warmup {}  seed {}  connection {}\n",
            self.meta.endpoint,
            self.meta.graph,
            self.meta.samples,
            self.meta.warmup,
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
        for (name, op) in &self.operations {
            out.push_str(&format!("\n{}\n", name));
            if let Some(m) = &op.cached {
                out.push_str("  [cached — plan reused, execution only]\n");
                render_metric_set(&mut out, m);
            }
            if let Some(m) = &op.uncached {
                out.push_str("  [uncached — plan-cache miss each run, execution + compilation]\n");
                render_metric_set(&mut out, m);
            }
            if let Some(comp) = op.compilation_ms_median {
                out.push_str(&format!(
                    "  compilation_ms (median uncached-cached server time)  {:.3}\n",
                    comp
                ));
            }
        }
        out
    }
}

/// Render one metric set (server/total/residual + cache health) as indented console lines.
fn render_metric_set(
    out: &mut String,
    m: &MetricSet,
) {
    out.push_str(&format!(
        "    server_ms  median {:.3}  mean {:.3}  p99 {:.3}  (n={}, removed {})\n",
        m.server_ms.median, m.server_ms.mean, m.server_ms.p99, m.server_ms.n, m.server_ms.removed
    ));
    out.push_str(&format!(
        "    total_ms   median {:.3}  mean {:.3}  p99 {:.3}  (n={}, removed {})\n",
        m.total_ms.median, m.total_ms.mean, m.total_ms.p99, m.total_ms.n, m.total_ms.removed
    ));
    out.push_str(&format!(
        "    non_internal_ms (paired total-server)  median {:.3}\n",
        m.non_internal_ms.median
    ));
    out.push_str(&format!(
        "    cached_execution=false: {:.1}%  (unknown {})\n",
        m.cached_false_rate * 100.0, m.cached_unknown
    ));
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

    fn sample_report() -> Report {
        let mut operations = BTreeMap::new();
        operations.insert(
            "return_const".to_string(),
            OperationReport {
                cached: Some(sample_metric_set()),
                uncached: Some(sample_metric_set()),
                compilation_ms_median: Some(0.05),
            },
        );
        Report {
            meta: Meta {
                tool_version: "0.1.0".to_string(),
                endpoint: "falkor://127.0.0.1:6379".to_string(),
                graph: "falkor".to_string(),
                samples: 1000,
                warmup: 200,
                seed: 0,
                corpus_size: 256,
                server_timeout_ms: 5000,
                client_deadline_ms: 6000,
                connection: "pool(size=1)".to_string(),
                started_at_epoch_secs: 42,
                server: ServerInfo {
                    module_graph_ver: Some(42001),
                    cache_size: Some(25),
                    redis_version: Some("8.6.3".to_string()),
                    ..Default::default()
                },
            },
            operations,
        }
    }

    #[test]
    fn json_round_trips() {
        let report = sample_report();
        let json = report.to_json().unwrap();
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(back.meta.samples, 1000);
        assert_eq!(back.operations.len(), 1);
        let op = back.operations.get("return_const").unwrap();
        assert!(op.cached.is_some());
        assert!(op.uncached.is_some());
        assert_eq!(op.compilation_ms_median, Some(0.05));
        assert_eq!(back.meta.server.module_graph_ver, Some(42001));
        assert_eq!(back.meta.server.cache_size, Some(25));
    }

    #[test]
    fn pre_part2_report_deserializes_with_defaults() {
        // A report written before Part 2 has no graph/seed/corpus_size fields; `#[serde(default)]`
        // must let it deserialize (falling back to empty/0) rather than erroring.
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
        assert_eq!(report.meta.graph, "");
        assert_eq!(report.meta.seed, 0);
        assert_eq!(report.meta.corpus_size, 0);
        assert_eq!(report.meta.samples, 1000);
    }

    #[test]
    fn console_contains_key_fields() {
        let mut r = sample_report();
        r.meta.server.server_image =
            Some("falkordb/falkordb:v4.2.1@sha256:deadbeef".to_string());
        let out = r.to_console();
        assert!(out.contains("return_const"));
        assert!(out.contains("server_ms"));
        assert!(out.contains("total_ms"));
        assert!(out.contains("4.20.1"));
        assert!(out.contains("cached"));
        assert!(out.contains("compilation_ms"));
        assert!(out.contains("CACHE_SIZE 25"));
        // The operator-supplied image identity is echoed when present.
        assert!(out.contains("server image: falkordb/falkordb:v4.2.1@sha256:deadbeef"));
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
