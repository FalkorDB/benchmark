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

/// The client host the probe ran on (best-effort via `sysinfo`). Every field is optional because
/// `sysinfo` can't always determine them and availability varies by platform, so a missing value
/// is `None`/`0` rather than an error. This is the **client** machine driving the benchmark, not
/// the FalkorDB server (whose OS/arch live in [`ServerInfo`]).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostInfo {
    /// Client hostname (kept out of the pasteable Markdown, present in JSON/console).
    pub hostname: Option<String>,
    /// Long OS name, e.g. `"macOS 15.1"` / `"Linux 6.8 Ubuntu 24.04"`.
    pub os: Option<String>,
    /// Kernel version string.
    pub kernel: Option<String>,
    /// CPU architecture, e.g. `"aarch64"` / `"x86_64"`.
    pub arch: Option<String>,
    /// CPU brand string, e.g. `"Apple M2"` / `"Intel(R) Xeon(R) …"`.
    pub cpu: Option<String>,
    /// Physical core count (`None` if `sysinfo` can't determine it).
    pub physical_cores: Option<usize>,
    /// Logical CPU count.
    pub logical_cores: usize,
    /// Total physical memory in bytes.
    pub total_memory_bytes: u64,
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
    /// The client host the probe ran on. `#[serde(default)]` so pre-Part-6 reports (which lacked it)
    /// still deserialize, defaulting to an empty [`HostInfo`].
    #[serde(default)]
    pub host: HostInfo,
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
    /// A `sha256:…` digest of this operation's **result cardinality** across its recorded commands
    /// (present only for a `synthetic run --recording` run). Two versions that return a different number of
    /// rows for the same recorded command produce different digests, so a version returning wrong
    /// or empty results faster can't masquerade as an improvement. `None` for a `synthetic run`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_digest: Option<String>,
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
    /// On-disk schema version (see [`SCHEMA_VERSION`]). Because the `operations[].levels[]` shape
    /// changed in v2, a v1 report that actually contains operation data will **not** deserialize
    /// into this struct — so tooling cannot rely on parsing a full [`Report`] to detect an old
    /// version. Instead, read `schema_version` from the raw JSON first (a tiny
    /// `{ "schema_version": u32 }` probe) and branch/migrate on it; the `serde` default of `1` only
    /// covers reports that predate the field on otherwise-compatible reads (e.g. metadata-only or
    /// an empty `operations` map).
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
        let host_line = host_summary(&self.meta.host, true);
        if !host_line.is_empty() {
            out.push_str(&format!("client host — {}\n", host_line));
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

    /// Render the report as GitHub-flavoured **Markdown** (a metadata table + one latency-vs-
    /// throughput table per operation and cache mode), suitable for pasting into a PR. Shares the
    /// numeric row model with [`Report::to_console`] so the two never disagree.
    pub fn to_markdown(&self) -> String {
        let m = &self.meta;
        let mut out = String::from("# Synthetic per-operation benchmark\n\n");

        let module_ver = m
            .server
            .module_graph_ver
            .map(crate::synthetic::provenance::decode_module_version)
            .unwrap_or_else(|| "unknown".to_string());
        let module_note = if m.server.is_placeholder() {
            " ⚠️ dev placeholder — use a tagged image for comparisons"
        } else {
            ""
        };
        let concurrency = if m.concurrency.is_empty() {
            "1".to_string()
        } else {
            m.concurrency
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };

        out.push_str("| field | value |\n|---|---|\n");
        out.push_str(&format!("| tool | v{} |\n", md_cell(&m.tool_version)));
        out.push_str(&format!(
            "| endpoint / graph | `{}` / `{}` |\n",
            md_cell(&m.endpoint),
            md_cell(&m.graph)
        ));
        out.push_str(&format!(
            "| FalkorDB module | {}{} |\n",
            module_ver, module_note
        ));
        if let Some(redis) = &m.server.redis_version {
            out.push_str(&format!("| redis | {} |\n", md_cell(redis)));
        }
        if let Some(cs) = m.server.cache_size {
            out.push_str(&format!("| CACHE_SIZE | {} |\n", cs));
        }
        if let Some(img) = &m.server.server_image {
            out.push_str(&format!("| server image | `{}` |\n", md_cell(img)));
        }
        // Client host: omit the hostname from the pasteable Markdown (it can be sensitive; it stays
        // in the JSON and console).
        let host_line = host_summary(&m.host, false);
        if !host_line.is_empty() {
            out.push_str(&format!("| client host | {} |\n", md_cell(&host_line)));
        }
        out.push_str(&format!("| samples / warmup | {} / {} |\n", m.samples, m.warmup));
        out.push_str(&format!("| concurrency | {} |\n", concurrency));
        out.push_str(&format!("| cache seed | {} |\n", m.seed));
        out.push_str(&format!("| connection | {} |\n", md_cell(&m.connection)));
        if let Some(d) = &m.dataset {
            out.push_str(&format!(
                "| dataset | seed {} · {} nodes · {} edges |\n",
                d.seed, d.nodes, d.edges
            ));
            out.push_str(&format!("| corpus_hash | `{}` |\n", md_cell(&d.corpus_hash)));
        }

        for (name, op) in &self.operations {
            out.push_str(&format!("\n## `{}`\n", name));
            render_op_levels_markdown(&mut out, op);
        }
        out
    }
}

