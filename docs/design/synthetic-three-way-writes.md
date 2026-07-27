# Design: write ops join the per-PR three-way synthetic report

**Status:** reviewed — duck findings folded in (engine-coverage validation, container hygiene,
flag semantics, single digest resolution, timeout arithmetic, kind-aware page semantics).
**Scope:** almost entirely `FalkorDB/falkordb-rs-next-gen` (CI wiring + report schema/page/comment
+ tests). The `FalkorDB/benchmark` tool needs **zero code changes** — everything required shipped
in Phase 7 (PRs [#265](https://github.com/FalkorDB/benchmark/pull/265)–[#269](https://github.com/FalkorDB/benchmark/pull/269));
this repo contributes this design doc and a **v2.4 release tag** so the CI can pin a tool that has
`--repo-writes`.
**Parents:** [`synthetic-pr-regression-report.md`](./synthetic-pr-regression-report.md) (which
scoped writes out: *"Write-op comparison (reads only; writes aren't recordable/deterministically
comparable)"* — Phase 7 has since made them recordable),
[`synthetic-three-way-report.md`](./synthetic-three-way-report.md) (the report this extends), and
[`synthetic-cover-writes-phase7.md`](./synthetic-cover-writes-phase7.md) (the write record/replay
machinery this consumes).

## 1. Goal

The per-PR three-way synthetic report (`main-pr` / `c-pr` / `c-main`, published to
`synthetic-benchmark/branch/pr-<N>/`) today measures only the 50 recorded **read** shapes. Extend
it so the A/B benchmark's **10 write shapes** (`--repo-writes`: CREATE / SET / MERGE / DELETE /
REMOVE / FOREACH point mutations) are recorded, measured on the same three engine images, and
rendered in the same page, comment and job summary — without weakening any existing signal.

## 2. Decisions

| Decision | Choice |
|---|---|
| Tier | **Latency tier only** — no outcome oracle in CI (see §3 *Why no oracle*). Write ops render `correctness: not_gated`, exactly like the fulltext/vector reads do today. |
| Bundle | A **second recorded bundle** (`--repo-writes`; write bundles are single-kind by design §4 of Phase 7 — they cannot share the reads bundle). Recorded **offline** from the same pinned `.github/synthetic-workload.toml` (same seed/graph/nodes/edges ⇒ one dataset definition; its own `workload_hash`). |
| Sweep / cache | The recorded per-op write budget **pins C=1, 100 samples, warm-up 10** (§6.5 policy, enforced at record and replay) and overrides the CI's global `--concurrency`/`--samples` flags; cache mode follows the run's global knob (`uncached` per-PR, like reads). Measured: **10 ops × 1 cache mode × C=1 = 10 cells** per comparison. |
| Comparisons | The same three, same profiles/policies: `main-pr` strict + gate, `c-pr`/`c-main` cross-engine + advisory. |
| Budgets | Write ops fall under the existing `[default]` / `[cross-engine.default]` budgets, with **one pre-seeded strict override**: `single_edge_update` (the one ~20 ms write, server-`rand()` targeted) gets `budget_pct 25` — the `floor_ms 0.5` floor that guards every sub-ms write (A/A jitter up to ±33 % observed) is meaningless at 20 ms where 10 % = 2 ms, and there is no A/A variance data on the CI VM class yet. Tighten after real runs, exactly as the reads' `expand_hops_5`/`shortest_path` were calibrated. |
| Failure isolation | Writes must never cost a read signal, and a C-write hiccup must never cost `main-pr` writes: **two new timeout-bounded child scripts** mirroring the existing read C-leg pattern (§5.2). |
| Schema | `data.json` **schema_version 2**: each comparison becomes `{"reads": {…}, "writes": {…}}` slots (§5.4). The page and data ship together, so no cross-version compat shim. |
| Feature flag | `REPO_WRITES` env, **exact truthiness pinned**: the write pass is disabled iff the value is `0`, `false`, `no`, `off` or the empty string; unset defaults to enabled. The script logs `writes pass: enabled/disabled` at start so a run's state is never ambiguous. No workflow input initially (env-only knob; nothing in the workflow can render it empty by accident). When disabled the write slots are simply absent and the page hides the writes UI. |
| Tool ref | Bump `SYNTHETIC_BENCHMARK_REF` to the **v2.4** release SHA (first tag containing `--repo-writes`; v2.3 = `e2706fa` predates it). |
| Cost | Measured on the CI workload (10 k nodes / 50 k edges): record **0.6 s** (offline), replay **≈ 48 s per engine leg** (10 cells, one base-graph drop+reload per cell) → **≈ +3 min nominal** wall-clock for all three legs incl. container churn. Worst-case arithmetic is a different matter: the bounded children alone can sum to 2700 + 900 + 900 s = 75 min, and the unbounded prelude (toolchain + release build on a fresh VM + reads record + two reads legs) realistically needs 15–25 min — so the job `timeout-minutes` is raised **90 → 110**, and `data.json`/`run-meta.json` are **assembled incrementally after every phase** (cheap pure-python step) with the artifact upload on `if: always()`, so even a job-timeout run publishes whatever completed. |

## 3. Why no oracle in CI (and when to revisit)

Phase 7's §6.3/§6.4 outcome oracle is deliberately **not** wired into this check:

1. **Online record.** `record --oracle` needs a live engine at record time (two full passes over
   every eligible op's complete corpus, per-invocation restore — ~13½ min at 1 k/5 k on the dev
   image, substantially more at the CI's 10 k/50 k). Today's CI record step is offline and cheap.
2. **Per-leg verification cost.** An oracle-attested bundle makes every `run --recording` re-verify
   every recorded outcome (another full per-invocation-restore pass) **per engine leg** before
   measuring.
3. **Divergence is a hard replay error.** The tool treats any counter mismatch as a fatal error
   naming the op/seq/command — correct for the benchmark repo's own determinism gate, but on a
   cross-engine informational report it would turn any legitimate C-vs-Rust counter difference into
   a dead leg (stubs instead of a latency table). There is no advisory oracle mode in the tool.

Revisit when (a) an advisory oracle replay mode exists in the tool (divergence → per-op ⚠ instead
of abort) and (b) a smaller dedicated oracle workload makes the record+verify cost acceptable. That
is a `FalkorDB/benchmark` feature with its own design; explicitly out of scope here.

## 4. What the tool already provides (verified end-to-end, zero changes needed)

Validated locally with the CI's own workload + thresholds files and the CI's exact flags,
**against both engines the pipeline measures** — the C engine (`falkordb/falkordb:edge`) and the
Rust engine (`ghcr.io/falkordb/falkordb-server:edge-rs`): both survive the full write corpus
(FOREACH / MERGE ON CREATE+ON MATCH / DETACH DELETE / REMOVE label, per-cell drop+reload, final
content-verified restore), and a genuine cross-engine C→Rust `report --regression` produced a
clean advisory analysis (one real budget red, `detach_delete_user` — signal, not breakage). The
nightly `--cache both` knob also works on write bundles (2 cache modes × C=1 = 20 cells/leg).
This is what makes **default-on** safe:

- `record --config synthetic-workload.toml --repo-writes --out-dir rec-writes` — offline, 0.6 s,
  10 ops, deterministic `workload_hash`.
- `run --recording rec-writes --concurrency 1,8 --cache uncached --samples 200 --warmup 50 …` —
  the recorded write budget (C=[1], samples 100, warm-up 10) **overrides** the global flags, so the
  CI can pass the same env-driven flags to both bundles; per-cell base reset + final
  content-verified restore. 48 s/leg.
- `report --diff … --regression --thresholds synthetic-thresholds.toml` with **both**
  `--budget-profile strict --divergence-policy gate` and
  `--budget-profile cross-engine --divergence-policy advisory` — write ops flow into the same
  summary v3 + cells v2 artifacts (`tier: full`, `correctness: not_gated`, `op_outcome`,
  10 comparable cells), and budgets resolve for the dynamic op names (#255) — no new thresholds
  entry is *required* for the tool to run, but §5.7 deliberately pre-seeds one strict override
  (`[op.single_edge_update]`) before enabling the gate.

## 5. Part B — `falkordb-rs-next-gen` changes (all on the `barakb/synthetic-pr-regression` branch, PR #745)

### 5.1 Pass ordering (protect the strongest signals first)

```text
record reads (offline)
measure pr reads → measure main reads          (fatal on failure, as today)
report main-pr reads → persist                 → assemble (incremental)
[writes child]   record writes (offline) → measure pr writes → measure main writes
                 → report main-pr writes → persist            → assemble (incremental)
[C reads child]  resolve C digest → measure C reads → report c-pr / c-main reads
                                                              → assemble (incremental)
[C writes child] measure C writes (reuses the resolved digest) → report c-pr / c-main writes
                                                              → assemble (final)
```

`main-pr` writes (same-engine, gate policy) outrank the cross-engine advisory comparisons, so the
writes child runs **before** the C legs. Each child gets its own container lifecycle (one container
at a time is preserved; a container boot is ~seconds against a 48 s leg).

Two hardening rules learned from review:

- **Incremental assembly.** `run-meta.json` + `data.json` are (re)assembled after **every** phase
  (a cheap pure-python step), with phases not yet reached marked unavailable
  ("not reached — the run ended before this phase"). Combined with `if: always()` on the artifact
  upload, a job timeout mid-leg still publishes every signal measured before it.
- **Container hygiene.** `timeout --kill-after` SIGKILLs a wedged child — its EXIT trap never
  runs and its container would survive holding the shared `DB_PORT`, cascading "port already
  allocated" failures into every later leg (a write failure costing the read signal — the exact
  thing the isolation must prevent). Therefore the **parent** assigns every container name
  (`synthetic-db-$$`, `synthetic-writes-$$`, `synthetic-cengine-$$`, `synthetic-cwrites-$$`,
  passed via env) and force-removes all of them before each leg and in its own EXIT trap.

### 5.2 Two new child scripts (the proven isolation pattern)

Same rationale as `synthetic-c-leg.sh` (a child script keeps `set -e` live inside an `if`-guard;
`timeout` bounds a hang below the job timeout; the parent owns every stub):

- **`synthetic-writes-leg.sh`** (`WRITES_LEG_TIMEOUT`, default 900 s): record the write bundle
  (offline), measure `pr` then `main` (own containers), report `main-pr` writes
  (strict + gate) into `report-main-pr-writes.md` / `summary-main-pr-writes.json` /
  `cells-main-pr-writes.json`. On failure the parent stubs **all three** write slots (the C
  writes leg is then "not attempted — the writes leg failed").
- **`synthetic-c-writes-leg.sh`** (`C_WRITES_LEG_TIMEOUT`, default 900 s): measure C writes,
  report `c-pr` / `c-main` writes (cross-engine + advisory). Runs only when the writes leg
  succeeded; on failure the parent stubs the two C write slots. Independent of the C **reads**
  leg outcome (a C-reads hiccup doesn't forfeit C-writes, and vice versa). **Digest reuse:** it
  reuses `$WORKDIR/c-digest` when the C reads leg already resolved it, resolving fresh only when
  that file is absent — `falkordb:edge` is a moving tag, and two independent resolutions could
  measure C reads and C writes on *different builds* while run-meta attributes both to one digest.

Shared plumbing (`bench`, `wait_for_redis`, the measure-one-image function) is extracted into a
sourced **`synthetic-measure-lib.sh`** (like `synthetic-digest-lib.sh`) instead of a fourth copy.
`write_stub_summary` is generalized: profile, policy, slug and a leg description for the headline
become parameters (today's helper hardcodes cross-engine/advisory and a C-engine headline, which
would mis-label a `main-pr`-writes stub). The parent's existing "no `timeout` binary" fail-closed
guard covers **all three** children: without a bound every guarded leg is skipped with an honest
stub, never run unbounded.

### 5.3 Parent script (`synthetic-run.sh`) changes

- `REPO_WRITES` gate with pinned truthiness (§2): disabled iff `0|false|no|off|""`
  (case-insensitive); unset → enabled; state logged at start.
- Stub bookkeeping: `W_STATUS`/`W_REASON` (writes leg) and `CW_STATUS`/`CW_REASON` (C writes leg),
  with stage files (`$WORKDIR/writes-stage`, `$WORKDIR/c-writes-stage`) for attribution, mirroring
  the C leg's `c-stage`. Statuses feed the incremental assembly (§5.1): `pending` phases are
  written as unavailable-with-reason so a mid-run death still leaves an honest `data.json`.
- `run-meta.json` is unchanged in shape (`schema_version` stays 1): profiles/policies per
  comparison apply to both kinds identically, and the page derives kind availability from the
  comparison slots, not from meta.
- Assembly (§5.4): up to six `--ok`/`--unavailable` slot arguments instead of three.

### 5.4 `data.json` schema v2 (`assemble-synthetic-data.py`)

```text
{ "schema_version": 2,
  "meta": { …run-meta.json, unchanged… },
  "comparisons": {
    "main-pr": {
      "reads":  {"status": "ok", "analysis": {…cells JSON…}} | {"status": "unavailable", "reason": "…"},
      "writes": {"status": "ok", "analysis": {…}} | {"status": "unavailable", "reason": "…"}
    },
    "c-pr":  { …same shape… },
    "c-main": { …same shape… } } }
```

- Slot grammar: `--ok 'main-pr/writes=cells-main-pr-writes.json'`,
  `--unavailable 'c-pr/writes=<reason>'`; `<id>/reads` for the read slots. Unknown ids/kinds
  rejected; duplicates rejected; an `--ok` file must still contain `"ops"`. The assembler also
  **rejects an op name appearing in both kinds of one comparison** — the matrix keys rows by bare
  op name and resolves each to its kind, so a silent reads/writes collision would merge two
  different ops into one row.
- A comparison with **no** slots is omitted (page renders any subset defensively, as today); with
  `REPO_WRITES` off the `writes` keys are simply absent.

### 5.5 Page (`synthetic-report.html`)

- Accept `schema_version === 2` + the slot shape (the page ships next to its data — no dual-schema
  support). Availability is now per **(comparison, kind)**; each op resolves to its kind via the
  union of slot op-maps (disjointness is assembler-enforced).
- **Comparison view**: one card per kind — the reads card (badge, totals, banners, op details)
  then the writes card, each fed by its own analysis. An unavailable kind renders a reason banner
  in place of its card body; a comparison is greyed in the selector only when **both** kinds are
  unavailable (title carries the per-kind reasons).
- **Matrix**: rows = union of reads+writes op names; each cell looks the op up in that
  comparison's slot for the op's own kind. A cell whose kind-slot is unavailable renders `—` with
  the slot reason in its tooltip (the column header shows per-kind availability instead of
  all-or-nothing). Write rows render their single C=1 cell — the concurrency columns are already
  derived per-op from the cells array, so nothing breaks.
- **Filter chips**: new `reads` / `writes` kind chips compose (AND) with the verdict chips and the
  free-text filter. The verdict quantifiers are kind-aware: for each op, `any-red` / `all-green`
  quantify over the comparisons where **that op's kind** is available (mirroring today's
  "available comparisons" semantics), and `red-vs-c` / `red-vs-main` look up the op in its own
  kind's slots.
- **Header**: warning strips prefix writes-analysis warnings with "(writes)"; reads warnings keep
  today's prefix (no churn).

### 5.6 Sticky comment (`render-synthetic-comment.py`)

- Inputs: `--summary <id>=<reads.json>` as today plus `--summary-writes <id>=<writes.json>`.
- Each writes summary renders as its **own verdict line** right under its comparison's reads line
  (`🟢 **PR vs main (writes)** — …`), reusing the existing verdict/stub/degradation vocabulary
  verbatim. A missing writes summary omits the line entirely (flag-off and legacy runs degrade to
  exactly today's comment — no "no summary produced" noise for a deliberately absent kind).
- Worst offenders: a separate `**Worst offenders (PR vs main, writes):**` line from the writes
  summary when non-empty — no cross-list merge (each tool-emitted offender list is already
  sorted and truncated internally; merging two pre-truncated lists has no defensible order).

### 5.7 Publish + workflow

- `synthetic-publish.sh` copies `summary-*.json` / `report-*.md` by glob already — the `-writes`
  artifacts flow through unchanged; `data.json` is one file either way.
- `_benchmark.yml`: bump `SYNTHETIC_BENCHMARK_REF` to the v2.4 SHA; **`timeout-minutes: 90 → 110`**
  (the bounded children alone can sum to 75 min and the unbounded prelude — toolchain + release
  build + reads legs — needs 15–25 min on a fresh VM; 90 was already tight); `if: always()` on the
  synthetic job's artifact-upload step so incremental artifacts survive a job timeout; the
  comment step's summary loop gains the `--summary-writes` file checks (without this the renderer
  change is dead code). `REPO_WRITES` stays env-only — no workflow input (an unset input would
  render as the empty string, which the pinned truthiness treats as *disabled*: a silent
  feature-off footgun).
- `.github/synthetic-workload.toml`: doc comment gains the write-bundle sentence (same
  seed/graph/nodes/edges define **two** recorded bundles — reads and writes — each with its own
  `workload_hash`), and notes that the nightly SWEEP override is deliberately neutralized for
  writes (the recorded budget pins C=1) while CACHE applies to both bundles.
- `.github/synthetic-thresholds.toml`: `[op.single_edge_update]` strict pre-seed (§2) + comment
  note that the other write ops ride the defaults until calibrated.

### 5.8 Tests (`tests/synthetic-report/`)

- **Fixtures**: regenerate `data*.json` fixtures to schema v2 (reads+writes slots); add a
  writes-unavailable fixture and a writes-XSS op name to the XSS fixture; add
  `summary-main-pr-writes.json` (+ a stub variant).
- **`test_static.py`**: schema assertions updated (v2, slot shape); assembler unit cases for the
  `id/kind` grammar, kind-level unavailability, duplicate/unknown rejection.
- **`test_page_dom.py`**: writes section renders; kind chips filter; write op appears in the
  matrix with its C=1 cell; kind-level unavailable reason shown; XSS op names stay inert.
- **`test_comment.py`**: reads+writes line rendering, writes stub line, missing-writes-summary
  degradation.
- Shellcheck (existing workflow) covers the two new scripts + lib automatically.

## 6. Part A — `FalkorDB/benchmark` deliverables

1. This design doc (docs-only PR; `just doc-check`).
2. **Release v2.4** from current `master` (first release containing `--repo-writes`, #265–#269),
   so `SYNTHETIC_BENCHMARK_REF` can pin its immutable SHA with the `# v2.4` comment convention.

## 7. Risks & mitigations

- **Write-op noise vs the 10 % default budget**: sub-ms write ops are floor-guarded
  (`floor_ms 0.5` ⇒ a red needs an absolute +0.5 ms, i.e. roughly a doubling); the one heavy write
  (`single_edge_update`, ~20 ms, server-`rand()` targeted) is pre-seeded at `budget_pct 25` (§2)
  until CI-VM A/A variance data exists. The check stays informational either way.
- **Job-time creep**: +3 min nominal, but the worst-case bound arithmetic forced
  `timeout-minutes: 110` plus incremental assembly + `if: always()` upload — a timeout can no
  longer erase already-measured signals (§2, §5.1).
- **Failure blast radius**: child-script isolation + parent-owned container names/sweeps (§5.1)
  mean the worst a write failure can do is stub the write slots — every read signal survives
  (and vice versa for the C legs), even on the SIGKILL path.
- **Engine coverage**: both measured engines were validated against the write corpus before this
  design was finalized (§4) — the "zero tool changes / default-on" premise is tested, not assumed.
- **Moving C tag**: single digest resolution shared by both C legs (§5.2) keeps c-pr/c-main reads
  and writes on the same C build and run-meta's one `c-engine` digest truthful.
- **Restore correctness**: replay's per-cell reset + final content-verified restore is tool-owned
  and already covered by the benchmark repo's own tests; the CI adds no new state handling.
- **Two bundles drift apart**: both record from the same `synthetic-workload.toml`; the workload
  comment documents that either bundle's hash changing breaks comparability with older runs.

## 8. Out of scope

- The outcome oracle in CI (§3) and any tool-side advisory oracle mode.
- `single_edge_update` determinism (permanently latency-only per Phase 7 §3.4).
- Write ops in the benchmark repo's own `synthetic-verify` divergence gate (writes have no
  result digests to verify; the latency tier asserts nothing).
- C>1 recorded writes (decided by Phase 7 §6.5 — policy, enforced by the tool).
- The canonical main-branch trend page (this is the per-PR report only).
