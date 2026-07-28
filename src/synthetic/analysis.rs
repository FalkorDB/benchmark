//! The **regression analysis model**: one serializable [`RegressionAnalysis`] built once per
//! two-report comparison, from which every consumer renders — the Markdown regression report
//! ([`crate::synthetic::diff::regression_markdown`]), the compact PR summary
//! ([`crate::synthetic::diff::summarize`]) and the machine-readable cells file
//! (`report --cells`). Because all three consume the same in-memory value, they can never
//! disagree (design §A1 of `synthetic-three-way-report.md`).

use crate::synthetic::baseline::RegressionGuard;
use crate::synthetic::provenance::decode_module_version;
use crate::synthetic::report::{LevelMetrics, LevelReport, Report};
use crate::synthetic::shapes::shape_tier;
use crate::synthetic::thresholds::{
    BudgetProfile, ResolvedBudget, Thresholds, ThresholdsEcho, Verdict,
};
use crate::synthetic::{OpName, Tier};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Schema version of the serialized [`RegressionAnalysis`] (`report --cells`), bumped on any
/// breaking field change. v2 (design Phase 6 §3.5): adds the `skipped` [`OpOutcome`] value and
/// [`OutcomeCounts`] bucket plus the per-op `skipped_baseline`/`skipped_candidate` reasons —
/// a v1 consumer parsing `op_outcome` exhaustively would reject `"skipped"`, so this is a bump,
/// not a silent extension.
pub const ANALYSIS_SCHEMA_VERSION: u32 = 2;

/// The gated metric id carried in [`RegressionAnalysis::gated_metric`] under the opt-in
/// [`GatedMetric::TotalMs`]: the total-latency median is gated; every other metric is
/// informational context.
pub const GATED_METRIC: &str = "total_ms.p50";

/// The gated metric id carried in [`RegressionAnalysis::gated_metric`] under the default
/// [`GatedMetric::ServerMs`]: the server-reported execution-time median is gated.
pub const SERVER_GATED_METRIC: &str = "server_ms.p50";

/// Which latency metric's **median** drives the per-cell budget verdicts
/// (`report --regression --gated-metric`). The budget is applied to this metric's p50 pair; the
/// tails/throughput `context:` line follows the same metric so a cell never mixes clocks.
/// Client scheduling + network jitter pollutes `total_ms`; gating on the server-reported
/// execution time removes that noise entirely, which is why **server-ms is the default gate**
/// (maintainer decision: only the server execution time is measured; the total round trip is an
/// explicit opt-in). There is **never** a silent fallback: a cell whose
/// selected median is missing/invalid on either side gets a [`Verdict::NotApplicable`] and the op
/// is named in one advisory warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GatedMetric {
    /// Client-observed total latency (`total_ms.p50`) — explicit opt-in escape hatch.
    TotalMs,
    /// Server-reported execution time (`server_ms.p50`) — the default gate.
    #[default]
    ServerMs,
}

impl GatedMetric {
    /// The stable lowercase id used in the `--gated-metric` flag.
    pub fn as_str(self) -> &'static str {
        match self {
            GatedMetric::TotalMs => "total-ms",
            GatedMetric::ServerMs => "server-ms",
        }
    }

    /// The metric id carried in [`RegressionAnalysis::gated_metric`] and echoed by every renderer.
    pub fn id(self) -> &'static str {
        match self {
            GatedMetric::TotalMs => GATED_METRIC,
            GatedMetric::ServerMs => SERVER_GATED_METRIC,
        }
    }

    /// The selected metric's [`Summary`] within one measured cell.
    fn summary(
        self,
        m: &LevelMetrics,
    ) -> &crate::synthetic::stats::Summary {
        match self {
            GatedMetric::TotalMs => &m.metrics.total_ms,
            GatedMetric::ServerMs => &m.metrics.server_ms,
        }
    }
}

impl std::str::FromStr for GatedMetric {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "total-ms" => Ok(GatedMetric::TotalMs),
            "server-ms" => Ok(GatedMetric::ServerMs),
            other => Err(format!(
                "unknown gated metric '{other}' (expected 'total-ms' or 'server-ms')"
            )),
        }
    }
}

/// How a diverged op (differing per-op `result_digest`s) affects the verdicts (design §A3).
/// Under **both** policies a diverged op's perf cells are N/A — diverged results mean the two
/// sides did different work, so a latency comparison would be meaningless; only the severity of
/// the correctness signal differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DivergencePolicy {
    /// Same-engine (default, today's behavior): a diverged op is a 🔴 correctness failure and any
    /// divergence makes the overall verdict [`OverallVerdict::Regressed`].
    #[default]
    Gate,
    /// Cross-engine: a diverged op is a ⚠ [`OpOutcome::DivergedAdvisory`]; divergences count in
    /// [`OutcomeCounts::diverged`] (never in `regressed`) and cap the overall verdict at
    /// [`OverallVerdict::Advisory`].
    Advisory,
}

impl DivergencePolicy {
    /// The stable lowercase id used in flags, JSON and reports.
    pub fn as_str(self) -> &'static str {
        match self {
            DivergencePolicy::Gate => "gate",
            DivergencePolicy::Advisory => "advisory",
        }
    }
}

impl std::str::FromStr for DivergencePolicy {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "gate" => Ok(DivergencePolicy::Gate),
            "advisory" => Ok(DivergencePolicy::Advisory),
            other => Err(format!(
                "unknown divergence policy '{other}' (expected 'gate' or 'advisory')"
            )),
        }
    }
}

/// Per-op result-correctness state, from the two runs' `result_digest`s: both present and equal =
/// [`Match`](Self::Match); present but different, **or only one side recorded a digest** =
/// [`Diverged`](Self::Diverged); both absent = [`NotGated`](Self::NotGated) (comparable and timed,
/// but no correctness claim — never counted as diverged).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Correctness {
    Match,
    Diverged,
    NotGated,
}

/// Per-op outcome — the collapsed-row verdict, rolled up across every cache mode and concurrency
/// (worst cell wins: `Regressed` > `DivergedAdvisory` > `Pass` > `NotApplicable`; a `Skipped` op
/// never has a comparable cell, so it never competes with the others).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpOutcome {
    /// ≥1 comparable p50 cell and none over budget (🟢).
    Pass,
    /// ≥1 p50 cell over budget, or (under the `gate` policy) diverged results (🔴).
    Regressed,
    /// Results diverged under the `advisory` policy — needs a human look, not a gate (⚠).
    DivergedAdvisory,
    /// Capability-skipped on ≥1 side (design Phase 6 §3.5) — recorded but never executed there
    /// because the engine lacks the op's required procedure. Neither a pass nor a divergence, and
    /// never a gate (⏭).
    Skipped,
    /// No evaluable cell and no divergence signal — no verdict (N/A).
    NotApplicable,
}

impl OpOutcome {
    /// The marker shown on the op's collapsed row (`🟢`/`🔴`/`⚠`/`⏭`/`N/A`).
    pub fn emoji(self) -> &'static str {
        match self {
            OpOutcome::Pass => "🟢",
            OpOutcome::Regressed => "🔴",
            OpOutcome::DivergedAdvisory => "⚠",
            OpOutcome::Skipped => "⏭",
            OpOutcome::NotApplicable => "N/A",
        }
    }
}

/// A 🟢 / 🔴 / ⚠ / ⏭ / N-A tally of per-op outcomes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeCounts {
    pub pass: usize,
    pub regressed: usize,
    /// Ops diverged under the `advisory` policy ([`OpOutcome::DivergedAdvisory`]). Always 0 under
    /// the `gate` policy, where a divergence counts in `regressed`.
    pub diverged: usize,
    /// Ops capability-skipped on ≥1 side ([`OpOutcome::Skipped`]) — under **both** policies:
    /// a skip is an expected engine limitation, never a regression or a divergence.
    pub skipped: usize,
    pub not_applicable: usize,
}

impl OutcomeCounts {
    pub(crate) fn add(
        &mut self,
        outcome: OpOutcome,
    ) {
        match outcome {
            OpOutcome::Pass => self.pass += 1,
            OpOutcome::Regressed => self.regressed += 1,
            OpOutcome::DivergedAdvisory => self.diverged += 1,
            OpOutcome::Skipped => self.skipped += 1,
            OpOutcome::NotApplicable => self.not_applicable += 1,
        }
    }
}

/// The overall verdict of a comparison — the four-state rollup defined in design §A1:
///
/// 1. [`NotComparable`](Self::NotComparable) — the runs measured different workloads/configs;
///    nothing else counts.
/// 2. [`Regressed`](Self::Regressed) (🔴) — ≥1 regressed perf cell, **or** (under the `gate`
///    policy) ≥1 diverged op.
/// 3. [`Advisory`](Self::Advisory) (⚠) — not regressed, but something needs a human look: ≥1
///    diverged op under the `advisory` policy, **or** zero comparable perf cells anywhere
///    (an all-N/A / all-diverged run is never green).
/// 4. [`Pass`](Self::Pass) (🟢) — ≥1 comparable cell, no regression, no divergence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverallVerdict {
    Pass,
    Regressed,
    Advisory,
    NotComparable,
}

impl OverallVerdict {
    /// The emoji shown before the headline.
    pub fn emoji(self) -> &'static str {
        match self {
            OverallVerdict::Pass => "🟢",
            OverallVerdict::Regressed => "🔴",
            OverallVerdict::Advisory | OverallVerdict::NotComparable => "⚠",
        }
    }
}

/// Whether the two runs may be compared at all (the workload/config guard). Version mismatches
/// are advisory [`RegressionAnalysis::warnings`], never comparability guards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ComparisonStatus {
    Comparable,
    /// The workload/config guard failed: `workload_hash`, sampling (samples/warmup), the
    /// concurrency sweep, or a known server setting differs between the two runs.
    WorkloadMismatch { reason: String },
    /// The runs are config-comparable but share no operation name — nothing to compare.
    NoCommonOps { reason: String },
}

impl ComparisonStatus {
    /// The not-comparable reason, if any — `None` exactly when the runs are comparable.
    pub fn not_comparable_reason(&self) -> Option<&str> {
        match self {
            ComparisonStatus::Comparable => None,
            ComparisonStatus::WorkloadMismatch { reason }
            | ComparisonStatus::NoCommonOps { reason } => Some(reason),
        }
    }
}

/// Which cache mode a cell was measured in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheMode {
    Cached,
    Uncached,
}

