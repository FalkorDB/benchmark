//! Cross-run **diff** report: render two synthetic [`Report`]s side by side across every op, cache
//! mode and concurrency level (throughput + total-latency p50/p90/p95/p99 with per-metric deltas),
//! as pasteable Markdown. Used by `synthetic report --diff` after the [`crate::synthetic::baseline`]
//! guard confirms the two runs measured the same workload.
//!
//! The **regression** flavors ([`regression_markdown`], [`summarize`]) are pure renderers of the
//! [`RegressionAnalysis`] model built by [`crate::synthetic::analysis::analyze`] — the verdicts
//! are computed once, there.

use crate::synthetic::analysis::{
    CacheMode, CellAnalysis, CellContextSide, Correctness, DivergencePolicy,
    OpAnalysis, OpOutcome, OutcomeCounts, OverallVerdict, RegressionAnalysis,
};
use crate::synthetic::provenance::decode_module_version;
use crate::synthetic::report::{md_cell, LevelMetrics, LevelReport, Report};
use crate::synthetic::thresholds::{BudgetProfile, Verdict};
use crate::synthetic::Tier;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Which cache mode of a [`LevelReport`] to read.
#[derive(Clone, Copy)]
enum Mode {
    Cached,
    Uncached,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Mode::Cached => "cached (plan reused — execution only)",
            Mode::Uncached => "uncached (forced plan-cache miss — execution + compilation)",
        }
    }
    fn pick(self, lvl: &LevelReport) -> Option<&LevelMetrics> {
        match self {
            Mode::Cached => lvl.cached.as_ref(),
            Mode::Uncached => lvl.uncached.as_ref(),
        }
    }
}

/// Render the Markdown diff of `baseline` (A) vs `candidate` (B). `warnings` are advisory notes from
/// the guard (e.g. an image change) surfaced at the top.
pub fn diff_markdown(
    baseline: &Report,
    candidate: &Report,
    warnings: &[String],
) -> String {
    let mut out = String::new();
    let la = col_label(baseline, "A");
    let lb = col_label(candidate, "B");
    out.push_str(&format!(
        "# Synthetic benchmark diff — {} → {}\n\n",
        md_cell(&la),
        md_cell(&lb)
    ));
    out.push_str(&format!(
        "| field | {} (baseline) | {} (candidate) |\n|---|---|---|\n",
        md_cell(&la),
        md_cell(&lb)
    ));
    row2(&mut out, "FalkorDB module", &ver(baseline), &ver(candidate));
    row2(
        &mut out,
        "server image",
        baseline.meta.server.server_image.as_deref().unwrap_or("—"),
        candidate.meta.server.server_image.as_deref().unwrap_or("—"),
    );
    row2(
        &mut out,
        "endpoint / graph",
        &format!("`{}` / `{}`", baseline.meta.endpoint, baseline.meta.graph),
        &format!("`{}` / `{}`", candidate.meta.endpoint, candidate.meta.graph),
    );
    row2(
        &mut out,
        "workload_hash",
        &opt_hash(baseline),
        &opt_hash(candidate),
    );
    row2(
        &mut out,
        "samples / warmup",
        &format!("{} / {}", baseline.meta.samples, baseline.meta.warmup),
        &format!("{} / {}", candidate.meta.samples, candidate.meta.warmup),
    );

    out.push_str(
        "\n_Δ is 100·(candidate−baseline)/baseline. **Latency: lower is better** (a positive Δ = \
         slower / regressed); **throughput: higher is better**. `—` = not measured in that run._\n",
    );
    for w in warnings {
        out.push_str(&format!("\n> ⚠ {w}\n"));
    }

    // Every op measured by either run, in stable order.
    let ops: BTreeSet<&String> = baseline
        .operations
        .keys()
        .chain(candidate.operations.keys())
        .collect();
    for op in ops {
        out.push_str(&format!("\n## `{op}`\n"));
        for mode in [Mode::Cached, Mode::Uncached] {
            render_mode(&mut out, baseline, candidate, op, mode);
        }
    }
    out
}

/// The display name for a run's column: its `--label` if set, else the caller-supplied `fallback`
/// (`A`/`B` for `diff_markdown`; `baseline`/`candidate` for the regression report).
fn col_label(r: &Report, fallback: &str) -> String {
    r.meta.label.clone().unwrap_or_else(|| fallback.to_string())
}

/// Render one op × cache-mode table (rows = concurrency levels present in either run). Skipped
/// entirely when neither run measured this op in this mode.
fn render_mode(
    out: &mut String,
    a: &Report,
    b: &Report,
    op: &str,
    mode: Mode,
) {
    // Union of concurrency levels that have this mode in either run.
    let mut levels: BTreeSet<usize> = BTreeSet::new();
    for rep in [a, b] {
        if let Some(opr) = rep.operations.get(op) {
            for lvl in &opr.levels {
                if mode.pick(lvl).is_some() {
                    levels.insert(lvl.concurrency);
                }
            }
        }
    }
    if levels.is_empty() {
        return;
    }
    out.push_str(&format!("\n_{}_\n\n", mode.label()));
    let la = md_cell(&col_label(a, "A"));
    let lb = md_cell(&col_label(b, "B"));
    out.push_str(&format!(
        "| C | {la} total p50/p90/p95/p99 (ms) | {lb} total p50/p90/p95/p99 (ms) | Δp50 | {la} tput (ops/s) | {lb} tput (ops/s) | Δtput |\n\
         |---:|---|---|---:|---:|---:|---:|\n",
    ));
    for c in levels {
        let am = level_metrics(a, op, c, mode);
        let bm = level_metrics(b, op, c, mode);
        let a_pct = am.map(percentiles).unwrap_or_else(|| "—".to_string());
        let b_pct = bm.map(percentiles).unwrap_or_else(|| "—".to_string());
        let dp50 = match (am, bm) {
            (Some(x), Some(y)) => pct(x.metrics.total_ms.median, y.metrics.total_ms.median),
            _ => "—".to_string(),
        };
        let a_tp = am.map(|m| format!("{:.0}", m.throughput_ops_per_sec)).unwrap_or_else(|| "—".to_string());
        let b_tp = bm.map(|m| format!("{:.0}", m.throughput_ops_per_sec)).unwrap_or_else(|| "—".to_string());
        let dtp = match (am, bm) {
            (Some(x), Some(y)) => pct(x.throughput_ops_per_sec, y.throughput_ops_per_sec),
            _ => "—".to_string(),
        };
        out.push_str(&format!(
            "| {c} | {a_pct} | {b_pct} | {dp50} | {a_tp} | {b_tp} | {dtp} |\n"
        ));
    }
}

/// The [`LevelMetrics`] for `op` at concurrency `c` in `mode`, if present.
fn level_metrics<'a>(
    report: &'a Report,
    op: &str,
    c: usize,
    mode: Mode,
) -> Option<&'a LevelMetrics> {
    report
        .operations
        .get(op)?
        .levels
        .iter()
        .find(|lvl| lvl.concurrency == c)
        .and_then(|lvl| mode.pick(lvl))
}

fn percentiles(m: &LevelMetrics) -> String {
    let s = &m.metrics.total_ms;
    format!("{:.3} / {:.3} / {:.3} / {:.3}", s.median, s.p90, s.p95, s.p99)
}

/// A regression-table latency cell: the gated **p50** on the primary line, with p90/p99 and
/// throughput folded onto a smaller `context:` line (informational, never gated). `—` when the
/// side is absent. Values are fixed-precision measurements, so no operator-supplied text is
/// interpolated (no `md_cell` escaping needed).
fn latency_cell(
    p50: Option<f64>,
    ctx: Option<&CellContextSide>,
) -> String {
    match (p50, ctx) {
        (Some(p50), Some(c)) => format!(
            "{:.3}<br><sub>context: p90 {:.3} · p99 {:.3} · {:.0} op/s</sub>",
            p50, c.p90_ms, c.p99_ms, c.throughput_ops_per_sec
        ),
        _ => "—".to_string(),
    }
}

/// Escape a string for safe embedding as **HTML text** (e.g. inside a `<code>`/`<summary>`): a
/// crafted report could carry an op key with `<`, `>` or `&` that would otherwise break the
/// `<details>` markup or inject HTML into the PR comment. Order matters — `&` first.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// `100·(b−a)/a`, formatted with a sign; `n/a` when `a == 0`.
fn pct(
    a: f64,
    b: f64,
) -> String {
    if a == 0.0 {
        "n/a".to_string()
    } else {
        format!("{:+.1}%", (b - a) / a * 100.0)
    }
}

/// Human-readable duration from seconds: `1h 2m 3s`, `4m 5s`, `12s`, or `0.4s` sub-second.
/// `n/a` for a non-finite or negative input.
fn fmt_duration_secs(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "n/a".to_string();
    }
    if secs < 1.0 {
        return format!("{secs:.1}s");
    }
    let total = secs.round() as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

