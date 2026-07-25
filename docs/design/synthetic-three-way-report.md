# Design: three-way synthetic PR report — PR vs main vs C engine, with an interactive page

**Status: draft (v2, post rubber-duck review)**
**Extends:** [`synthetic-pr-regression-report.md`](synthetic-pr-regression-report.md) (approved; Part A merged in this repo, Part B is falkordb-rs-next-gen PR #745).

## 1. Goal

Every falkordb-rs-next-gen PR currently gets a synthetic per-op regression report comparing the
**PR build vs Rust main** (`edge-rs`). Extend it to a **three-way comparison**:

| # | Comparison (baseline → candidate) | Question it answers | Budget profile |
|---|---|---|---|
| 1 | main → PR | did this PR regress the Rust engine? | `[ops]` (strict, existing) |
| 2 | C → PR | how does the PR stand vs the C engine? | `[cross-engine]` (looser) |
| 3 | C → main | how does Rust main stand vs the C engine? | `[cross-engine]` (looser) |

plus an **interactive GitHub-Pages report** (metric selector like the
[trend page](https://falkordb.github.io/falkordb-rs-next-gen/benchmark/trend/), per-comparison
views, a green/red verdict matrix with filtering across all/C/main) and the **total benchmark
wall-clock time in the report header** (already merged: `report --elapsed-secs`).

Comparison 3 (C → main) is intentionally included per user decision: the page must let us see the
C engine compared against **both** the PR build and current Rust main.

## 2. Decisions

| Decision | Choice | Rationale |
|---|---|---|
| C-engine image | `falkordb/falkordb:edge` **with `-e BROWSER=0`** | User picked the `falkordb:edge` bundle deliberately — measure the image users actually run. Its `run.sh` starts a Node browser server in the same container by default (verified: `BROWSER:-1` → `node server.js &`), which would contaminate latency, so the C leg passes `-e BROWSER=0` (supported by the image's `run.sh`) to run it server-only. `falkordb/falkordb-server:edge` (the A/B benchmark's C leg) was considered but rejected: user preference is the bundle image, and with `BROWSER=0` the delta is negligible. |
| Where the new next-gen work lands | Stacked PR on `barakb/synthetic-pr-regression` (#745's branch) | #745 is unmerged; stacking avoids conflicts and reviews only the delta. |
| Cross-engine budgets | Separate `[cross-engine]` section in the thresholds TOML, same shape as `[ops]` | Engines legitimately differ; strict same-engine budgets would drown the report in red. Separate profile keeps PR-gating strict while cross-engine stays informative. |
| Verdict computation | Rust tool emits per-cell verdicts as JSON (`report --cells`); the page's JS only renders | Single source of truth — no reimplementation drift between Markdown, summary, and page. |
| Page hosting | gh-pages, same isolated subtree as #745 (`synthetic-benchmark/…`) | Pages runs JS; sibling-JSON + self-contained HTML is the proven trend-page pattern. |
| Divergence vs perf | Split `correctness` from `perf` per cell/op (see §4.3): same-engine divergence stays 🔴; **cross-engine divergence is ⚠ (informative), not red** | Cross-engine result differences are routine (feature gaps, ordering); flagging them red would make every C comparison permanently red and useless as a signal. |

## 3. Verified current state (what exists vs what's missing)

Verified against benchmark `master` (ff1d459) and next-gen PR #745.

**Already merged in this repo (Part A of the parent design):** `run --label`,
`report --diff --regression --thresholds --elapsed-secs --out --summary`, per-op×C×cache p50
verdicts with budget precedence (op×C > op > default), `SyntheticSummary` schema v1, slug,
divergence detection via `result_digest`.

**Already in next-gen PR #745 (Part B):** `synthetic-run.sh` (record once → measure `pr`,
`main`, optional third `IMAGE_RELEASE` leg → one report per baseline + summaries in a
trap-surviving `SUMMARY_DIR`), `synthetic-publish.sh` (isolated gh-pages subtree,
`latest|branch/<view>` leaves), `render-synthetic-comment.py` (lean sticky comment from
summaries), `render-report-html.py` (static pre-rendered page), `_benchmark.yml` synthetic jobs
(pinned `SYNTHETIC_BENCHMARK_REF`), thresholds + workload TOMLs.

**Gaps this design closes:**

| # | Gap | Where |
|---|---|---|
| G1 | **Dynamic op names get no budget/tier** — `diff.rs` resolves budgets via `OpName::from_tag` (legacy catalog enum); every `--repo-reads` shape (e.g. `single_vertex_read`) resolves to `None` → guard `—`, verdict N/A, tier `None`. #745 records `--repo-reads full`, so ~49/50 ops would render N/A. A string-keyed `Thresholds::resolve_by_name` already exists (tested with dynamic shapes) but is never called from the regression path; the TOML parser also rejects dynamic op keys. | benchmark `src/synthetic/diff.rs:339,:443,:623,:788`, `thresholds.rs:219,:270` |
| G2 | No machine-readable **per-cell** verdicts (only per-op summary counts); Markdown and summary independently re-enumerate cells. | benchmark `diff.rs` |
| G3 | Verdict conflates correctness + perf: diverged ops are unconditionally 🔴, wrong for cross-engine. | benchmark `diff.rs:354-400` |
| G4 | No cross-engine budget profile in the thresholds format. | benchmark `thresholds.rs` |
| G5 | No C leg, no per-comparison failure isolation, digest resolution breaks on Docker Hub images (`docker.io/` prefix never matches `RepoDigests`' normalized `falkordb/falkordb@sha256:…`). | next-gen `synthetic-run.sh` |
| G6 | Page is static pre-render, one comparison; no metric selector, no verdict matrix, no filters. | next-gen `render-report-html.py` |
| G7 | Summary JSON lacks budget-profile/elapsed metadata; comment can't distinguish profiles. | benchmark `diff.rs` (schema), next-gen comment renderer |

## 4. Part A — benchmark repo (the tool)

### A0. Prerequisite fix: budgets + tiers for dynamic op names (G1)

This is a **latent bug fix** independent of the three-way feature, delivered first:

- `diff.rs`: replace every `OpName::from_tag(op)`-based budget resolution with
  `Thresholds::resolve_by_name(op, c)` (already implemented + tested); budgets then apply to
  recorded repo-read shapes exactly as to legacy catalog ops.
- Tier lookup by **name**: `shapes.rs` owns the repo-read shapes with their `Tier`; add a
  string-keyed tier lookup (legacy catalog names keep resolving via `OpName::from_tag`; repo-read
  shape names resolve via the shapes registry; unknown names stay `None`).
- `thresholds.rs::from_toml_str`: accept `[ops.<name>]` keys that match either a legacy catalog
  tag **or** a known repo-read shape name; keep rejecting truly unknown keys (typo guard).
- Tests: end-to-end regression-render test where a dynamic op (`single_vertex_read`) gets a real
  budget from TOML and a tier in the summary; TOML parse accepts shape names, rejects typos.

### A1. One analysis model, three consumers (G2)

Build the comparison **once** into a `RegressionAnalysis` and render everything from it:

```text
RegressionAnalysis {
  baseline_label, candidate_label, slug, elapsed_secs: Option<u64>,
  budget_profile: String,              // "ops" | "cross-engine"
  ops: BTreeMap<String, OpAnalysis>,   // op → analysis
}
OpAnalysis {
  tier: Option<Tier>,
  correctness: Correctness,            // Match | Diverged | Unknown (a side missing digests)
  cells: Vec<CellAnalysis>,
}
CellAnalysis {
  concurrency, cache_mode,
  baseline_p50_ms / candidate_p50_ms: Option<f64>,
  delta_pct / delta_ms: Option<f64>,
  budget: Option<ResolvedBudget { pct, floor_ms, source }>,
  perf_verdict: PerfVerdict,           // Ok | Regressed | NotApplicable
  context: {p90, p99, throughput for both sides},   // informational, never gated
}
```

`regression_markdown`, `summarize` (summary JSON) and the new `--cells` export all **consume**
this model; `op_cell_counts` (today's duplicate enumeration) is deleted. A unit test asserts
Markdown cell verdicts equal the model's (no drift by construction).

### A2. `--cells <path>`: machine-readable per-cell verdicts

`report --diff A B --regression … --cells cells.json` writes the full `RegressionAnalysis` as
JSON (`schema_version: 1`, serde snake_case; example in §5.3's page data). `--cells`, like
`--summary`, is only valid with `--diff --regression` (clap `requires`); a doc-tested example
lands in the readme. This is the page's only data source — JS never computes a verdict.

### A3. Correctness/perf split + divergence policy (G3)

- Per-op `correctness` (Match/Diverged/Unknown) is separated from per-cell `perf_verdict`
  (already in the A1 model). Markdown and summary keep today's **same-engine** behavior: a
  diverged op is 🔴 and its perf cells are N/A.
- New: divergence **presentation policy** comes from the budget profile. Under
  `--budget-profile cross-engine`, a diverged op renders **⚠ diverged (informative)** — not 🔴 —
  and is counted in a separate `diverged` bucket, never in `regressed`. Top-line verdict for a
  cross-engine comparison is driven by perf cells only, with the ⚠ count shown beside it.
- `OpOutcome` gains `Diverged` (today divergence is folded into `Regressed`); summary schema
  bumps to v2 (see A5).

### A4. `[cross-engine]` budget profile (G4)

Thresholds TOML gains an optional section with the same shape as the strict defaults +
`[ops.*]` overrides:

```toml
[default]                 # existing strict same-engine profile
budget_pct = 5.0
floor_ms   = 0.3

[cross-engine]            # new: looser cross-engine profile
budget_pct = 25.0
floor_ms   = 1.0
[cross-engine.ops.single_vertex_read]   # optional per-op overrides, same precedence rules
budget_pct = 40.0
```

`report` gains `--budget-profile <ops|cross-engine>` (default `ops`, requires `--regression`).
Profile name is recorded in the analysis/summary/cells output (G7). Missing `[cross-engine]`
section + `--budget-profile cross-engine` is a hard error (no silent fallback to strict).

### A5. Summary schema v2 (G7)

`SyntheticSummary` bumps `schema_version` to 2, adding: `budget_profile`, `elapsed_secs:
Option<u64>`, `gated_metric: "total_ms.p50"`, and `diverged` as a first-class outcome count
(`OutcomeCounts` gains it; `OpOutcome::Diverged` from A3). The comment renderer (Part B) is the
only consumer and ships in the same stacked PR, so no compat shim is needed — but the renderer
still hard-checks `schema_version` and fails loudly on mismatch.

### A6. Small fixes folded in

- **`--elapsed-secs` single emission**: it is run-level metadata, not per-comparison. The header
  line moves to the *caller-assembled* comment/page header; `report` keeps accepting the flag
  (renders "benchmark wall-clock" only when passed) so single-diff use keeps working. The CI
  script passes it **once** (to the cells/summary metadata), not to all three Markdown reports.
- **Placeholder-version warning suppression**: edge/RC images report version `999999`; the
  "same version on both sides" warning is suppressed when the version is the known placeholder.

### A7. Release + docs

Readme (synthetic section) documents `--cells`, `--budget-profile`, the `[cross-engine]` TOML
section and the dynamic-op-name budget behavior; `copilot-instructions` recipe table untouched
(no recipe changes). Tag a new release (post-v2.2) once merged so next-gen can pin
`SYNTHETIC_BENCHMARK_REF` to a released SHA.

## 5. Part B — falkordb-rs-next-gen (CI + interactive page)

Stacked PR on `barakb/synthetic-pr-regression`. All scripts stay under
`.github/scripts/benchmark/`.

### B1. C-engine leg in `synthetic-run.sh` (G5)

- New env: `IMAGE_CENGINE` (default `docker.io/falkordb/falkordb:edge`), `CENGINE_DOCKER_ARGS`
  (default `-e BROWSER=0`). The existing optional `IMAGE_RELEASE` third-leg pattern is replaced
  by the C leg (release comparison was never wired by any caller).
- **Digest resolution fix**: `resolve_digest` currently prefix-matches the requested ref against
  `RepoDigests`, which fails for `docker.io/…` refs (Docker normalizes to
  `falkordb/falkordb@sha256:…`). Normalize the ref (strip `docker.io/`, add `library/` only for
  official images) before matching; covered by a bats-style shell test or an inline self-test
  mode, and by the sanity run.
- **Failure isolation / ordering** (a C-engine hiccup must never cost the PR-vs-main signal):
  1. record once; measure `pr`, then `main`; **immediately** produce and persist the
     main→pr report + summary + cells into the trap-surviving dirs;
  2. only then resolve + measure the C leg under `set +e` guards; on any failure, write a
     `summary-cengine-unavailable.json` stub (`verdict: not_comparable`,
     `not_comparable_reason`) for **both** C comparisons and continue;
  3. on success, produce C→pr and C→main reports/summaries/cells.
- Both cross-engine diffs run with `--budget-profile cross-engine`; main→pr keeps the strict
  default profile. `ELAPSED_SECS` measured once after all measuring, passed once (A6).

### B2. Artifact flow (who writes what)

The **measure job** owns all data artifacts: `SUMMARY_DIR` (exists today) gains a sibling
`CELLS_DIR`, both surviving the `WORKDIR` cleanup trap and uploaded as one artifact. Per run:
`summary-{main,cengine-pr,cengine-main}.json`, `cells-{main,cengine-pr,cengine-main}.json`,
`report-{…}.md`, `run-meta.json` (elapsed, arch, images, profile names, PR number, head SHA).
The **publish job** only assembles and pushes: page HTML + the JSONs, never recomputing.

### B3. Interactive page (G6)

`render-report-html.py` is replaced by `build-synthetic-page.py` + `synthetic-report.html`
(template): a **single dependency-free HTML file** (inline CSS/JS, no external libs, no build
step) that fetches its **sibling `data.json`** (the three cells files + run-meta merged by the
builder). This copies the trend page's *visual* pattern (segmented metric buttons, cards,
light/dark) — not its innerHTML string-building: the page renders via `createElement` /
`textContent` only, so op labels/engine labels are inert (builder-side test injects
`<script>`-shaped labels and asserts they render as text).

Page model (all client-side, driven only by `data.json`):

- **Comparison selector** (segmented): `PR vs main` · `PR vs C` · `main vs C` · `matrix`.
- **Metric selector** (segmented, like trend): `p50` · `p90` · `p99` · `throughput`. p50 is the
  gated metric (verdict badge); other metrics render values/deltas labeled *informational* —
  throughput deltas render with reversed better-direction arrows.
- **Cache-mode selector** when both modes exist in the data (`uncached` default, per current CI
  sweep); hidden when only one mode is present.
- **Matrix view** (the summary the user asked for): rows = ops, columns = the three comparisons,
  cell = op verdict emoji (🟢/🔴/⚠/N-A from the Rust cells data, worst-cell-wins per op:
  🔴 > ⚠ > 🟢 > N/A). **Filter chips**: `all` · `any red` (OR across selected comparisons) ·
  `all green` (AND) · comparison-scoped `red in C-comparisons only` · `red vs main only`;
  a text filter narrows by op name.
- **Card/table view** per comparison: per-op collapsible tables (C × cache-mode × metric),
  verdict column straight from `perf_verdict`, ⚠ correctness banner per A3 policy.
- **Header**: PR number/SHA, arch, images (with digests), budget profiles, and **total
  benchmark wall-clock** from `run-meta.json`.
- **Two-way degradation**: the page renders whatever comparisons exist in `data.json` — the
  canonical/nightly publish path (only C→main available) and the C-unavailable stub both render
  correctly with absent comparisons greyed out (`not_comparable_reason` shown). This closes the
  canonical-path gap without a separate page.

### B4. Sticky comment

`render-synthetic-comment.py` renders **three** verdict lines (one per comparison, from the v2
summaries; ⚠ diverged counts shown for cross-engine), the total wall-clock, worst offenders for
the main→pr comparison only (the gating signal), and one link to the interactive page. Markers
(`<!-- synthetic-benchmark -->` / `-arm`) unchanged.

### B5. Workflow wiring (`_benchmark.yml`)

- Pass `IMAGE_CENGINE`; bump `SYNTHETIC_BENCHMARK_REF` to the new benchmark release SHA;
  upload `SUMMARY_DIR`+`CELLS_DIR` artifact; `timeout-minutes: 90` (three measured legs ≈ +50%
  over two; 60→90 gives real headroom).
- **Closed-PR race**: `synthetic-publish` re-checks PR state (`gh pr view --json state`) and
  skips publishing when closed, so a close event racing the arm leg can't resurrect a leaf the
  cleanup job just removed.
- **Fork PRs**: explicitly out of scope (the existing prepare job already excludes fork heads —
  images aren't pushed for forks); the design states this rather than pretending otherwise.

## 6. What the rubber-duck review corrected (v1 → v2)

An independent review (two-repo verification) found 6 blocking, 8 important, 3 nit issues in v1;
all are folded in above: dynamic-op budgets/tiers were silently N/A (→ A0, new prerequisite);
per-cell verdicts had no single source (→ A1); divergence semantics conflated correctness with
perf and would render cross-engine permanently red (→ A3, ⚠ policy); `docker.io/` digests never
matched `RepoDigests` (→ B1 normalization); the bundle image's browser server would contaminate
measurements (→ `-e BROWSER=0`, decisions table); a C failure aborted the whole run losing the
PR-vs-main signal (→ B1 ordering/isolation); cells artifacts would die with the workdir trap
(→ B2 `CELLS_DIR`); summary lacked profile/elapsed metadata (→ A5); triple-emitted elapsed line
(→ A6); undefined card view under two cache modes (→ B3 cache selector); undefined filter
algebra and throughput direction (→ B3); canonical two-way path unrendered (→ B3 degradation);
closed-PR/arm publish race (→ B5); fork PRs unstated (→ B5); timeout 80 was not the claimed
+50% (→ 90); placeholder-version warning noise (→ A6); "CSP-friendly like the trend page" was
false — the trend page uses innerHTML (→ B3 `textContent`-only + escaping test).

## 7. Deliverables & order

1. **benchmark PR 1 (A0)** — dynamic-op budgets/tiers bug fix. Small, independently valuable.
2. **benchmark PR 2 (A1–A7)** — analysis model, `--cells`, profiles, summary v2, docs; then tag.
3. **next-gen PR (B1–B5)** — stacked on #745, pinned to the new tag.
4. `just synthetic-sanity` extended to exercise `--cells` + `--budget-profile cross-engine`
   round-trip; CI (`synthetic-verify`) unchanged (still two identical-build runs).

Each PR: design-first (this doc), ≥90% patch coverage on Rust changes, `just ci` +
`just coverage` green locally, docs synced. next-gen PRs await human review (no self-merge).

## 8. Out of scope

- Gating (red stays non-blocking); historical trend storage for synthetic results; comparing
  more than the three fixed images; fork-PR support; changing the A/B (non-synthetic) benchmark.