impl CacheMode {
    /// The explanatory label rendered above each op × cache-mode table.
    pub fn label(self) -> &'static str {
        match self {
            CacheMode::Cached => "cached (plan reused — execution only)",
            CacheMode::Uncached => "uncached (forced plan-cache miss — execution + compilation)",
        }
    }

    fn pick(
        self,
        lvl: &LevelReport,
    ) -> Option<&LevelMetrics> {
        match self {
            CacheMode::Cached => lvl.cached.as_ref(),
            CacheMode::Uncached => lvl.uncached.as_ref(),
        }
    }
}

/// The identity of the comparison: the two runs' display labels and the stable slug CI uses to
/// host/link the full report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonIdentity {
    pub baseline_label: String,
    pub candidate_label: String,
    /// Stable, filesystem- and anchor-safe id for this comparison. The same pair of runs always
    /// yields the same slug.
    pub slug: String,
}

/// One side's run/config metadata, lifted from its [`Report`] so renderers never reach back into
/// the raw reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SideMeta {
    /// Decoded FalkorDB module version (e.g. `"4.20.1"`), `None` when unreadable.
    pub module_version: Option<String>,
    pub server_image: Option<String>,
    pub workload_hash: Option<String>,
    pub samples: usize,
    pub warmup: usize,
    /// The concurrency sweep this side measured.
    pub concurrency: Vec<usize>,
    /// §6.3 oracle attestation this side's report carries (`meta.oracle_verified`): op → number
    /// of recorded write outcomes the replay re-verified before measuring. `None` for read runs
    /// and latency-tier-only write runs. Additive optional field — absent from pre-§6.3 models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oracle_verified: Option<std::collections::BTreeMap<String, usize>>,
}

/// Header/config metadata the renderers need: both sides plus the threshold settings that were
/// applied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisMeta {
    pub baseline: SideMeta,
    pub candidate: SideMeta,
    pub thresholds: ThresholdsEcho,
}

/// One side's informational (never gated) tail/throughput context for a cell. The tails are read
/// from the **selected gated metric's** summary (server-ms by default, total-ms under
/// `--gated-metric total-ms`) so they always describe the same clock as the gated p50.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CellContextSide {
    pub p90_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub throughput_ops_per_sec: f64,
    /// The client-observed total-latency median (`total_ms.p50`), demoted to informational
    /// context under the default server-ms gate: the benchmark measures server execution time,
    /// so the wall clock stays visible but never drives a verdict. `None` under total-ms gating
    /// (the primary p50 already is that clock) or when the side has no valid total median.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_p50_ms: Option<f64>,
    /// Number of retained measured samples that actually carried a **server-reported execution
    /// time** — the cohort every dispersion stat (and the gated server p50) describes, counted
    /// after severe-outlier removal and pooled across the C workers. `None` when the side has no
    /// valid server time (a report predating server-time capture, or an engine that doesn't
    /// report execution time) — the same degradation rule as every server column. Accepts the
    /// transient v2.7 name `n_server` on deserialization (the same server-timed cohort) so cells
    /// JSON written by that release keeps its count.
    #[serde(default, alias = "n_server", skip_serializing_if = "Option::is_none")]
    pub server_n: Option<usize>,
    /// **Sample** standard deviation (n−1 denominator — see
    /// [`Summary::sample_stddev`](crate::synthetic::stats::Summary::sample_stddev)) of this
    /// side's retained `server_ms` samples, in ms: **within-run** dispersion (how spread this
    /// run's own samples are), not run-to-run noise. `None` when the side has no valid server
    /// time or fewer than 2 samples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_stddev_ms: Option<f64>,
    /// Coefficient of variation of `server_ms`, percent (`100·σ/mean`, sample σ) — within-run
    /// dispersion relative to the cell's own scale. `None` under the same conditions as
    /// [`server_stddev_ms`](Self::server_stddev_ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_cv_pct: Option<f64>,
}

/// Informational context for a cell (both sides optional — a side may not have measured it).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct CellContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<CellContextSide>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<CellContextSide>,
}

/// One op × cache-mode × concurrency cell: the gated p50 pair, its delta, the budget that was
/// applied and the resulting perf verdict, plus informational context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellAnalysis {
    pub concurrency: usize,
    pub cache_mode: CacheMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_p50_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_p50_ms: Option<f64>,
    /// `100·(candidate−baseline)/baseline`; present exactly when both p50s are valid (finite, >0)
    /// — i.e. exactly when a real verdict exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_pct: Option<f64>,
    /// `candidate − baseline` (ms); present under the same condition as `delta_pct`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_ms: Option<f64>,
    /// The budget resolved for this exact cell (per-op×concurrency > per-op > default).
    pub budget: ResolvedBudget,
    /// The p50 verdict. [`Verdict::NotApplicable`] for every cell of a diverged op (under both
    /// divergence policies) and whenever either p50 is missing/invalid.
    pub perf_verdict: Verdict,
    pub context: CellContext,
}

/// One op's analysis: correctness, the rolled-up outcome and every measured cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpAnalysis {
    /// Coverage tier (`"core"`/`"full"`) from the catalog or the recorded-shape registry (repo
    /// reads and algorithm shapes); `None` for names known to neither.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    pub correctness: Correctness,
    pub op_outcome: OpOutcome,
    /// Why the **baseline** run skipped this op (capability probe — design Phase 6 §3.5), or
    /// `None` when it measured it. A skipped side has no cells; the other side's raw numbers stay
    /// visible for context but no cell is comparable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_baseline: Option<String>,
    /// Why the **candidate** run skipped this op, or `None` when it measured it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_candidate: Option<String>,
    /// The op's deterministic representative Cypher text
    /// ([`OperationReport::example_query`](crate::synthetic::report::OperationReport::example_query)),
    /// preferring the **candidate** run's text (the run under test), falling back to the
    /// baseline's — a comparable pair measured the same workload, so the two are normally
    /// identical. `None` when neither report carries one (both predate the field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example_query: Option<String>,
    pub cells: Vec<CellAnalysis>,
}

/// The full analysis of one baseline→candidate comparison — the single source of truth every
/// consumer (Markdown report, PR summary, cells JSON, interactive page) renders from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegressionAnalysis {
    pub schema_version: u32,
    pub comparison: ComparisonIdentity,
    pub meta: AnalysisMeta,
    pub budget_profile: BudgetProfile,
    pub divergence_policy: DivergencePolicy,
    /// The gated metric id ([`GatedMetric::id`] — [`SERVER_GATED_METRIC`] by default,
    /// [`GATED_METRIC`] under the `--gated-metric total-ms` opt-in); everything else is
    /// informational.
    pub gated_metric: String,
    pub status: ComparisonStatus,
    /// Advisory notes (version/image), never comparability guards.
    pub warnings: Vec<String>,
    /// Total wall-clock seconds the caller spent computing the check, as passed via
    /// `--elapsed-secs`.
    pub elapsed_secs: Option<f64>,
    pub verdict: OverallVerdict,
    /// Per-op outcome tallies. Every op with ≥1 measured cell is tallied; a **diverged** op is
    /// tallied even with no cell (gate ⇒ `regressed`, advisory ⇒ `diverged` — every divergence
    /// counts), and so is a **skipped** op (every skip is visible). Only a cell-less non-diverged,
    /// non-skipped op has nothing to tally.
    pub totals: OutcomeCounts,
    /// Cells with a real verdict (🟢 or 🔴) across all ops; a diverged op's cells are N/A and
    /// never counted.
    pub comparable_cells: usize,
    /// Comparable cells whose p50 was over budget.
    pub regressed_cells: usize,
    pub ops: BTreeMap<String, OpAnalysis>,
}

/// The comparison-level knobs [`analyze`] needs beyond the two reports.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnalysisOptions {
    pub budget_profile: BudgetProfile,
    pub divergence_policy: DivergencePolicy,
    /// Which latency metric's median the budget verdicts gate on (default: total-ms).
    pub gated_metric: GatedMetric,
    pub elapsed_secs: Option<f64>,
}

impl RegressionAnalysis {
    /// The overall verdict as `(emoji, headline)` — the single wording both the Markdown report's
    /// top line and the summary's headline use, so they can never drift.
    pub fn verdict_line(&self) -> (&'static str, String) {
        let emoji = self.verdict.emoji();
        let diverged = self.diverged_ops().len();
        let mut headline = match self.verdict {
            OverallVerdict::NotComparable => {
                let reason = self.status.not_comparable_reason().unwrap_or("unknown reason");
                format!("not comparable — {reason}")
            }
            OverallVerdict::Regressed if self.regressed_cells > 0 => format!(
                "{} of {} comparable cell(s) over budget",
                self.regressed_cells, self.comparable_cells
            ),
            OverallVerdict::Regressed => format!(
                "no p50 regression beyond budget, but {diverged} op(s) have differing results \
                 (correctness)"
            ),
            OverallVerdict::Advisory if diverged > 0 => format!(
                "pass, {diverged} diverged — no p50 regression beyond budget across {} comparable \
                 cell(s); divergence is advisory under this policy",
                self.comparable_cells
            ),
            OverallVerdict::Advisory => {
                "no comparable cells — no cell had a valid p50 on both sides; nothing was gated"
                    .to_string()
            }
            OverallVerdict::Pass => format!(
                "no p50 regression beyond budget across {} comparable cell(s)",
                self.comparable_cells
            ),
        };
        // Skips are annotated, never gated (design Phase 6 §3.5) — visible on every comparable
        // verdict so a partially-supported engine can't silently shrink the compared set.
        if self.verdict != OverallVerdict::NotComparable && self.totals.skipped > 0 {
            headline.push_str(&format!(
                "; {} op(s) skipped (engine lacks their required procedure)",
                self.totals.skipped
            ));
        }
        (emoji, headline)
    }