fn ver(report: &Report) -> String {
    report
        .meta
        .server
        .module_graph_ver
        .map(decode_module_version)
        .unwrap_or_else(|| "unknown".to_string())
}

fn opt_hash(report: &Report) -> String {
    report
        .meta
        .dataset
        .as_ref()
        .map(|d| format!("`{}`", d.workload_hash))
        .unwrap_or_else(|| "—".to_string())
}

fn row2(
    out: &mut String,
    field: &str,
    a: &str,
    b: &str,
) {
    // Escape table-breaking characters — endpoint/graph/server_image are operator-supplied.
    out.push_str(&format!(
        "| {} | {} | {} |\n",
        md_cell(field),
        md_cell(a),
        md_cell(b)
    ));
}

// ==== Non-fatal regression report ===============================================================

/// Render the **non-fatal** `report --regression` markdown from the [`RegressionAnalysis`] model:
/// per-cell 🟢/🔴/N-A verdicts on p50 (total-latency median) against the threshold budget, with
/// throughput shown for context. Diverged ops get a perf verdict of N/A — 🔴 under the `gate`
/// divergence policy, ⚠ under `advisory`. A `NotComparable` status renders a single "not
/// comparable" note. Never errors.
pub fn regression_markdown(analysis: &RegressionAnalysis) -> String {
    let la = analysis.comparison.baseline_label.as_str();
    let lb = analysis.comparison.candidate_label.as_str();
    let mut head = String::new();
    head.push_str(&format!(
        "### 🧪 Synthetic per-op regression — {} vs {}\n\n",
        md_cell(lb),
        md_cell(la)
    ));
    if let Some(secs) = analysis.elapsed_secs {
        head.push_str(&format!(
            "⏱ Computed in {} (benchmark + reporting).\n\n",
            fmt_duration_secs(secs)
        ));
    }
    head.push_str(&format!("| field | {} | {} |\n|---|---|---|\n", md_cell(la), md_cell(lb)));
    let meta = &analysis.meta;
    row2(
        &mut head,
        "FalkorDB module",
        meta.baseline.module_version.as_deref().unwrap_or("unknown"),
        meta.candidate.module_version.as_deref().unwrap_or("unknown"),
    );
    row2(
        &mut head,
        "server image",
        meta.baseline.server_image.as_deref().unwrap_or("—"),
        meta.candidate.server_image.as_deref().unwrap_or("—"),
    );
    let hash_cell = |h: &Option<String>| {
        h.as_ref().map(|h| format!("`{h}`")).unwrap_or_else(|| "—".to_string())
    };
    row2(
        &mut head,
        "workload_hash",
        &hash_cell(&meta.baseline.workload_hash),
        &hash_cell(&meta.candidate.workload_hash),
    );
    row2(
        &mut head,
        "samples / warmup",
        &format!("{} / {}", meta.baseline.samples, meta.baseline.warmup),
        &format!("{} / {}", meta.candidate.samples, meta.candidate.warmup),
    );
    head.push('\n');
    head.push_str(&meta.thresholds.settings_markdown());

    if let Some(reason) = analysis.status.not_comparable_reason() {
        head.push_str(&format!(
            "\n> ⚠ **not comparable** — {}. No latency verdict is shown.\n",
            md_cell(reason)
        ));
        return head;
    }

    // Render the per-op tables into `body`, straight from the model's cells.
    let mut body = String::new();
    for (op, oa) in &analysis.ops {
        // Render this op's cache-mode tables into a temp buffer so the whole op section can be
        // wrapped in a **collapsed** <details> — keeps the PR sticky comment compact by default.
        let mut op_body = String::new();
        for mode in [CacheMode::Cached, CacheMode::Uncached] {
            render_regression_mode(&mut op_body, oa, mode, analysis.divergence_policy, la, lb);
        }
        // Ops with no measured cell get no report section; the totals still tally them when
        // they diverged (gate → regressed, advisory → diverged).
        if op_body.trim().is_empty() {
            continue;
        }
        let diverged_note = if oa.correctness == Correctness::Diverged {
            match analysis.divergence_policy {
                DivergencePolicy::Gate => " — ⚠ results differ (perf verdict N/A)",
                DivergencePolicy::Advisory => " — ⚠ results differ (advisory; perf verdict N/A)",
            }
        } else {
            ""
        };
        body.push_str(&format!(
            "\n<details><summary>{} <code>{}</code>{diverged_note}</summary>\n{op_body}\n</details>\n",
            oa.op_outcome.emoji(),
            html_escape(op)
        ));
    }

    // Assemble: header + verdict line + divergence list + warnings + legend + body.
    let mut out = head;
    let (emoji, headline) = analysis.verdict_line();
    out.push_str(&format!(
        "\n**{} vs {}** — {emoji} {headline}\n",
        md_cell(lb),
        md_cell(la)
    ));
    let diverged = analysis.diverged_ops();
    if !diverged.is_empty() {
        out.push_str(&format!(
            "\n_⚠ {} op(s) with differing results (perf N/A): {}_\n",
            diverged.len(),
            diverged.join(", ")
        ));
    }
    for w in &analysis.warnings {
        out.push_str(&format!("\n> ⚠ {}\n", md_cell(w)));
    }
    out.push_str(match analysis.divergence_policy {
        DivergencePolicy::Gate => {
            "\n🟢 = faster or within budget · 🔴 = slower than budget **or** results differ · \
             N/A = no perf verdict. Only **p50** is gated — the `context:` line (p90/p99 · throughput) \
             and `Δms` are informational, never part of the verdict. Non-blocking.\n"
        }
        DivergencePolicy::Advisory => {
            "\n🟢 = faster or within budget · 🔴 = slower than budget · ⚠ = results differ \
             (advisory — the engines did different work, so perf is N/A) · N/A = no perf verdict. \
             Only **p50** is gated — the `context:` line (p90/p99 · throughput) and `Δms` are \
             informational, never part of the verdict. Non-blocking.\n"
        }
    });
    out.push_str(&body);
    out
}

/// Render one op × cache-mode regression table (rows = this mode's cells) with a verdict column.
fn render_regression_mode(
    out: &mut String,
    oa: &OpAnalysis,
    mode: CacheMode,
    policy: DivergencePolicy,
    la: &str,
    lb: &str,
) {
    let cells: Vec<&CellAnalysis> =
        oa.cells.iter().filter(|c| c.cache_mode == mode).collect();
    if cells.is_empty() {
        return;
    }
    out.push_str(&format!("\n_{}_\n\n", mode.label()));
    out.push_str(&format!(
        "| C | {} p50 (ms) | {} p50 (ms) | Δp50 (Δms) | p50 guard (>% AND >ms) | verdict |\n\
         |---:|---:|---:|---:|:--:|:--:|\n",
        md_cell(la),
        md_cell(lb),
    ));
    for cell in cells {
        let a_cell = latency_cell(cell.baseline_p50_ms, cell.context.baseline.as_ref());
        let b_cell = latency_cell(cell.candidate_p50_ms, cell.context.candidate.as_ref());
        // Gated delta: p50 % change + signed absolute ms change (so the ms floor is auditable).
        // Present exactly when both p50s are valid — otherwise `—` (an absolute Δ would mislead).
        let dp50 = match (cell.delta_pct, cell.delta_ms) {
            (Some(pct), Some(ms)) => format!("{pct:+.1}% ({ms:+.3})"),
            _ => "—".to_string(),
        };
        // The configured guard for this exact cell.
        let guard = cell.budget.guard_cell();
        // A diverged op's cells are N/A with the policy's severity marker; otherwise the p50
        // verdict computed in the model.
        let verdict = if oa.correctness == Correctness::Diverged {
            match policy {
                DivergencePolicy::Gate => "🔴 N/A",
                DivergencePolicy::Advisory => "⚠ N/A",
            }
        } else {
            cell.perf_verdict.emoji()
        };
        out.push_str(&format!(
            "| {} | {a_cell} | {b_cell} | {dp50} | {guard} | {verdict} |\n",
            cell.concurrency
        ));
    }
}

// -------------------------------------------------------------------------------------------------
// Lean, machine-usable summary (design §3.5 "lean PR comment" / Decision 5).
//
// The full `regression_markdown` report is authoritative but too big to embed in a PR comment for
// the full 64-shape corpus (GitHub caps a comment at 65 KB). [`summarize`] distills the *same*
// [`RegressionAnalysis`] model into a compact structure — overall verdict, per-tier 🟢/🔴/⚠/N-A
// counts and the worst offenders — that CI can post inline while hosting the full report
// externally under [`SyntheticSummary::slug`]. Because both renderers consume the same model,
// drift is impossible by construction (a consistency test still pins the two together).
// -------------------------------------------------------------------------------------------------

