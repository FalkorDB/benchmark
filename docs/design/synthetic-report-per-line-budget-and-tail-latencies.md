# Design — per-line budget/floor + non-gated tail latencies (p90/p99) in the synthetic regression report

**Status:** proposal for review — **not implemented**. Shared for approval before any code lands.
**Scope:** presentation only, in `benchmark synthetic report --diff … --regression`
(`regression_markdown` / `render_regression_mode`, `src/synthetic/diff.rs`). **No change** to the
gate (still **p50-only**, `ResolvedBudget::verdict`), the comparable-cell counts, the report JSON, or
the CLI. This doc was rubber-duck reviewed; §7/§8/§9 fold in that feedback.

## 1. Goals

1. **Show the effective budget + floor on every cell row** — the exact threshold that applied to that
   `op × cache-mode × concurrency` cell (including per-op×concurrency overrides), so a reviewer never
   has to cross-reference the header **Thresholds** table.
2. **Show p90 + p99 tail latencies** for context, **without gating on them**, in a way that is
   **compact** yet **unmistakably not part of the gate**.

## 2. Current format (reference)

Per `op × cache-mode`, one table, one row per concurrency `C` (8 columns):

```
_cached_

| C | main p50 (ms) | pr p50 (ms) | Δp50 | main tput | pr tput | Δtput | verdict |
|--:|--:|--:|--:|--:|--:|--:|:--:|
| 1 | 0.180 | 0.205 | +13.8% | 5550 | 4880 | -12.1% | 🟢 |
| 2 | 0.190 | 0.202 |  +6.4% | 10200 | 9600 |  -5.9% | 🟢 |
```

Since #235 the **header** already renders a `Thresholds` summary table (`Thresholds::settings_markdown`,
default + per-op/per-op×C overrides) and the 🟢/🔴 rule. The per-cell `budget_pct`/`floor_ms` already
exist — `render_regression_mode` calls `thresholds.resolve(op, C)` for the verdict; today that resolved
value is not printed on the row.

## 3. Recommended layout — lean gated line + a folded context line (your steer)

