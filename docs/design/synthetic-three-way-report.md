# Design: three-way synthetic PR report — PR vs main vs C engine, with an interactive page

**Status:** draft (v5, after four rubber-duck review rounds)
**Extends:** [`synthetic-pr-regression-report.md`](synthetic-pr-regression-report.md) (approved; Part A merged in this repo, Part B is falkordb-rs-next-gen PR #745).

## 1. Goal

Every falkordb-rs-next-gen PR currently gets a synthetic per-op regression report comparing the
**PR build vs Rust main** (`edge-rs`). Extend it to a **three-way comparison**:

| ID | Comparison (baseline → candidate) | Question it answers | Budget profile |
|---|---|---|---|
| `main-pr` | main → PR | did this PR regress the Rust engine? | strict (existing) |
| `c-pr` | C → PR | how does the PR stand vs the C engine? | `cross-engine` (looser) |
| `c-main` | C → main | how does Rust main stand vs the C engine? | `cross-engine` (looser) |

The comparison IDs above are **stable identifiers** used in filenames, JSON, and the page's
filter logic. Comparison `c-main` is intentionally included (user decision, re-confirmed
2026-07-25): it is the **attribution baseline** for `c-pr` — when PR-vs-C is red it tells you
whether main was already red vs C (pre-existing gap) or the PR introduced it, and it costs only
one extra offline `report` invocation since C is measured once anyway.

Also: an **interactive GitHub-Pages report** (metric selector like the
[trend page](https://falkordb.github.io/falkordb-rs-next-gen/benchmark/trend/), per-comparison
views, a green/red verdict matrix with filtering) and the **total benchmark wall-clock time in
the report header** (tool support already merged: `report --elapsed-secs`).

## 2. Decisions

| Decision | Choice | Rationale |
|---|---|---|
| C-engine image | `falkordb/falkordb:edge` **run with `--env BROWSER=0`** | User picked the bundle image deliberately. Its entrypoint starts a Node browser server in-container when `${BROWSER:-1}` evaluates to `1` (i.e. by default; verified in FalkorDB/FalkorDB `build/docker/run.sh:3-7`), so the C leg hardcodes `--env BROWSER=0` in the `docker run` line — no pass-through env var (shell-fragile, per review) — to keep the measured container server-only. |
| Where the new next-gen work lands | Stacked PR on `barakb/synthetic-pr-regression` (#745's branch) | #745 is unmerged; stacking avoids conflicts and reviews only the delta. |
| Cross-engine budgets | New `[cross-engine]` profile in the thresholds TOML, mirroring the existing `[default]`/`[op.*]` syntax | Engines legitimately differ; strict same-engine budgets would drown the report in red. |
| Verdict computation | The Rust tool emits per-cell **and per-op and overall** verdicts as JSON (`report --cells`); page JS only renders, never computes | Single source of truth; no drift between Markdown, summary, and page. |
| Divergence policy | Explicit `--divergence-policy <gate\|advisory>` flag (not inferred from the budget profile): `gate` (same-engine) = correctness 🔴, perf cells N/A; `advisory` (cross-engine) = correctness ⚠, perf cells N/A | Diverged results mean the engines did **different work**, so a latency verdict would be meaningless — perf is N/A under both policies; only the severity of the correctness signal differs. A separate flag means a budget-profile typo can never silently downgrade a same-engine correctness failure. A divergence-only comparison is never green. |
| Page hosting | gh-pages, same isolated subtree as #745 (`synthetic-benchmark/…`) | Pages serves static files + JS; sibling-JSON + self-contained HTML is the proven trend-page pattern. |
| Canonical/nightly runs | **Explicitly out of scope** — the synthetic pipeline stays PR-only | #745's runner/cleanup/publisher are PR-only end to end (`IMAGE_PR` required, `IS_CANONICAL=false` hardcoded). Wiring a canonical C→main mode is a separate feature (measurement mode, job conditions, cleanup, publish flow); the page still renders any subset of comparisons defensively (§B3), which is a robustness property, not canonical support. |

## 3. Verified current state (what exists vs what's missing)

Verified against benchmark `master` (ff1d459) and next-gen PR #745 (`cb52b36`).

**Already merged in this repo (Part A of the parent design):** `run --label`,
`report --diff --regression --thresholds --elapsed-secs --out --summary`, per-op×C×cache p50
verdicts with budget precedence (op×C > op > default; built-in defaults 10% / 0.5 ms),
`SyntheticSummary` schema v1, slug, divergence detection via order-independent `result_digest`
(asymmetric missing digest = diverged; both missing = no correctness info, still timed).

**Already in next-gen PR #745 (Part B):** `synthetic-run.sh` (record once → measure `pr`,
`main`, optional third `IMAGE_RELEASE` leg — verified unused by every caller → one report per
baseline + summaries in a trap-surviving `SUMMARY_DIR`), `synthetic-publish.sh` (isolated
gh-pages subtree, `latest|branch/<view>` leaves), `render-synthetic-comment.py` (lean sticky
comment; skips-with-warning on unknown summary `schema_version`), `render-report-html.py`
(static pre-rendered page), `_benchmark.yml` synthetic jobs (pinned `SYNTHETIC_BENCHMARK_REF`),
thresholds + workload TOMLs.

**Gaps this design closes:**

| # | Gap | Where |
|---|---|---|
| G1 | **Dynamic op names get no budget/tier** — `diff.rs` resolves budgets via `OpName::from_tag` (legacy catalog enum); every `--repo-reads` shape (e.g. `single_vertex_read`) resolves to `None` → guard `—`, verdict N/A, tier `None`. #745 records `--repo-reads full`, so ~49/50 ops would render N/A. A string-keyed `Thresholds::resolve_by_name` already exists (tested with dynamic shapes) but is never called from the regression path; the TOML parser also rejects dynamic op keys. | benchmark `src/synthetic/diff.rs:339,:443,:623,:788`, `thresholds.rs:219,:270` |
| G2 | No machine-readable **per-cell** verdicts; Markdown and summary independently re-enumerate cells. | benchmark `diff.rs` |
| G3 | Verdict conflates correctness + perf: diverged ops are unconditionally 🔴 — wrong severity for cross-engine. | benchmark `diff.rs:354-400` |
| G4 | No cross-engine budget profile in the thresholds format. | benchmark `thresholds.rs` |
| G5 | No C leg; no per-comparison failure isolation; digest resolution breaks on Docker Hub refs (`docker.io/` prefix never matches `RepoDigests`' normalized `falkordb/falkordb@sha256:…`). | next-gen `synthetic-run.sh` |
| G6 | Page is a static pre-render of one comparison; no metric selector, no verdict matrix, no filters. | next-gen `render-report-html.py` |
| G7 | Summary JSON lacks budget-profile/divergence-policy/elapsed metadata. | benchmark `diff.rs` (schema) |

## 4. Part A — benchmark repo (the tool)

### A0. Prerequisite fix: budgets + tiers for dynamic op names (G1)

A **latent bug fix** independent of the three-way feature, delivered first as its own PR:

- `diff.rs`: replace every `OpName::from_tag(op)`-based budget resolution with
  `Thresholds::resolve_by_name(op, c)` (already implemented + tested), in the regression render,
  the summary counts, and the tier rollup.
- Tier lookup by **name**: `shapes.rs` owns the repo-read shape registry (name + `Tier`,
  enumerable without a recording — verified feasible at `shapes.rs:93-107,254-261`); add a
  string-keyed tier lookup consulting the legacy catalog first, then the shape registry, else
  `None`.
- `thresholds.rs::from_toml_str`: accept `[op.<name>]` keys naming either a legacy catalog tag
  **or** a known repo-read shape; keep rejecting unknown keys (typo guard).
- Tests: end-to-end regression-render test where a dynamic op (`single_vertex_read`) gets a real
  budget from TOML and a tier in the summary; TOML parse accepts shape names, rejects typos.
- **No schema/format change** — #745 stays pinned to its current `SYNTHETIC_BENCHMARK_REF` and
  simply picks this up at the next ref bump (verified: no compat trap).

### A1. One analysis model, three consumers (G2)

Build the comparison **once** into a serializable `RegressionAnalysis` and render everything
from it. Sketch (field names final at implementation; serde snake_case):

```text
RegressionAnalysis {
  schema_version: 1,
  comparison: { baseline_label, candidate_label, slug },
  // Header/config metadata Markdown needs, lifted from the two Reports so the renderer never
  // reaches back into them: per-side module version, server image, workload_hash, samples,
  // warmup, concurrency sweep — plus the resolved threshold settings table.
  meta: { baseline/candidate: { module_version, server_image, workload_hash, samples, warmup,
          concurrency: [usize] }, thresholds: ThresholdsEcho },
  budget_profile: "strict" | "cross-engine",
  divergence_policy: "gate" | "advisory",
  gated_metric: "total_ms.p50",
  status: Comparable | NotComparable { reason },     // workload-hash/config guard only —
                                                      // version mismatches are advisory warnings
                                                      // (as today), never comparability guards
  warnings: [String],                                 // advisory lines (placeholder-aware, §A6)
  elapsed_secs: Option<f64>,                          // as passed via --elapsed-secs (f64 kept)
  verdict: OverallVerdict,                            // Rust-computed (see below)
  ops: BTreeMap<String, OpAnalysis>,
}
OpAnalysis {
  tier: Option<"core"|"full">,
  correctness: Match | Diverged | NotGated,           // NotGated = neither side recorded digests
                                                      // (still timed); asymmetric = Diverged
                                                      // (matches today's semantics)
  op_outcome: Pass | Regressed | DivergedAdvisory | NotApplicable,   // Rust-computed rollup
  cells: [CellAnalysis],
}
CellAnalysis {
  concurrency, cache_mode: "cached"|"uncached",
  baseline_p50_ms / candidate_p50_ms: Option<f64>,
  delta_pct / delta_ms: Option<f64>,
  budget: Option<{ metric: "p50", budget_pct, floor_ms }>,   // serialized ResolvedBudget
  perf_verdict: Ok | Regressed | NotApplicable,
  context: { baseline/candidate p90_ms, p95_ms, p99_ms, throughput_ops_per_sec },  // informational
}
```

Notes locked by review rounds 2–3:

- **`OverallVerdict` (summary schema v2)** is a four-state enum with a fixed aggregation rule,
  replacing v1's three-state `SummaryVerdict`:
  - `NotComparable` — `status` is NotComparable (workload/config mismatch); nothing else counts.
  - `Regressed` (🔴) — ≥ 1 regressed perf cell, **or** (under `gate` policy) ≥ 1 diverged op.
  - `Advisory` (⚠) — not regressed, but something needs a human look: ≥ 1 diverged op under
    `advisory` policy, **or** zero comparable perf cells anywhere (all-N/A / all-diverged runs
    are never green). The rendered annotation says which (e.g. `⚠ pass, 3 diverged` vs
    `⚠ no comparable cells`).
  - `Pass` (🟢) — ≥ 1 comparable cell, no regression, no divergence.
- `correctness` truth table matches **today's digest semantics** exactly: both digests present +
  equal = `Match`; present + different **or asymmetric** = `Diverged`; both absent = `NotGated`
  (comparable, timed, no correctness claim). For bucket counts this is "as today" for catalog
  ops; the four non-gated **repo-read** shapes currently land in N/A via the G1 bug, so **after
  A0** they enter the normal pass/fail buckets like any other op. `NotGated` never counts as
  diverged.
- p95 **is included** in `context` (the report schema already carries it; v2 of this design had
  dropped it by accident).
- `op_outcome` and the overall `verdict` are **computed in Rust** and serialized — the page
  never derives them. If a future page wants per-cache-mode op rollups, that is a schema rev;
  v1 defines `op_outcome` across all modes (worst-cell-wins: Regressed > DivergedAdvisory >
  Pass > NotApplicable).
- `regression_markdown` and `summarize` become pure renderers of this model; `op_cell_counts`
  (today's duplicate enumeration) is deleted. Because both consume the same in-memory value,
  drift is impossible by construction (a golden test still pins Markdown output).

### A2. `--cells <path>`: machine-readable analysis export

`report --diff A B --regression … --cells cells.json` serializes the `RegressionAnalysis` to
JSON. `--cells`, `--budget-profile` and `--divergence-policy` are only valid with
`--diff --regression` (clap `requires`, like `--summary`; plain `--diff --cells` is rejected).
`readme.md` gains a doc-tested example. Cells files are the **source material** for the page's
`data.json` (§B2) — the page itself fetches only `data.json`.

### A3. Divergence policy (G3)

New flag `--divergence-policy <gate|advisory>` (default `gate`, requires `--regression`):

- **`gate`** (same-engine, today's behavior): diverged op ⇒ op 🔴, all its perf cells N/A,
  overall verdict red on any divergence.
- **`advisory`** (cross-engine): diverged op ⇒ op ⚠ (`DivergedAdvisory`), perf cells **still
  N/A** — diverged results mean the engines did different work, so a latency comparison would
  be meaningless; raw measurements stay visible in `context` for diagnosis. Diverged ops count
  in a new `diverged` bucket of `OutcomeCounts`, never in `regressed`; perf cells decide
  🔴-vs-not, and any divergence caps the overall verdict at `Advisory` (⚠, per the A1
  aggregation — e.g. `⚠ pass, 3 diverged`; never 🟢).

`OpOutcome` gains the `DivergedAdvisory` variant; summary schema bumps to v2 (§A5).

### A4. `cross-engine` budget profile (G4)

The thresholds TOML gains an optional profile section, **mirroring the existing singular-`[op]`
syntax** (`deny_unknown_fields` stays):

```toml
[default]                              # existing strict profile (unchanged, incl. its defaults)
budget_pct = 10.0
floor_ms   = 0.5

[cross-engine.default]                 # new: looser cross-engine profile
budget_pct = 25.0
floor_ms   = 1.0

[cross-engine.op.single_vertex_read]   # optional per-op overrides, same precedence rules
budget_pct = 40.0
```

`report` gains `--budget-profile <strict|cross-engine>` (default `strict`, requires
`--regression`). The profile name is recorded in analysis/summary/cells output (G7). Selecting
`cross-engine` when the TOML has no `[cross-engine]` section is a **hard error** (no silent
fallback). Existing built-in defaults (10% / 0.5 ms) are unchanged.

### A5. Summary schema v2 (G7)

`SyntheticSummary` bumps `schema_version` to 2, adding `budget_profile`, `divergence_policy`,
`gated_metric`, `elapsed_secs: Option<f64>`, and a `diverged` count in `OutcomeCounts` (+ the
`DivergedAdvisory` op outcome). Rollout is safe (verified): #745's renderer warns-and-skips on
unknown versions rather than failing, and the renderer update ships in the same stacked PR as
the pin bump, so old-pin→v1 and new-pin→v2 are the only combinations that can occur.

### A6. Small fixes folded in

- **Elapsed-time ownership**: `run-meta.json` (written by the CI script, §B2) is the
  authoritative wall-clock record for the run; the comment/page header renders from it. The
  tool's `--elapsed-secs` stays as-is (`f64`, fractional contract preserved) for standalone
  single-diff use; CI passes it only where a header line is wanted and never sums per-comparison
  values.
- **Placeholder-version warning suppression**: edge/RC images report placeholder version
  `999999`; suppress the "same version on both sides" warning when the version equals the known
  placeholder (warnings live in the analysis model, so Markdown and page render identically).

### A7. Release + docs

`readme.md` (synthetic section) documents `--cells`, `--budget-profile`, `--divergence-policy`, the
`[cross-engine]` TOML profile and the dynamic-op-name budget behavior. `just synthetic-sanity`
is extended to round-trip `--cells` + `--budget-profile cross-engine` +
`--divergence-policy advisory`; its Justfile doc-comment and the recipe tables in
`.github/copilot-instructions.md` are updated accordingly. Tag a new release (post-v2.2) once
merged so next-gen can pin `SYNTHETIC_BENCHMARK_REF` to a released SHA.

## 5. Part B — falkordb-rs-next-gen (CI + interactive page)

Stacked PR on `barakb/synthetic-pr-regression`. All scripts stay under
`.github/scripts/benchmark/`.

### B1. C-engine leg in `synthetic-run.sh` (G5)

- New env: `IMAGE_CENGINE` (default `docker.io/falkordb/falkordb:edge`). The C measurement's
  `docker run` line hardcodes `--env BROWSER=0` (no arg-string pass-through; §2). The unused
  `IMAGE_RELEASE` leg is removed.
- **Digest resolution fix**: normalize Docker Hub refs before matching `RepoDigests` (strip
  `docker.io/`, add `library/` only for official single-name images); keep GHCR and pinned
  `@sha256:` refs working. Covered by a self-test mode exercised in `synthetic-sanity`-style CI
  (Docker Hub, GHCR, port-qualified registry, pre-pinned digest).
- **Failure isolation without masking** (a C hiccup must never cost the `main-pr` signal, and a
  C failure must still be *visible*):
  1. record once; measure `pr`, then `main` (all under `set -e`, as today);
  2. **immediately** produce and persist every `main-pr` artifact (report + summary + cells)
     into the trap-surviving output dir;
  3. run the C leg as a **separate child script** (`synthetic-c-leg.sh`, `bash -euo pipefail`)
     invoked under `timeout <bound> bash …` from an explicit `if`: a child script keeps `errexit`
     live inside the guarded code (Bash disables `set -e` in any function/compound command tested
     by `if`, so in-process guarding silently masks mid-stage failures), and `timeout` — which
     cannot run a shell function — bounds the leg well below the job timeout so a hang cannot
     prevent the artifact upload;
  4. on C failure: write **two** stub summaries (`summary-c-pr.json`, `summary-c-main.json`,
     `verdict: not_comparable`, `not_comparable_reason` = the failure stage) and mark both
     comparisons `unavailable` in `data.json` (§B2) — the comment and page show *why* C is
     missing; the job itself stays green (the `main-pr` signal is intact);
  5. on success: produce `c-pr` and `c-main` reports/summaries/cells with
     `--budget-profile cross-engine --divergence-policy advisory`.
- `main-pr` keeps the strict profile + `gate` policy. `ELAPSED_SECS` measured once after all
  measuring; recorded in `run-meta.json` (not passed to the three Markdown reports).

### B2. Artifact flow (one owner, one directory)

The **measure job** owns every data artifact in one persistent output dir (the existing
trap-surviving `SUMMARY_DIR` pattern, renamed `SYNTHETIC_OUT`):
`report-{main-pr,c-pr,c-main}.md`, `summary-{main-pr,c-pr,c-main}.json`,
`cells-{main-pr,c-pr,c-main}.json`, `run-meta.json` (elapsed, arch, image refs + digests,
profile/policy per comparison, PR number, head SHA), and **`data.json`** — the page's single
input, assembled by the measure job:

```text
{ "schema_version": 1,
  "meta": { …run-meta fields… },
  "comparisons": {
    "main-pr": { "status": "ok", "analysis": { …cells-main-pr.json… } },
    "c-pr":    { "status": "unavailable", "reason": "image pull failed" },
    "c-main":  { "status": "unavailable", "reason": "image pull failed" } } }
```

The **publish job** only copies `data.json` + the page HTML + the reports into the gh-pages
leaf — it never assembles or recomputes anything.

### B3. Interactive page (G6)

`render-report-html.py` is replaced by a **static template** `synthetic-report.html` committed
as-is (no build step): a single dependency-free HTML file (inline CSS/JS, no external libs)
that fetches its **sibling `data.json`**. It copies the trend page's *visual* pattern
(segmented buttons, cards, light/dark) — not its `innerHTML` string-building: all dynamic
content renders via `createElement`/`textContent` only.

Page model (all client-side, driven only by `data.json`):

- **Comparison selector** (segmented): `PR vs main` (`main-pr`) · `PR vs C` (`c-pr`) ·
  `main vs C` (`c-main`) · `matrix`. Unavailable comparisons render greyed out with their
  `reason` — the page renders **any subset** of comparisons defensively.
- **Metric selector** (segmented, like trend): `p50` · `p90` · `p95` · `p99` · `throughput`.
  p50 is the only gated metric — its column carries the verdict badge; every other metric
  renders values/deltas **neutrally labeled "informational — not gated"** (no red/green), and
  throughput deltas render with the direction convention stated inline (`higher is better`).
- **Cache-mode selector** shown when both modes exist in the data (`uncached` default); hidden
  otherwise. Cell tables follow the selected mode; the **matrix op verdict does not** — it is
  the Rust-emitted all-modes `op_outcome` (v1 schema has no per-mode rollup; the matrix header
  says "all cache modes").
- **Matrix view**: rows = ops, columns = the three comparisons, cell = the Rust-emitted
  `op_outcome` emoji (🟢/🔴/⚠/N/A). **Filter chips** (predicates over comparison IDs, absent
  comparisons excluded from quantifiers):
  - `all` — no filter;
  - `any red` — 🔴 in ≥ 1 available comparison (OR);
  - `all green` — 🟢 in **every** available comparison (AND; an op with any N/A/⚠ cell in some
    comparison does not qualify);
  - `red vs C` — 🔴 in `c-pr` or `c-main` (regardless of `main-pr`);
  - `red vs main` — 🔴 in `main-pr` (regardless of the C comparisons);
  - free-text op-name filter composes (AND) with the selected chip.
- **Card/table view** per comparison: per-op collapsible tables (C × selected cache-mode ×
  metric), verdict column straight from `perf_verdict`, ⚠/🔴 correctness banner per the
  comparison's `divergence_policy`.
- **Header**: PR number/SHA, arch, images (ref + digest), per-comparison profile/policy, and
  **total benchmark wall-clock** from `meta`.
- **Tests** (new infrastructure — next-gen has **no** browser-test setup today, only Python
  `tests/requirements.txt`): a static Python test asserts the committed template contains no
  `innerHTML`/`insertAdjacentHTML` and that a sample `data.json` round-trips the schema; a
  **Playwright DOM test** — added in this PR as new dev tooling (`pytest-playwright` +
  chromium, own requirements file + CI step, browser cached) — serves the template with a
  `data.json` containing `<script>`-shaped op/engine labels and asserts they render inert as
  text and the selectors/filters behave (the only claim a browser can actually prove).

### B4. Sticky comment

`render-synthetic-comment.py` renders **three** verdict lines (one per comparison ID, from the
v2 summaries; ⚠ diverged counts shown for cross-engine lines; `not_comparable_reason` shown for
unavailable ones), the total wall-clock from `run-meta.json`, worst offenders for `main-pr`
only (the gating signal), and one link to the interactive page. Markers
(`<!-- synthetic-benchmark -->` / `-arm`) unchanged. The renderer hard-checks
`schema_version == 2` per summary and warns-and-skips otherwise (existing behavior).

### B5. Workflow wiring (`_benchmark.yml` / `benchmark.yml`)

- Pass `IMAGE_CENGINE`; bump `SYNTHETIC_BENCHMARK_REF` to the new benchmark release SHA;
  upload the single `SYNTHETIC_OUT` artifact; `timeout-minutes: 90` (three measured legs vs
  two, plus C image pull; 60→90 is deliberate headroom, and the C leg's own `timeout` bound
  keeps a hang from eating the margin).
- **Closed-PR race**: the workflow-level arch-split concurrency stays unchanged (both arches
  must run for one PR); serialization is added at the **job level** — both arch `synthetic-publish`
  jobs and the cleanup job share one per-PR concurrency group (`synthetic-pages-pr-<N>`,
  `cancel-in-progress: false`, **`queue: max`**). `queue: max` keeps every queued job pending
  (FIFO by wait-start, up to 100) instead of the default single-pending-slot behaviour where a
  newer queued job cancels and replaces an older pending one — so x86/arm publishers (different
  leaves; "newest wins" would not hold across arches) and cleanup are all guaranteed to run, in
  order. Each publisher additionally re-checks PR state (`gh pr view --json state`) inside the
  group and skips fail-closed when closed, covering a publisher that queued before close but
  runs after cleanup.
- **Fork PRs**: same-repository PRs only (the existing prepare job already excludes fork
  heads — images aren't pushed for forks). Supporting forks would need a separate trusted
  workflow design; explicitly out of scope.

## 6. What the rubber-duck reviews corrected (v1 → v2 → v3 → v4 → v5)

Round 1 (17 findings) — folded into v2: dynamic-op budgets/tiers silently N/A (→ A0); no
single verdict source (→ A1); divergence conflated with perf (→ A3); `docker.io/` digest
mismatch (→ B1); bundle image's browser server contaminating measurements (→ `BROWSER=0`);
C failure losing the `main-pr` signal (→ B1 ordering); cells artifacts dying with the workdir
trap (→ B2); summary metadata gaps (→ A5); triple-emitted elapsed line (→ A6); undefined
cache-mode card view, filter algebra, throughput direction (→ B3); canonical two-way page,
closed-PR race, fork PRs (→ §2/B5); timeout arithmetic (→ 90); placeholder-version noise
(→ A6); false CSP claim (→ B3).

Round 2 (17 findings) — folded into v3: TOML syntax was wrong (`[ops.*]` vs the real singular
`[op.*]` + `deny_unknown_fields`; invented 5%/0.3ms defaults vs the real 10%/0.5ms — → A4);
`RegressionAnalysis` lacked schema_version/status/warnings and used a nonexistent
`ResolvedBudget` shape (→ A1); p95 silently dropped (→ restored); matrix verdict was still
JS-computed (→ Rust `op_outcome`/`verdict`, A1/B3); divergence policy split from the budget
profile into `--divergence-policy` with perf N/A under both policies (→ A3, §2); `Unknown`
correctness conflated cases — replaced by a truth table matching today's asymmetric=diverged /
both-missing=comparable semantics (→ A1); blanket `set +e` could mask C failures (→ explicit
`if` + status-preserving `measure` + `timeout`, B1); a single C-unavailable stub couldn't feed
two comparisons and the page had no per-comparison status (→ two stubs + `data.json` with
per-comparison `{status, reason}`, B1/B2); elapsed ownership contradiction (→ `run-meta.json`
authoritative, `f64` kept, A6); canonical mode was presentation-only wishful thinking (→
explicitly out of scope, §2); PR-state re-check alone doesn't close the race (→ shared
concurrency group + fail-closed check, B5); `CENGINE_DOCKER_ARGS` shell-fragility (→ hardcoded
`--env BROWSER=0`, B1); filter/cache semantics under-specified (→ explicit predicates over
comparison IDs + all-modes matrix rollup, B3); recipe-doc contradiction (→ A7 updates the
Justfile doc-comment + instruction tables); "old renderer breaks" wording (→ warns-and-skips,
A5); unverified "negligible delta" claim removed (→ §2); XSS test overclaim (→ builder test +
Playwright DOM test, B3).

Round 3 (6 findings) — folded into v4: the A1 model couldn't render Markdown alone (missing
module versions/images/hashes/samples/warmup/threshold echo — → A1 `meta`; "version checks"
corrected to advisory warnings); the overall verdict enum/aggregation was undefined (→ A1
`OverallVerdict` four-state rule incl. all-diverged and zero-comparable-cells); `if c_leg`
would disable `errexit` inside the guarded code (→ child script under `timeout`, B1); "as
today" was inaccurate for the four non-gated repo-read shapes (→ A1 truth-table note); the
shared concurrency group needed job-level scoping to avoid blocking both arches (→ B5); page
test/data wording contradictions and the nonexistent
Playwright infra (→ A2 source-material wording, B3 new-tooling setup).

Round 4 (2 findings) — folded into v5: `PassWithDivergences` misnamed its zero-comparable
case and contradicted A3's 🟢-annotated rendering (→ renamed `Advisory`, A3 aligned: any
divergence caps the verdict at ⚠); the default single-pending-slot queue could silently
replace a queued publisher/cleanup, unsafe because the two arches publish different leaves
(→ B5 uses `queue: max` — real GA syntax since May 2026 — guaranteeing every queued job in
the per-PR group runs).

## 7. Deliverables & order

1. **benchmark PR 1 (A0)** — dynamic-op budgets/tiers bug fix. Small, independently valuable,
   no schema change (safe under #745's pin). **Status: implemented — PR
   [#255](https://github.com/FalkorDB/benchmark/pull/255).**
2. **benchmark PR 2 (A1–A7)** — analysis model, `--cells`, profiles, divergence policy,
   summary v2, sanity-recipe + docs; then tag post-v2.2.
3. **next-gen PR (B1–B5)** — stacked on #745, pinned to the new tag.

Each PR: design-first (this doc), ≥90% patch coverage on Rust changes, `just ci` +
`just coverage` green locally, docs synced. Next-gen PRs await human review (no self-merge).

## 8. Out of scope

- Gating (red stays non-blocking); canonical/nightly/manual synthetic runs (§2); historical
  trend storage for synthetic results; comparing more than the three fixed images; fork-PR
  support; per-cache-mode op rollups in the cells schema (future rev); changing the A/B
  (non-synthetic) benchmark.