/// Schema version of the JSON emitted by `report --summary`, bumped on any breaking field change.
/// v2 (design §A5 of `synthetic-three-way-report.md`): adds `budget_profile`,
/// `divergence_policy`, `gated_metric`, `elapsed_secs`, a `diverged` bucket in [`OutcomeCounts`]
/// and the four-state [`OverallVerdict`] as `overall_verdict` (replacing v1's three-state
/// `verdict`).
pub const SUMMARY_SCHEMA_VERSION: u32 = 2;

/// Maximum number of regressed ops listed under "worst offenders" (keeps the comment compact).
const MAX_OFFENDERS: usize = 5;

/// Per-[`crate::synthetic::Tier`] outcome counts (`core` / `full`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierSummary {
    /// The tier tag: `core` or `full`.
    pub tier: String,
    pub counts: OutcomeCounts,
}

/// A regressed op highlighted in the summary's "worst offenders" list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offender {
    pub op: String,
    /// The op's tier tag (`core`/`full`), or `None` for an unknown op.
    pub tier: Option<String>,
    /// Results diverged (correctness) — takes priority over a latency regression.
    pub diverged: bool,
    /// Number of p50 cells over budget.
    pub regressed_cells: usize,
}

/// A compact, machine-usable summary of a `report --diff --regression` comparison: overall verdict,
/// per-tier 🟢/🔴/⚠/N-A counts and the worst offenders — small enough to embed in a PR comment while
/// the full Markdown report is hosted externally and linked by [`slug`](Self::slug). Emitted as JSON
/// by `report --summary`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyntheticSummary {
    pub schema_version: u32,
    pub baseline_label: String,
    pub candidate_label: String,
    /// Stable, filesystem- and anchor-safe id for this comparison (for hosting/linking the full
    /// report). The same pair of runs always yields the same slug.
    pub slug: String,
    /// The budget profile that was applied (`strict` / `cross-engine`).
    pub budget_profile: BudgetProfile,
    /// How divergences were treated (`gate` / `advisory`).
    pub divergence_policy: DivergencePolicy,
    /// The gated metric id (`total_ms.p50`); everything else is informational.
    pub gated_metric: String,
    /// Total wall-clock seconds the caller spent computing the check (`--elapsed-secs`), if given.
    pub elapsed_secs: Option<f64>,
    /// The four-state overall verdict (see [`OverallVerdict`] for the aggregation rule).
    pub overall_verdict: OverallVerdict,
    /// One-line human headline (no leading emoji — [`overall_verdict`](Self::overall_verdict)
    /// carries it).
    pub headline: String,
    /// Present only when `overall_verdict == NotComparable`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub not_comparable_reason: Option<String>,
    pub comparable_cells: usize,
    pub regressed_cells: usize,
    /// Ops whose results differ (correctness divergence), sorted.
    pub diverged_ops: Vec<String>,
    /// Outcome counts across every op.
    pub totals: OutcomeCounts,
    /// Outcome counts split by coverage tier (`core` then `full`).
    pub per_tier: Vec<TierSummary>,
    /// Worst offenders (regressed ops): diverged first, then by cells-over-budget, capped at
    /// [`MAX_OFFENDERS`].
    pub worst_offenders: Vec<Offender>,
}

/// Distill a [`RegressionAnalysis`] into the compact [`SyntheticSummary`]. A pure renderer of the
/// model — verdicts, counts and cell tallies were computed once in
/// [`crate::synthetic::analysis::analyze`], so this can never disagree with
/// [`regression_markdown`].
pub fn summarize(analysis: &RegressionAnalysis) -> SyntheticSummary {
    let (_, headline) = analysis.verdict_line();
    let mut summary = SyntheticSummary {
        schema_version: SUMMARY_SCHEMA_VERSION,
        baseline_label: analysis.comparison.baseline_label.clone(),
        candidate_label: analysis.comparison.candidate_label.clone(),
        slug: analysis.comparison.slug.clone(),
        budget_profile: analysis.budget_profile,
        divergence_policy: analysis.divergence_policy,
        gated_metric: analysis.gated_metric.clone(),
        elapsed_secs: analysis.elapsed_secs,
        overall_verdict: analysis.verdict,
        headline,
        not_comparable_reason: analysis.status.not_comparable_reason().map(str::to_string),
        comparable_cells: analysis.comparable_cells,
        regressed_cells: analysis.regressed_cells,
        diverged_ops: analysis.diverged_ops().iter().map(|s| s.to_string()).collect(),
        totals: analysis.totals,
        per_tier: Vec::new(),
        worst_offenders: Vec::new(),
    };
    if summary.not_comparable_reason.is_some() {
        // Not comparable: the headline carries the reason; there is nothing to tally.
        return summary;
    }

    let mut core = OutcomeCounts::default();
    let mut full = OutcomeCounts::default();
    let mut offenders: Vec<Offender> = Vec::new();
    for (op, oa) in &analysis.ops {
        // Mirror the model's totals: ops with ≥1 cell are tallied, and a cell-less **diverged**
        // op is tallied too (every divergence counts) — it also surfaces as a worst offender
        // below (correctness is the worst signal).
        if !oa.cells.is_empty() || oa.correctness == Correctness::Diverged {
            match oa.tier.as_deref() {
                Some("core") => core.add(oa.op_outcome),
                Some("full") => full.add(oa.op_outcome),
                _ => {}
            }
        }
        if oa.op_outcome == OpOutcome::Regressed {
            offenders.push(Offender {
                op: op.clone(),
                tier: oa.tier.clone(),
                diverged: oa.correctness == Correctness::Diverged,
                regressed_cells: oa
                    .cells
                    .iter()
                    .filter(|c| c.perf_verdict == Verdict::Regressed)
                    .count(),
            });
        }
    }
    // Worst first: correctness divergences, then most cells over budget, then name for stability.
    offenders.sort_by(|a, b| {
        b.diverged
            .cmp(&a.diverged)
            .then(b.regressed_cells.cmp(&a.regressed_cells))
            .then(a.op.cmp(&b.op))
    });
    offenders.truncate(MAX_OFFENDERS);

    summary.per_tier = vec![
        TierSummary { tier: Tier::Core.as_str().to_string(), counts: core },
        TierSummary { tier: Tier::Full.as_str().to_string(), counts: full },
    ];
    summary.worst_offenders = offenders;
    summary
}