Keep the **primary line** on the gate (**p50** value, `Δp50`, absolute `Δms`, **guard**, verdict) and
**fold the non-gated data** (p90/p99; absolute throughput) into a smaller second line **under each
cell**, via `<br>` + a `context:` label (both render in GitHub PR comments and Actions job summaries —
verified against GitHub's Markdown renderer). One row per `C`; each row reads as two visual lines.

Raw Markdown:

```
_cached_ — **only p50 is gated.** The `context:` line (p90/p99, throughput) is informational and never affects the verdict.

| C | main p50 (ms) | pr p50 (ms) | Δp50 (Δms) | p50 guard (>% AND >ms) | verdict |
|--:|--:|--:|--:|:--:|:--:|
| 1 | 0.180<br><sub>context: p90 0.34 · p99 0.51 · 5550 op/s</sub> | 0.205<br><sub>context: p90 0.39 · p99 0.60 · 4880 op/s</sub> | +13.8% (+0.025) | 15% AND 0.5 ms | 🟢 |
| 2 | 0.190<br><sub>context: p90 0.36 · p99 0.55 · 10200 op/s</sub> | 0.202<br><sub>context: p90 0.38 · p99 0.58 · 9600 op/s</sub> |  +6.4% (+0.012) | 15% AND 0.5 ms | 🟢 |
```

Rendered, each row is two lines — the gate on top, the tucked-under context beneath each value:

```
 C | main p50 (ms)          | pr p50 (ms)           | Δp50 (Δms)      | p50 guard (>% AND >ms) | verdict
 2 | 0.190                  | 0.202                 | +6.4% (+0.012)  | 15% AND 0.5 ms         | 🟢
     context: p90 0.36 …       context: p90 0.38 …
```

Why this meets both goals:

- **Budget/floor per line** — the `p50 guard (>% AND >ms)` column prints the **same** `ResolvedBudget`
  object used for the verdict, e.g. `15% AND 0.5 ms`; a per-C override (e.g. `expand_hops_5` at C=16 →
  `18% AND 0.5 ms`) is visible in context. The `AND` header makes explicit that a 🔴 needs **both**
  the % **and** the ms floor exceeded (`verdict` in `thresholds.rs`).
- **p90/p99 shown, clearly not gated** — the tails live only on the smaller `context:` line, the
  header keeps `main p50 (ms)`/`pr p50 (ms)` (p50 named, not implied), the `Δp50`/`guard`/`verdict`
  columns are all p50, and the caption states *"only p50 is gated; the context line never affects the
  verdict."* Absolute throughput is folded into the same context line (also not gated).
- **Auditable floor** — the primary line adds the absolute `Δms` next to `Δp50` so the reader can
  check the ms floor directly (the % alone can't be checked against a ms floor).

## 4. Alternatives (so we choose deliberately)

- **A. Collapsed context table (rubber-duck's pick; most size- and clarity-safe).** Keep a lean 6-col
  gated table (`C | main p50 | pr p50 | Δp50 (Δms) | guard | verdict`), then **one collapsed block per
  mode**: `<details><summary>tail latencies + throughput (context — not gated)</summary>` wrapping a
  small table `C | main p90/p99 | pr p90/p99 | main→pr op/s (Δ)`. Strongest gate/context separation and
  the **smallest** rendered comment; the tails are one click away.
- **B. Packed single-line cell.** `0.190 · 0.36 · 0.55` (p50·p90·p99 in one cell, no second line).
  Most vertically compact, but tails share the p50 line, so "not gated" rests on the caption alone.
- **C. Extra plain columns** (`main p90`, `pr p90`, `main p99`, `pr p99`). Clearest alignment, but ~13
  columns — wide horizontal scroll and the largest comment.

**Recommendation:** ship **§3** (your folded-line steer) with the §7 guardrails, *or* **§4-A** if we
want the leanest comment. §3 keeps every number on-screen; §4-A is smaller and separates gate/context
most strongly. This is **open question §8.1**.

## 5. Gated vs. context (the gate does not change)

| Value | Role | Where |
| --- | --- | --- |
| **p50** (`total_ms.median`) | **Gated** — unchanged `ResolvedBudget::verdict` | primary line |
| p90, p99 (`total_ms.p90/.p99`) | Context only | `context:` line (or §4-A block) |
| throughput (per-side `op/s`) | Context only | `context:` line |
| budget_pct / floor_ms | The threshold **applied to p50** | new `guard` column |
| absolute Δms | Audit aid for the ms floor | primary line, next to Δp50 |

`p95` is also computed but **left out initially** (§8.2). Tail **Δ**s are left out initially (§8.3).

## 6. Data availability (no new collection)

- `Summary` already carries `median`/`p90`/`p95`/`p99` (`src/synthetic/stats.rs`); percentiles are a
  monotonic non-decreasing sequence over the same sorted samples, so `p90 ≥ p50` always holds — a
  `p90 < p50` would mean malformed external JSON; the renderer prints values verbatim (no reordering).
- `thresholds.resolve(op, C) -> ResolvedBudget { budget_pct, floor_ms }` already exists and is already
  called for the verdict. **Resolve once per (op, C) and reuse the same object for both the verdict and
  the `guard` cell**, so the printed threshold can never disagree with the one that was applied.

Pure formatting change in the renderer; measurement/recording/JSON untouched.

## 7. Implementation plan + guardrails (from review)

- `render_regression_mode` (`src/synthetic/diff.rs`): new column set; build each p50 cell as
  `{p50:.3}<br><sub>context: p90 {..:.3} · p99 {..:.3} · {tput:.0} op/s</sub>`; print `Δms` as the
  signed absolute delta; print the `guard` from the reused `ResolvedBudget`.
- **Threshold formatting must be lossless-enough:** print `budget_pct` and `floor_ms` with trailing
  zeros trimmed and enough precision to round-trip the configured value (e.g. `15%`, `12.5%`,
  `0.5 ms`, `0.05 ms`) — never round `10.04%`→`10.0%`. Guard against implying false precision. Apply
  the **same** formatter to the header `Thresholds::settings_markdown` (today fixed `{:.1}%` / `{:.2}
  ms`) so the header *policy* and the per-line *guard* can never disagree on the same number.
- **Missing / partial data:** `LevelMetrics` is optional per side. If one side is absent, still render
  the present side and show the absent side's tails **and** throughput as `—` (e.g.
  `context: p90 — · p99 — · — op/s`). A zero/None/non-finite p50 on **either** side (baseline or
  candidate) stays **N/A**, matching `ResolvedBudget::verdict` (unchanged). For an
  **unknown op** (`OpName::from_tag` → `None`) there is no resolvable guard → print `—`. For a
  **diverged** op **whose name is known**, show the context tails and the resolved `guard` value with
  the verdict already `🔴 N/A` ("shown, not evaluated"); an **unknown** diverged op stays `—` in the
  guard (no `resolve` input). Neither counts toward comparable cells.
- Header `Thresholds` table (`settings_markdown`): **keep** as the policy-at-a-glance summary (defaults
  + overrides); per-line `guard` is the *effective* value, the header is the *policy* (drop only if
  §8.1 says so).
- **Tests (must prove tail isolation, not just presence):**
  - unchanged p50 + a *catastrophic* p90/p99 regression → **same** 🟢 verdict and same comparable count;
  - 🔴 p50 + *improved* tails → still 🔴;
  - zero/missing p50 with valid tails → N/A, **0** comparable;
  - diverged op → tails rendered but excluded from counts;
  - per-C override renders its budget **with the inherited per-op floor**;
  - assert the **summary line + verdict sequence** (not the whole Markdown), plus that the `guard` and
    `context:` substrings render.
- Docs: `readme.md` `--regression` section + this doc.

## 8. Open questions (for you)

1. **Layout:** §3 (folded context line — your steer) **or** §4-A (collapsed context table, leanest +
   clearest separation)?
2. **p95:** include it (`p90 · p95 · p99`) or keep just `p90 · p99`?
3. **Tail Δ:** show only raw p90/p99 (compact) or also `Δp90`/`Δp99` to quantify tail moves?
4. **Header Thresholds table:** keep it as the policy summary, or drop it now that each line shows the
   effective guard?
5. **`<br>` dependency:** the report targets GitHub Markdown (PR comments + job summaries) where `<br>`
   renders; it degrades in a plain terminal. OK to depend on that? (§4-A/B/C avoid multi-line cells.)

## 9. Comment-size budget (important, from review)

The `falkordb-rs-next-gen` sticky comment embeds this report (up to ~108 cells: ~9 read ops × 6
concurrencies × 2 cache modes). Folding adds `<br><sub>context: …</sub>` per side per row, which can
**roughly double** the rendered size and push a large run toward GitHub's **65,536-char** comment cap.
Mitigation, to decide alongside the layout:

- Prefer **§4-A** (collapsed `<details>`) if size is a concern — it adds little to the primary table.
- Add a **rendered-size assertion** in the tool tests (fail if a full 108-cell report exceeds a safe
  budget, e.g. ~45 KB), and have the **Part-B job** post a **lean comment** (gate table only) while
  uploading the **full report** to the job summary + artifact if the budget is exceeded. `<details>`
  hides visual bulk but **not** body length, so the byte budget is what matters.

## 10. Rollout

Land in `FalkorDB/benchmark` (renderer + tests + readme); `falkordb-rs-next-gen` picks it up on the
next `SYNTHETIC_BENCHMARK_REF` bump (same mechanism as the settings/compute-time change). Non-blocking,
report-only; the `synthetic-verify` determinism gate is unaffected (it compares result digests, not the
Markdown).