    /// The sorted names of ops whose results diverged.
    pub fn diverged_ops(&self) -> Vec<&str> {
        self.ops
            .iter()
            .filter(|(_, o)| o.correctness == Correctness::Diverged)
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// Whether this comparison gated on the server-reported execution time
    /// (`--gated-metric server-ms`) rather than the default client-observed total latency.
    pub fn gated_on_server_ms(&self) -> bool {
        self.gated_metric == SERVER_GATED_METRIC
    }

    /// Pretty-printed JSON — the `report --cells` artifact. Fallible so a serialization failure
    /// surfaces loudly instead of writing a misleading empty object.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

/// Coverage [`Tier`] for an op key of either kind: legacy catalog tags resolve via
/// [`OpName::from_tag`]; dynamic (string-keyed) recorded shapes — repo reads and algorithm shapes
/// alike — via the shape registry ([`shape_tier`]); names known to neither have no tier.
pub(crate) fn op_tier(op: &str) -> Option<Tier> {
    OpName::from_tag(op).map(OpName::tier).or_else(|| shape_tier(op))
}

/// The display name for a run's column: its `--label` if set, else the caller-supplied `fallback`.
fn col_label(
    r: &Report,
    fallback: &str,
) -> String {
    r.meta.label.clone().unwrap_or_else(|| fallback.to_string())
}

fn side_meta(r: &Report) -> SideMeta {
    SideMeta {
        module_version: r.meta.server.module_graph_ver.map(decode_module_version),
        server_image: r.meta.server.server_image.clone(),
        workload_hash: r.meta.dataset.as_ref().map(|d| d.workload_hash.clone()),
        samples: r.meta.samples,
        warmup: r.meta.warmup,
        concurrency: r.meta.concurrency.clone(),
        oracle_verified: r.meta.oracle_verified.clone(),
    }
}

/// Lowercase, hyphenate and trim `s` into an anchor/filesystem-safe slug fragment (runs of
/// non-alphanumerics collapse to a single `-`; empty input becomes `run`).
fn slugify(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("run");
    }
    out
}

/// A stable slug identifying this comparison, so CI can host the full report at a predictable path
/// and link it from the summary. Derived from the run labels and the shared `workload_hash` digest,
/// so the same pair of runs always produces the same slug.
fn summary_slug(
    candidate: &Report,
    baseline: &Report,
    candidate_label: &str,
    baseline_label: &str,
) -> String {
    let hash = candidate
        .meta
        .dataset
        .as_ref()
        .or(baseline.meta.dataset.as_ref())
        .map(|d| d.workload_hash.as_str())
        .unwrap_or("nohash");
    // Keep the digest, not the algorithm prefix (e.g. `sha256:abcdef…` → `abcdef…`).
    let digest: String = hash
        .rsplit(':')
        .next()
        .unwrap_or(hash)
        .chars()
        .take(12)
        .collect();
    format!(
        "synthetic-{}-vs-{}-{}",
        slugify(candidate_label),
        slugify(baseline_label),
        slugify(&digest)
    )
}

/// The [`LevelMetrics`] for `op` at concurrency `c` in `mode`, if present.
fn level_metrics<'a>(
    report: &'a Report,
    op: &str,
    c: usize,
    mode: CacheMode,
) -> Option<&'a LevelMetrics> {
    report
        .operations
        .get(op)?
        .levels
        .iter()
        .find(|lvl| lvl.concurrency == c)
        .and_then(|lvl| mode.pick(lvl))
}

/// One side's informational tails/throughput, read from the **selected gated metric's** summary
/// so a cell's primary p50 and its `context:` line always describe the same clock — plus the
/// within-run dispersion stats (`server_n`, sample σ, CV%), all computed on **`server_ms`**:
/// the distribution whose p50 is gated. Field names carry the clock so they stay unambiguous.
fn context_side(
    metric: GatedMetric,
    m: &LevelMetrics,
) -> CellContextSide {
    let s = metric.summary(m);
    // Under the server-ms gate the wall clock rides along as demoted, informational context;
    // under total-ms gating the primary p50 already is the wall clock, so nothing is duplicated.
    let total = m.metrics.total_ms.median;
    let total_p50_ms = (metric == GatedMetric::ServerMs && total.is_finite() && total > 0.0)
        .then_some(total);
    // Server-clock stats degrade exactly like every other server column: absent whenever the
    // side has no valid server time (same finite-and-positive-median rule as the p50 columns).
    let server = &m.metrics.server_ms;
    let server_valid = server.median.is_finite() && server.median > 0.0;
    // Derived dispersion stats are rounded to 6 decimals: sub-nanosecond precision is noise,
    // and short decimals keep the cells JSON compact and byte-stable across serde round-trips.
    let round6 = |v: Option<f64>| v.map(|x| (x * 1e6).round() / 1e6);
    CellContextSide {
        p90_ms: s.p90,
        p95_ms: s.p95,
        p99_ms: s.p99,
        throughput_ops_per_sec: m.throughput_ops_per_sec,
        total_p50_ms,
        server_n: server_valid.then_some(server.n),
        server_stddev_ms: round6(if server_valid { server.sample_stddev() } else { None }),
        server_cv_pct: round6(if server_valid { server.cv_pct() } else { None }),
    }
}

/// Build the analysis for one op: its cells across both cache modes (rows = the union of
/// concurrency levels present in either run, exactly like the rendered tables), correctness and
/// the rolled-up outcome.
fn analyze_op(
    baseline: &Report,
    candidate: &Report,
    op: &str,
    diverged: bool,
    thresholds: &Thresholds,
    policy: DivergencePolicy,
    metric: GatedMetric,
) -> OpAnalysis {
    // Skip reasons (capability probe — design Phase 6 §3.5), per side. A skipped side recorded no
    // levels, so its cells are one-sided; forcing the verdict N/A below also covers a hand-crafted
    // report that pairs a skip reason with levels.
    let skipped_side = |rep: &Report| rep.operations.get(op).and_then(|o| o.skipped.clone());
    let skipped_baseline = skipped_side(baseline);
    let skipped_candidate = skipped_side(candidate);
    let skipped = skipped_baseline.is_some() || skipped_candidate.is_some();

    let mut cells = Vec::new();
    for mode in [CacheMode::Cached, CacheMode::Uncached] {
        let mut levels: BTreeSet<usize> = BTreeSet::new();
        for rep in [baseline, candidate] {
            if let Some(opr) = rep.operations.get(op) {
                for lvl in &opr.levels {
                    if mode.pick(lvl).is_some() {
                        levels.insert(lvl.concurrency);
                    }
                }
            }
        }
        for c in levels {
            let am = level_metrics(baseline, op, c, mode);
            let bm = level_metrics(candidate, op, c, mode);
            let ap = am.map(|m| metric.summary(m).median);
            let bp = bm.map(|m| metric.summary(m).median);
            let budget = thresholds.resolve_by_name(op, c);
            // Perf verdict: N/A for every cell of a diverged op (different work ⇒ a latency
            // comparison is meaningless, under BOTH policies) and of a skipped op (one side never
            // executed it), else the budget rule.
            let perf_verdict = if diverged || skipped {
                Verdict::NotApplicable
            } else {
                match (ap, bp) {
                    (Some(x), Some(y)) => budget.verdict(x, y),
                    _ => Verdict::NotApplicable,
                }
            };
            // The gated delta exists exactly when both p50s are valid (finite, > 0) — for a
            // diverged op it stays visible for diagnosis even though the verdict is N/A.
            let (delta_pct, delta_ms) = match (ap, bp) {
                (Some(x), Some(y)) if x.is_finite() && x > 0.0 && y.is_finite() && y > 0.0 => {
                    (Some((y - x) / x * 100.0), Some(y - x))
                }
                _ => (None, None),
            };
            cells.push(CellAnalysis {
                concurrency: c,
                cache_mode: mode,
                baseline_p50_ms: ap,
                candidate_p50_ms: bp,
                delta_pct,
                delta_ms,
                budget,
                perf_verdict,
                context: CellContext {
                    baseline: am.map(|m| context_side(metric, m)),
                    candidate: bm.map(|m| context_side(metric, m)),
                },
            });
        }
    }

    let correctness = if diverged {
        Correctness::Diverged
    } else if skipped {
        // Skipped on ≥1 side ⇒ that side never ran ⇒ no correctness claim, even when the measured
        // side recorded a digest.
        Correctness::NotGated
    } else {
        // Non-diverged ⇒ the digests either match on both sides or are absent on both.
        let has_digest = baseline
            .operations
            .get(op)
            .and_then(|o| o.result_digest.as_ref())
            .is_some();
        if has_digest { Correctness::Match } else { Correctness::NotGated }
    };

    // Worst cell wins: Regressed > DivergedAdvisory > Pass > NotApplicable. A diverged op's cells
    // are all N/A, so divergence and a cell regression are mutually exclusive. A skip on either
    // side outranks everything: the op was never executed there, so no comparison exists — it is
    // neither a pass nor a divergence, and never a gate.
    let op_outcome = if skipped {
        OpOutcome::Skipped
    } else if diverged {
        match policy {
            DivergencePolicy::Gate => OpOutcome::Regressed,
            DivergencePolicy::Advisory => OpOutcome::DivergedAdvisory,
        }
    } else if cells.iter().any(|c| c.perf_verdict == Verdict::Regressed) {
        OpOutcome::Regressed
    } else if cells.iter().any(|c| c.perf_verdict == Verdict::Ok) {
        OpOutcome::Pass
    } else {
        OpOutcome::NotApplicable
    };

    OpAnalysis {
        tier: op_tier(op).map(|t| t.as_str().to_string()),
        correctness,
        op_outcome,
        skipped_baseline,
        skipped_candidate,
        // Candidate first (the run under test), baseline as the fallback — deterministic, and
        // normally identical on both sides of a comparable pair (same workload_hash).
        example_query: [candidate, baseline]
            .iter()
            .find_map(|r| r.operations.get(op).and_then(|o| o.example_query.clone())),
        cells,
    }
}

