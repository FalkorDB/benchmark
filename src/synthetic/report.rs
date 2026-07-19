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
    pub samples: usize,
    pub warmup: usize,
    pub server_timeout_ms: i64,
    pub client_deadline_ms: u64,
    /// Connection strategy, e.g. `"pool(size=1)"`.
    pub connection: String,
    pub started_at_epoch_secs: u64,
    pub server: ServerInfo,
}

/// Stats for one operation: the two paired metrics plus the derived residual, and cache health.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationReport {
    pub server_ms: Summary,
    pub total_ms: Summary,
    /// Paired `total − server` per invocation (everything outside the DB's internal timer).
    pub non_internal_ms: Summary,
    /// Fraction of invocations *with a known cache stat* that reported an un-cached execution plan
    /// (denominator excludes `cached_unknown`).
    pub cached_false_rate: f64,
    /// Count of invocations whose response omitted the cache statistic.
    pub cached_unknown: usize,
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
            "synthetic benchmark — endpoint {}  samples {}  warmup {}  connection {}\n",
            self.meta.endpoint, self.meta.samples, self.meta.warmup, self.meta.connection
        ));
        let v = self
            .meta
            .server
            .module_graph_ver
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        out.push_str(&format!(
            "server — falkordb module ver {}{}  redis {}\n",
            v,
            if self.meta.server.is_placeholder() {
                " (dev placeholder — use a tagged image for comparisons)"
            } else {
                ""
            },
            self.meta.server.redis_version.as_deref().unwrap_or("?"),
        ));
        if let Some(img) = &self.meta.server.server_image {
            out.push_str(&format!("server image: {}\n", img));
        }
        for (name, op) in &self.operations {
            out.push_str(&format!("\n{}\n", name));
            out.push_str(&format!(
                "  server_ms  median {:.3}  mean {:.3}  p99 {:.3}  (n={}, removed {})\n",
                op.server_ms.median, op.server_ms.mean, op.server_ms.p99, op.server_ms.n, op.server_ms.removed
            ));
            out.push_str(&format!(
                "  total_ms   median {:.3}  mean {:.3}  p99 {:.3}  (n={}, removed {})\n",
                op.total_ms.median, op.total_ms.mean, op.total_ms.p99, op.total_ms.n, op.total_ms.removed
            ));
            out.push_str(&format!(
                "  non_internal_ms (paired total-server)  median {:.3}\n",
                op.non_internal_ms.median
            ));
            out.push_str(&format!(
                "  cached_execution=false: {:.1}%  (unknown {})\n",
                op.cached_false_rate * 100.0, op.cached_unknown
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_summary() -> Summary {
        crate::synthetic::stats::summarize(&[1.0, 2.0, 3.0, 4.0, 5.0]).unwrap()
    }

    fn sample_report() -> Report {
        let mut operations = BTreeMap::new();
        operations.insert(
            "return_const".to_string(),
            OperationReport {
                server_ms: sample_summary(),
                total_ms: sample_summary(),
                non_internal_ms: sample_summary(),
                cached_false_rate: 0.0,
                cached_unknown: 0,
            },
        );
        Report {
            meta: Meta {
                tool_version: "0.1.0".to_string(),
                endpoint: "falkor://127.0.0.1:6379".to_string(),
                samples: 1000,
                warmup: 200,
                server_timeout_ms: 5000,
                client_deadline_ms: 6000,
                connection: "pool(size=1)".to_string(),
                started_at_epoch_secs: 42,
                server: ServerInfo {
                    module_graph_ver: Some(42001),
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
        assert!(back.operations.contains_key("return_const"));
        assert_eq!(back.meta.server.module_graph_ver, Some(42001));
    }

    #[test]
    fn console_contains_key_fields() {
        let out = sample_report().to_console();
        assert!(out.contains("return_const"));
        assert!(out.contains("server_ms"));
        assert!(out.contains("total_ms"));
        assert!(out.contains("42001"));
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
