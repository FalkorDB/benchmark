//! Cross-run **diff** report: render two synthetic [`Report`]s side by side across every op, cache
//! mode and concurrency level (throughput + total-latency p50/p90/p95/p99 with per-metric deltas),
//! as pasteable Markdown. Used by `synthetic report --diff` after the [`crate::synthetic::baseline`]
//! guard confirms the two runs measured the same workload.

use crate::synthetic::baseline::RegressionGuard;
use crate::synthetic::provenance::decode_module_version;
use crate::synthetic::report::{md_cell, LevelMetrics, LevelReport, Report};
use crate::synthetic::thresholds::{Thresholds, Verdict};
use crate::synthetic::OpName;
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
fn latency_cell(m: Option<&LevelMetrics>) -> String {
    match m {
        None => "—".to_string(),
        Some(m) => {
            let s = &m.metrics.total_ms;
            format!(
                "{:.3}<br><sub>context: p90 {:.3} · p99 {:.3} · {:.0} op/s</sub>",
                s.median, s.p90, s.p99, m.throughput_ops_per_sec
            )
        }
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

/// Render the **non-fatal** `report --regression` markdown: per-cell 🟢/🔴/N/A verdicts on p50
/// (total-latency median) against the threshold budget, with throughput shown for context. Ops the
/// `guard` flags as diverged get a perf verdict of N/A (correctness-🔴). A `NotComparable` guard
/// renders a single "not comparable" note. Never errors.
pub fn regression_markdown(
    baseline: &Report,
    candidate: &Report,
    guard: &RegressionGuard,
    thresholds: &Thresholds,
    elapsed_secs: Option<f64>,
) -> String {
    let la = col_label(baseline, "baseline");
    let lb = col_label(candidate, "candidate");
    let mut head = String::new();
    head.push_str(&format!(
        "### 🧪 Synthetic per-op regression — {} vs {}\n\n",
        md_cell(&lb),
        md_cell(&la)
    ));
    if let Some(secs) = elapsed_secs {
        head.push_str(&format!(
            "⏱ Computed in {} (benchmark + reporting).\n\n",
            fmt_duration_secs(secs)
        ));
    }
    head.push_str(&format!("| field | {} | {} |\n|---|---|---|\n", md_cell(&la), md_cell(&lb)));
    row2(&mut head, "FalkorDB module", &ver(baseline), &ver(candidate));
    row2(
        &mut head,
        "server image",
        baseline.meta.server.server_image.as_deref().unwrap_or("—"),
        candidate.meta.server.server_image.as_deref().unwrap_or("—"),
    );
    row2(&mut head, "workload_hash", &opt_hash(baseline), &opt_hash(candidate));
    row2(
        &mut head,
        "samples / warmup",
        &format!("{} / {}", baseline.meta.samples, baseline.meta.warmup),
        &format!("{} / {}", candidate.meta.samples, candidate.meta.warmup),
    );
    head.push('\n');
    head.push_str(&thresholds.settings_markdown());

    let (diverged, warnings) = match guard {
        RegressionGuard::NotComparable { reason } => {
            head.push_str(&format!(
                "\n> ⚠ **not comparable** — {}. No latency verdict is shown.\n",
                md_cell(reason)
            ));
            return head;
        }
        RegressionGuard::Comparable { diverged_ops, warnings } => (diverged_ops, warnings),
    };

    // Render the per-op tables into `body`, counting regressed cells as we go.
    let mut body = String::new();
    let mut regressed = 0usize;
    let mut comparable_cells = 0usize;
    let ops: BTreeSet<&String> = baseline
        .operations
        .keys()
        .chain(candidate.operations.keys())
        .collect();
    for op in ops {
        let op_diverged = diverged.contains(op);
        let opname = OpName::from_tag(op);
        let regressed_before = regressed;
        let comparable_before = comparable_cells;
        // Render this op's cache-mode tables into a temp buffer so the whole op section can be
        // wrapped in a **collapsed** <details> — keeps the PR sticky comment compact by default.
        let mut op_body = String::new();
        for mode in [Mode::Cached, Mode::Uncached] {
            render_regression_mode(
                &mut op_body, baseline, candidate, op, opname, mode, op_diverged, thresholds, &la,
                &lb, &mut regressed, &mut comparable_cells,
            );
        }
        if op_body.trim().is_empty() {
            continue;
        }
        // Per-op headline on the collapsed row: 🔴 if any cell regressed OR results diverged; 🟢 if
        // it had ≥1 comparable cell and none regressed; N/A when no cell was evaluable (all rows
        // N/A) so it never reads like a pass.
        let op_emoji = if op_diverged || regressed > regressed_before {
            "🔴"
        } else if comparable_cells > comparable_before {
            "🟢"
        } else {
            "N/A"
        };
        let diverged_note =
            if op_diverged { " — ⚠ results differ (perf verdict N/A)" } else { "" };
        body.push_str(&format!(
            "\n<details><summary>{op_emoji} <code>{}</code>{diverged_note}</summary>\n{op_body}\n</details>\n",
            html_escape(op)
        ));
    }

    // Assemble: header + summary + warnings + legend + body. The top-line is 🟢 only when there
    // is neither a p50 regression NOR a correctness (result) divergence.
    let mut out = head;
    let summary = if regressed > 0 {
        format!("🔴 {regressed} of {comparable_cells} comparable cell(s) over budget")
    } else if !diverged.is_empty() {
        format!(
            "🔴 no p50 regression beyond budget, but {} op(s) have differing results (correctness)",
            diverged.len()
        )
    } else {
        format!("🟢 no p50 regression beyond budget across {comparable_cells} comparable cell(s)")
    };
    out.push_str(&format!("\n**{} vs {}** — {}\n", md_cell(&lb), md_cell(&la), summary));
    if !diverged.is_empty() {
        let names: Vec<&str> = diverged.iter().map(String::as_str).collect();
        out.push_str(&format!(
            "\n_⚠ {} op(s) with differing results (perf N/A): {}_\n",
            diverged.len(),
            names.join(", ")
        ));
    }
    for w in warnings {
        out.push_str(&format!("\n> ⚠ {}\n", md_cell(w)));
    }
    out.push_str(
        "\n🟢 = faster or within budget · 🔴 = slower than budget **or** results differ · \
         N/A = no perf verdict. Only **p50** is gated — the `context:` line (p90/p99 · throughput) \
         and `Δms` are informational, never part of the verdict. Non-blocking.\n",
    );
    out.push_str(&body);
    out
}

/// Render one op × cache-mode regression table with a verdict column, accumulating the
/// regressed/comparable cell counts.
#[allow(clippy::too_many_arguments)]
fn render_regression_mode(
    out: &mut String,
    a: &Report,
    b: &Report,
    op: &str,
    opname: Option<OpName>,
    mode: Mode,
    op_diverged: bool,
    thresholds: &Thresholds,
    la: &str,
    lb: &str,
    regressed: &mut usize,
    comparable_cells: &mut usize,
) {
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
    out.push_str(&format!(
        "| C | {} p50 (ms) | {} p50 (ms) | Δp50 (Δms) | p50 guard (>% AND >ms) | verdict |\n\
         |---:|---:|---:|---:|:--:|:--:|\n",
        md_cell(la),
        md_cell(lb),
    ));
    for c in levels {
        let am = level_metrics(a, op, c, mode);
        let bm = level_metrics(b, op, c, mode);
        let ap = am.map(|m| m.metrics.total_ms.median);
        let bp = bm.map(|m| m.metrics.total_ms.median);
        // Resolve the budget ONCE per (op, C) and reuse it for both the printed guard and the
        // verdict, so the shown threshold can never disagree with the one that was applied.
        let resolved = opname.map(|name| thresholds.resolve(name, c));

        let a_cell = latency_cell(am);
        let b_cell = latency_cell(bm);
        // Gated delta: p50 % change + signed absolute ms change (so the ms floor is auditable).
        // Only shown when both p50s are valid (finite, > 0) — i.e. exactly when a verdict exists;
        // otherwise the cell is N/A and an absolute Δ would be misleading.
        let dp50 = match (ap, bp) {
            (Some(x), Some(y)) if x.is_finite() && x > 0.0 && y.is_finite() && y > 0.0 => {
                format!("{} ({:+.3})", pct(x, y), y - x)
            }
            _ => "—".to_string(),
        };
        // The configured guard for this exact cell — only resolvable for a known op.
        let guard = resolved.map(|r| r.guard_cell()).unwrap_or_else(|| "—".to_string());
        let verdict = if op_diverged {
            "🔴 N/A".to_string()
        } else {
            match (ap, bp, resolved) {
                (Some(x), Some(y), Some(r)) => {
                    let v = r.verdict(x, y);
                    match v {
                        Verdict::Regressed => {
                            *regressed += 1;
                            *comparable_cells += 1;
                        }
                        // A real (comparable) 🟢 cell.
                        Verdict::Ok => *comparable_cells += 1,
                        // Zero/non-finite p50 on either side ⇒ no verdict; not a comparable cell.
                        Verdict::NotApplicable => {}
                    }
                    v.emoji().to_string()
                }
                _ => "N/A".to_string(),
            }
        };
        out.push_str(&format!("| {c} | {a_cell} | {b_cell} | {dp50} | {guard} | {verdict} |\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::report::{DatasetInfo, MetricSet, Meta, OperationReport, ServerInfo};
    use crate::synthetic::stats::Summary;
    use std::collections::BTreeMap;

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
        let md = regression_markdown(&a, &b_ok, &g, &Thresholds::builtin(), None);
        assert!(md.contains("🟢"), "{md}");
        assert!(md.contains("no p50 regression"), "{md}");
        // over budget: +100% and +1 ms => red
        let b_bad = report(42002, 2.0, 500.0);
        let g2 = regression_guard(&a, &b_bad);
        let md2 = regression_markdown(&a, &b_bad, &g2, &Thresholds::builtin(), None);
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
        let md = regression_markdown(&a, &b, &g, &Thresholds::builtin(), None);
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
        let md = regression_markdown(&a, &b, &g, &Thresholds::builtin(), None);
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
        let reg = regression_markdown(&a, &b, &g, &Thresholds::builtin(), None);
        assert!(reg.contains("v1\\|x") && reg.contains("v2\\|y"), "regression headers not escaped: {reg}");
    }

    #[test]
    fn regression_na_cells_are_not_counted_as_comparable() {
        use crate::synthetic::baseline::regression_guard;
        use crate::synthetic::thresholds::Thresholds;
        // A zero baseline p50 ⇒ the cell's verdict is N/A; it must NOT inflate the comparable count.
        let a = report(42001, 0.0, 1000.0);
        let b = report(42002, 1.0, 1000.0);
        let g = regression_guard(&a, &b);
        let md = regression_markdown(&a, &b, &g, &Thresholds::builtin(), None);
        assert!(md.contains("across 0 comparable cell(s)"), "{md}");
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
        let md = regression_markdown(&a, &b, &g, &Thresholds::builtin(), Some(754.0));
        assert!(md.contains("**Thresholds**"), "settings table missing: {md}");
        assert!(md.contains("| _default_ | 10% | 0.5 ms |"), "{md}");
        assert!(md.contains("Budget precedence: per-op×concurrency"), "rule missing: {md}");
        assert!(
            md.contains("⏱ Computed in 12m 34s (benchmark + reporting)."),
            "timing missing: {md}"
        );
        // Without an elapsed value there is no compute-time line.
        let md_none = regression_markdown(&a, &b, &g, &Thresholds::builtin(), None);
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
        let md = regression_markdown(&a, &b, &g, &Thresholds::builtin(), None);
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
        let md = regression_markdown(&a, &b, &g, &Thresholds::builtin(), None);
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
        let md = regression_markdown(&a, &b, &g, &Thresholds::builtin(), None);
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
        let md = regression_markdown(&a, &b, &g, &t, None);
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
        let md = regression_markdown(&a, &b, &g, &Thresholds::builtin(), Some(300.0));
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
        let md = regression_markdown(&a, &b, &g, &Thresholds::builtin(), None);
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
        let md = regression_markdown(&a, &b, &g, &Thresholds::builtin(), None);
        assert!(md.contains("<code>x&lt;b&gt;&amp;y</code>"), "op not HTML-escaped: {md}");
        assert!(!md.contains("<code>x<b>&y"), "raw HTML leaked: {md}");
    }
}
