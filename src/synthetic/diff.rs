//! Cross-run **diff** report: render two synthetic [`Report`]s side by side across every op, cache
//! mode and concurrency level (throughput + total-latency p50/p90/p95/p99 with per-metric deltas),
//! as pasteable Markdown. Used by `synthetic report --diff` after the [`crate::synthetic::baseline`]
//! guard confirms the two runs measured the same workload.
//!
//! The **regression** flavors ([`regression_markdown`], [`summarize`]) are pure renderers of the
//! [`RegressionAnalysis`] model built by [`crate::synthetic::analysis::analyze`] — the verdicts
//! are computed once, there.

use crate::synthetic::analysis::{
    CacheMode, CellAnalysis, CellContextSide, Correctness, DivergencePolicy, GatedMetric,
    OpAnalysis, OpOutcome, OutcomeCounts, OverallVerdict, RegressionAnalysis,
};
use crate::synthetic::provenance::decode_module_version;
use crate::synthetic::report::{html_escape, md_cell, md_inline, LevelMetrics, LevelReport, Report};
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
    // §6.3 attestation, surfaced per side: a write-bundle replay that ran the correctness tier
    // says so here; "—" on a write run means latency tier only (see the guard's warnings).
    row2(
        &mut out,
        "outcome oracle",
        &oracle_cell(baseline.meta.oracle_verified.as_ref()),
        &oracle_cell(candidate.meta.oracle_verified.as_ref()),
    );

    out.push_str(
        "\n_Δ is 100·(candidate−baseline)/baseline. Latency percentiles and Δp50 are the \
         **server-reported execution time** (`server_ms`); the client-observed total p50 rides \
         along as an informational sub-line. **Latency: lower is better** (a positive Δ = \
         slower / regressed); **throughput: higher is better**. Each side's `n / σ (ms) / CV` \
         describes its own **within-run** dispersion of `server_ms` (not run-to-run noise): \
         `n` = samples retained after severe-outlier removal (pooled across the C workers), \
         `σ` = their **sample** standard deviation (n−1 denominator), `CV` = 100·σ/mean; when \
         only part of the retained cohort carries a server time (older engines), `n (server m)` \
         names both counts. `—` = not measured in that run (or, in a latency/σ/CV column, no \
         valid server time on that side — e.g. a report predating server-time capture or an \
         engine that doesn't report execution time). Each op's `example query` block shows its \
         first measured command (cached-mode base text; uncached appends a unique cache-buster \
         comment)._\n",
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
    let (la, lb) = (col_label(baseline, "A"), col_label(candidate, "B"));
    for op in ops {
        // Op names are report content in an inline-Markdown context (unlike the regression
        // renderer's raw-HTML <summary>, Markdown stays active here) — a backtick or newline in a
        // name could terminate the span or the heading, so render <code> with the full chain.
        out.push_str(&format!("\n## <code>{}</code>\n", md_cell(&md_inline(&html_escape(op)))));
        // A capability-skipped op has no levels, so its tables render empty — say why instead
        // (design Phase 6 §3.5), per side; reasons are manifest content, hence HTML-escaped.
        if let Some(note) = diff_skip_note(baseline, candidate, op, &la, &lb) {
            out.push_str(&format!("\n_{}_\n", md_cell(&md_inline(&html_escape(&note)))));
        }
        // The op's deterministic representative query, collapsed so the diff stays compact.
        // Both sides of a comparable pair measured the same workload; candidate (the run under
        // test) wins when both carry a text.
        if let Some(example) = [candidate, baseline]
            .iter()
            .find_map(|r| r.operations.get(op).and_then(|o| o.example_query.as_deref()))
        {
            out.push_str(&example_query_block(example));
        }
        for mode in [Mode::Cached, Mode::Uncached] {
            render_mode(&mut out, baseline, candidate, op, mode);
        }
    }
    out
}

/// The ⏭ note for a per-side capability skip in the plain diff — same four cases as the
/// regression report's [`skip_note`], derived straight from the two [`Report`]s.
fn diff_skip_note(
    a: &Report,
    b: &Report,
    op: &str,
    la: &str,
    lb: &str,
) -> Option<String> {
    let side = |r: &Report| r.operations.get(op).and_then(|o| o.skipped.clone());
    match (side(a), side(b)) {
        (Some(ra), Some(rb)) if ra == rb => Some(format!("⏭ skipped on both sides — {ra}")),
        (Some(ra), Some(rb)) => {
            Some(format!("⏭ skipped on both sides — {la}: {ra}; {lb}: {rb}"))
        }
        (Some(ra), None) => Some(format!("⏭ skipped on {la} — {ra}; measured on {lb} only")),
        (None, Some(rb)) => Some(format!("⏭ skipped on {lb} — {rb}; measured on {la} only")),
        (None, None) => None,
    }
}

/// The display name for a run's column: its `--label` if set, else the caller-supplied `fallback`
/// (`A`/`B` for `diff_markdown`; `baseline`/`candidate` for the regression report).
fn col_label(r: &Report, fallback: &str) -> String {
    r.meta.label.clone().unwrap_or_else(|| fallback.to_string())
}