/// Build the [`RegressionAnalysis`] for `baseline` → `candidate` under `guard`, `thresholds` and
/// the comparison `options`. This is the **only** place verdicts are computed; every renderer
/// consumes the returned model.
pub fn analyze(
    baseline: &Report,
    candidate: &Report,
    guard: &RegressionGuard,
    thresholds: &Thresholds,
    options: &AnalysisOptions,
) -> RegressionAnalysis {
    let baseline_label = col_label(baseline, "baseline");
    let candidate_label = col_label(candidate, "candidate");
    let slug = summary_slug(candidate, baseline, &candidate_label, &baseline_label);
    let comparison = ComparisonIdentity { baseline_label, candidate_label, slug };
    let meta = AnalysisMeta {
        baseline: side_meta(baseline),
        candidate: side_meta(candidate),
        thresholds: thresholds.echo(),
    };
    let base = RegressionAnalysis {
        schema_version: ANALYSIS_SCHEMA_VERSION,
        comparison,
        meta,
        budget_profile: options.budget_profile,
        divergence_policy: options.divergence_policy,
        gated_metric: options.gated_metric.id().to_string(),
        status: ComparisonStatus::Comparable,
        warnings: Vec::new(),
        elapsed_secs: options.elapsed_secs,
        verdict: OverallVerdict::NotComparable,
        totals: OutcomeCounts::default(),
        comparable_cells: 0,
        regressed_cells: 0,
        ops: BTreeMap::new(),
    };

    let (diverged, mut warnings) = match guard {
        RegressionGuard::NotComparable { reason } => {
            return RegressionAnalysis {
                status: ComparisonStatus::WorkloadMismatch { reason: reason.clone() },
                ..base
            };
        }
        RegressionGuard::Comparable { diverged_ops, warnings } => {
            (diverged_ops, warnings.clone())
        }
    };

    // Configs match, but a comparison still needs at least one op name present on both sides.
    if !baseline.operations.keys().any(|op| candidate.operations.contains_key(op)) {
        return RegressionAnalysis {
            status: ComparisonStatus::NoCommonOps {
                reason: "the two runs share no operation name — there is nothing to compare"
                    .to_string(),
            },
            warnings,
            ..base
        };
    }

    let mut ops = BTreeMap::new();
    let mut totals = OutcomeCounts::default();
    let mut comparable_cells = 0usize;
    let mut regressed_cells = 0usize;
    // Ops whose selected metric was missing/invalid on ≥1 side of an otherwise-measured cell —
    // surfaced as one advisory warning under server-ms gating (never a fallback to total_ms).
    let mut metric_degraded: Vec<String> = Vec::new();
    let op_names: BTreeSet<&String> = baseline
        .operations
        .keys()
        .chain(candidate.operations.keys())
        .collect();
    for op in op_names {
        let oa = analyze_op(
            baseline,
            candidate,
            op,
            diverged.contains(op),
            thresholds,
            options.divergence_policy,
            options.gated_metric,
        );
        // Missing-data guard for the selectable metric: a cell measured on BOTH sides whose
        // server_ms median is missing/invalid (≤ 0 / non-finite — a report predating server-time
        // capture, or an engine that doesn't report execution time) is N/A, and the op is named
        // in one loud warning below — this now triggers by default, since server-ms is the
        // default gate. `total_ms` is the always-captured wall clock, so the opt-in total-ms
        // gating never degrades (no warning). Diverged/skipped ops are already N/A with their
        // own, louder markers.
        if options.gated_metric == GatedMetric::ServerMs
            && oa.correctness != Correctness::Diverged
            && oa.skipped_baseline.is_none()
            && oa.skipped_candidate.is_none()
            && oa.cells.iter().any(|c| {
                matches!(
                    (c.baseline_p50_ms, c.candidate_p50_ms),
                    (Some(x), Some(y)) if !(x.is_finite() && x > 0.0 && y.is_finite() && y > 0.0)
                )
            })
        {
            metric_degraded.push(op.clone());
        }
        for cell in &oa.cells {
            match cell.perf_verdict {
                Verdict::Regressed => {
                    regressed_cells += 1;
                    comparable_cells += 1;
                }
                Verdict::Ok => comparable_cells += 1,
                Verdict::NotApplicable => {}
            }
        }
        // Every op with a measured cell is tallied — and so is a cell-less **diverged** op
        // (gate ⇒ `regressed`, advisory ⇒ `diverged`): every divergence counts in the outcome
        // table. A **skipped** op is always tallied too (every skip is visible — design Phase 6
        // §3.5), even when the measured side contributed no cell. Only a cell-less non-diverged,
        // non-skipped op has nothing to tally.
        if !oa.cells.is_empty()
            || oa.correctness == Correctness::Diverged
            || oa.op_outcome == OpOutcome::Skipped
        {
            totals.add(oa.op_outcome);
        }
        ops.insert(op.clone(), oa);
    }

    let any_diverged = ops
        .values()
        .any(|o: &OpAnalysis| o.correctness == Correctness::Diverged);
    // The four-state aggregation rule (design §A1) — see [`OverallVerdict`].
    let verdict = if regressed_cells > 0
        || (options.divergence_policy == DivergencePolicy::Gate && any_diverged)
    {
        OverallVerdict::Regressed
    } else if any_diverged || comparable_cells == 0 {
        OverallVerdict::Advisory
    } else {
        OverallVerdict::Pass
    };

    if !metric_degraded.is_empty() {
        warnings.push(format!(
            "gated metric {SERVER_GATED_METRIC} (the default gate) is missing/invalid on ≥1 side \
             for {} op(s): {} — those cells have NO verdict (N/A); the report predates \
             server-time capture or the engine does not report execution time. There is no \
             silent fallback to total_ms — re-run with `--gated-metric total-ms` to gate on the \
             client-observed total latency instead",
            metric_degraded.len(),
            metric_degraded.join(", ")
        ));
    }

    RegressionAnalysis {
        warnings,
        verdict,
        totals,
        comparable_cells,
        regressed_cells,
        ops,
        ..base
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::baseline::regression_guard;
    use crate::synthetic::report::{
        DatasetInfo, Meta, MetricSet, OperationReport, ServerInfo,
    };
    use crate::synthetic::stats::Summary;

    fn summ(median: f64) -> Summary {
        Summary {
            n: 100,
            removed: 0,
            min: median * 0.9,
            mean: median,
            median,
            p90: median * 1.2,
            p95: median * 1.3,
            p99: median * 1.5,
            max: median * 2.0,
            stddev: median * 0.1,
        }
    }

    fn metrics(median: f64) -> LevelMetrics {
        LevelMetrics {
            throughput_ops_per_sec: 1000.0,
            metrics: MetricSet {
                // Both clocks carry the same medians so metric-agnostic tests behave identically
                // under either gate; metric-disagreement tests override via `set_server_median`.
                server_ms: summ(median),
                total_ms: summ(median),
                non_internal_ms: summ(median * 0.8),
                cached_false_rate: 0.0,
                cached_unknown: 0,
            },
        }
    }

    /// Build a report with one cached level (concurrency 1) per `(op_tag, p50_median, digest)`;
    /// a `None` digest leaves the op's correctness ungated.
    fn rpt(
        label: &str,
        ver: u64,
        ops: &[(&str, f64, Option<&str>)],
    ) -> Report {
        let mut operations = BTreeMap::new();
        for (tag, median, digest) in ops {
            operations.insert(
                (*tag).to_string(),
                OperationReport {
                    levels: vec![LevelReport {
                        concurrency: 1,
                        cached: Some(metrics(*median)),
                        uncached: None,
                        compilation_ms_median: None,
                    }],
                    result_digest: digest.map(str::to_string),
                    policy: None,
                    skipped: None,
                    example_query: None,
                },
            );
        }
        Report {
            schema_version: 2,
            meta: Meta {
                tool_version: "0.1.0".to_string(),
                endpoint: "e".to_string(),
                graph: "g".to_string(),
                samples: 1000,
                warmup: 200,
                concurrency: vec![1],
                seed: 0,
                corpus_size: 256,
                server_timeout_ms: 5000,
                client_deadline_ms: 6000,
                connection: "c".to_string(),
                started_at_epoch_secs: 0,
                server: ServerInfo { module_graph_ver: Some(ver), ..Default::default() },
                host: Default::default(),
                dataset: Some(DatasetInfo {
                    seed: 0,
                    nodes: 10,
                    edges: 20,
                    workload_hash: "sha256:abc".to_string(),
                }),
                label: Some(label.to_string()),
                oracle_verified: None,
            },
            operations,
        }
    }

    fn gate(a: &Report, b: &Report) -> RegressionAnalysis {
        let g = regression_guard(a, b);
        analyze(a, b, &g, &Thresholds::builtin(), &AnalysisOptions::default())
    }

    fn advisory(a: &Report, b: &Report) -> RegressionAnalysis {
        let g = regression_guard(a, b);
        analyze(
            a,
            b,
            &g,
            &Thresholds::builtin(),
            &AnalysisOptions {
                divergence_policy: DivergencePolicy::Advisory,
                ..Default::default()
            },
        )
    }

    /// Analyze gating on the server-reported execution time — the default
    /// (`--gated-metric server-ms`), spelled out for symmetry with [`total_gate`].
    fn server_gate(
        a: &Report,
        b: &Report,
    ) -> RegressionAnalysis {
        let g = regression_guard(a, b);
        analyze(
            a,
            b,
            &g,
            &Thresholds::builtin(),
            &AnalysisOptions {
                gated_metric: GatedMetric::ServerMs,
                ..Default::default()
            },
        )
    }

    /// Analyze gating on the client-observed total latency (the `--gated-metric total-ms`
    /// opt-in escape hatch).
    fn total_gate(
        a: &Report,
        b: &Report,
    ) -> RegressionAnalysis {
        let g = regression_guard(a, b);
        analyze(
            a,
            b,
            &g,
            &Thresholds::builtin(),
            &AnalysisOptions {
                gated_metric: GatedMetric::TotalMs,
                ..Default::default()
            },
        )
    }

    /// Overwrite `op`'s cached C=1 `server_ms` summary (the fabricated reports have exactly one
    /// level), decoupling it from the `metrics()` helper's shared server = total medians.
    fn set_server_median(
        rep: &mut Report,
        op: &str,
        median: f64,
    ) {
        rep.operations.get_mut(op).expect("op present").levels[0]
            .cached
            .as_mut()
            .expect("cached level")
            .metrics
            .server_ms = summ(median);
    }

    #[test]
    fn pass_when_within_budget_with_comparable_cells() {
        let a = rpt("main", 42001, &[("match_by_index", 1.0, Some("d1"))]);
        let b = rpt("pr", 42002, &[("match_by_index", 1.05, Some("d1"))]);
        let an = gate(&a, &b);
        assert_eq!(an.verdict, OverallVerdict::Pass);
        assert_eq!(an.status, ComparisonStatus::Comparable);
        assert_eq!(an.comparable_cells, 1);
        assert_eq!(an.regressed_cells, 0);
        assert_eq!(
            an.totals,
            OutcomeCounts {
                pass: 1,
                regressed: 0,
                diverged: 0,
                skipped: 0,
                not_applicable: 0
            }
        );
        let op = &an.ops["match_by_index"];
        assert_eq!(op.correctness, Correctness::Match);
        assert_eq!(op.op_outcome, OpOutcome::Pass);
        assert_eq!(op.tier.as_deref(), Some("core"));
        let cell = &op.cells[0];
        assert_eq!(cell.concurrency, 1);
        assert_eq!(cell.cache_mode, CacheMode::Cached);
        assert_eq!(cell.perf_verdict, Verdict::Ok);
        assert!(cell.delta_pct.unwrap() > 4.9 && cell.delta_pct.unwrap() < 5.1);
        let (emoji, headline) = an.verdict_line();
        assert_eq!(emoji, "🟢");
        assert_eq!(headline, "no p50 regression beyond budget across 1 comparable cell(s)");
    }

    #[test]
    fn regressed_when_a_cell_is_over_budget() {
        let a = rpt("main", 42001, &[("match_by_index", 1.0, Some("d1"))]);
        let b = rpt("pr", 42002, &[("match_by_index", 2.0, Some("d1"))]);
        let an = gate(&a, &b);
        assert_eq!(an.verdict, OverallVerdict::Regressed);
        assert_eq!(an.regressed_cells, 1);
        assert_eq!(an.ops["match_by_index"].op_outcome, OpOutcome::Regressed);
        let (emoji, headline) = an.verdict_line();
        assert_eq!(emoji, "🔴");
        assert_eq!(headline, "1 of 1 comparable cell(s) over budget");
        // A perf regression is Regressed under BOTH policies (advisory only affects divergence).
        assert_eq!(advisory(&a, &b).verdict, OverallVerdict::Regressed);
    }

    #[test]
    fn gate_policy_makes_divergence_regressed_with_na_cells() {
        let a = rpt("main", 42001, &[("match_by_index", 1.0, Some("d1"))]);
        let b = rpt("pr", 42002, &[("match_by_index", 1.0, Some("d2"))]);
        let an = gate(&a, &b);
        assert_eq!(an.verdict, OverallVerdict::Regressed);
        let op = &an.ops["match_by_index"];
        assert_eq!(op.correctness, Correctness::Diverged);
        assert_eq!(op.op_outcome, OpOutcome::Regressed);
        // Perf cells are N/A — but the raw medians and delta stay visible for diagnosis.
        assert_eq!(op.cells[0].perf_verdict, Verdict::NotApplicable);
        assert!(op.cells[0].delta_pct.is_some());
        assert_eq!(an.comparable_cells, 0, "diverged op's cells are never comparable");
        assert_eq!(an.diverged_ops(), vec!["match_by_index"]);
        let (emoji, headline) = an.verdict_line();
        assert_eq!(emoji, "🔴");
        assert!(headline.contains("1 op(s) have differing results (correctness)"), "{headline}");
    }

    #[test]
    fn advisory_policy_caps_divergence_at_advisory() {
        let a = rpt(
            "main",
            42001,
            &[("match_by_index", 1.0, Some("d1")), ("aggregate_count", 1.0, Some("x1"))],
        );
        let b = rpt(
            "pr",
            42002,
            &[("match_by_index", 1.0, Some("d1")), ("aggregate_count", 1.0, Some("x2"))],
        );
        let an = advisory(&a, &b);
        assert_eq!(an.verdict, OverallVerdict::Advisory);
        let op = &an.ops["aggregate_count"];
        assert_eq!(op.op_outcome, OpOutcome::DivergedAdvisory);
        assert_eq!(op.cells[0].perf_verdict, Verdict::NotApplicable);
        // The diverged bucket counts it; `regressed` never does.
        assert_eq!(
            an.totals,
            OutcomeCounts {
                pass: 1,
                regressed: 0,
                diverged: 1,
                skipped: 0,
                not_applicable: 0
            }
        );
        let (emoji, headline) = an.verdict_line();
        assert_eq!(emoji, "⚠");
        assert!(headline.starts_with("pass, 1 diverged"), "{headline}");
        assert!(
            headline.contains("advisory under this policy"),
            "{headline}"
        );
    }

    #[test]
    fn zero_comparable_cells_is_advisory_never_pass() {
        // Zero baseline p50 ⇒ the only cell is N/A ⇒ nothing was actually compared.
        let a = rpt("main", 42001, &[("match_by_index", 0.0, Some("d1"))]);
        let b = rpt("pr", 42002, &[("match_by_index", 1.0, Some("d1"))]);
        let an = gate(&a, &b);
        assert_eq!(an.verdict, OverallVerdict::Advisory);
        assert_eq!(an.comparable_cells, 0);
        assert_eq!(an.ops["match_by_index"].op_outcome, OpOutcome::NotApplicable);
        let (emoji, headline) = an.verdict_line();
        assert_eq!(emoji, "⚠");
        assert!(headline.starts_with("no comparable cells"), "{headline}");
    }

    /// Mark `op` in `rep` as capability-skipped: no levels, no digest, no policy — exactly what
    /// replay writes for an op whose required procedure the engine lacks (design Phase 6 §3.5).
    fn skip_op(
        rep: &mut Report,
        op: &str,
        reason: &str,
    ) {
        let o = rep.operations.get_mut(op).expect("op present");
        o.levels = vec![];
        o.result_digest = None;
        o.policy = None;
        o.skipped = Some(reason.to_string());
    }

    #[test]
    fn skipped_op_is_neither_pass_nor_divergence() {
        // Baseline measured `aggregate_count` (with a digest); the candidate engine lacks its
        // procedure and skipped it. The clean op still compares; the skipped op is ⏭ — not a
        // pass, not a divergence, not a gate — under BOTH policies.
        let a = rpt(
            "main",
            42001,
            &[
                ("match_by_index", 1.0, Some("d1")),
                ("aggregate_count", 1.0, Some("d2")),
            ],
        );
        let mut b = rpt(
            "pr",
            42002,
            &[
                ("match_by_index", 1.0, Some("d1")),
                ("aggregate_count", 1.0, Some("d2")),
            ],
        );
        skip_op(
            &mut b,
            "aggregate_count",
            "engine lacks procedure 'algo.maxFlow' (capability probe)",
        );
        for an in [gate(&a, &b), advisory(&a, &b)] {
            assert_eq!(
                an.verdict,
                OverallVerdict::Pass,
                "a skip never gates or caps"
            );
            assert!(an.diverged_ops().is_empty(), "a skip is not a divergence");
            let oa = &an.ops["aggregate_count"];
            assert_eq!(oa.op_outcome, OpOutcome::Skipped);
            assert_eq!(oa.correctness, Correctness::NotGated);
            assert!(oa.skipped_baseline.is_none());
            assert_eq!(
                oa.skipped_candidate.as_deref(),
                Some("engine lacks procedure 'algo.maxFlow' (capability probe)")
            );
            // The measured side's cell stays visible for context but is never comparable.
            assert!(oa
                .cells
                .iter()
                .all(|c| c.perf_verdict == Verdict::NotApplicable));
            assert_eq!(
                an.totals,
                OutcomeCounts {
                    pass: 1,
                    regressed: 0,
                    diverged: 0,
                    skipped: 1,
                    not_applicable: 0
                }
            );
            assert_eq!(an.comparable_cells, 1);
            let (emoji, headline) = an.verdict_line();
            assert_eq!(emoji, "🟢");
            assert!(
                headline.contains("1 op(s) skipped (engine lacks their required procedure)"),
                "{headline}"
            );
        }
    }

    #[test]
    fn op_skipped_on_both_sides_is_tallied_without_cells() {
        let mut a = rpt(
            "main",
            42001,
            &[
                ("match_by_index", 1.0, Some("d1")),
                ("aggregate_count", 1.0, Some("d2")),
            ],
        );
        let mut b = rpt(
            "pr",
            42002,
            &[
                ("match_by_index", 1.0, Some("d1")),
                ("aggregate_count", 1.0, Some("d2")),
            ],
        );
        skip_op(
            &mut a,
            "aggregate_count",
            "engine lacks procedure 'algo.MSF' (capability probe)",
        );
        skip_op(
            &mut b,
            "aggregate_count",
            "engine lacks procedure 'algo.MSF' (capability probe)",
        );
        let an = gate(&a, &b);
        let oa = &an.ops["aggregate_count"];
        assert!(
            oa.cells.is_empty(),
            "skipped on both sides ⇒ no measured cell"
        );
        assert_eq!(oa.op_outcome, OpOutcome::Skipped);
        assert!(oa.skipped_baseline.is_some() && oa.skipped_candidate.is_some());
        // Every skip is visible in the tallies, even with no cell.
        assert_eq!(an.totals.skipped, 1);
        assert_eq!(an.verdict, OverallVerdict::Pass);
    }

    #[test]
    fn all_ops_skipped_is_advisory_never_green() {
        let a = rpt("main", 42001, &[("match_by_index", 1.0, Some("d1"))]);
        let mut b = rpt("pr", 42002, &[("match_by_index", 1.0, Some("d1"))]);
        skip_op(
            &mut b,
            "match_by_index",
            "engine lacks procedure 'algo.pageRank' (capability probe)",
        );
        let an = gate(&a, &b);
        // The only op is skipped ⇒ zero comparable cells ⇒ Advisory (never a silent green).
        assert_eq!(an.verdict, OverallVerdict::Advisory);
        assert_eq!(an.comparable_cells, 0);
        assert_eq!(
            an.totals,
            OutcomeCounts {
                pass: 0,
                regressed: 0,
                diverged: 0,
                skipped: 1,
                not_applicable: 0
            }
        );
        let (emoji, headline) = an.verdict_line();
        assert_eq!(emoji, "⚠");
        assert!(headline.starts_with("no comparable cells"), "{headline}");
        assert!(headline.contains("1 op(s) skipped"), "{headline}");
    }

    #[test]
    fn cells_json_carries_skip_reasons_and_skipped_outcome() {
        // The serialized skip contract: `op_outcome: "skipped"`, the per-side reason fields
        // present exactly when a side skipped, and the `skipped` bucket in `totals`.
        let a = rpt(
            "main",
            42001,
            &[
                ("match_by_index", 1.0, Some("d1")),
                ("aggregate_count", 1.0, Some("d2")),
            ],
        );
        let mut b = rpt(
            "pr",
            42002,
            &[
                ("match_by_index", 1.0, Some("d1")),
                ("aggregate_count", 1.0, Some("d2")),
            ],
        );
        skip_op(
            &mut b,
            "aggregate_count",
            "engine lacks procedure 'algo.maxFlow' (capability probe)",
        );
        let an = gate(&a, &b);
        let value: serde_json::Value = serde_json::from_str(&an.to_json().unwrap()).unwrap();
        let op = &value["ops"]["aggregate_count"];
        assert_eq!(op["op_outcome"], "skipped");
        assert!(
            op.get("skipped_baseline").is_none(),
            "absent side omits its skip field"
        );
        assert_eq!(
            op["skipped_candidate"],
            "engine lacks procedure 'algo.maxFlow' (capability probe)"
        );
        assert_eq!(value["totals"]["skipped"], 1);
        // Round-trips through serde (the skip fields deserialize too).
        let back: RegressionAnalysis = serde_json::from_value(value).unwrap();
        assert_eq!(back, an);
    }

    #[test]
    fn not_comparable_short_circuits_everything() {
        let a = rpt("main", 42001, &[("match_by_index", 1.0, Some("d1"))]);
        let mut b = rpt("pr", 42002, &[("match_by_index", 1.0, Some("d1"))]);
        b.meta.dataset.as_mut().unwrap().workload_hash = "sha256:zzz".to_string();
        let an = gate(&a, &b);
        assert_eq!(an.verdict, OverallVerdict::NotComparable);
        assert!(matches!(an.status, ComparisonStatus::WorkloadMismatch { .. }));
        assert!(an.ops.is_empty());
        assert_eq!(an.totals, OutcomeCounts::default());
        let (emoji, headline) = an.verdict_line();
        assert_eq!(emoji, "⚠");
        assert!(headline.starts_with("not comparable — "), "{headline}");
        // Contract: the discriminated status kind + its mismatch detail.
        let value: serde_json::Value = serde_json::from_str(&an.to_json().unwrap()).unwrap();
        assert_eq!(value["status"]["kind"], "workload_mismatch");
        assert!(value["status"]["reason"].as_str().unwrap().contains("workload_hash"));
    }

    #[test]
    fn disjoint_op_sets_are_no_common_ops() {
        // Config-comparable runs that share no op name: nothing to compare — a NotComparable
        // verdict with the `no_common_ops` status kind, warnings preserved.
        let a = rpt("main", 42001, &[("match_by_index", 1.0, Some("d1"))]);
        let b = rpt("pr", 42002, &[("expand_1_hop", 1.0, Some("d2"))]);
        let an = gate(&a, &b);
        assert_eq!(an.verdict, OverallVerdict::NotComparable);
        assert!(matches!(an.status, ComparisonStatus::NoCommonOps { .. }));
        assert!(an.ops.is_empty());
        assert!(
            an.warnings.iter().any(|w| w.contains("module version changed")),
            "advisory warnings survive the short-circuit: {:?}",
            an.warnings
        );
        let (emoji, headline) = an.verdict_line();
        assert_eq!(emoji, "⚠");
        assert!(headline.contains("share no operation name"), "{headline}");
        let value: serde_json::Value = serde_json::from_str(&an.to_json().unwrap()).unwrap();
        assert_eq!(value["status"]["kind"], "no_common_ops");
        assert!(value["status"]["reason"].is_string());
    }

    #[test]
    fn version_mismatch_is_an_advisory_warning_not_a_guard() {
        // Different module versions: still Comparable — surfaced only in `warnings` (design §A1).
        let a = rpt("main", 42001, &[("match_by_index", 1.0, Some("d1"))]);
        let b = rpt("pr", 43005, &[("match_by_index", 1.0, Some("d1"))]);
        let an = gate(&a, &b);
        assert_eq!(an.status, ComparisonStatus::Comparable);
        assert_eq!(an.verdict, OverallVerdict::Pass);
        assert!(
            an.warnings.iter().any(|w| w.contains("module version changed")),
            "a real version delta is noted as an advisory warning: {:?}",
            an.warnings
        );
    }

    #[test]
    fn missing_digests_are_not_gated_and_never_diverged() {
        let a = rpt("main", 42001, &[("match_by_index", 1.0, None)]);
        let b = rpt("pr", 42002, &[("match_by_index", 1.0, None)]);
        let an = gate(&a, &b);
        let op = &an.ops["match_by_index"];
        assert_eq!(op.correctness, Correctness::NotGated);
        assert_eq!(op.op_outcome, OpOutcome::Pass);
        assert_eq!(an.verdict, OverallVerdict::Pass);
        assert!(an.diverged_ops().is_empty());
    }

    #[test]
    fn asymmetric_digest_counts_as_diverged() {
        let a = rpt("main", 42001, &[("match_by_index", 1.0, Some("d1"))]);
        let b = rpt("pr", 42002, &[("match_by_index", 1.0, None)]);
        let an = gate(&a, &b);
        assert_eq!(an.ops["match_by_index"].correctness, Correctness::Diverged);
        assert_eq!(an.verdict, OverallVerdict::Regressed);
    }

    #[test]
    fn no_cell_diverged_op_still_drives_the_verdict() {
        // Op present on both sides with diverged digests but zero measured levels: no cells, but
        // the divergence still gates (or warns, under advisory) AND is tallied — every divergence
        // counts in the outcome table (design v5).
        let mut a = rpt("main", 42001, &[("match_by_index", 1.0, Some("d1"))]);
        let mut b = rpt("pr", 42002, &[("match_by_index", 1.0, Some("d2"))]);
        a.operations.get_mut("match_by_index").unwrap().levels.clear();
        b.operations.get_mut("match_by_index").unwrap().levels.clear();
        let an = gate(&a, &b);
        assert_eq!(an.verdict, OverallVerdict::Regressed);
        assert_eq!(
            an.totals,
            OutcomeCounts { regressed: 1, ..OutcomeCounts::default() },
            "a cell-less diverged op is tallied under gate"
        );
        assert!(an.ops["match_by_index"].cells.is_empty());
        assert_eq!(an.ops["match_by_index"].op_outcome, OpOutcome::Regressed);
        let adv = advisory(&a, &b);
        assert_eq!(adv.verdict, OverallVerdict::Advisory, "divergence caps at Advisory");
        assert_eq!(adv.ops["match_by_index"].op_outcome, OpOutcome::DivergedAdvisory);
        assert_eq!(
            adv.totals,
            OutcomeCounts { diverged: 1, ..OutcomeCounts::default() },
            "a cell-less diverged op is tallied under advisory"
        );
    }

    #[test]
    fn meta_block_echoes_run_and_threshold_settings() {
        let a = rpt("main", 42001, &[("match_by_index", 1.0, Some("d1"))]);
        let b = rpt("pr", 42002, &[("match_by_index", 1.0, Some("d1"))]);
        let g = regression_guard(&a, &b);
        let an = analyze(
            &a,
            &b,
            &g,
            &Thresholds::builtin(),
            &AnalysisOptions { elapsed_secs: Some(12.5), ..Default::default() },
        );
        assert_eq!(an.comparison.baseline_label, "main");
        assert_eq!(an.comparison.candidate_label, "pr");
        assert!(an.comparison.slug.starts_with("synthetic-pr-vs-main-"), "{}", an.comparison.slug);
        assert_eq!(an.meta.baseline.module_version.as_deref(), Some("4.20.1"));
        assert_eq!(an.meta.candidate.module_version.as_deref(), Some("4.20.2"));
        assert_eq!(an.meta.baseline.workload_hash.as_deref(), Some("sha256:abc"));
        assert_eq!(an.meta.baseline.samples, 1000);
        assert_eq!(an.meta.baseline.warmup, 200);
        assert_eq!(an.meta.baseline.concurrency, vec![1]);
        assert_eq!(an.elapsed_secs, Some(12.5));
        assert_eq!(an.gated_metric, "server_ms.p50");
        assert_eq!(an.budget_profile, BudgetProfile::Strict);
        assert_eq!(an.divergence_policy, DivergencePolicy::Gate);
        // The thresholds echo carries the resolved default budget.
        assert!((an.meta.thresholds.default_budget_pct - 10.0).abs() < 1e-9);
    }

    #[test]
    fn analysis_meta_carries_oracle_attestation_additively() {
        // §6.3 attestation is an additive optional SideMeta field: the key is absent (not null)
        // for un-attested runs — pre-oracle cells consumers never see it — and a plain copy of
        // `meta.oracle_verified` when the replay re-verified a write outcome oracle.
        let mut a = rpt("main", 42001, &[("match_by_index", 1.0, Some("d1"))]);
        let mut b = rpt("pr", 42002, &[("match_by_index", 1.0, Some("d1"))]);
        let un_attested = gate(&a, &b);
        let value: serde_json::Value =
            serde_json::from_str(&un_attested.to_json().unwrap()).unwrap();
        assert!(value["meta"]["baseline"].get("oracle_verified").is_none(), "{value}");

        let attestation: std::collections::BTreeMap<String, usize> =
            [("single_vertex_write".to_string(), 256)].into();
        a.meta.oracle_verified = Some(attestation.clone());
        b.meta.oracle_verified = Some(attestation.clone());
        let attested = gate(&a, &b);
        assert_eq!(attested.meta.baseline.oracle_verified, Some(attestation.clone()));
        assert_eq!(attested.meta.candidate.oracle_verified, Some(attestation));
        let value: serde_json::Value =
            serde_json::from_str(&attested.to_json().unwrap()).unwrap();
        assert_eq!(value["meta"]["candidate"]["oracle_verified"]["single_vertex_write"], 256);
    }

    #[test]
    fn analysis_json_round_trips() {
        let a = rpt(
            "main",
            42001,
            &[("match_by_index", 1.0, Some("d1")), ("aggregate_count", 1.0, Some("x1"))],
        );
        let b = rpt(
            "pr",
            42002,
            &[("match_by_index", 2.0, Some("d1")), ("aggregate_count", 1.0, Some("x2"))],
        );
        let an = advisory(&a, &b);
        let json = an.to_json().unwrap();
        let back: RegressionAnalysis = serde_json::from_str(&json).unwrap();
        assert_eq!(an, back);
        assert!(json.contains("\"divergence_policy\": \"advisory\""), "{json}");
    }

    #[test]
    fn cells_json_freezes_the_top_level_field_set() {
        // The cells JSON is a machine contract (schema v1). This test freezes the exact top-level
        // field set — adding/renaming/removing a field must bump ANALYSIS_SCHEMA_VERSION and
        // update this list deliberately.
        let a = rpt(
            "main",
            42001,
            &[("match_by_index", 1.0, Some("d1")), ("aggregate_count", 1.0, Some("d2"))],
        );
        let b = rpt(
            "pr",
            42002,
            &[("match_by_index", 2.0, Some("d1")), ("aggregate_count", 1.0, Some("d2"))],
        );
        let an = gate(&a, &b);
        let value: serde_json::Value = serde_json::from_str(&an.to_json().unwrap()).unwrap();
        let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "budget_profile",
                "comparable_cells",
                "comparison",
                "divergence_policy",
                "elapsed_secs",
                "gated_metric",
                "meta",
                "ops",
                "regressed_cells",
                "schema_version",
                "status",
                "totals",
                "verdict",
                "warnings",
            ]
        );
        assert_eq!(value["schema_version"], 2);
        // …and the per-op shape: the op-level field set (skip fields are omitted when absent —
        // they appear exactly when a side skipped the op) plus the outcome-counts bucket set.
        let op = &value["ops"]["match_by_index"];
        let mut op_keys: Vec<&str> = op.as_object().unwrap().keys().map(String::as_str).collect();
        op_keys.sort_unstable();
        assert_eq!(op_keys, vec!["cells", "correctness", "op_outcome", "tier"]);
        let mut count_keys: Vec<&str> = value["totals"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        count_keys.sort_unstable();
        assert_eq!(
            count_keys,
            vec!["diverged", "not_applicable", "pass", "regressed", "skipped"]
        );
        // …and the per-cell shape (what the interactive page consumes).
        let cell = &value["ops"]["match_by_index"]["cells"][0];
        let mut cell_keys: Vec<&str> =
            cell.as_object().unwrap().keys().map(String::as_str).collect();
        cell_keys.sort_unstable();
        assert_eq!(
            cell_keys,
            vec![
                "baseline_p50_ms",
                "budget",
                "cache_mode",
                "candidate_p50_ms",
                "concurrency",
                "context",
                "delta_ms",
                "delta_pct",
                "perf_verdict",
            ]
        );
        assert_eq!(cell["cache_mode"], "cached");
        assert_eq!(cell["perf_verdict"], "regressed");
        // The contract names: a within-budget cell is "pass" (not the Rust variant name `Ok`).
        assert_eq!(value["ops"]["aggregate_count"]["cells"][0]["perf_verdict"], "pass");
        assert_eq!(value["status"]["kind"], "comparable");
    }

    #[test]
    fn divergence_policy_parses_and_round_trips() {
        assert_eq!("gate".parse::<DivergencePolicy>().unwrap(), DivergencePolicy::Gate);
        assert_eq!("advisory".parse::<DivergencePolicy>().unwrap(), DivergencePolicy::Advisory);
        assert!("both".parse::<DivergencePolicy>().is_err());
        assert_eq!(DivergencePolicy::default(), DivergencePolicy::Gate);
        for p in [DivergencePolicy::Gate, DivergencePolicy::Advisory] {
            assert_eq!(p.as_str().parse::<DivergencePolicy>().unwrap(), p);
            let json = serde_json::to_string(&p).unwrap();
            assert_eq!(json, format!("\"{}\"", p.as_str()));
        }
    }

    #[test]
    fn gated_metric_parses_ids_and_round_trips() {
        assert_eq!(
            "total-ms".parse::<GatedMetric>().unwrap(),
            GatedMetric::TotalMs
        );
        assert_eq!(
            "server-ms".parse::<GatedMetric>().unwrap(),
            GatedMetric::ServerMs
        );
        assert!("server_ms".parse::<GatedMetric>().is_err());
        assert!("p50".parse::<GatedMetric>().is_err());
        assert_eq!(
            "both".parse::<GatedMetric>().unwrap_err(),
            "unknown gated metric 'both' (expected 'total-ms' or 'server-ms')"
        );
        assert_eq!(GatedMetric::default(), GatedMetric::ServerMs);
        assert_eq!(GatedMetric::TotalMs.id(), GATED_METRIC);
        assert_eq!(GatedMetric::ServerMs.id(), SERVER_GATED_METRIC);
        for m in [GatedMetric::TotalMs, GatedMetric::ServerMs] {
            assert_eq!(m.as_str().parse::<GatedMetric>().unwrap(), m);
        }
    }

    #[test]
    fn server_ms_gating_ignores_a_total_only_regression() {
        // The candidate doubles its client-observed total latency (+100 %, way over budget) but
        // its server-reported execution time grows only 5 % — pure client/network noise. The
        // opt-in total-ms gate regresses; the default (server-ms) passes, and the cells carry
        // the server medians (and server tails in the context line), not the total ones.
        let mut a = rpt("main", 42001, &[("match_by_index", 10.0, Some("d1"))]);
        let mut b = rpt("pr", 42002, &[("match_by_index", 20.0, Some("d1"))]);
        set_server_median(&mut a, "match_by_index", 2.0);
        set_server_median(&mut b, "match_by_index", 2.1);

        let total = total_gate(&a, &b);
        assert_eq!(total.verdict, OverallVerdict::Regressed);
        assert_eq!(total.gated_metric, GATED_METRIC);
        assert!(!total.gated_on_server_ms());
        assert_eq!(
            total.ops["match_by_index"].cells[0].baseline_p50_ms,
            Some(10.0)
        );

        let server = server_gate(&a, &b);
        assert_eq!(server.verdict, OverallVerdict::Pass);
        assert_eq!(server.gated_metric, SERVER_GATED_METRIC);
        assert!(server.gated_on_server_ms());
        assert_eq!(server.regressed_cells, 0);
        assert_eq!(server.comparable_cells, 1);
        let cell = &server.ops["match_by_index"].cells[0];
        assert_eq!(cell.baseline_p50_ms, Some(2.0));
        assert_eq!(cell.candidate_p50_ms, Some(2.1));
        assert_eq!(cell.perf_verdict, Verdict::Ok);
        assert!(cell.delta_pct.unwrap() > 4.9 && cell.delta_pct.unwrap() < 5.1);
        // The context tails follow the gated metric (server p90 = 2.1 · 1.2), so the cell never
        // mixes clocks.
        let ctx = cell.context.candidate.unwrap();
        assert!(
            (ctx.p90_ms - 2.52).abs() < 1e-9,
            "server tails expected, got {}",
            ctx.p90_ms
        );
        // The budget resolves identically under both metrics — only the medians change.
        assert_eq!(cell.budget, total.ops["match_by_index"].cells[0].budget);
    }

    #[test]
    fn server_ms_gating_catches_a_server_only_regression() {
        // Mirror image: totals stay within budget (+4 %, under the 0.5 ms floor too) while the
        // server-reported execution time doubles. Only server-ms gating (the default) regresses.
        let mut a = rpt("main", 42001, &[("match_by_index", 10.0, Some("d1"))]);
        let mut b = rpt("pr", 42002, &[("match_by_index", 10.4, Some("d1"))]);
        set_server_median(&mut a, "match_by_index", 2.0);
        set_server_median(&mut b, "match_by_index", 4.0);

        assert_eq!(total_gate(&a, &b).verdict, OverallVerdict::Pass);
        let server = server_gate(&a, &b);
        assert_eq!(server.verdict, OverallVerdict::Regressed);
        assert_eq!(server.regressed_cells, 1);
        let cell = &server.ops["match_by_index"].cells[0];
        assert_eq!(cell.perf_verdict, Verdict::Regressed);
        assert!((cell.delta_ms.unwrap() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn server_gate_demotes_total_p50_to_cell_context() {
        // Under the default server-ms gate the client-observed total median stays visible as
        // demoted, informational context; the total-ms opt-in carries no duplicate (its primary
        // p50 already is the wall clock). The cells JSON mirrors that: the additive optional
        // field is omitted entirely when absent, so total-ms analyses are byte-identical to
        // before.
        let mut a = rpt("main", 42001, &[("match_by_index", 10.0, Some("d1"))]);
        let mut b = rpt("pr", 42002, &[("match_by_index", 10.4, Some("d1"))]);
        set_server_median(&mut a, "match_by_index", 2.0);
        set_server_median(&mut b, "match_by_index", 2.1);

        let server = server_gate(&a, &b);
        let cell = &server.ops["match_by_index"].cells[0];
        assert_eq!(cell.baseline_p50_ms, Some(2.0), "primary p50 is the server median");
        assert_eq!(cell.context.baseline.unwrap().total_p50_ms, Some(10.0));
        assert_eq!(cell.context.candidate.unwrap().total_p50_ms, Some(10.4));
        assert!(server.to_json().unwrap().contains("total_p50_ms"));

        let total = total_gate(&a, &b);
        let cell = &total.ops["match_by_index"].cells[0];
        assert_eq!(cell.context.baseline.unwrap().total_p50_ms, None);
        assert!(!total.to_json().unwrap().contains("total_p50_ms"));

        // A side with no usable wall clock (total median 0) omits the context field; the
        // server-gated verdict is untouched.
        let mut c = rpt("pr", 42003, &[("match_by_index", 0.0, Some("d1"))]);
        set_server_median(&mut c, "match_by_index", 2.1);
        let server = server_gate(&a, &c);
        let cell = &server.ops["match_by_index"].cells[0];
        assert_eq!(cell.perf_verdict, Verdict::Ok);
        assert_eq!(cell.context.candidate.unwrap().total_p50_ms, None);
    }

    #[test]
    fn cell_context_carries_within_run_dispersion_stats() {
        // Each side's context carries the within-run dispersion of `server_ms` — the gated
        // distribution: server_n (samples that actually carried a server time), sample σ (n−1)
        // and CV% — all rounded to 6 decimals so the cells JSON stays compact and byte-stable
        // across serde round-trips.
        let a = rpt("main", 42001, &[("match_by_index", 2.0, Some("d1"))]);
        let b = rpt("pr", 42002, &[("match_by_index", 2.0, Some("d1"))]);
        let an = server_gate(&a, &b);
        let side = an.ops["match_by_index"].cells[0].context.baseline.unwrap();
        assert_eq!(side.server_n, Some(100));
        // summ(2.0): population σ = 0.2 over n = 100 ⇒ sample σ = 0.2·√(100/99) ≈ 0.201008.
        assert_eq!(side.server_stddev_ms, Some(0.201008));
        assert_eq!(side.server_cv_pct, Some(10.050378));
    }

    #[test]
    fn dispersion_stats_degrade_with_invalid_server_median() {
        // No valid server time on a side (median 0/NaN — a report predating server-time
        // capture): every dispersion stat is absent, exactly like the server p50 columns —
        // there is deliberately no wall-clock fallback.
        let a = rpt("main", 42001, &[("match_by_index", 2.0, Some("d1"))]);
        let mut b = rpt("pr", 42002, &[("match_by_index", 2.0, Some("d1"))]);
        set_server_median(&mut b, "match_by_index", 0.0);
        let an = server_gate(&a, &b);
        let side = an.ops["match_by_index"].cells[0].context.candidate.unwrap();
        assert_eq!(side.server_n, None);
        assert_eq!(side.server_stddev_ms, None);
        assert_eq!(side.server_cv_pct, None);
    }

    #[test]
    fn example_query_prefers_candidate_and_falls_back_to_baseline() {
        // The op-level example query is additive context: the candidate's recorded text wins
        // (it reflects what THIS run measured), an older candidate report without the field
        // falls back to the baseline's, and the field is omitted when neither side carries one.
        let mut a = rpt("main", 42001, &[("match_by_index", 1.0, Some("d1"))]);
        let mut b = rpt("pr", 42002, &[("match_by_index", 1.0, Some("d1"))]);
        a.operations.get_mut("match_by_index").unwrap().example_query =
            Some("MATCH (old) RETURN old".to_string());
        b.operations.get_mut("match_by_index").unwrap().example_query =
            Some("MATCH (new) RETURN new".to_string());
        let an = gate(&a, &b);
        assert_eq!(
            an.ops["match_by_index"].example_query.as_deref(),
            Some("MATCH (new) RETURN new"),
            "candidate wins"
        );
        assert!(an.to_json().unwrap().contains("\"example_query\""));

        b.operations.get_mut("match_by_index").unwrap().example_query = None;
        let an = gate(&a, &b);
        assert_eq!(
            an.ops["match_by_index"].example_query.as_deref(),
            Some("MATCH (old) RETURN old"),
            "baseline fallback"
        );

        a.operations.get_mut("match_by_index").unwrap().example_query = None;
        let an = gate(&a, &b);
        assert_eq!(an.ops["match_by_index"].example_query, None);
        assert!(!an.to_json().unwrap().contains("example_query"), "omitted when absent");
    }

    #[test]
    fn old_cells_json_without_dispersion_fields_still_deserializes() {
        // Cells JSON written before the dispersion stats existed (schema v2, additive change):
        // the optional server-clock stats default to None, which renderers omit.
        let side: CellContextSide = serde_json::from_str(
            r#"{"p90_ms":1.2,"p95_ms":1.3,"p99_ms":1.5,"throughput_ops_per_sec":1000.0}"#,
        )
        .unwrap();
        assert_eq!(side.server_n, None);
        assert_eq!(side.server_stddev_ms, None);
        assert_eq!(side.server_cv_pct, None);
        assert_eq!(side.total_p50_ms, None);

        // A v2.7 payload (the short-lived wall-clock schema): the removed keys `n`,
        // `total_stddev_ms` and `total_cv_pct` are ignored, and `n_server` — the same
        // server-timed cohort — still feeds `server_n` through its serde alias.
        let side: CellContextSide = serde_json::from_str(
            r#"{"p90_ms":1.2,"p95_ms":1.3,"p99_ms":1.5,"throughput_ops_per_sec":1000.0,
                "n":300,"n_server":297,"server_stddev_ms":0.05,"server_cv_pct":12.0,
                "total_stddev_ms":0.06,"total_cv_pct":13.0}"#,
        )
        .unwrap();
        assert_eq!(side.server_n, Some(297));
        assert_eq!(side.server_stddev_ms, Some(0.05));
        assert_eq!(side.server_cv_pct, Some(12.0));
    }

    #[test]
    fn missing_server_median_yields_na_cell_and_one_advisory_warning() {
        // One clean op and one whose candidate reports no usable server time (median 0 — e.g. an
        // engine that doesn't report execution time). Under the default server-ms gating the
        // degraded cell is N/A — never a silent fallback to total_ms — and ONE loud warning names
        // exactly the affected op and the escape hatch. The total-ms opt-in of the same pair
        // stays warning-free.
        let a = rpt(
            "main",
            42001,
            &[
                ("match_by_index", 1.0, Some("d1")),
                ("aggregate_count", 1.0, Some("x1")),
            ],
        );
        let mut b = rpt(
            "pr",
            42002,
            &[
                ("match_by_index", 1.05, Some("d1")),
                ("aggregate_count", 1.05, Some("x1")),
            ],
        );
        set_server_median(&mut b, "aggregate_count", 0.0);

        let server = server_gate(&a, &b);
        assert_eq!(
            server.verdict,
            OverallVerdict::Pass,
            "the clean cell still compares"
        );
        assert_eq!(server.comparable_cells, 1);
        let degraded = &server.ops["aggregate_count"];
        assert_eq!(degraded.op_outcome, OpOutcome::NotApplicable);
        assert_eq!(degraded.cells[0].perf_verdict, Verdict::NotApplicable);
        assert!(
            degraded.cells[0].delta_pct.is_none(),
            "no delta from an invalid median"
        );
        let warning = server
            .warnings
            .iter()
            .find(|w| w.contains("gated metric server_ms.p50"))
            .expect("degraded-metric advisory present");
        assert!(warning.contains("1 op(s): aggregate_count"), "{warning}");
        assert!(
            !warning.contains("match_by_index"),
            "clean op not named: {warning}"
        );
        assert!(warning.contains("no silent fallback to total_ms"), "{warning}");
        assert!(
            warning.contains("re-run with `--gated-metric total-ms`"),
            "escape hatch named: {warning}"
        );

        let total = total_gate(&a, &b);
        assert_eq!(total.verdict, OverallVerdict::Pass);
        assert_eq!(
            total.comparable_cells, 2,
            "total_ms is present — both cells compare"
        );
        assert!(
            total.warnings.iter().all(|w| !w.contains("gated metric")),
            "no degraded-metric warning under the total-ms opt-in: {:?}",
            total.warnings
        );
    }

    #[test]
    fn all_server_medians_invalid_is_advisory_with_warning_never_pass() {
        // A baseline predating usable server time (NaN median) on the only op: every cell is N/A
        // under server-ms gating ⇒ zero comparable cells ⇒ Advisory (never a silent green), plus
        // the advisory warning.
        let mut a = rpt("main", 42001, &[("match_by_index", 1.0, Some("d1"))]);
        let b = rpt("pr", 42002, &[("match_by_index", 1.0, Some("d1"))]);
        set_server_median(&mut a, "match_by_index", f64::NAN);
        let an = server_gate(&a, &b);
        assert_eq!(an.verdict, OverallVerdict::Advisory);
        assert_eq!(an.comparable_cells, 0);
        assert!(
            an.warnings
                .iter()
                .any(|w| w.contains("gated metric server_ms.p50") && w.contains("match_by_index")),
            "{:?}",
            an.warnings
        );
        let (emoji, headline) = an.verdict_line();
        assert_eq!(emoji, "⚠");
        assert!(headline.starts_with("no comparable cells"), "{headline}");
    }

    #[test]
    fn diverged_and_skipped_ops_never_join_the_degraded_metric_warning() {
        // A diverged op and a skipped op are already N/A with their own louder markers; a missing
        // server median on them must not add degraded-metric noise.
        let a = rpt(
            "main",
            42001,
            &[
                ("match_by_index", 1.0, Some("d1")),
                ("aggregate_count", 1.0, Some("x1")),
            ],
        );
        let mut b = rpt(
            "pr",
            42002,
            &[
                ("match_by_index", 1.0, Some("d2")),
                ("aggregate_count", 1.0, Some("x1")),
            ],
        );
        set_server_median(&mut b, "match_by_index", 0.0); // diverged AND degraded
        skip_op(
            &mut b,
            "aggregate_count",
            "engine lacks procedure 'algo.maxFlow' (capability probe)",
        );
        let an = server_gate(&a, &b);
        assert!(
            an.warnings.iter().all(|w| !w.contains("gated metric")),
            "diverged/skipped ops carry their own signal: {:?}",
            an.warnings
        );
        assert_eq!(an.ops["match_by_index"].correctness, Correctness::Diverged);
        assert_eq!(an.ops["aggregate_count"].op_outcome, OpOutcome::Skipped);
    }

    #[test]
    fn server_gated_analysis_json_carries_the_metric_id() {
        let a = rpt("main", 42001, &[("match_by_index", 1.0, Some("d1"))]);
        let b = rpt("pr", 42002, &[("match_by_index", 1.0, Some("d1"))]);
        let g = regression_guard(&a, &b);
        let an = analyze(
            &a,
            &b,
            &g,
            &Thresholds::builtin(),
            &AnalysisOptions {
                gated_metric: GatedMetric::ServerMs,
                ..Default::default()
            },
        );
        let json = an.to_json().unwrap();
        assert!(
            json.contains("\"gated_metric\": \"server_ms.p50\""),
            "{json}"
        );
        let back: RegressionAnalysis = serde_json::from_str(&json).unwrap();
        assert_eq!(back, an);
        assert!(back.gated_on_server_ms());
    }

    #[test]
    fn slugify_normalizes_hyphenates_and_falls_back() {
        assert_eq!(slugify("Release 1.2.3"), "release-1-2-3");
        assert_eq!(slugify("  PR / main  "), "pr-main");
        assert_eq!(slugify("a__b--c"), "a-b-c");
        assert_eq!(slugify("***"), "run");
        assert_eq!(slugify(""), "run");
    }

    #[test]
    fn missing_side_yields_na_cell_with_context_only_where_present() {
        // `match_by_index` exists only in the baseline (ungated digest so the guard doesn't flag
        // asymmetry): cells exist (union of levels) with the candidate side None. A second,
        // common op (zero baseline p50 ⇒ N/A too) keeps the pair off the no-common-ops path.
        let a = rpt(
            "main",
            42001,
            &[("match_by_index", 1.0, None), ("aggregate_count", 0.0, None)],
        );
        let b = rpt("pr", 42002, &[("aggregate_count", 1.0, None)]);
        let an = gate(&a, &b);
        let cell = &an.ops["match_by_index"].cells[0];
        assert_eq!(cell.baseline_p50_ms, Some(1.0));
        assert_eq!(cell.candidate_p50_ms, None);
        assert_eq!(cell.perf_verdict, Verdict::NotApplicable);
        assert!(cell.delta_pct.is_none() && cell.delta_ms.is_none());
        assert!(cell.context.baseline.is_some());
        assert!(cell.context.candidate.is_none());
        // One-sided ⇒ no comparable cell anywhere ⇒ Advisory (never green).
        assert_eq!(an.verdict, OverallVerdict::Advisory);
    }

    #[test]
    fn digest_present_only_in_baseline_of_a_missing_op_is_diverged() {
        // Baseline carries a digest; candidate lacks the op entirely ⇒ asymmetric ⇒ diverged.
        // (A shared op keeps the pair comparable at all — off the no-common-ops path.)
        let a = rpt(
            "main",
            42001,
            &[("match_by_index", 1.0, Some("d1")), ("aggregate_count", 1.0, Some("dc"))],
        );
        let b = rpt("pr", 42002, &[("aggregate_count", 1.0, Some("dc"))]);
        let an = gate(&a, &b);
        assert_eq!(an.ops["match_by_index"].correctness, Correctness::Diverged);
        assert_eq!(an.verdict, OverallVerdict::Regressed);
    }
}