/// Escape a value for a GitHub-flavoured Markdown **table cell**: an unescaped `|` ends the cell
/// (even inside a code span) and a newline breaks the row, so escape the former and fold the latter
/// to `<br>`. `\r` is dropped so a CRLF doesn't yield a doubled break.
fn md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\r', "").replace('\n', "<br>")
}

/// A one-line client-host summary (` · `-joined, skipping unknown fields). With `with_hostname`,
/// the hostname is prefixed — used for the local console but not the pasteable Markdown.
fn host_summary(
    h: &crate::synthetic::report::HostInfo,
    with_hostname: bool,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if with_hostname {
        if let Some(name) = &h.hostname {
            parts.push(name.clone());
        }
    }
    if let Some(os) = &h.os {
        parts.push(os.clone());
    }
    if let Some(cpu) = &h.cpu {
        let cores = match h.physical_cores {
            Some(p) => format!("{}c/{}t", p, h.logical_cores),
            None => format!("{}t", h.logical_cores),
        };
        parts.push(format!("{} ({})", cpu, cores));
    } else if h.logical_cores > 0 {
        parts.push(format!("{} threads", h.logical_cores));
    }
    if h.total_memory_bytes > 0 {
        parts.push(format!(
            "{:.1} GiB",
            h.total_memory_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
        ));
    }
    if let Some(arch) = &h.arch {
        parts.push(arch.clone());
    }
    parts.join(" · ")
}

/// One cache-plan condition a sweep is measured under. Selects the matching per-level metrics and
/// its label in each renderer, so the console and Markdown tables share one source of truth.
#[derive(Clone, Copy, PartialEq)]
enum CacheView {
    Cached,
    Uncached,
}

impl CacheView {
    fn pick(self, level: &LevelReport) -> Option<&LevelMetrics> {
        match self {
            CacheView::Cached => level.cached.as_ref(),
            CacheView::Uncached => level.uncached.as_ref(),
        }
    }

    fn console_label(self) -> &'static str {
        match self {
            CacheView::Cached => "cached — plan reused, execution only",
            CacheView::Uncached => "uncached — plan-cache miss each run, execution + compilation",
        }
    }

    fn markdown_label(self) -> &'static str {
        match self {
            CacheView::Cached => "cached — plan reused, execution only",
            CacheView::Uncached => "uncached — plan-cache miss each run, execution + compilation",
        }
    }
}

/// One rendered row of an operation's latency-vs-throughput table: the numbers both the console and
/// Markdown renderers format, extracted once so the two can't drift.
struct LevelRow {
    concurrency: usize,
    throughput: f64,
    /// server-time percentiles [p50, p90, p95, p99] (ms).
    server: [f64; 4],
    /// total round-trip percentiles [p50, p90, p95, p99] (ms).
    total: [f64; 4],
    miss_pct: f64,
    /// Number of samples whose cache flag was absent (a `0.0` miss% may just mean "unknown").
    cached_unknown: usize,
    /// Whether this level is the sweep's saturation knee (highest achieved throughput across modes).
    is_knee: bool,
}

/// The `(cache mode, rows)` tables present for an operation, cached before uncached. A mode with no
/// measured level is omitted. The shared model behind both [`render_op_levels`] and
/// [`render_op_levels_markdown`].
fn op_mode_tables(op: &OperationReport) -> Vec<(CacheView, Vec<LevelRow>)> {
    let knee = op.knee_concurrency();
    [CacheView::Cached, CacheView::Uncached]
        .into_iter()
        .filter_map(|mode| {
            let rows: Vec<LevelRow> = op
                .levels
                .iter()
                .filter_map(|level| {
                    let m = mode.pick(level)?;
                    let s = &m.metrics.server_ms;
                    let t = &m.metrics.total_ms;
                    Some(LevelRow {
                        concurrency: level.concurrency,
                        throughput: m.throughput_ops_per_sec,
                        server: [s.median, s.p90, s.p95, s.p99],
                        total: [t.median, t.p90, t.p95, t.p99],
                        miss_pct: m.metrics.cached_false_rate * 100.0,
                        cached_unknown: m.metrics.cached_unknown,
                        is_knee: knee == Some(level.concurrency),
                    })
                })
                .collect();
            (!rows.is_empty()).then_some((mode, rows))
        })
        .collect()
}