/// One side's §6.3 oracle-attestation cell: `"verified — 7 op(s), 1792 outcome(s)"` when the
/// replay re-verified a write outcome oracle, `"—"` otherwise (read run or latency tier only).
fn oracle_cell(att: Option<&std::collections::BTreeMap<String, usize>>) -> String {
    att.map_or_else(
        || "—".to_string(),
        |m| {
            format!("verified — {} op(s), {} outcome(s)", m.len(), m.values().sum::<usize>())
        },
    )
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
        "| C | {la} server p50/p90/p95/p99 (ms) | {la} n / σ (ms) / CV | {lb} server p50/p90/p95/p99 (ms) | {lb} n / σ (ms) / CV | Δp50 | {la} tput (ops/s) | {lb} tput (ops/s) | Δtput |\n\
         |---:|---|---:|---|---:|---:|---:|---:|---:|\n",
    ));
    for c in levels {
        let am = level_metrics(a, op, c, mode);
        let bm = level_metrics(b, op, c, mode);
        let a_pct = am.map(percentiles).unwrap_or_else(|| "—".to_string());
        let b_pct = bm.map(percentiles).unwrap_or_else(|| "—".to_string());
        let a_disp = am.map(dispersion_cell).unwrap_or_else(|| "—".to_string());
        let b_disp = bm.map(dispersion_cell).unwrap_or_else(|| "—".to_string());
        let dp50 = match (am.map(server_median), bm.map(server_median)) {
            (Some(Some(x)), Some(Some(y))) => pct(x, y),
            _ => "—".to_string(),
        };
        let a_tp = am.map(|m| format!("{:.0}", m.throughput_ops_per_sec)).unwrap_or_else(|| "—".to_string());
        let b_tp = bm.map(|m| format!("{:.0}", m.throughput_ops_per_sec)).unwrap_or_else(|| "—".to_string());
        let dtp = match (am, bm) {
            (Some(x), Some(y)) => pct(x.throughput_ops_per_sec, y.throughput_ops_per_sec),
            _ => "—".to_string(),
        };
        out.push_str(&format!(
            "| {c} | {a_pct} | {a_disp} | {b_pct} | {b_disp} | {dp50} | {a_tp} | {b_tp} | {dtp} |\n"
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

/// A side's valid server-time median: `Some` iff finite and positive. A non-positive or
/// non-finite value means the side carries no usable server time (a report predating
/// server-time capture, or an engine that doesn't report execution time) — no silent fallback.
fn server_median(m: &LevelMetrics) -> Option<f64> {
    let v = m.metrics.server_ms.median;
    (v.is_finite() && v > 0.0).then_some(v)
}

/// A diff-table latency cell: server-time percentiles on the primary line (`—` when the side
/// has no valid server time — see [`server_median`]), with the client-observed total p50
/// demoted to a `sub` line whenever it is valid — mirroring the regression report's demotion.
fn percentiles(m: &LevelMetrics) -> String {
    let primary = if server_median(m).is_some() {
        let s = &m.metrics.server_ms;
        format!("{:.3} / {:.3} / {:.3} / {:.3}", s.median, s.p90, s.p95, s.p99)
    } else {
        "—".to_string()
    };
    let t = m.metrics.total_ms.median;
    if t.is_finite() && t > 0.0 {
        format!("{primary}<br><sub>total p50 {t:.3}</sub>")
    } else {
        primary
    }
}

/// A diff-table `n / σ (ms) / CV` cell: the retained-sample count with the **within-run**
/// sample standard deviation and coefficient of variation of `server_ms` — see
/// [`format_dispersion`]. σ/CV degrade to `—` exactly like the server latency columns when the
/// side carries no valid server time.
fn dispersion_cell(m: &LevelMetrics) -> String {
    let server_valid = server_median(m).is_some();
    let server = &m.metrics.server_ms;
    format_dispersion(
        " / ",
        m.metrics.total_ms.n,
        server_valid.then_some(server.n),
        if server_valid { server.sample_stddev() } else { None },
        if server_valid { server.cv_pct() } else { None },
    )
}

/// Format one side's within-run dispersion stats (`n`, sample σ of `server_ms` in ms, CV%),
/// joined by `sep`. `n` is the retained `total_ms` cohort (the always-captured wall clock);
/// when the server-time cohort exists but differs — possible only on foreign/older reports —
/// both counts are named (`n (server m)`). Undefined σ/CV (no valid server time, or n < 2)
/// render `—`, mirroring the server latency columns.
fn format_dispersion(
    sep: &str,
    n: usize,
    n_server: Option<usize>,
    stddev_ms: Option<f64>,
    cv_pct: Option<f64>,
) -> String {
    let n_cell = match n_server {
        Some(ns) if ns != n => format!("{n} (server {ns})"),
        _ => n.to_string(),
    };
    let sd = stddev_ms.map(|s| format!("{s:.3}")).unwrap_or_else(|| "—".to_string());
    let cv = cv_pct.map(|c| format!("{c:.1}%")).unwrap_or_else(|| "—".to_string());
    format!("{n_cell}{sep}{sd}{sep}{cv}")
}

/// Maximum number of characters of an example query rendered into the Markdown reports. The
/// full corpus is ~64 ops and the regression report doubles as a sticky PR comment capped at
/// ~65 KB by GitHub, so very long recorded texts are cut with an explicit truncation note.
const EXAMPLE_QUERY_MAX_CHARS: usize = 600;

/// A collapsed `<details>` block showing an op's deterministic example query as a fenced
/// `cypher` code block. The fence is made longer than any backtick run inside the text (Cypher
/// identifiers may be backtick-quoted, and a recorded text could otherwise close the fence), and
/// texts beyond [`EXAMPLE_QUERY_MAX_CHARS`] are truncated with a note naming both lengths.
/// Rendering is pure — the same text always produces the same block.
fn example_query_block(text: &str) -> String {
    let total = text.chars().count();
    let (shown, truncated): (String, bool) = if total > EXAMPLE_QUERY_MAX_CHARS {
        (text.chars().take(EXAMPLE_QUERY_MAX_CHARS).collect(), true)
    } else {
        (text.to_string(), false)
    };
    // A fenced block is delimited by a backtick run at least as long as any inside it.
    let longest_run = shown
        .split(|ch| ch != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat((longest_run + 1).max(3));
    let mut out = format!(
        "\n<details><summary>example query</summary>\n\n{fence}cypher\n{shown}\n{fence}\n",
    );
    if truncated {
        out.push_str(&format!(
            "\n_truncated — showing the first {EXAMPLE_QUERY_MAX_CHARS} of {total} characters_\n"
        ));
    }
    out.push_str("\n</details>\n");
    out
}

/// A regression-table latency cell: the gated **p50** on the primary line, with p90/p95/p99,
/// throughput, the within-run `n`/σ/CV of `server_ms` and — under the server-ms gate — the
/// demoted client-observed total p50 folded onto a smaller `context:` line (informational, never
/// gated, appended only when the report carries it). `—` only when the side's p50 is absent.
/// Values are fixed-precision measurements, so no operator-supplied text is interpolated (no
/// `md_cell` escaping needed).
fn latency_cell(
    p50: Option<f64>,
    ctx: Option<&CellContextSide>,
) -> String {
    match (p50, ctx) {
        (Some(p50), Some(c)) => {
            // A zero `n` means the model predates the dispersion stats (deserialized old cells
            // JSON) — omit the segment rather than claim zero samples.
            let disp = if c.n > 0 {
                format!(
                    " · n/σ/CV {}",
                    format_dispersion("/", c.n, c.n_server, c.server_stddev_ms, c.server_cv_pct)
                )
            } else {
                String::new()
            };
            let total = c
                .total_p50_ms
                .map(|t| format!(" · total p50 {t:.3}"))
                .unwrap_or_default();
            format!(
                "{:.3}<br><sub>context: p90 {:.3} · p95 {:.3} · p99 {:.3} · {:.0} op/s{}{}</sub>",
                p50, c.p90_ms, c.p95_ms, c.p99_ms, c.throughput_ops_per_sec, disp, total
            )
        }
        (Some(p50), None) => format!("{p50:.3}"),
        (None, _) => "—".to_string(),
    }
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
/// per-cell 🟢/🔴/N-A verdicts on the gated p50 — the server-reported execution-time median by
/// default, the total-latency median under the `--gated-metric total-ms` opt-in — against the
/// threshold budget, with throughput shown for context. Diverged ops get a perf verdict of N/A —
/// 🔴 under the `gate` divergence policy, ⚠ under `advisory`. A `NotComparable` status renders a
/// single "not comparable" note. Never errors.
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
    // §6.3 attestation per side (mirrors the plain diff's row): "—" on a write run means the
    // latency tier only — the guard's warnings call that out below.
    row2(
        &mut head,
        "outcome oracle",
        &oracle_cell(meta.baseline.oracle_verified.as_ref()),
        &oracle_cell(meta.candidate.oracle_verified.as_ref()),
    );
    head.push('\n');
    head.push_str(&meta.thresholds.settings_markdown());
    // The gated metric is always named — the default gate changed to server-ms (maintainer
    // decision), so no reader should have to guess which clock the verdicts are on.
    if analysis.gated_on_server_ms() {
        head.push_str(
            "\n**Gated metric: `server_ms.p50`** (default) — the server-reported execution time; \
             client-observed total latency is demoted to the `context:` line and is not part of \
             any verdict in this comparison.\n",
        );
    } else {
        head.push_str(
            "\n**Gated metric: `total_ms.p50`** (opt-in) — the client-observed total latency, \
             including client scheduling and network time; the default gate is the \
             server-reported execution time (`server_ms.p50`).\n",
        );
    }

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
        // A skipped op keeps its section even with no measured cell — the skip reason is the
        // content (design Phase 6 §3.5). Other ops with no measured cell get no report section;
        // the totals still tally them when they diverged (gate → regressed, advisory → diverged).
        let skip_note = skip_note(oa, la, lb);
        if let Some(note) = &skip_note {
            op_body.push_str(&format!("\n_{}_\n", md_cell(&md_inline(&html_escape(note)))));
        }
        if op_body.trim().is_empty() {
            continue;
        }
        // The op's deterministic representative query, nested-collapsed so the sticky PR comment
        // stays compact. Appended only to ops that render a section anyway — an example alone
        // never resurrects a section for a cell-less op.
        if let Some(example) = &oa.example_query {
            op_body.push_str(&example_query_block(example));
        }
        let diverged_note = if oa.correctness == Correctness::Diverged {
            match analysis.divergence_policy {
                DivergencePolicy::Gate => " — ⚠ results differ (perf verdict N/A)",
                DivergencePolicy::Advisory => " — ⚠ results differ (advisory; perf verdict N/A)",
            }
        } else {
            ""
        };
        let skipped_note = if skip_note.is_some() {
            " — skipped (perf verdict N/A)"
        } else {
            ""
        };
        body.push_str(&format!(
            "\n<details><summary>{} <code>{}</code>{diverged_note}{skipped_note}</summary>\n{op_body}\n</details>\n",
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
    // The legend always names the gated metric — server_ms by default, total_ms under the
    // explicit opt-in.
    let metric_clause = if analysis.gated_on_server_ms() {
        "Only **p50** of `server_ms` (server-reported execution time) is gated"
    } else {
        "Only **p50** of `total_ms` (client-observed total latency) is gated"
    };
    // The context descriptor matches what the cells actually fold in: under the server gate the
    // client-observed total p50 rides along as demoted context.
    let context_clause = if analysis.gated_on_server_ms() {
        "the `context:` line (p90/p95/p99 · throughput · within-run n/σ/CV of `server_ms` · \
         client-observed total p50)"
    } else {
        "the `context:` line (p90/p95/p99 · throughput · within-run n/σ/CV of `server_ms`)"
    };
    // The dispersion stats' one-line definition, shared by both policies' legends.
    let stats_clause = "n = samples retained after severe-outlier removal (pooled across the C \
                        workers; `n (server m)` when only `m` carry a server time); σ = their \
                        **sample** standard deviation (n−1) of `server_ms` **within this run** — \
                        not run-to-run noise; CV = 100·σ/mean.";
    out.push_str(&match analysis.divergence_policy {
        DivergencePolicy::Gate => format!(
            "\n🟢 = faster or within budget · 🔴 = slower than budget **or** results differ · \
             N/A = no perf verdict. {metric_clause} — {context_clause} \
             and `Δms` are informational, never part of the verdict. {stats_clause} \
             Non-blocking.\n"
        ),
        DivergencePolicy::Advisory => format!(
            "\n🟢 = faster or within budget · 🔴 = slower than budget · ⚠ = results differ \
             (advisory — the engines did different work, so perf is N/A) · N/A = no perf verdict. \
             {metric_clause} — {context_clause} and `Δms` are \
             informational, never part of the verdict. {stats_clause} Non-blocking.\n"
        ),
    });
    if analysis.totals.skipped > 0 {
        out.push_str(
            "\n_⏭ = skipped: the engine lacks the op's required procedure (capability probe) — \
             recorded but never executed there, so it is neither a pass nor a divergence._\n",
        );
    }
    out.push_str(&body);
    out
}

/// The one-line skip explanation for an op skipped on either side (capability probe — design
/// Phase 6 §3.5), or `None` for a measured op. Labels name the runs so an asymmetric skip reads
/// unambiguously.
fn skip_note(
    oa: &OpAnalysis,
    la: &str,
    lb: &str,
) -> Option<String> {
    match (&oa.skipped_baseline, &oa.skipped_candidate) {
        (Some(b), Some(c)) if b == c => Some(format!("⏭ skipped on both sides — {b}")),
        (Some(b), Some(c)) => Some(format!("⏭ skipped on both sides — {la}: {b}; {lb}: {c}")),
        (Some(b), None) => Some(format!(
            "⏭ skipped on {la} — {b}; measured on {lb} only, so no cell is comparable"
        )),
        (None, Some(c)) => Some(format!(
            "⏭ skipped on {lb} — {c}; measured on {la} only, so no cell is comparable"
        )),
        (None, None) => None,
    }
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
// [`RegressionAnalysis`] model into a compact structure — overall verdict, per-tier 🟢/🔴/⚠/⏭/N-A
// counts and the worst offenders — that CI can post inline while hosting the full report
// externally under [`SyntheticSummary::slug`]. Because both renderers consume the same model,
// drift is impossible by construction (a consistency test still pins the two together).
// -------------------------------------------------------------------------------------------------

/// Schema version of the JSON emitted by `report --summary`, bumped on any breaking field change.
/// v2 (design §A5 of `synthetic-three-way-report.md`): adds `budget_profile`,
/// `divergence_policy`, `gated_metric`, `elapsed_secs`, a `diverged` bucket in [`OutcomeCounts`]
/// and the four-state [`OverallVerdict`] as `overall_verdict` (replacing v1's three-state
/// `verdict`). v3 (design Phase 6 §3.5 of `synthetic-cover-algorithms-phase6.md`): adds the
/// `skipped` [`OpOutcome`] value and [`OutcomeCounts`] bucket for capability-skipped ops — a v2
/// consumer parsing outcomes exhaustively would reject `"skipped"`, so this is a bump, not a
/// silent extension.
pub const SUMMARY_SCHEMA_VERSION: u32 = 3;

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
/// per-tier 🟢/🔴/⚠/⏭/N-A counts and the worst offenders — small enough to embed in a PR comment while
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
    /// The gated metric id (`server_ms.p50` by default, `total_ms.p50` under the
    /// `--gated-metric total-ms` opt-in); everything else is informational.
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
        // Mirror the model's totals: ops with ≥1 cell are tallied, a cell-less **diverged**
        // op is tallied too (every divergence counts), and so is a **skipped** op (every skip
        // is visible — design Phase 6 §3.5). Under the `gate` policy (divergence ⇒ `Regressed`)
        // a divergence also surfaces as a worst offender below; under `advisory` it stays a
        // `DivergedAdvisory` and is intentionally kept out of the offender list, as is a skip
        // (an expected engine limitation, not a failure).
        if !oa.cells.is_empty()
            || oa.correctness == Correctness::Diverged
            || oa.op_outcome == OpOutcome::Skipped
        {
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
    /// verdict headline, a per-tier 🟢/🔴/⚠/⏭/N-A table and the worst offenders, ending with the
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
        // The sticky comment always names the gated metric — the default gate changed to
        // server-ms (maintainer decision), so the clock is never ambiguous.
        if self.gated_metric == GatedMetric::ServerMs.id() {
            out.push_str("\n_gated metric: `server_ms.p50` (server-reported execution time)_\n");
        } else {
            out.push_str(
                "\n_gated metric: `total_ms.p50` (client-observed total latency — opt-in; the \
                 default gate is `server_ms.p50`)_\n",
            );
        }
        if self.not_comparable_reason.is_some() {
            // NotComparable: the headline already carries the reason; there is nothing to tally.
            out.push_str(&format!("\n_report: {}_\n", self.slug));
            return out;
        }
        out.push_str("\n| tier | 🟢 | 🔴 | ⚠ | ⏭ | N/A |\n|---|---:|---:|---:|---:|---:|\n");
        for t in &self.per_tier {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                md_cell(&t.tier),
                t.counts.pass,
                t.counts.regressed,
                t.counts.diverged,
                t.counts.skipped,
                t.counts.not_applicable
            ));
        }
        out.push_str(&format!(
            "| **all** | {} | {} | {} | {} | {} |\n",
            self.totals.pass,
            self.totals.regressed,
            self.totals.diverged,
            self.totals.skipped,
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

    /// Analyze under `--gated-metric server-ms` — the default, spelled out for symmetry with
    /// [`analyze_total`] (gate divergence policy).
    fn analyze_server(
        a: &Report,
        b: &Report,
        g: &crate::synthetic::baseline::RegressionGuard,
        t: &Thresholds,
    ) -> RegressionAnalysis {
        analyze(
            a,
            b,
            g,
            t,
            &AnalysisOptions {
                gated_metric: GatedMetric::ServerMs,
                ..Default::default()
            },
        )
    }

    /// Analyze under the `--gated-metric total-ms` opt-in (gate divergence policy).
    fn analyze_total(
        a: &Report,
        b: &Report,
        g: &crate::synthetic::baseline::RegressionGuard,
        t: &Thresholds,
    ) -> RegressionAnalysis {
        analyze(
            a,
            b,
            g,
            t,
            &AnalysisOptions {
                gated_metric: GatedMetric::TotalMs,
                ..Default::default()
            },
        )
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
                // Both clocks carry the same medians so metric-agnostic tests behave identically
                // under either gate; metric-specific tests overwrite one side explicitly.
                server_ms: summ(median),
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
                policy: None,
                skipped: None,
                example_query: Some("MATCH (u:User {id: $id}) RETURN u".to_string()),
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
                oracle_verified: None,
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
        assert!(md.contains("## <code>match\\_by\\_index</code>"));
        assert!(md.contains("cached (plan reused"));
        // p50 delta +10.0%, throughput delta -10.0%.
        assert!(md.contains("+10.0%"), "expected latency +10%: {md}");
        assert!(md.contains("-10.0%"), "expected throughput -10%: {md}");
        assert!(md.contains("⚠ server image changed"));
    }

    #[test]
    fn diff_gates_dp50_on_server_time_and_demotes_total() {
        // Server clocks disagree with the wall clocks: Δp50 must follow server_ms (+50 %), not
        // total_ms (+10 %), and each cell leads with server percentiles, total p50 demoted.
        let mut a = report(42001, 1.000, 1000.0);
        let mut b = report(42002, 1.100, 1000.0);
        set_server_median(&mut a, 0.400);
        set_server_median(&mut b, 0.600);
        let md = diff_markdown(&a, &b, &[]);
        assert!(md.contains("A server p50/p90/p95/p99"), "server-led header: {md}");
        assert!(md.contains("+50.0%"), "Δp50 from server medians: {md}");
        assert!(!md.contains("+10.0%"), "wall-clock Δ must not be gated: {md}");
        assert!(
            md.contains("<sub>total p50 1.000</sub>") && md.contains("<sub>total p50 1.100</sub>"),
            "demoted totals: {md}"
        );
    }

    #[test]
    fn diff_degrades_to_na_when_server_time_is_missing() {
        // A report predating server-time capture (zeroed server_ms): no silent fallback — the
        // latency cell shows — (with the total demoted alongside) and Δp50 is —.
        let mut a = report(42001, 1.000, 1000.0);
        let b = report(42002, 1.100, 900.0);
        set_server_median(&mut a, 0.0);
        let md = diff_markdown(&a, &b, &[]);
        assert!(
            md.contains("| —<br><sub>total p50 1.000</sub> | 100 / — / — |"),
            "missing server side degrades to — with demoted total and — σ/CV: {md}"
        );
        assert!(
            md.contains("10.1% | — |"),
            "Δp50 must be — when either server median is invalid: {md}"
        );
        assert!(md.contains("predating server-time capture"), "legend explains —: {md}");
    }

    #[test]
    fn diff_renders_dispersion_columns_and_example_query() {
        let a = report(42001, 1.000, 1000.0);
        let b = report(42002, 1.100, 900.0);
        let md = diff_markdown(&a, &b, &[]);
        // One combined `n / σ (ms) / CV` column per side, right of its percentile column.
        assert!(
            md.contains("| A n / σ (ms) / CV |") && md.contains("| B n / σ (ms) / CV |"),
            "dispersion headers: {md}"
        );
        // summ(median): population σ = median·0.1 over n = 100 ⇒ sample σ = ·√(100/99).
        assert!(md.contains("| 100 / 0.101 / 10.1% |"), "A cell (median 1.0): {md}");
        assert!(md.contains("| 100 / 0.111 / 10.1% |"), "B cell (median 1.1): {md}");
        // The op's example query renders as a collapsed details block with a cypher fence.
        assert!(md.contains("<details><summary>example query</summary>"), "{md}");
        assert!(
            md.contains("```cypher\nMATCH (u:User {id: $id}) RETURN u\n```"),
            "fenced example text: {md}"
        );
        // The legend explains the new columns and their within-run scope.
        assert!(md.contains("**within-run** dispersion"), "{md}");
        assert!(md.contains("**sample** standard deviation (n−1 denominator)"), "{md}");
        assert!(md.contains("`n (server m)`"), "{md}");
    }

    #[test]
    fn diff_example_query_prefers_the_candidate_text() {
        let mut a = report(42001, 1.0, 1000.0);
        let mut b = report(42002, 1.0, 1000.0);
        a.operations.get_mut("match_by_index").unwrap().example_query =
            Some("MATCH (base) RETURN base".to_string());
        b.operations.get_mut("match_by_index").unwrap().example_query =
            Some("MATCH (cand) RETURN cand".to_string());
        let md = diff_markdown(&a, &b, &[]);
        assert!(md.contains("MATCH (cand) RETURN cand"), "candidate text wins: {md}");
        assert!(!md.contains("MATCH (base) RETURN base"), "{md}");
        // An older candidate report without the field falls back to the baseline's text.
        b.operations.get_mut("match_by_index").unwrap().example_query = None;
        let md = diff_markdown(&a, &b, &[]);
        assert!(md.contains("MATCH (base) RETURN base"), "baseline fallback: {md}");
        // Neither side carries one (both reports predate the field): no details block at all.
        a.operations.get_mut("match_by_index").unwrap().example_query = None;
        let md = diff_markdown(&a, &b, &[]);
        assert!(!md.contains("<details><summary>example query</summary>"), "{md}");
    }

    #[test]
    fn example_query_block_is_deterministic_and_outruns_inner_backticks() {
        let text = "MATCH (n:`weird``label`) WHERE n.x = ``` RETURN n";
        let block = example_query_block(text);
        assert_eq!(block, example_query_block(text), "pure and deterministic");
        // The fence must be longer than the longest inner backtick run (3) — 4 backticks.
        assert!(block.contains("\n````cypher\n"), "{block}");
        assert!(block.contains("\n````\n"), "{block}");
        assert!(!block.contains("truncated"), "{block}");
        // GitHub only renders Markdown inside HTML <details> after a blank line.
        assert!(block.contains("</summary>\n\n"), "{block}");
    }

    #[test]
    fn example_query_block_truncates_very_long_texts() {
        let text = "x".repeat(700);
        let block = example_query_block(&text);
        assert!(block.contains(&"x".repeat(600)), "{block}");
        assert!(!block.contains(&"x".repeat(601)), "{block}");
        assert!(
            block.contains("_truncated — showing the first 600 of 700 characters_"),
            "{block}"
        );
    }

    #[test]
    fn format_dispersion_names_a_differing_server_cohort() {
        // In this tool's own reports both cohorts always match (paired capture); a foreign or
        // older report may carry fewer server-timed samples — both counts are then named.
        assert_eq!(
            format_dispersion(" / ", 100, Some(100), Some(0.5), Some(10.0)),
            "100 / 0.500 / 10.0%"
        );
        assert_eq!(
            format_dispersion(" / ", 100, Some(80), Some(0.5), Some(10.0)),
            "100 (server 80) / 0.500 / 10.0%"
        );
        assert_eq!(format_dispersion("/", 100, None, None, None), "100/—/—");
    }

    #[test]
    fn latency_cell_omits_dispersion_for_pre_stats_cells() {
        use crate::synthetic::analysis::CellContextSide;
        // n = 0 marks a context deserialized from cells JSON written before the dispersion
        // stats existed — the segment is omitted rather than claiming zero samples.
        let old = CellContextSide {
            p90_ms: 1.2,
            p95_ms: 1.3,
            p99_ms: 1.5,
            throughput_ops_per_sec: 1000.0,
            total_p50_ms: Some(1.0),
            n: 0,
            n_server: None,
            server_stddev_ms: None,
            server_cv_pct: None,
            total_stddev_ms: None,
            total_cv_pct: None,
        };
        let cell = latency_cell(Some(1.0), Some(&old));
        assert!(!cell.contains("n/σ/CV"), "{cell}");
        assert!(cell.contains("· total p50 1.000"), "{cell}");
        // A populated context folds the stats between throughput and the demoted total.
        let new = CellContextSide {
            n: 100,
            n_server: Some(100),
            server_stddev_ms: Some(0.1),
            server_cv_pct: Some(10.0),
            ..old
        };
        let cell = latency_cell(Some(1.0), Some(&new));
        assert!(
            cell.contains("op/s · n/σ/CV 100/0.100/10.0% · total p50 1.000"),
            "{cell}"
        );
    }

    #[test]
    fn regression_report_nests_the_example_query_per_op() {
        use crate::synthetic::baseline::regression_guard;
        use crate::synthetic::thresholds::Thresholds;
        let a = report(42001, 1.0, 1000.0);
        let b = report(42002, 1.0, 1000.0);
        let g = regression_guard(&a, &b);
        let md = regression_md(&a, &b, &g, &Thresholds::builtin(), None);
        assert!(md.contains("<details><summary>example query</summary>"), "{md}");
        assert!(md.contains("MATCH (u:User {id: $id}) RETURN u"), "{md}");
        // The legend defines the context-line stats.
        assert!(md.contains("n = samples retained after severe-outlier removal"), "{md}");
    }

    /// Overwrite every level's `server_ms` summary median (fixtures default both clocks equal).
    fn set_server_median(
        r: &mut Report,
        v: f64,
    ) {
        for op in r.operations.values_mut() {
            for lvl in &mut op.levels {
                for m in [&mut lvl.cached, &mut lvl.uncached].into_iter().flatten() {
                    m.metrics.server_ms = summ(v);
                }
            }
        }
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
        assert!(md.contains("main server p50") && md.contains("pr tput"), "op header: {md}");
    }

    #[test]
    fn diff_and_regression_render_the_oracle_attestation_row() {
        use crate::synthetic::baseline::regression_guard;
        let mut a = report(42001, 1.0, 1000.0);
        let b = report(42002, 1.1, 900.0);
        // Un-attested pair: both sides show the placeholder.
        let md = diff_markdown(&a, &b, &[]);
        assert!(md.contains("| outcome oracle | — | — |"), "{md}");
        // An attested side renders the compact verified summary — per side, so a one-sided
        // (downgrade-shaped) pair is visible at a glance even before the guard's warnings.
        a.meta.oracle_verified =
            Some([("single_vertex_write".to_string(), 256)].into_iter().collect());
        let md = diff_markdown(&a, &b, &[]);
        assert!(
            md.contains("| outcome oracle | verified — 1 op(s), 256 outcome(s) | — |"),
            "{md}"
        );
        // The regression report renders the same row from the analysis SideMeta.
        let mut b2 = report(42002, 1.0, 1000.0);
        b2.meta.oracle_verified = a.meta.oracle_verified.clone();
        let g = regression_guard(&a, &b2);
        let md = regression_md(&a, &b2, &g, &Thresholds::builtin(), None);
        assert!(
            md.contains(
                "| outcome oracle | verified — 1 op(s), 256 outcome(s) | verified — 1 op(s), \
                 256 outcome(s) |"
            ),
            "{md}"
        );
    }

    #[test]
    fn diff_marks_missing_cells() {
        let a = report(42001, 1.0, 1000.0);
        let mut b = report(42002, 1.0, 1000.0);
        // Drop B's only op so A-only ops render with "—".
        b.operations.clear();
        let md = diff_markdown(&a, &b, &[]);
        assert!(md.contains("## <code>match\\_by\\_index</code>"));
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
    fn diff_renders_a_both_sides_skip_note() {
        // A both-skipped op has no levels on either side — the plain diff must say why instead of
        // rendering a bare heading; reasons are manifest content, hence HTML-escaped.
        let mut a = report(42001, 1.0, 1000.0);
        let mut b = report(42002, 1.0, 1000.0);
        for r in [&mut a, &mut b] {
            let o = r.operations.get_mut("match_by_index").unwrap();
            o.levels = vec![];
            o.result_digest = None;
            o.skipped = Some("engine lacks procedure 'algo.maxFlow' <v2&up>".to_string());
        }
        let md = diff_markdown(&a, &b, &[]);
        assert!(md.contains("## <code>match\\_by\\_index</code>"));
        assert!(
            md.contains(
                "⏭ skipped on both sides — engine lacks procedure 'algo.maxFlow' &lt;v2&amp;up&gt;"
            ),
            "{md}"
        );
        assert!(!md.contains("<v2&up>"), "raw HTML leaked: {md}");
        // Different per-side reasons render both, labeled by run (A/B fallbacks — no --label).
        b.operations.get_mut("match_by_index").unwrap().skipped =
            Some("engine lacks procedure 'algo.MaxFlowV2'".to_string());
        let md = diff_markdown(&a, &b, &[]);
        assert!(
            md.contains(
                "⏭ skipped on both sides — A: engine lacks procedure 'algo.maxFlow' &lt;v2&amp;up&gt;; \
                 B: engine lacks procedure 'algo.MaxFlowV2'"
            ),
            "{md}"
        );
    }

    #[test]
    fn diff_renders_a_one_sided_skip_note() {
        // One-sided skip: the note names the skipping side and that the other side measured; the
        // measured side's table still renders below the note.
        let a = report(42001, 1.0, 1000.0);
        let mut b = report(42002, 1.0, 1000.0);
        {
            let o = b.operations.get_mut("match_by_index").unwrap();
            o.levels = vec![];
            o.result_digest = None;
            o.skipped = Some("engine lacks procedure 'algo.maxFlow'".to_string());
        }
        let md = diff_markdown(&a, &b, &[]);
        assert!(
            md.contains(
                "⏭ skipped on B — engine lacks procedure 'algo.maxFlow'; measured on A only"
            ),
            "{md}"
        );
        assert!(md.contains("| 1 | 1.000"), "A's measured cells still render: {md}");
        // The skipping side labels by --label when set.
        let mut a2 = a.clone();
        a2.meta.label = Some("main".to_string());
        let mut b2 = b.clone();
        b2.meta.label = Some("pr".to_string());
        let md = diff_markdown(&a2, &b2, &[]);
        assert!(
            md.contains(
                "⏭ skipped on pr — engine lacks procedure 'algo.maxFlow'; measured on main only"
            ),
            "{md}"
        );
        // Markdown inline syntax in a reason must not terminate the note's `_…_` emphasis span.
        b2.operations.get_mut("match_by_index").unwrap().skipped =
            Some("lacks `algo_x` *v2*".to_string());
        let md = diff_markdown(&a2, &b2, &[]);
        assert!(md.contains(r"lacks \`algo\_x\` \*v2\*"), "markdown specials not escaped: {md}");
    }

    #[test]
    fn diff_op_headings_render_hostile_names_inert() {
        // Op names are report content: a backtick or newline in a name must not terminate the
        // heading's code styling or inject Markdown/HTML into the diff.
        let a = report(42001, 1.0, 1000.0);
        let mut b = report(42002, 1.0, 1000.0);
        let hostile = "evil`op\nx*<b>&y";
        let op = b.operations.remove("match_by_index").unwrap();
        b.operations.insert(hostile.to_string(), op);
        let md = diff_markdown(&a, &b, &[]);
        assert!(md.contains("## <code>evil\\`op<br>x\\*&lt;b&gt;&amp;y</code>"), "{md}");
        assert!(!md.contains(hostile), "raw hostile name leaked: {md}");
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

    /// Mutate the candidate's cached `server_ms` percentiles in place (keeping p50) so tests can
    /// isolate tail behaviour from the gated p50 — the context tails follow the gated metric,
    /// server_ms by default.
    fn set_tails(r: &mut Report, p90: f64, p99: f64) {
        let m = r
            .operations
            .get_mut("match_by_index")
            .unwrap()
            .levels[0]
            .cached
            .as_mut()
            .unwrap();
        m.metrics.server_ms.p90 = p90;
        m.metrics.server_ms.p95 = (p90 + p99) / 2.0;
        m.metrics.server_ms.p99 = p99;
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
        // p90/p99 + throughput are folded onto the context line (not their own columns), with
        // the within-run n/σ/CV and the demoted client-observed total p50 riding along under
        // the default server gate.
        assert!(
            md.contains("<br><sub>context: p90 ")
                && md.contains("op/s · n/σ/CV 100/0.101/10.1% · total p50 1.000</sub>"),
            "{md}"
        );
        // The per-line guard shows the resolved default (10%) + floor.
        assert!(md.contains("10% AND 0.5 ms"), "guard cell: {md}");
        // Legend states the gate is p50-only, naming the default gated metric.
        assert!(
            md.contains("Only **p50** of `server_ms` (server-reported execution time) is gated"),
            "{md}"
        );
    }

    #[test]
    fn regression_markdown_names_the_gated_metric_for_both_gates() {
        use crate::synthetic::baseline::regression_guard;
        use crate::synthetic::thresholds::Thresholds;
        // +100 % total p50 (over budget) while the server p50 grows only +0.2 ms — over its 10 %
        // budget but under the 0.5 ms floor, so the default (server-ms) gate passes while the
        // total-ms opt-in regresses.
        let mut a = report(42001, 1.0, 1000.0);
        let mut b = report(42002, 2.0, 900.0);
        a.operations.get_mut("match_by_index").unwrap().levels[0]
            .cached
            .as_mut()
            .unwrap()
            .metrics
            .server_ms = summ(0.2);
        b.operations.get_mut("match_by_index").unwrap().levels[0]
            .cached
            .as_mut()
            .unwrap()
            .metrics
            .server_ms = summ(0.4);
        let g = regression_guard(&a, &b);

        // Default (server-ms): the header and the legend both name the metric; the verdict
        // follows the server medians (0.200 → 0.400), which the table carries.
        let md = regression_markdown(&analyze_gate(&a, &b, &g, &Thresholds::builtin()));
        assert!(
            md.contains("**Gated metric: `server_ms.p50`** (default)"),
            "{md}"
        );
        assert!(
            md.contains("Only **p50** of `server_ms` (server-reported execution time) is gated"),
            "{md}"
        );
        assert!(
            md.contains("🟢 no p50 regression beyond budget across 1 comparable cell(s)"),
            "{md}"
        );
        assert!(
            md.contains("0.200") && md.contains("0.400"),
            "server medians in cells: {md}"
        );
        // The wall clock is demoted, not hidden: each side's total p50 rides on the context
        // line, and the legend says so.
        assert!(
            md.contains("· total p50 1.000") && md.contains("· total p50 2.000"),
            "demoted total p50 in context: {md}"
        );
        assert!(
            md.contains("throughput · within-run n/σ/CV of `server_ms` · client-observed total p50)"),
            "legend names the demoted total: {md}"
        );
        assert!(
            md.contains("demoted to the `context:` line"),
            "header states the demotion: {md}"
        );

        // total-ms opt-in: named as such, and the total-latency regression is caught.
        let md = regression_markdown(&analyze_total(&a, &b, &g, &Thresholds::builtin()));
        assert!(
            md.contains("**Gated metric: `total_ms.p50`** (opt-in)"),
            "{md}"
        );
        assert!(
            md.contains("Only **p50** of `total_ms` (client-observed total latency) is gated"),
            "{md}"
        );
        assert!(
            md.contains("🔴 1 of 1 comparable cell(s) over budget"),
            "{md}"
        );
        assert!(
            md.contains("1.000") && md.contains("2.000"),
            "total medians in cells: {md}"
        );
        // No duplicate: the primary p50 already is the wall clock, so the context line carries
        // only tails + throughput.
        assert!(!md.contains("· total p50"), "no demoted total under total-ms gating: {md}");
    }

    #[test]
    fn regression_markdown_renders_the_degraded_server_metric_advisory() {
        use crate::synthetic::baseline::regression_guard;
        use crate::synthetic::thresholds::Thresholds;
        // The candidate carries no usable server time (all-zero summary): under the default
        // server-ms gating the only cell is N/A, the overall verdict is Advisory, and the loud
        // degraded-metric warning renders as a `> ⚠` line naming the op and the escape hatch.
        let a = report(42001, 1.0, 1000.0);
        let mut b = report(42002, 1.0, 1000.0);
        b.operations.get_mut("match_by_index").unwrap().levels[0]
            .cached
            .as_mut()
            .unwrap()
            .metrics
            .server_ms = summ(0.0);
        let g = regression_guard(&a, &b);
        let md = regression_markdown(&analyze_server(&a, &b, &g, &Thresholds::builtin()));
        assert!(
            md.contains("> ⚠ gated metric server_ms.p50 (the default gate) is missing/invalid"),
            "advisory line: {md}"
        );
        assert!(
            md.contains("match_by_index — those cells have NO verdict"),
            "{md}"
        );
        assert!(
            md.contains("re-run with `--gated-metric total-ms`"),
            "escape hatch named: {md}"
        );
        assert!(md.contains("⚠ no comparable cells"), "{md}");
    }

    #[test]
    fn summary_carries_and_renders_the_server_gated_metric() {
        use crate::synthetic::baseline::regression_guard;
        use crate::synthetic::thresholds::Thresholds;
        let a = report(42001, 1.0, 1000.0);
        let b = report(42002, 1.0, 1000.0);
        let g = regression_guard(&a, &b);

        // Default: server-ms — carried in the summary JSON and named in the sticky comment.
        let server = summarize_gate(&a, &b, &g, &Thresholds::builtin());
        assert_eq!(server.gated_metric, "server_ms.p50");
        let json = server.to_json().unwrap();
        assert!(
            json.contains("\"gated_metric\": \"server_ms.p50\""),
            "{json}"
        );
        let md = server.to_markdown();
        assert!(
            md.contains("_gated metric: `server_ms.p50` (server-reported execution time)_"),
            "{md}"
        );

        // total-ms opt-in: carried and named as the non-default escape hatch.
        let total = summarize(&analyze_total(&a, &b, &g, &Thresholds::builtin()));
        assert_eq!(total.gated_metric, "total_ms.p50");
        let md = total.to_markdown();
        assert!(
            md.contains("_gated metric: `total_ms.p50` (client-observed total latency"),
            "{md}"
        );
        assert!(
            md.contains("the default gate is `server_ms.p50`"),
            "{md}"
        );
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
        assert!(md.contains("context: p90 50.000 · p95 275.000 · p99 500.000"), "{md}");
    }

    #[test]
    fn latency_cell_renders_p50_without_context() {
        // A valid p50 must never be hidden just because the tail context is absent.
        assert_eq!(latency_cell(Some(1.5), None), "1.500");
        assert_eq!(latency_cell(None, None), "—");
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
                OperationReport {
                    levels,
                    result_digest: Some("sha256:aa".to_string()),
                    policy: None,
                    skipped: None,
                    example_query: None,
                },
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
                oracle_verified: None,
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
                skipped: 0,
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
                skipped: 0,
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
                skipped: 0,
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
                skipped: 0,
                not_applicable: 0
            }
        );
        assert_eq!(
            full.counts,
            OutcomeCounts {
                pass: 0,
                regressed: 1,
                diverged: 0,
                skipped: 0,
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
                skipped: 0,
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
    fn algorithm_shape_gets_budget_and_tier_end_to_end() {
        // Phase 6 duck finding: algorithm ops must be first-class in reporting/thresholds — an
        // `[op.algo_*]` override applies, the op rolls into its registry tier (`full`, from the
        // family-agnostic `shape_tier`) rather than vanishing from the per-tier buckets, and an
        // offender carries that tier.
        let toml = "[op.algo_max_flow_single_pair]\nbudget_pct = 50.0\nfloor_ms = 0.1\n";
        let t = Thresholds::from_toml_str(toml).unwrap();
        // 2.0 → 2.6 ms = +30%, Δ0.6 ms: within the 50% override (pass), but over the strict
        // built-in default (10% AND 0.5 ms) — proving the override, not the default, was applied.
        let a = rpt("main", 42001, &[("algo_max_flow_single_pair", 2.0, "d1")]);
        let b = rpt("pr", 42002, &[("algo_max_flow_single_pair", 2.6, "d1")]);
        let g = regression_guard(&a, &b);
        let md = regression_md(&a, &b, &g, &t, None);
        assert!(md.contains("50% AND 0.1 ms"), "guard column shows the override:\n{md}");
        assert!(md.contains("🟢 <code>algo_max_flow_single_pair</code>"), "{md}");
        let s = summarize_gate(&a, &b, &g, &t);
        assert_eq!(s.totals.pass, 1, "{s:?}");
        // `algo_max_flow_single_pair` is a Tier::Full algorithm shape (shapes.rs registry) — it
        // must count in `full`, not fall out of both buckets like an unknown tag.
        let full = s.per_tier.iter().find(|t| t.tier == "full").unwrap();
        assert_eq!(full.counts.pass, 1, "{s:?}");
        let core = s.per_tier.iter().find(|t| t.tier == "core").unwrap();
        assert_eq!(core.counts, OutcomeCounts::default(), "{s:?}");
        // Same inputs under the strict built-in default: +30%/Δ0.6 ms regresses, and the offender
        // carries the registry tier.
        let strict = summarize_gate(&a, &b, &g, &Thresholds::builtin());
        assert_eq!(strict.totals.regressed, 1, "{strict:?}");
        assert_eq!(strict.worst_offenders[0].tier.as_deref(), Some("full"), "{strict:?}");
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
        assert!(json.contains("\"gated_metric\": \"server_ms.p50\""), "{json}");
        // No `--elapsed-secs` ⇒ explicit null (the field is always present).
        assert!(json.contains("\"elapsed_secs\": null"), "{json}");
    }

    #[test]
    fn summary_v3_freezes_the_top_level_field_set() {
        // The summary JSON is a machine contract (schema v3). This test freezes the exact
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
        assert_eq!(value["schema_version"], 3);
        // The outcome-counts shape (shared by totals and per_tier) is part of the contract too.
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
            OutcomeCounts {
                pass: 1,
                regressed: 0,
                diverged: 1,
                skipped: 0,
                not_applicable: 0
            }
        );
        assert_eq!(s.diverged_ops, vec!["aggregate_count".to_string()]);
        // Advisory-diverged ops are not "worst offenders" (nothing regressed).
        assert!(s.worst_offenders.is_empty(), "{s:?}");
        let md = s.to_markdown();
        assert!(md.contains("⚠ pass, 1 diverged"), "{md}");
        // Tier table gains the ⚠ and ⏭ columns; both ops are Core.
        assert!(md.contains("| tier | 🟢 | 🔴 | ⚠ | ⏭ | N/A |"), "{md}");
        assert!(md.contains("| core | 1 | 0 | 1 | 0 | 0 |"), "{md}");
        assert!(md.contains("| **all** | 1 | 0 | 1 | 0 | 0 |"), "{md}");
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

    /// Mark `op` in `rep` as capability-skipped, exactly as replay records it.
    fn skip_op(
        rep: &mut Report,
        op: &str,
        reason: &str,
    ) {
        let o = rep.operations.get_mut(op).unwrap();
        o.levels = vec![];
        o.result_digest = None;
        o.policy = None;
        o.skipped = Some(reason.to_string());
    }

    #[test]
    fn skipped_op_renders_the_skip_note_and_lands_in_the_skipped_bucket() {
        let a = rpt(
            "main",
            42001,
            &[("match_by_index", 1.0, "d1"), ("algo_max_flow_single_pair", 1.0, "d2")],
        );
        let mut b = rpt(
            "pr",
            42002,
            &[("match_by_index", 1.0, "d1"), ("algo_max_flow_single_pair", 1.0, "d2")],
        );
        skip_op(
            &mut b,
            "algo_max_flow_single_pair",
            "engine lacks procedure 'algo.maxFlow'",
        );
        let g = regression_guard(&a, &b);
        // The skip is not a divergence — the pair is Comparable and the other op still gates.
        let analysis = analyze_gate(&a, &b, &g, &Thresholds::builtin());
        assert_eq!(analysis.verdict, OverallVerdict::Pass);
        let md = regression_markdown(&analysis);
        // Op section survives with the asymmetric skip note, ⏭ emoji and legend line.
        assert!(
            md.contains(
                "⏭ skipped on pr — engine lacks procedure 'algo.maxFlow'; measured on main only"
            ),
            "{md}"
        );
        assert!(md.contains("— skipped (perf verdict N/A)"), "{md}");
        assert!(md.contains("_⏭ = skipped:"), "{md}");
        assert_eq!(
            md_collapsed_emoji(&md, "algo_max_flow_single_pair").as_deref(),
            Some("⏭"),
            "{md}"
        );
        // Headline annotates the skip; the summary tallies it in its own bucket, never offenders.
        let s = summarize(&analysis);
        assert_eq!(s.overall_verdict, OverallVerdict::Pass);
        assert!(s.headline.contains("1 op(s) skipped"), "{}", s.headline);
        assert_eq!(
            s.totals,
            OutcomeCounts {
                pass: 1,
                regressed: 0,
                diverged: 0,
                skipped: 1,
                not_applicable: 0
            }
        );
        assert!(s.worst_offenders.is_empty(), "{s:?}");
        assert!(s.diverged_ops.is_empty(), "{s:?}");
        // The tier table: the skipped op is a real algorithm shape, so `shape_tier` places it in
        // the **full** row's ⏭ column, and the **all** row (fed by `totals`) shows it too — every
        // skip is visible in the summary Markdown.
        let smd = s.to_markdown();
        assert!(smd.contains("| core | 1 | 0 | 0 | 0 | 0 |"), "{smd}");
        assert!(smd.contains("| full | 0 | 0 | 0 | 1 | 0 |"), "{smd}");
        assert!(smd.contains("| **all** | 1 | 0 | 0 | 1 | 0 |"), "{smd}");
    }

    #[test]
    fn op_skipped_on_both_sides_renders_a_single_note() {
        let a = rpt(
            "main",
            42001,
            &[("match_by_index", 1.0, "d1"), ("algo_max_flow_single_pair", 1.0, "d2")],
        );
        let mut b = rpt(
            "pr",
            42002,
            &[("match_by_index", 1.0, "d1"), ("algo_max_flow_single_pair", 1.0, "d2")],
        );
        let mut a2 = a.clone();
        skip_op(
            &mut a2,
            "algo_max_flow_single_pair",
            "engine lacks procedure 'algo.maxFlow'",
        );
        skip_op(
            &mut b,
            "algo_max_flow_single_pair",
            "engine lacks procedure 'algo.maxFlow'",
        );
        let g = regression_guard(&a2, &b);
        let analysis = analyze_gate(&a2, &b, &g, &Thresholds::builtin());
        let md = regression_markdown(&analysis);
        assert!(
            md.contains("⏭ skipped on both sides — engine lacks procedure 'algo.maxFlow'"),
            "{md}"
        );
        // Different per-side reasons render both, labeled by run.
        skip_op(
            &mut b,
            "algo_max_flow_single_pair",
            "engine lacks procedure 'algo.MaxFlowV2'",
        );
        let g = regression_guard(&a2, &b);
        let analysis = analyze_gate(&a2, &b, &g, &Thresholds::builtin());
        let md = regression_markdown(&analysis);
        assert!(
            md.contains(
                "⏭ skipped on both sides — main: engine lacks procedure 'algo.maxFlow'; \
                 pr: engine lacks procedure 'algo.MaxFlowV2'"
            ),
            "{md}"
        );
    }

    #[test]
    fn skip_reasons_are_html_escaped_in_the_regression_markdown() {
        // Skip reasons flow from manifest/probe content — HTML-special chars must not break the
        // <details> markup or inject markup into the PR comment.
        let a = rpt(
            "main",
            42001,
            &[("match_by_index", 1.0, "d1"), ("algo_max_flow_single_pair", 1.0, "d2")],
        );
        let mut b = rpt(
            "pr",
            42002,
            &[("match_by_index", 1.0, "d1"), ("algo_max_flow_single_pair", 1.0, "d2")],
        );
        skip_op(&mut b, "algo_max_flow_single_pair", "needs <engine&co> v2");
        let g = regression_guard(&a, &b);
        let md = regression_markdown(&analyze_gate(&a, &b, &g, &Thresholds::builtin()));
        assert!(md.contains("needs &lt;engine&amp;co&gt; v2"), "{md}");
        assert!(!md.contains("needs <engine&co> v2"), "raw HTML leaked: {md}");
        // Markdown inline syntax in a reason must not terminate the note's `_…_` emphasis span.
        skip_op(&mut b, "algo_max_flow_single_pair", "lacks `algo_x` *v2*");
        let g = regression_guard(&a, &b);
        let md = regression_markdown(&analyze_gate(&a, &b, &g, &Thresholds::builtin()));
        assert!(md.contains(r"lacks \`algo\_x\` \*v2\*"), "markdown specials not escaped: {md}");
    }

    #[test]
    fn skip_free_comparisons_render_no_skip_legend() {
        let a = rpt("main", 42001, &[("match_by_index", 1.0, "d1")]);
        let b = rpt("pr", 42002, &[("match_by_index", 1.0, "d1")]);
        let g = regression_guard(&a, &b);
        let md = regression_markdown(&analyze_gate(&a, &b, &g, &Thresholds::builtin()));
        assert!(!md.contains('⏭'), "{md}");
        let s = summarize_gate(&a, &b, &g, &Thresholds::builtin());
        assert!(!s.headline.contains("skipped"), "{}", s.headline);
    }
}
