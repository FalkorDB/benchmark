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

/// The display name for a run's column: its `--label` if set, else the `fallback` (`A`/`B`).
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
) -> String {
    let la = col_label(baseline, "baseline");
    let lb = col_label(candidate, "candidate");
    let mut head = String::new();
    head.push_str(&format!(
        "### 🧪 Synthetic per-op regression — {} vs {}\n\n",
        md_cell(&lb),
        md_cell(&la)
    ));
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
        body.push_str(&format!(
            "\n#### `{op}`{}\n",
            if op_diverged { "  —  ⚠ results differ (perf verdict N/A)" } else { "" }
        ));
        let opname = OpName::from_tag(op);
        for mode in [Mode::Cached, Mode::Uncached] {
            render_regression_mode(
                &mut body, baseline, candidate, op, opname, mode, op_diverged, thresholds, &la,
                &lb, &mut regressed, &mut comparable_cells,
            );
        }
    }

    // Assemble: header + summary + warnings + legend + body.
    let mut out = head;
    let summary = if regressed == 0 {
        format!("🟢 no p50 regression beyond budget across {comparable_cells} comparable cell(s)")
    } else {
        format!("🔴 {regressed} of {comparable_cells} comparable cell(s) over budget")
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
         N/A = no perf verdict. Non-blocking.\n",
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
        "| C | {} p50 (ms) | {} p50 (ms) | Δp50 | {} tput | {} tput | Δtput | verdict |\n\
         |---:|---:|---:|---:|---:|---:|---:|:--:|\n",
        md_cell(la),
        md_cell(lb),
        md_cell(la),
        md_cell(lb),
    ));
    for c in levels {
        let am = level_metrics(a, op, c, mode);
        let bm = level_metrics(b, op, c, mode);
        let ap = am.map(|m| m.metrics.total_ms.median);
        let bp = bm.map(|m| m.metrics.total_ms.median);
        let a_s = ap.map(|v| format!("{v:.3}")).unwrap_or_else(|| "—".to_string());
        let b_s = bp.map(|v| format!("{v:.3}")).unwrap_or_else(|| "—".to_string());
        let dp50 = match (ap, bp) {
            (Some(x), Some(y)) => pct(x, y),
            _ => "—".to_string(),
        };
        let a_tp = am
            .map(|m| format!("{:.0}", m.throughput_ops_per_sec))
            .unwrap_or_else(|| "—".to_string());
        let b_tp = bm
            .map(|m| format!("{:.0}", m.throughput_ops_per_sec))
            .unwrap_or_else(|| "—".to_string());
        let dtp = match (am, bm) {
            (Some(x), Some(y)) => pct(x.throughput_ops_per_sec, y.throughput_ops_per_sec),
            _ => "—".to_string(),
        };
        let verdict = if op_diverged {
            "🔴 N/A".to_string()
        } else {
            match (ap, bp, opname) {
                (Some(x), Some(y), Some(name)) => {
                    let v = thresholds.resolve(name, c).verdict(x, y);
                    match v {
                        Verdict::Regressed => {
                            *regressed += 1;
                            *comparable_cells += 1;
                        }
                        // A real (comparable) 🟢 cell.
                        Verdict::Ok => *comparable_cells += 1,
                        // Zero/non-finite baseline ⇒ no verdict; not a comparable cell.
                        Verdict::NotApplicable => {}
                    }
                    v.emoji().to_string()
                }
                _ => "N/A".to_string(),
            }
        };
        out.push_str(&format!(
            "| {c} | {a_s} | {b_s} | {dp50} | {a_tp} | {b_tp} | {dtp} | {verdict} |\n"
        ));
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
        let md = regression_markdown(&a, &b_ok, &g, &Thresholds::builtin());
        assert!(md.contains("🟢"), "{md}");
        assert!(md.contains("no p50 regression"), "{md}");
        // over budget: +100% and +1 ms => red
        let b_bad = report(42002, 2.0, 500.0);
        let g2 = regression_guard(&a, &b_bad);
        let md2 = regression_markdown(&a, &b_bad, &g2, &Thresholds::builtin());
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
        let md = regression_markdown(&a, &b, &g, &Thresholds::builtin());
        assert!(md.contains("results differ"), "{md}");
        assert!(md.contains("🔴 N/A"), "{md}");
    }

    #[test]
    fn regression_not_comparable_when_workload_differs() {
        use crate::synthetic::baseline::regression_guard;
        use crate::synthetic::thresholds::Thresholds;
        let a = report(42001, 1.0, 1000.0);
        let mut b = report(42002, 1.0, 1000.0);
        b.meta.dataset.as_mut().unwrap().workload_hash = "sha256:zzz".to_string();
        let g = regression_guard(&a, &b);
        let md = regression_markdown(&a, &b, &g, &Thresholds::builtin());
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
        let reg = regression_markdown(&a, &b, &g, &Thresholds::builtin());
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
        let md = regression_markdown(&a, &b, &g, &Thresholds::builtin());
        assert!(md.contains("across 0 comparable cell(s)"), "{md}");
    }
}