/// Render one operation's concurrency sweep as a latency-vs-throughput table per cache mode,
/// followed by the derived per-level compilation cost. The highest-throughput level (the
/// saturation "knee") is flagged.
fn render_op_levels(
    out: &mut String,
    op: &OperationReport,
) {
    for (mode, rows) in op_mode_tables(op) {
        out.push_str(&format!("  [{}]\n", mode.console_label()));
        out.push_str(
            "    C    throughput(ops/s)   server p50/p90/p95/p99             total p50/p90/p95/p99              miss%\n",
        );
        for r in rows {
            out.push_str(&format!(
                "  {:>3}   {:>15.0}   {:>7.3} /{:>6.3} /{:>6.3} /{:>6.3}   {:>7.3} /{:>6.3} /{:>6.3} /{:>6.3}   {:>5.1}{}\n",
                r.concurrency,
                r.throughput,
                r.server[0],
                r.server[1],
                r.server[2],
                r.server[3],
                r.total[0],
                r.total[1],
                r.total[2],
                r.total[3],
                r.miss_pct,
                if r.is_knee { "  <- knee" } else { "" },
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

/// Render one operation's sweep as GitHub-flavoured Markdown tables (one per cache mode) plus a
/// compilation-cost table, sharing [`op_mode_tables`] with the console renderer.
fn render_op_levels_markdown(
    out: &mut String,
    op: &OperationReport,
) {
    for (mode, rows) in op_mode_tables(op) {
        // Surface "unknown cache stats" so a 0.0 miss% isn't mistaken for "definitely cached".
        let any_unknown = rows.iter().any(|r| r.cached_unknown > 0);
        out.push_str(&format!("\n_{}_\n\n", mode.markdown_label()));
        out.push_str(
            "| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |\n",
        );
        out.push_str("|---:|---:|---|---|---:|---|\n");
        for r in rows {
            out.push_str(&format!(
                "| {} | {:.0} | {:.3} / {:.3} / {:.3} / {:.3} | {:.3} / {:.3} / {:.3} / {:.3} | {}{:.1} | {} |\n",
                r.concurrency,
                r.throughput,
                r.server[0],
                r.server[1],
                r.server[2],
                r.server[3],
                r.total[0],
                r.total[1],
                r.total[2],
                r.total[3],
                if r.cached_unknown > 0 { "~" } else { "" },
                r.miss_pct,
                if r.is_knee { "⬅ knee" } else { "" },
            ));
        }
        if any_unknown {
            out.push_str(
                "\n> `~` some samples reported no cache stat (counted separately); the miss% is computed over the samples with a known cache stat only.\n",
            );
        }
    }
    if op.levels.iter().any(|l| l.compilation_ms_median.is_some()) {
        out.push_str("\ncompilation_ms (median uncached − cached server time):\n\n");
        out.push_str("| C | compilation_ms |\n|---:|---:|\n");
        for level in &op.levels {
            if let Some(comp) = level.compilation_ms_median {
                out.push_str(&format!("| {} | {:.3} |\n", level.concurrency, comp));
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
                result_digest: None,
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
                host: HostInfo {
                    hostname: Some("bench-host".to_string()),
                    os: Some("Linux 6.8 Ubuntu 24.04".to_string()),
                    kernel: Some("6.8.0-40-generic".to_string()),
                    arch: Some("aarch64".to_string()),
                    cpu: Some("Test CPU @ 3.2GHz".to_string()),
                    physical_cores: Some(8),
                    logical_cores: 16,
                    total_memory_bytes: 34_359_738_368,
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
    fn markdown_contains_metadata_and_tables() {
        let mut r = sample_report();
        r.meta.server.server_image = Some("falkordb/falkordb:v4.2.1@sha256:deadbeef".to_string());
        let md = r.to_markdown();
        assert!(md.contains("# Synthetic per-operation benchmark"));
        assert!(md.contains("| FalkorDB module | 4.20.1 |"));
        assert!(md.contains("| server image | `falkordb/falkordb:v4.2.1@sha256:deadbeef` |"));
        assert!(md.contains("| concurrency | 1, 8 |"));
        // The client-host summary is present, but the hostname is NOT (kept out of pasteable md).
        assert!(md.contains("Test CPU @ 3.2GHz (8c/16t)"));
        assert!(
            !md.contains("bench-host"),
            "hostname must stay out of the markdown"
        );
        // Per-op section, markdown table header, both cache modes, knee, and compilation table.
        assert!(md.contains("## `return_const`"));
        assert!(md.contains("| C | throughput (ops/s) |"));
        assert!(md.contains("cached — plan reused"));
        assert!(md.contains("uncached — plan-cache miss"));
        assert!(md.contains("⬅ knee"));
        assert!(md.contains("compilation_ms"));
    }

    #[test]
    fn markdown_and_console_agree_on_numbers() {
        // The shared row model means a level's throughput renders identically in both surfaces.
        let r = sample_report();
        let (console, md) = (r.to_console(), r.to_markdown());
        for needle in ["2950", "return_const"] {
            assert!(
                console.contains(needle) && md.contains(needle),
                "both surfaces must render {needle}"
            );
        }
    }

    #[test]
    fn host_summary_hides_hostname_unless_requested() {
        let h = HostInfo {
            hostname: Some("secret-box".to_string()),
            os: Some("Linux 6.8".to_string()),
            kernel: None,
            arch: Some("x86_64".to_string()),
            cpu: Some("CPU X".to_string()),
            physical_cores: Some(4),
            logical_cores: 8,
            total_memory_bytes: 16 * 1024 * 1024 * 1024,
        };
        let with = host_summary(&h, true);
        let without = host_summary(&h, false);
        assert!(with.contains("secret-box"));
        assert!(!without.contains("secret-box"), "hostname omitted when not requested");
        assert!(without.contains("CPU X (4c/8t)"));
        assert!(without.contains("16.0 GiB"));
        assert!(without.contains("x86_64"));
    }

    #[test]
    fn host_summary_without_cpu_reports_thread_count() {
        let h = HostInfo {
            cpu: None,
            logical_cores: 4,
            total_memory_bytes: 8 * 1024 * 1024 * 1024,
            ..Default::default()
        };
        let s = host_summary(&h, false);
        assert!(s.contains("4 threads"), "falls back to a thread count: {s}");
        assert!(s.contains("8.0 GiB"));
    }

    #[test]
    fn host_summary_cpu_without_physical_cores_shows_threads_only() {
        let h = HostInfo {
            cpu: Some("CPU Y".to_string()),
            physical_cores: None,
            logical_cores: 6,
            total_memory_bytes: 4 * 1024 * 1024 * 1024,
            ..Default::default()
        };
        assert!(host_summary(&h, false).contains("CPU Y (6t)"));
    }

    #[test]
    fn markdown_flags_placeholder_version_and_default_concurrency() {
        let mut r = sample_report();
        r.meta.server.module_graph_ver = Some(ServerInfo::PLACEHOLDER_VER);
        r.meta.concurrency.clear(); // no sweep configured ⇒ header shows the implicit single level
        let md = r.to_markdown();
        assert!(md.contains("dev placeholder"));
        assert!(md.contains("| concurrency | 1 |"));
    }

    #[test]
    fn markdown_escapes_pipe_and_newline_in_cells() {
        let mut r = sample_report();
        r.meta.graph = "a|b\nc".to_string();
        r.meta.endpoint = "falkor://h|x".to_string();
        let md = r.to_markdown();
        // A raw `|`/newline in a user-provided value must not leak into the table (it would break
        // the row/columns); it is escaped to `\|` and folded to `<br>`.
        assert!(md.contains("a\\|b<br>c"), "graph pipe/newline escaped: {md}");
        assert!(md.contains("falkor://h\\|x"));
        assert!(!md.contains("a|b"), "no raw pipe survives in a cell");
    }

    #[test]
    fn markdown_renders_dataset_and_unknown_cache_marker() {
        let mut r = sample_report();
        r.meta.dataset = Some(DatasetInfo {
            seed: 7,
            nodes: 100,
            edges: 200,
            corpus_hash: "sha256:abc123".to_string(),
        });
        // Force an "unknown cache stat" sample so the `~` marker + note render.
        if let Some(op) = r.operations.get_mut("return_const") {
            if let Some(lm) = op.levels[0].cached.as_mut() {
                lm.metrics.cached_unknown = 3;
            }
        }
        let md = r.to_markdown();
        assert!(md.contains("| dataset | seed 7 · 100 nodes · 200 edges |"));
        assert!(md.contains("| corpus_hash | `sha256:abc123` |"));
        assert!(md.contains('~'), "unknown-cache miss% is marked with ~");
        assert!(md.contains("known cache stat only"));
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
                result_digest: None,
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
                result_digest: None,
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
                result_digest: None,
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