impl SyntheticSummary {
    /// The machine-usable artifact written by `report --summary`: pretty-printed JSON. Fallible so
    /// a serialization failure surfaces loudly instead of writing a misleading empty object.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// A compact Markdown rendering for a PR sticky comment (well under GitHub's 65 KB limit): the
    /// verdict headline, a per-tier 🟢/🔴/⚠/N-A table and the worst offenders, ending with the
    /// stable [`slug`](Self::slug) so CI can link the externally-hosted full report.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "### 🧪 Synthetic per-op regression — {} vs {}\n\n",
            md_cell(&self.candidate_label),
            md_cell(&self.baseline_label)
        ));
        out.push_str(&format!(
            "{} {}\n",
            self.overall_verdict.emoji(),
            md_cell(&self.headline)
        ));
        if self.not_comparable_reason.is_some() {
            // NotComparable: the headline already carries the reason; there is nothing to tally.
            out.push_str(&format!("\n_report: {}_\n", self.slug));
            return out;
        }
        out.push_str("\n| tier | 🟢 | 🔴 | ⚠ | N/A |\n|---|---:|---:|---:|---:|\n");
        for t in &self.per_tier {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                md_cell(&t.tier),
                t.counts.pass,
                t.counts.regressed,
                t.counts.diverged,
                t.counts.not_applicable
            ));
        }
        out.push_str(&format!(
            "| **all** | {} | {} | {} | {} |\n",
            self.totals.pass,
            self.totals.regressed,
            self.totals.diverged,
            self.totals.not_applicable
        ));
        if !self.worst_offenders.is_empty() {
            let items: Vec<String> = self
                .worst_offenders
                .iter()
                .map(|o| {
                    let why = if o.diverged {
                        "results differ".to_string()
                    } else {
                        format!("{} cell(s) over budget", o.regressed_cells)
                    };
                    format!("`{}` ({})", md_cell(&o.op), why)
                })
                .collect();
            out.push_str(&format!("\n**Worst offenders:** {}\n", items.join(", ")));
        }
        out.push_str(&format!("\n_report: {}_\n", self.slug));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::analysis::{analyze, AnalysisOptions};
    use crate::synthetic::baseline::RegressionGuard;
    use crate::synthetic::report::{DatasetInfo, MetricSet, Meta, OperationReport, ServerInfo};
    use crate::synthetic::stats::Summary;
    use crate::synthetic::thresholds::Thresholds;
    use std::collections::BTreeMap;

    /// Analyze under the default (gate) policy — what almost every rendering test wants.
    fn analyze_gate(
        a: &Report,
        b: &Report,
        g: &crate::synthetic::baseline::RegressionGuard,
        t: &Thresholds,
    ) -> RegressionAnalysis {
        analyze(a, b, g, t, &AnalysisOptions::default())
    }

    /// Old-signature shim: render the regression Markdown for two reports under gate policy.
    fn regression_md(
        a: &Report,
        b: &Report,
        g: &crate::synthetic::baseline::RegressionGuard,
        t: &Thresholds,
        elapsed_secs: Option<f64>,
    ) -> String {
        let analysis = analyze(
            a,
            b,
            g,
            t,
            &AnalysisOptions { elapsed_secs, ..Default::default() },
        );
        regression_markdown(&analysis)
    }

    /// Old-signature shim: summarize two reports under gate policy.
    fn summarize_gate(
        a: &Report,
        b: &Report,
        g: &crate::synthetic::baseline::RegressionGuard,
        t: &Thresholds,
    ) -> SyntheticSummary {
        summarize(&analyze_gate(a, b, g, t))
    }

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
    fn metrics(median: f64, tput: f64) -> LevelMetrics {
        LevelMetrics {
            throughput_ops_per_sec: tput,
            metrics: MetricSet {
                server_ms: summ(median * 0.2),
                total_ms: summ(median),
                non_internal_ms: summ(median * 0.8),
                cached_false_rate: 0.0,
                cached_unknown: 0,
            },
        }
    }
    fn report(ver: u64, median: f64, tput: f64) -> Report {
        let mut operations = BTreeMap::new();
        operations.insert(
            "match_by_index".to_string(),
            OperationReport {
                levels: vec![LevelReport {
                    concurrency: 1,
                    cached: Some(metrics(median, tput)),
                    uncached: None,
                    compilation_ms_median: None,
                }],
                result_digest: Some("sha256:aa".to_string()),
            },
        );
        Report {
            schema_version: 2,
            meta: Meta {
                tool_version: "0.1.0".to_string(),
                endpoint: "falkor://127.0.0.1:6379".to_string(),
                graph: "g".to_string(),
                samples: 1000,
                warmup: 200,
                concurrency: vec![1],
                seed: 0,
                corpus_size: 256,
                server_timeout_ms: 5000,
                client_deadline_ms: 6000,
                connection: "pool(size=1) per worker".to_string(),
                started_at_epoch_secs: 0,
                server: ServerInfo {
                    module_graph_ver: Some(ver),
                    ..Default::default()
                },
                host: Default::default(),
                dataset: Some(DatasetInfo {
                    seed: 0,
                    nodes: 10,
                    edges: 20,
                    workload_hash: "sha256:abc".to_string(),
                }),
                label: None,
            },
            operations,
        }
    }

    #[test]
    fn diff_renders_deltas_and_identity() {
        let a = report(42001, 1.000, 1000.0);
        let b = report(42002, 1.100, 900.0); // 10% slower, 10% less throughput
        let md = diff_markdown(&a, &b, &["server image changed: x → y".to_string()]);
        assert!(md.contains("Synthetic benchmark diff"));
        assert!(md.contains("4.20.1") && md.contains("4.20.2"));
        assert!(md.contains("## `match_by_index`"));
        assert!(md.contains("cached (plan reused"));
        // p50 delta +10.0%, throughput delta -10.0%.
        assert!(md.contains("+10.0%"), "expected latency +10%: {md}");
        assert!(md.contains("-10.0%"), "expected throughput -10%: {md}");
        assert!(md.contains("⚠ server image changed"));
    }

    #[test]
    fn diff_uses_run_labels_as_headers() {
        let mut a = report(42001, 1.0, 1000.0);
        let mut b = report(42002, 1.1, 900.0);
        a.meta.label = Some("main".to_string());
        b.meta.label = Some("pr".to_string());
        let md = diff_markdown(&a, &b, &[]);
        assert!(md.contains("diff — main → pr"), "title: {md}");
        assert!(md.contains("| main (baseline) | pr (candidate) |"), "header: {md}");
        assert!(md.contains("main total p50") && md.contains("pr tput"), "op header: {md}");
    }

    #[test]
    fn diff_marks_missing_cells() {
        let a = report(42001, 1.0, 1000.0);
        let mut b = report(42002, 1.0, 1000.0);
        // Drop B's only op so A-only ops render with "—".
        b.operations.clear();
        let md = diff_markdown(&a, &b, &[]);
        assert!(md.contains("## `match_by_index`"));
        assert!(md.contains("| 1 | 1.000"), "A cell present");
        assert!(md.contains("| — |") || md.contains(" — "), "B cell missing marker: {md}");
    }

    #[test]
    fn pct_handles_zero_baseline() {
        assert_eq!(pct(0.0, 5.0), "n/a");
        assert_eq!(pct(2.0, 3.0), "+50.0%");
        assert_eq!(pct(2.0, 1.0), "-50.0%");
    }

    #[test]
    fn diff_escapes_table_breaking_cells() {
        let mut a = report(42001, 1.0, 1000.0);
        let mut b = report(42002, 1.0, 1000.0);
        // A pipe in an operator-supplied field must not break the Markdown table.
        a.meta.graph = "left|right".to_string();
        b.meta.graph = "left|right".to_string();
        let md = diff_markdown(&a, &b, &[]);
        assert!(md.contains("left\\|right"), "pipe not escaped: {md}");
        assert!(!md.contains("`left|right`"), "raw pipe leaked into a cell");
    }

    #[test]
    fn regression_marks_within_budget_green_and_over_budget_red() {
        use crate::synthetic::baseline::regression_guard;
        use crate::synthetic::thresholds::Thresholds;
        let a = report(42001, 1.0, 1000.0);
        // within budget: +5% and +0.05 ms (below the 0.5 ms floor) => green
        let b_ok = report(42002, 1.05, 1000.0);
        let g = regression_guard(&a, &b_ok);
        let md = regression_md(&a, &b_ok, &g, &Thresholds::builtin(), None);
        assert!(md.contains("🟢"), "{md}");
        assert!(md.contains("no p50 regression"), "{md}");
        // over budget: +100% and +1 ms => red
        let b_bad = report(42002, 2.0, 500.0);
        let g2 = regression_guard(&a, &b_bad);
        let md2 = regression_md(&a, &b_bad, &g2, &Thresholds::builtin(), None);
        assert!(md2.contains("🔴"), "{md2}");
        assert!(md2.contains("1 of 1 comparable cell(s) over budget"), "{md2}");
    }

    #[test]
    fn regression_marks_diverged_op_na_not_fatal() {
        use crate::synthetic::baseline::regression_guard;
        use crate::synthetic::thresholds::Thresholds;
        let a = report(42001, 1.0, 1000.0);
        let mut b = report(42002, 1.0, 1000.0);
        b.operations.get_mut("match_by_index").unwrap().result_digest =
            Some("sha256:bb".to_string());
        let g = regression_guard(&a, &b);
        let md = regression_md(&a, &b, &g, &Thresholds::builtin(), None);
        assert!(md.contains("results differ"), "{md}");
        assert!(md.contains("🔴 N/A"), "{md}");
        // The top-line summary must be 🔴 (correctness), not a misleading 🟢.
        assert!(md.contains("differing results (correctness)"), "{md}");
        assert!(!md.contains("🟢 no p50 regression"), "summary should not be green: {md}");
    }

    #[test]
    fn regression_not_comparable_when_workload_differs() {
        use crate::synthetic::baseline::regression_guard;
        use crate::synthetic::thresholds::Thresholds;
        let a = report(42001, 1.0, 1000.0);
        let mut b = report(42002, 1.0, 1000.0);
        b.meta.dataset.as_mut().unwrap().workload_hash = "sha256:zzz".to_string();
        let g = regression_guard(&a, &b);
        let md = regression_md(&a, &b, &g, &Thresholds::builtin(), None);
        assert!(md.contains("not comparable"), "{md}");
    }

    #[test]
    fn labels_with_pipes_are_escaped_in_headers() {
        use crate::synthetic::baseline::regression_guard;
        use crate::synthetic::thresholds::Thresholds;
        let mut a = report(42001, 1.0, 1000.0);
        let mut b = report(42002, 1.2, 1000.0);
        a.meta.label = Some("v1|x".to_string());
        b.meta.label = Some("v2|y".to_string());
        // diff headers (field header + per-op header)
        let md = diff_markdown(&a, &b, &[]);
        assert!(md.contains("v1\\|x") && md.contains("v2\\|y"), "diff headers not escaped: {md}");
        // regression headers (field header + per-op header)
        let g = regression_guard(&a, &b);
        let reg = regression_md(&a, &b, &g, &Thresholds::builtin(), None);
        assert!(reg.contains("v1\\|x") && reg.contains("v2\\|y"), "regression headers not escaped: {reg}");
    }

    #[test]
    fn regression_na_cells_are_not_counted_as_comparable() {
        use crate::synthetic::baseline::regression_guard;
        // A zero baseline p50 ⇒ the cell's verdict is N/A; it must NOT inflate the comparable count.
        // With every cell N/A the overall verdict is ⚠ Advisory ("no comparable cells"), never a
        // green pass (design §A1: all-N/A is never green).
        let a = report(42001, 0.0, 1000.0);
        let b = report(42002, 1.0, 1000.0);
        let g = regression_guard(&a, &b);
        let md = regression_md(&a, &b, &g, &Thresholds::builtin(), None);
        assert!(
            md.contains("⚠ no comparable cells — no cell had a valid p50 on both sides"),
            "{md}"
        );
        assert!(!md.contains("🟢 no p50 regression"), "all-N/A must not read as a pass: {md}");
        // An all-N/A op reads as N/A on its collapsed summary, never a green pass.
        assert!(md.contains("N/A <code>match_by_index</code></summary>"), "{md}");
        assert!(!md.contains("🟢 <code>match_by_index</code>"), "{md}");
        // A zero/invalid p50 ⇒ the Δp50 (Δms) cell is `—`, not a misleading `n/a (+…)`.
        assert!(!md.contains("n/a (+"), "no absolute Δ for N/A cells: {md}");
    }

    #[test]
    fn regression_header_shows_thresholds_and_compute_time() {
        use crate::synthetic::baseline::regression_guard;
        use crate::synthetic::thresholds::Thresholds;
        let a = report(42001, 1.0, 1000.0);
        let b = report(42002, 1.0, 1000.0);
        let g = regression_guard(&a, &b);
        // With an elapsed value the compute-time line renders alongside the thresholds settings.
        let md = regression_md(&a, &b, &g, &Thresholds::builtin(), Some(754.0));
        assert!(md.contains("**Thresholds**"), "settings table missing: {md}");
        assert!(md.contains("| _default_ | 10% | 0.5 ms |"), "{md}");
        assert!(md.contains("Budget precedence: per-op×concurrency"), "rule missing: {md}");
        assert!(
            md.contains("⏱ Computed in 12m 34s (benchmark + reporting)."),
            "timing missing: {md}"
        );
        // Without an elapsed value there is no compute-time line.
        let md_none = regression_md(&a, &b, &g, &Thresholds::builtin(), None);
        assert!(!md_none.contains('⏱'), "unexpected timing line: {md_none}");
    }

    #[test]
    fn fmt_duration_secs_formats_ranges() {
        assert_eq!(fmt_duration_secs(0.4), "0.4s");
        assert_eq!(fmt_duration_secs(12.0), "12s");
        assert_eq!(fmt_duration_secs(754.0), "12m 34s");
        assert_eq!(fmt_duration_secs(3723.0), "1h 2m 3s");
        assert_eq!(fmt_duration_secs(-1.0), "n/a");
        assert_eq!(fmt_duration_secs(f64::NAN), "n/a");
    }

    // --- folded layout: per-line guard + non-gated p90/p99 context -----------------------------

    /// Mutate the candidate's cached `total_ms` percentiles in place (keeping p50) so tests can
    /// isolate tail behaviour from the gated p50.
    fn set_tails(r: &mut Report, p90: f64, p99: f64) {
        let m = r
            .operations
            .get_mut("match_by_index")
            .unwrap()
            .levels[0]
            .cached
            .as_mut()
            .unwrap();
        m.metrics.total_ms.p90 = p90;
        m.metrics.total_ms.p99 = p99;
    }

    #[test]
    fn regression_row_folds_context_and_shows_per_line_guard() {
        use crate::synthetic::baseline::regression_guard;
        use crate::synthetic::thresholds::Thresholds;
        let a = report(42001, 1.0, 1000.0);
        let b = report(42002, 1.1, 900.0); // +10% p50 (+0.100 ms), −10% tput
        let g = regression_guard(&a, &b);
        let md = regression_md(&a, &b, &g, &Thresholds::builtin(), None);
        // Header keeps p50 named and adds the guard column.
        assert!(md.contains("p50 (ms)") && md.contains("p50 guard (>% AND >ms)"), "{md}");
        // Δp50 carries the signed absolute ms delta so the floor is auditable.
        assert!(md.contains("(+0.100)"), "Δms missing: {md}");
        // p90/p99 + throughput are folded onto the context line (not their own columns).
        assert!(md.contains("<br><sub>context: p90 ") && md.contains("op/s</sub>"), "{md}");
        // The per-line guard shows the resolved default (10%) + floor.
        assert!(md.contains("10% AND 0.5 ms"), "guard cell: {md}");
        // Legend states the gate is p50-only.
        assert!(md.contains("Only **p50** is gated"), "{md}");
    }

    #[test]
    fn catastrophic_tail_regression_does_not_change_the_p50_verdict() {
        use crate::synthetic::baseline::regression_guard;
        use crate::synthetic::thresholds::Thresholds;
        // Identical p50 on both sides ⇒ green. Baseline unchanged, candidate tails blown up.
        let a = report(42001, 1.0, 1000.0);
        let mut b = report(42002, 1.0, 1000.0);
        set_tails(&mut b, 50.0, 500.0);
        let g = regression_guard(&a, &b);
        let md = regression_md(&a, &b, &g, &Thresholds::builtin(), None);
        // Verdict + comparable count are exactly what they'd be without the tail blow-up.
        assert!(
            md.contains("🟢 no p50 regression beyond budget across 1 comparable cell(s)"),
            "tails must not gate: {md}"
        );
        // …the op's collapsed summary is 🟢 (its p50 didn't regress)…
        assert!(md.contains("🟢 <code>match_by_index</code></summary>"), "{md}");
        // …and the blown-up tail is still shown, as context.
        assert!(md.contains("context: p90 50.000 · p99 500.000"), "{md}");
    }

    #[test]
    fn red_p50_stays_red_even_with_improved_tails() {
        use crate::synthetic::baseline::regression_guard;
        use crate::synthetic::thresholds::Thresholds;
        let a = report(42001, 1.0, 1000.0);
        let mut b = report(42002, 2.0, 500.0); // +100% p50 ⇒ red
        set_tails(&mut b, 0.10, 0.20); // tails *better* than baseline — must not rescue the verdict
        let g = regression_guard(&a, &b);
        let md = regression_md(&a, &b, &g, &Thresholds::builtin(), None);
        assert!(md.contains("🔴 1 of 1 comparable cell(s) over budget"), "{md}");
    }

    #[test]
    fn per_line_guard_reflects_op_override_with_inherited_floor() {
        use crate::synthetic::baseline::regression_guard;
        use crate::synthetic::thresholds::Thresholds;
        // Op override changes the budget; the floor is inherited from [default].
        let t = Thresholds::from_toml_str("[op.match_by_index]\nbudget_pct = 20.0\n").unwrap();
        let a = report(42001, 1.0, 1000.0);
        let b = report(42002, 1.0, 1000.0);
        let g = regression_guard(&a, &b);
        let md = regression_md(&a, &b, &g, &t, None);
        assert!(md.contains("20% AND 0.5 ms"), "resolved override guard: {md}");
    }

    /// A full-size report: every read op × the whole concurrency sweep × both cache modes.
    fn big_report(ver: u64) -> Report {
        let ops = [
            "return_const",
            "match_by_index",
            "match_by_label_scan",
            "expand_1_hop",
            "expand_hops_5",
            "aggregate_count",
            "aggregate_group",
            "shortest_path",
            "property_projection",
        ];
        let sweep = [1usize, 2, 4, 8, 16, 32];
        let mut operations = BTreeMap::new();
        for op in ops {
            let levels = sweep
                .iter()
                .map(|&c| LevelReport {
                    concurrency: c,
                    cached: Some(metrics(0.512, 5000.0)),
                    uncached: Some(metrics(0.987, 3000.0)),
                    compilation_ms_median: None,
                })
                .collect();
            operations.insert(
                op.to_string(),
                OperationReport { levels, result_digest: Some("sha256:aa".to_string()) },
            );
        }
        let mut r = report(ver, 1.0, 1000.0);
        r.operations = operations;
        r
    }

    #[test]
    fn full_report_stays_under_comment_budget() {
        use crate::synthetic::baseline::regression_guard;
        use crate::synthetic::thresholds::Thresholds;
        let a = big_report(1);
        let b = big_report(2);
        let g = regression_guard(&a, &b);
        let md = regression_md(&a, &b, &g, &Thresholds::builtin(), Some(300.0));
        // 9 ops × 6 concurrencies × 2 cache modes = 108 cells. Keep the rendered report well under
        // GitHub's 65_536-char comment cap so the Part-B sticky comment keeps headroom for its
        // wrappers/warnings (see the design's comment-size budget).
        assert!(md.len() < 45_000, "regression report too large: {} bytes", md.len());
        assert!(
            md.contains("<code>shortest_path</code>") && md.contains("<code>return_const</code>"),
            "missing ops"
        );
    }

    #[test]
    fn per_op_sections_are_collapsed_with_verdict_in_summary() {
        use crate::synthetic::baseline::regression_guard;
        use crate::synthetic::thresholds::Thresholds;
        // A regressed op shows 🔴 on its collapsed summary row; the `####` heading is gone.
        let a = report(42001, 1.0, 1000.0);
        let b = report(42002, 2.0, 500.0); // +100% p50 ⇒ regressed
        let g = regression_guard(&a, &b);
        let md = regression_md(&a, &b, &g, &Thresholds::builtin(), None);
        assert!(md.contains("<details><summary>"), "sections must be collapsible: {md}");
        assert!(md.contains("</details>"), "{md}");
        assert!(
            md.contains("🔴 <code>match_by_index</code></summary>"),
            "per-op verdict in the collapsed summary: {md}"
        );
        assert!(!md.contains("#### `match_by_index`"), "old heading should be gone: {md}");
    }

    #[test]
    fn op_name_is_html_escaped_in_the_collapsed_summary() {
        use crate::synthetic::baseline::regression_guard;
        use crate::synthetic::thresholds::Thresholds;
        // A crafted report could carry an op key with HTML-special chars; it must not break markup.
        let evil = "x<b>&y";
        let mut a = report(42001, 1.0, 1000.0);
        let mut b = report(42002, 1.0, 1000.0);
        let va = a.operations.remove("match_by_index").unwrap();
        a.operations.insert(evil.to_string(), va);
        let vb = b.operations.remove("match_by_index").unwrap();
        b.operations.insert(evil.to_string(), vb);
        let g = regression_guard(&a, &b);
        let md = regression_md(&a, &b, &g, &Thresholds::builtin(), None);
        assert!(md.contains("<code>x&lt;b&gt;&amp;y</code>"), "op not HTML-escaped: {md}");
        assert!(!md.contains("<code>x<b>&y"), "raw HTML leaked: {md}");
    }
    // --- Lean summary (Decision 5) ---------------------------------------------------------------

    use crate::synthetic::baseline::regression_guard;

    /// Build a report with one cached level (concurrency 1) per `(op_tag, p50_median, digest)`.
    fn rpt(
        label: &str,
        ver: u64,
        ops: &[(&str, f64, &str)],
    ) -> Report {
        let mut operations = BTreeMap::new();
        for (tag, median, digest) in ops {
            operations.insert(
                (*tag).to_string(),
                OperationReport {
                    levels: vec![LevelReport {
                        concurrency: 1,
                        cached: Some(metrics(*median, 1000.0)),
                        uncached: None,
                        compilation_ms_median: None,
                    }],
                    result_digest: Some((*digest).to_string()),
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
                server: ServerInfo {
                    module_graph_ver: Some(ver),
                    ..Default::default()
                },
                host: Default::default(),
                dataset: Some(DatasetInfo {
                    seed: 0,
                    nodes: 10,
                    edges: 20,
                    workload_hash: "sha256:abc".to_string(),
                }),
                label: Some(label.to_string()),
            },
            operations,
        }
    }

    /// The collapsed-row emoji `regression_markdown` renders for `op` (`🟢`/`🔴`/`⚠`/`N/A`), or
    /// `None` when the op has no collapsed row (skipped for having no cell).
    fn md_collapsed_emoji(
        md: &str,
        op: &str,
    ) -> Option<String> {
        let needle = format!("<code>{}</code>", op);
        md.lines().find_map(|line| {
            let pos = line.find(&needle)?;
            let before = &line[..pos];
            let start = before.rfind("<summary>")? + "<summary>".len();
            Some(before[start..].trim().to_string())
        })
    }

    #[test]
    fn summarize_counts_a_within_budget_op_green() {
        let a = report(42001, 1.0, 1000.0);
        let b = report(42002, 1.05, 1000.0); // +5%, below the 0.5ms floor ⇒ within budget
        let g = regression_guard(&a, &b);
        let s = summarize_gate(&a, &b, &g, &Thresholds::builtin());
        assert_eq!(s.schema_version, SUMMARY_SCHEMA_VERSION);
        assert_eq!(s.overall_verdict, OverallVerdict::Pass);
        assert_eq!(
            s.totals,
            OutcomeCounts {
                pass: 1,
                regressed: 0,
                diverged: 0,
                not_applicable: 0
            }
        );
        assert_eq!(s.comparable_cells, 1);
        assert_eq!(s.regressed_cells, 0);
        assert!(s.diverged_ops.is_empty());
        assert!(s.worst_offenders.is_empty());
        // match_by_index is a Core op.
        let core = s.per_tier.iter().find(|t| t.tier == "core").unwrap();
        assert_eq!(
            core.counts,
            OutcomeCounts {
                pass: 1,
                regressed: 0,
                diverged: 0,
                not_applicable: 0
            }
        );
        let md = s.to_markdown();
        assert!(md.contains("🟢 no p50 regression"), "{md}");
        assert!(md.contains("| **all** | 1 | 0 | 0 |"), "{md}");
        assert!(md.contains(&format!("_report: {}_", s.slug)), "{md}");
    }

    #[test]
    fn summarize_flags_an_over_budget_op_red() {
        let a = report(42001, 1.0, 1000.0);
        let b = report(42002, 2.0, 500.0); // +100%, +1ms ⇒ over budget
        let g = regression_guard(&a, &b);
        let s = summarize_gate(&a, &b, &g, &Thresholds::builtin());
        assert_eq!(s.overall_verdict, OverallVerdict::Regressed);
        assert_eq!(s.regressed_cells, 1);
        assert_eq!(s.comparable_cells, 1);
        assert_eq!(s.totals.regressed, 1);
        assert_eq!(s.worst_offenders.len(), 1);
        let o = &s.worst_offenders[0];
        assert_eq!(o.op, "match_by_index");
        assert!(!o.diverged);
        assert_eq!(o.regressed_cells, 1);
        assert_eq!(o.tier.as_deref(), Some("core"));
        let md = s.to_markdown();
        assert!(
            md.contains("🔴 1 of 1 comparable cell(s) over budget"),
            "{md}"
        );
        assert!(
            md.contains("`match_by_index` (1 cell(s) over budget)"),
            "{md}"
        );
    }

    #[test]
    fn summarize_marks_a_diverged_op_red_without_counting_its_cells() {
        let a = report(42001, 1.0, 1000.0);
        let mut b = report(42002, 1.0, 1000.0);
        b.operations
            .get_mut("match_by_index")
            .unwrap()
            .result_digest = Some("sha256:bb".to_string());
        let g = regression_guard(&a, &b);
        let s = summarize_gate(&a, &b, &g, &Thresholds::builtin());
        assert_eq!(s.overall_verdict, OverallVerdict::Regressed);
        assert_eq!(s.diverged_ops, vec!["match_by_index".to_string()]);
        // A diverged op's cells are NOT counted (they render as 🔴 N/A), mirroring the full report.
        assert_eq!(s.comparable_cells, 0);
        assert_eq!(s.regressed_cells, 0);
        assert_eq!(s.totals.regressed, 1);
        assert_eq!(s.worst_offenders.len(), 1);
        assert!(s.worst_offenders[0].diverged);
        let md = s.to_markdown();
        assert!(md.contains("differing results (correctness)"), "{md}");
        assert!(md.contains("`match_by_index` (results differ)"), "{md}");
    }

    #[test]
    fn summarize_not_comparable_yields_empty_tallies() {
        let a = report(42001, 1.0, 1000.0);
        let mut b = report(42002, 1.0, 1000.0);
        b.meta.dataset.as_mut().unwrap().workload_hash = "sha256:zzz".to_string();
        let g = regression_guard(&a, &b);
        let s = summarize_gate(&a, &b, &g, &Thresholds::builtin());
        assert_eq!(s.overall_verdict, OverallVerdict::NotComparable);
        assert!(s.not_comparable_reason.is_some());
        assert!(s.headline.starts_with("not comparable"));
        assert!(s.per_tier.is_empty());
        assert!(s.worst_offenders.is_empty());
        assert_eq!(s.totals, OutcomeCounts::default());
        let md = s.to_markdown();
        assert!(md.contains("⚠ not comparable"), "{md}");
        assert!(
            !md.contains("| tier |"),
            "no tier table when not comparable: {md}"
        );
        assert!(md.contains(&format!("_report: {}_", s.slug)), "{md}");
    }

    #[test]
    fn summarize_splits_counts_by_tier() {
        // Core op within budget (green) + Full op over budget (red).
        let a = rpt(
            "main",
            42001,
            &[("match_by_index", 1.0, "d1"), ("expand_hops_5", 1.0, "d2")],
        );
        let b = rpt(
            "pr",
            42002,
            &[("match_by_index", 1.05, "d1"), ("expand_hops_5", 2.0, "d2")],
        );
        let g = regression_guard(&a, &b);
        let s = summarize_gate(&a, &b, &g, &Thresholds::builtin());
        assert_eq!(
            s.totals,
            OutcomeCounts {
                pass: 1,
                regressed: 1,
                diverged: 0,
                not_applicable: 0
            }
        );
        let core = s.per_tier.iter().find(|t| t.tier == "core").unwrap();
        let full = s.per_tier.iter().find(|t| t.tier == "full").unwrap();
        assert_eq!(
            core.counts,
            OutcomeCounts {
                pass: 1,
                regressed: 0,
                diverged: 0,
                not_applicable: 0
            }
        );
        assert_eq!(
            full.counts,
            OutcomeCounts {
                pass: 0,
                regressed: 1,
                diverged: 0,
                not_applicable: 0
            }
        );
        let md = s.to_markdown();
        assert!(md.contains("| core | 1 | 0 | 0 |"), "{md}");
        assert!(md.contains("| full | 0 | 1 | 0 |"), "{md}");
    }

    #[test]
    fn summarize_gates_unknown_tag_ops_but_excludes_them_from_tier_counts() {
        // A string-keyed op known to neither the catalog nor the repo read shape registry still
        // resolves a budget (`[default]` fallback) so it gets a real verdict, but it has no tier:
        // it counts in the totals yet toward neither `core` nor `full`.
        let a = rpt("main", 42001, &[("mystery_op", 1.0, "d1")]);
        let b = rpt("pr", 42002, &[("mystery_op", 1.0, "d1")]);
        let g = regression_guard(&a, &b);
        let s = summarize_gate(&a, &b, &g, &Thresholds::builtin());
        assert_eq!(
            s.totals,
            OutcomeCounts {
                pass: 1,
                regressed: 0,
                diverged: 0,
                not_applicable: 0
            }
        );
        let core = s.per_tier.iter().find(|t| t.tier == "core").unwrap();
        let full = s.per_tier.iter().find(|t| t.tier == "full").unwrap();
        assert_eq!(core.counts, OutcomeCounts::default());
        assert_eq!(full.counts, OutcomeCounts::default());
        assert!(s.worst_offenders.is_empty());
    }

    #[test]
    fn dynamic_repo_read_shape_gets_budget_and_tier_end_to_end() {
        // The full A0 path (design §4 of synthetic-three-way-report.md): a recorded repo read
        // shape — a dynamic op name that is NOT a catalog `OpName` — gets its `[op.*]` TOML
        // override applied in the rendered report and rolls up into its registry tier.
        let toml = "[op.single_vertex_read]\nbudget_pct = 50.0\nfloor_ms = 0.1\n";
        let t = Thresholds::from_toml_str(toml).unwrap();
        // 2.0 → 2.6 ms = +30%, Δ0.6 ms: within the 50% override (pass), but over the strict
        // built-in default (10% AND 0.5 ms) — proving the override, not the default, was applied.
        let a = rpt("main", 42001, &[("single_vertex_read", 2.0, "d1")]);
        let b = rpt("pr", 42002, &[("single_vertex_read", 2.6, "d1")]);
        let g = regression_guard(&a, &b);
        let md = regression_md(&a, &b, &g, &t, None);
        assert!(md.contains("50% AND 0.1 ms"), "guard column shows the override:\n{md}");
        assert!(md.contains("🟢 <code>single_vertex_read</code>"), "{md}");
        let s = summarize_gate(&a, &b, &g, &t);
        assert_eq!(s.totals.pass, 1, "{s:?}");
        // `single_vertex_read` is a Tier::Core repo read shape (shapes.rs registry).
        let core = s.per_tier.iter().find(|t| t.tier == "core").unwrap();
        assert_eq!(core.counts.pass, 1, "{s:?}");
        // Same inputs under the strict built-in default (10% AND 0.5 ms): +30%/Δ0.6 ms regresses,
        // and the offender carries the registry tier.
        let strict = summarize_gate(&a, &b, &g, &Thresholds::builtin());
        assert_eq!(strict.totals.regressed, 1, "{strict:?}");
        assert_eq!(strict.worst_offenders[0].tier.as_deref(), Some("core"), "{strict:?}");
    }

    #[test]
    fn summarize_and_regression_markdown_agree_on_every_op() {
        // One green (Core), one red (Full), one diverged (Core), one N/A (Core, zero baseline).
        let a = rpt(
            "main",
            42001,
            &[
                ("match_by_index", 1.0, "d1"),
                ("expand_hops_5", 1.0, "d2"),
                ("aggregate_count", 1.0, "d3"),
                ("return_const", 0.0, "d4"),
            ],
        );
        let b = rpt(
            "pr",
            42002,
            &[
                ("match_by_index", 1.05, "d1"),
                ("expand_hops_5", 2.0, "d2"),
                ("aggregate_count", 1.0, "d3-diff"),
                ("return_const", 1.0, "d4"),
            ],
        );
        let th = Thresholds::builtin();
        let g = regression_guard(&a, &b);
        let analysis = analyze_gate(&a, &b, &g, &th);
        let md = regression_markdown(&analysis);
        let s = summarize(&analysis);

        // Per-op: the model's outcome emoji must equal the collapsed-row emoji in the report.
        for op in [
            "match_by_index",
            "expand_hops_5",
            "aggregate_count",
            "return_const",
        ] {
            let outcome = analysis.ops[op].op_outcome;
            assert_eq!(
                md_collapsed_emoji(&md, op).as_deref(),
                Some(outcome.emoji()),
                "op {op} emoji mismatch"
            );
        }
        // Top line: the reconstructed "{emoji} {headline}" must appear verbatim in the full report.
        assert!(
            md.contains(&format!("{} {}", s.overall_verdict.emoji(), s.headline)),
            "headline mismatch\nsummary: {} {}\nmd: {md}",
            s.overall_verdict.emoji(),
            s.headline
        );
    }

    #[test]
    fn summarize_caps_and_orders_worst_offenders() {
        // Six over-budget ops + one diverged op ⇒ capped at MAX_OFFENDERS with the divergence first.
        let over = [
            "aggregate_count",
            "aggregate_group",
            "expand_1_hop",
            "expand_hops_5",
            "match_by_index",
            "match_by_label_scan",
        ];
        let mut a_ops: Vec<(&str, f64, &str)> = over.iter().map(|op| (*op, 1.0, "same")).collect();
        let mut b_ops: Vec<(&str, f64, &str)> = over.iter().map(|op| (*op, 2.0, "same")).collect();
        a_ops.push(("property_projection", 1.0, "d"));
        b_ops.push(("property_projection", 1.0, "d-diff")); // diverged
        let a = rpt("main", 42001, &a_ops);
        let b = rpt("pr", 42002, &b_ops);
        let g = regression_guard(&a, &b);
        let s = summarize_gate(&a, &b, &g, &Thresholds::builtin());
        assert_eq!(s.worst_offenders.len(), 5, "capped at MAX_OFFENDERS");
        assert_eq!(
            s.worst_offenders[0].op, "property_projection",
            "divergence sorts first"
        );
        assert!(s.worst_offenders[0].diverged);
        // The remaining four are the alphabetically-first over-budget ops.
        let rest: Vec<&str> = s.worst_offenders[1..]
            .iter()
            .map(|o| o.op.as_str())
            .collect();
        assert_eq!(
            rest,
            vec![
                "aggregate_count",
                "aggregate_group",
                "expand_1_hop",
                "expand_hops_5"
            ]
        );
    }

    #[test]
    fn summarize_surfaces_a_diverged_op_with_no_measured_cell() {
        // An op present in both runs but with empty levels: tallied (every divergence counts,
        // design v5) and surfaced as a worst offender because its results diverged.
        let mut a = rpt("main", 42001, &[("match_by_index", 1.0, "d1")]);
        let mut b = rpt("pr", 42002, &[("match_by_index", 1.0, "d2")]);
        a.operations
            .get_mut("match_by_index")
            .unwrap()
            .levels
            .clear();
        b.operations
            .get_mut("match_by_index")
            .unwrap()
            .levels
            .clear();
        let g = regression_guard(&a, &b);
        let s = summarize_gate(&a, &b, &g, &Thresholds::builtin());
        assert_eq!(s.overall_verdict, OverallVerdict::Regressed);
        assert_eq!(
            s.totals,
            OutcomeCounts { regressed: 1, ..OutcomeCounts::default() },
            "a cell-less diverged op is tallied (gate ⇒ regressed)"
        );
        assert_eq!(
            s.per_tier.iter().find(|t| t.tier == "core").unwrap().counts.regressed,
            1,
            "…and rolls into its tier"
        );
        assert_eq!(s.worst_offenders.len(), 1);
        assert_eq!(s.worst_offenders[0].op, "match_by_index");
        assert!(s.worst_offenders[0].diverged);
    }

    #[test]
    fn summary_slug_is_stable_and_derived_from_labels_and_hash() {
        let a = report(42001, 1.0, 1000.0);
        let mut b = report(42002, 1.0, 1000.0);
        b.meta.label = Some("release 2.0".to_string());
        b.meta.dataset.as_mut().unwrap().workload_hash = "sha256:abc".to_string();
        let g = regression_guard(&a, &b);
        let s1 = summarize_gate(&a, &b, &g, &Thresholds::builtin());
        let s2 = summarize_gate(&a, &b, &g, &Thresholds::builtin());
        assert_eq!(s1.slug, s2.slug, "same inputs ⇒ same slug");
        assert!(
            s1.slug.starts_with("synthetic-release-2-0-vs-"),
            "{}",
            s1.slug
        );
        assert!(s1.slug.ends_with("-abc"), "digest suffix: {}", s1.slug);
    }

    #[test]
    fn synthetic_summary_json_round_trips() {
        let a = report(42001, 1.0, 1000.0);
        let b = report(42002, 2.0, 500.0);
        let g = regression_guard(&a, &b);
        let s = summarize_gate(&a, &b, &g, &Thresholds::builtin());
        let json = s.to_json().unwrap();
        let back: SyntheticSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
        // Machine-usable: snake_case verdict/profile/policy tokens.
        assert!(json.contains("\"overall_verdict\": \"regressed\""), "{json}");
        assert!(json.contains("\"budget_profile\": \"strict\""), "{json}");
        assert!(json.contains("\"divergence_policy\": \"gate\""), "{json}");
        assert!(json.contains("\"gated_metric\": \"total_ms.p50\""), "{json}");
        // No `--elapsed-secs` ⇒ explicit null (the field is always present).
        assert!(json.contains("\"elapsed_secs\": null"), "{json}");
    }

    #[test]
    fn summary_v2_freezes_the_top_level_field_set() {
        // The summary JSON is a machine contract (schema v2). This test freezes the exact
        // top-level field set — adding/renaming/removing a field must bump
        // SUMMARY_SCHEMA_VERSION and update this list deliberately.
        let a = report(42001, 1.0, 1000.0);
        let b = report(42002, 2.0, 500.0);
        let g = regression_guard(&a, &b);
        let s = summarize_gate(&a, &b, &g, &Thresholds::builtin());
        let value: serde_json::Value = serde_json::from_str(&s.to_json().unwrap()).unwrap();
        let mut keys: Vec<&str> =
            value.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "baseline_label",
                "budget_profile",
                "candidate_label",
                "comparable_cells",
                "diverged_ops",
                "divergence_policy",
                "elapsed_secs",
                "gated_metric",
                "headline",
                "overall_verdict",
                "per_tier",
                "regressed_cells",
                "schema_version",
                "slug",
                "totals",
                "worst_offenders",
            ]
        );
        assert_eq!(value["schema_version"], 2);
        // The outcome-counts shape (shared by totals and per_tier) is part of the contract too.
        let mut count_keys: Vec<&str> =
            value["totals"].as_object().unwrap().keys().map(String::as_str).collect();
        count_keys.sort_unstable();
        assert_eq!(count_keys, vec!["diverged", "not_applicable", "pass", "regressed"]);
    }

    #[test]
    fn summary_carries_elapsed_secs_when_given() {
        let a = report(42001, 1.0, 1000.0);
        let b = report(42002, 1.0, 1000.0);
        let g = regression_guard(&a, &b);
        let analysis = analyze(
            &a,
            &b,
            &g,
            &Thresholds::builtin(),
            &AnalysisOptions { elapsed_secs: Some(754.5), ..Default::default() },
        );
        let s = summarize(&analysis);
        assert_eq!(s.elapsed_secs, Some(754.5));
        assert!(s.to_json().unwrap().contains("\"elapsed_secs\": 754.5"));
    }

    // --- Advisory divergence policy (design §A3) ------------------------------------------------

    /// Analyze under the advisory divergence policy.
    fn analyze_advisory(
        a: &Report,
        b: &Report,
        g: &RegressionGuard,
        t: &Thresholds,
    ) -> RegressionAnalysis {
        analyze(
            a,
            b,
            g,
            t,
            &AnalysisOptions {
                divergence_policy: DivergencePolicy::Advisory,
                ..Default::default()
            },
        )
    }

    #[test]
    fn advisory_policy_renders_diverged_op_as_warning_not_failure() {
        // Two ops: one clean pass, one diverged. Under `advisory` the diverged op is ⚠ (not 🔴),
        // its perf cells stay N/A, and the overall verdict caps at Advisory.
        let a = rpt("main", 42001, &[("match_by_index", 1.0, "d1"), ("aggregate_count", 1.0, "d2")]);
        let b = rpt("pr", 42002, &[("match_by_index", 1.0, "d1"), ("aggregate_count", 1.0, "d2-diff")]);
        let g = regression_guard(&a, &b);
        let analysis = analyze_advisory(&a, &b, &g, &Thresholds::builtin());
        let md = regression_markdown(&analysis);
        // Top line: ⚠ pass-with-divergence, not a red failure.
        assert!(
            md.contains("⚠ pass, 1 diverged — no p50 regression beyond budget across 1 comparable cell(s)"),
            "{md}"
        );
        assert!(md.contains("divergence is advisory under this policy"), "{md}");
        // The diverged op's collapsed row is ⚠ with an advisory note; its cells are "⚠ N/A".
        assert_eq!(md_collapsed_emoji(&md, "aggregate_count").as_deref(), Some("⚠"), "{md}");
        assert!(md.contains("results differ (advisory; perf verdict N/A)"), "{md}");
        assert!(md.contains("⚠ N/A"), "{md}");
        assert!(!md.contains("🔴 N/A"), "no red N/A under advisory: {md}");
        // The clean op still passes.
        assert_eq!(md_collapsed_emoji(&md, "match_by_index").as_deref(), Some("🟢"), "{md}");
        // Legend explains ⚠.
        assert!(md.contains("⚠ = results differ"), "{md}");
    }

    #[test]
    fn advisory_policy_summary_counts_divergence_in_its_own_bucket() {
        let a = rpt("main", 42001, &[("match_by_index", 1.0, "d1"), ("aggregate_count", 1.0, "d2")]);
        let b = rpt("pr", 42002, &[("match_by_index", 1.0, "d1"), ("aggregate_count", 1.0, "d2-diff")]);
        let g = regression_guard(&a, &b);
        let analysis = analyze_advisory(&a, &b, &g, &Thresholds::builtin());
        let s = summarize(&analysis);
        assert_eq!(s.overall_verdict, OverallVerdict::Advisory);
        assert_eq!(s.divergence_policy, DivergencePolicy::Advisory);
        // Diverged is its own bucket — never `regressed`.
        assert_eq!(
            s.totals,
            OutcomeCounts { pass: 1, regressed: 0, diverged: 1, not_applicable: 0 }
        );
        assert_eq!(s.diverged_ops, vec!["aggregate_count".to_string()]);
        // Advisory-diverged ops are not "worst offenders" (nothing regressed).
        assert!(s.worst_offenders.is_empty(), "{s:?}");
        let md = s.to_markdown();
        assert!(md.contains("⚠ pass, 1 diverged"), "{md}");
        // Tier table gains the ⚠ column; both ops are Core.
        assert!(md.contains("| tier | 🟢 | 🔴 | ⚠ | N/A |"), "{md}");
        assert!(md.contains("| core | 1 | 0 | 1 | 0 |"), "{md}");
        assert!(md.contains("| **all** | 1 | 0 | 1 | 0 |"), "{md}");
    }

    #[test]
    fn gate_policy_is_the_default_and_keeps_divergence_red() {
        // Same reports as the advisory test, but under the default (gate) policy: 🔴 everywhere.
        let a = rpt("main", 42001, &[("aggregate_count", 1.0, "d2")]);
        let b = rpt("pr", 42002, &[("aggregate_count", 1.0, "d2-diff")]);
        let g = regression_guard(&a, &b);
        let analysis = analyze_gate(&a, &b, &g, &Thresholds::builtin());
        assert_eq!(analysis.divergence_policy, DivergencePolicy::Gate);
        assert_eq!(analysis.verdict, OverallVerdict::Regressed);
        let md = regression_markdown(&analysis);
        assert!(md.contains("🔴 N/A"), "{md}");
        assert!(md.contains("results differ (perf verdict N/A)"), "{md}");
        assert!(!md.contains("advisory"), "gate output must not mention advisory: {md}");
    }

    #[test]
    fn zero_comparable_cells_is_advisory_not_green() {
        // All-N/A comparison (zero baseline p50): verdict is Advisory with the no-cells wording.
        let a = report(42001, 0.0, 1000.0);
        let b = report(42002, 1.0, 1000.0);
        let g = regression_guard(&a, &b);
        let analysis = analyze_gate(&a, &b, &g, &Thresholds::builtin());
        assert_eq!(analysis.verdict, OverallVerdict::Advisory);
        let s = summarize(&analysis);
        assert_eq!(s.overall_verdict, OverallVerdict::Advisory);
        assert!(s.headline.starts_with("no comparable cells"), "{}", s.headline);
        let md = s.to_markdown();
        assert!(md.contains("⚠ no comparable cells"), "{md}");
    }
}
