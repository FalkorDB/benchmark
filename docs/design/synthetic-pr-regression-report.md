# Design: per-PR synthetic benchmark regression report for `falkordb-rs-next-gen`

**Status:** proposed (awaiting approval before implementation).
**Scope:** changes in **two** repos — `FalkorDB/benchmark` (the tool) and
`FalkorDB/falkordb-rs-next-gen` (the CI that consumes it).

## 1. Goal

On each `falkordb-rs-next-gen` PR benchmark run, produce a **non-blocking, colored** report that
compares the PR's engine build against **main** — and, once semver releases exist, against the
**last release** — on an *identical, pre-recorded* synthetic workload, executed **same-machine,
one container at a time**, so that per-operation slowdowns beyond a configurable budget are obvious
at a glance (🟢 good / 🔴 regressed). It never fails the PR.

## 2. Decisions (confirmed)

| Decision | Choice |
|---|---|
| Blocking? | **No** — informational report only; never fails the PR. |
| Relationship to the existing A/B benchmark | **Add to it**, never remove it. Same machine *type*, own VM, sequenced before/after; **never >1 benchmark per machine at once**. |
| Baselines | **PR vs main** always. **PR vs last release** only when a release image exists (none today → PR-vs-main only). |
| Names in the report | `pr`, `main`, `release X.Y.Z`. |
| Verdict metric | **p50 latency** (primary); throughput shown for context. |
| Default budget | **10 %** slowdown before 🔴; overridable **per-operation** and **per-operation×concurrency**. |
| Threshold config | **TOML file in `falkordb-rs-next-gen`** (tool ships built-in defaults). |
| Result divergence | **Non-fatal**: mark diverged ops 🔴 with `⚠ results differ`, keep going. |
| Coloring | 🟢 / 🔴 emoji per cell in the Markdown table. |
| Sweep | full concurrency `1,2,4,8,16,32` × both cache modes (matches merged `synthetic-verify`). |
| Verdict rule | 🟢 if PR is **faster** *or* **slower within budget**; 🔴 if slower beyond budget **and** the absolute p50 delta exceeds a noise floor; diverged op → correctness-🔴, perf verdict **N/A**. |
| Metric definition | `p50` = the **total-latency median** (`total_ms` p50) of a cell. |
| Tool ref | a **separate** `SYNTHETIC_BENCHMARK_REF` **pinned to an immutable commit SHA** (tagged for reference) — the A/B's `BENCHMARK_REF=v2.2` is left untouched. |
| Failure handling | the synthetic job is **non-blocking** (allowed-to-fail / not a required check); any tool/infra failure posts a "benchmark unavailable" note instead of blocking. |
| Trigger scope | PR-triggered runs only (dispatch has no PR); **arch-specific** comment markers to avoid x86/arm races. |

## 3. Context from recon (why the design is shaped this way)

- **Images** (all `ghcr.io/falkordb/falkordb-server:<tag>`, `EXPOSE 6379`, `redis-server
  --loadmodule …/falkordb.so`, no auth → `falkor://host:6379`):
  - PR build: `rc-pr-<N>` — published **early** by `rust-pr.yml` (before tests).
  - main build: `edge-rs` — retagged on every merge to `main` by `rust-push.yml`.
  - release build: `<X.Y.Z>` — only on a GitHub `release: published` event via `release-image.yml`;
    **none exist** (Cargo version is the `99.99.99` sentinel, no tags/releases).
- **Existing A/B benchmark** (`benchmark.yml` → reusable `_benchmark.yml`): label-triggered
  (`benchmark-{small,medium,large}[-arm]`) / `/benchmark` comment / dispatch; provisions ephemeral
  **GCE VMs** (one per size×variant); already uses **`FalkorDB/benchmark`** but pinned at
  `BENCHMARK_REF = deca7752 (v2.2)`, which **predates the `synthetic` subcommand**, in the
  `ab-compare` *vendor* mode; publishes a gh-pages dashboard + a sticky comment
  `<!-- ab-benchmark-comment -->`. It is **informational**, not a gate.
- **No latency-threshold/gating config** exists in either repo today.
- The PR image is awaited by `benchmark.yml`'s `wait-for-rc-image` (polls the registry, cross-checks
  the "Rust PR" run, 85-min timeout).

## 4. Part A — changes in `FalkorDB/benchmark` (the tool)

The tool already has: offline `record`; `run --recording` (closed-loop sweep × cache, per-op
`result_digest`, `verify_concurrent`); `report --diff A.json B.json` that **guards**
(`workload_hash` + digests) and renders a per-op×cache×concurrency Markdown table with
`Δ = 100·(B−A)/A`. It has **no** run label, **no** thresholds, and **aborts** on divergence.

### A1. Name the runs — `run --label <name>`

Add `--label <name>` to `synthetic run` (e.g. `--label pr`, `--label main`, `--label "release
1.2.3"`), stored in the report JSON (`meta`). `report --diff` uses the two runs' labels as the
column headers (falling back to `A (baseline)` / `B (candidate)` when absent), so the table reads
`main` vs `pr` instead of `A`/`B`. `--server-image` stays (image identity row).

### A2. Threshold config (built-in defaults + overridable TOML)

New optional flag `--thresholds <file.toml>` (see A3 for the mode flag it accompanies). The tool
ships a built-in default (budget 10 %, `metric = "p50"`, a small absolute floor); the file
overrides. `p50` means the cell's **total-latency median** (`total_ms` p50). Format (lives in
`falkordb-rs-next-gen`, TOML to match `synthetic-bench.toml`):

```toml
# Regression budget for the per-PR synthetic check. A cell is 🔴 only if the PR's p50 is slower
# than the baseline by MORE than budget_pct AND the absolute p50 increase exceeds floor_ms (a
# noise guard for sub-millisecond ops). Faster — or slower within either bound — is 🟢.
# Precedence per field: the matching C under [op.<name>].concurrency > [op.<name>] > [default].

[default]
metric     = "p50"   # p50 (implemented). throughput|both reserved for a later iteration.
budget_pct = 10.0
floor_ms   = 0.5     # ignore slowdowns whose absolute p50 delta is below this (noise on fast ops)

[op.match_by_index]
budget_pct = 12.0    # fastest op — a touch more headroom for high-C jitter

[op.expand_hops_5]
budget_pct  = 20.0
concurrency = { 16 = 30.0, 32 = 40.0 }
```

Parsing **validates** the file (unknown op keys and non-positive/invalid budgets are hard errors so
a typo can't silently disable a budget). **Resolution contract** — the config is a single inline
representation (`[op.<name>]` with an optional `concurrency = { <C> = <pct>, … }` inline table); for
each `(op, C)` and each field (`budget_pct`, `floor_ms`, `metric`) the tool takes
`op.<op>.concurrency.<C>` if present, else `op.<op>.<field>`, else `default.<field>`. Parsing +
precedence + the verdict function are pure logic → **unit-tested** (faster, exactly at budget, just
over, per-op vs per-op×C precedence, floor suppression, divergence → N/A, zero/missing baseline →
N/A).

### A3. Explicit non-fatal regression mode — `report --regression`

Add a dedicated mode rather than overloading the strict `--diff` guard. New invocation
`synthetic report --regression --baseline <base.json> --candidate <cand.json> [--thresholds
<file>] [--out <md>]` (or, equivalently, `report --diff … --regression`), which:

- **Splits the guard** (today `baseline::guard` conflates the two — src/synthetic/baseline.rs):
  - a **comparability mismatch** ⇒ *globally not comparable* → the whole table is rendered as
    `⚠ not comparable`. The **comparability manifest** — the behavior-affecting inputs that must
    match for a latency comparison to be valid — is: the `workload_hash` (recorded graph +
    commands), the `generator_version` (tool workload version), `samples`/`warmup`, the concurrency
    sweep + cache modes, and the controlled **server settings** that affect throughput (e.g.
    `MAX_QUEUED_QUERIES`). The report JSON already records the first four; the design **adds the
    applied server settings to the report** so a mismatch is *detectable*, not assumed. In the CI
    flow these are identical by construction (same recorded bundle → one manifest hash copied into
    every report; same sweep; same settings applied to every container), so this should never trip
    for PR-vs-main — but the check fails safe;
  - a **per-op `result_digest` mismatch** ⇒ only *that* op is correctness-🔴 with `⚠ results
    differ`, its perf verdict **N/A** (a different result means different work — the latency Δ is
    kept only as a dim diagnostic, excluded from the "over budget" count). No abort.
- **Never exits non-zero** for regressions or divergence (informational). The strict guard/abort
  behavior is **unchanged** for plain `report --diff` (so the benchmark repo's own
  `synthetic-verify` gate is untouched). `--regression` works with built-in defaults even without a
  `--thresholds` file.
- Per comparable cell, verdict on `p50`: `slower = candidate_p50 > baseline_p50`;
  🔴 iff `slower` **and** `(candidate_p50 − baseline_p50) > floor_ms` **and**
  `candidate_p50 > baseline_p50 × (1 + budget_pct/100)`; else 🟢. Zero/missing baseline → N/A.
- Emits a per-op **verdict line** and an overall count (`🔴 2 of 108 cells over budget: …`) so the
  "suspect calls that got worse" are immediately visible, plus the full per-op × cache × concurrency
  table with a 🟢/🔴/N/A column and the two runs' `--label`s as headers.

### A4. Fail-soft is the *job's* responsibility, not the tool's

The tool must **not** hide malformed input or infra failures: `synthetic run` still aborts on load
failures / query errors / timeouts (src/synthetic/replay.rs), and `report` still errors on
unreadable/covertly-wrong JSON. The **CI job** (Part B) is what makes the check non-blocking — it
runs allowed-to-fail and, on any tool/infra error, publishes a "benchmark unavailable" note instead
of a table. This keeps correctness/infra failures visible without ever blocking the PR.

### A5. Two baselines in one report

Keep the tool's unit of work a single pair (`--baseline`, `--candidate`). The **workflow** invokes
it once per baseline (`main→pr`, `release→pr`) and concatenates the two tables under one sticky
comment. (A combined multi-baseline subcommand is a possible future consolidation, out of scope.)

### A6. Release + docs

- Update `readme.md`, `.github/copilot-instructions.md` recipe/flag tables, and the cookbook with
  `--label` and `--thresholds`.
- Cut a **new benchmark tag** (post-`v2.2`, containing `synthetic` + this feature) and pin
  `falkordb-rs-next-gen`'s `SYNTHETIC_BENCHMARK_REF` to that tag's **immutable commit SHA** (with a
  `# vX.Y.Z` comment, matching the existing `BENCHMARK_REF = deca7752 # v2.2` convention) so CI can
  never execute changed benchmark code without a corresponding change in `falkordb-rs-next-gen`.
- Patch coverage ≥ 90 % on the new logic (`just coverage`).

## 5. Part B — changes in `FalkorDB/falkordb-rs-next-gen` (the CI)

### B1. Config files

- `.github/synthetic-thresholds.toml` — the A2 threshold budget (10 % default + illustrative
  per-op overrides). The project owns its regression budget here.
- `.github/synthetic-workload.toml` — a pinned, versioned workload (`seed`, `graph`, `nodes`,
  `edges`) plus `samples`/`warmup`, so `record` is reproducible and the query volume is bounded
  (recording needs `--nodes/--edges` — the tool errors without them). Start modest (e.g. medium
  10k/50k, `--samples 200 --warmup 50`, matching `synthetic-verify`) and tune for VM runtime.

### B2. A new "synthetic A/B" job in the benchmark pipeline

Add a job (reusing the A/B's GCE-VM provisioning) on **its own VM** of the same type, with a
**unique runner label** so no A/B variant is ever scheduled onto it, running **one container at a
time**. It is **non-blocking** (`continue-on-error` + not a required check, so a failure never
blocks the merge);
every tool/infra failure is caught and turned into a "benchmark unavailable" comment (A4).

1. **Resolve immutable images.** Wait for the PR's RC build **for the event's exact head SHA** (not
   merely tag existence — reuse `wait-for-rc-image`), then resolve every tag to a **digest**
   (`docker buildx imagetools inspect`) *once*, up front: `pr@sha256:…` (from `rc-pr-<N>`),
   `main@sha256:…` (from `edge-rs`), and the release digest if a semver image exists. Measure the
   **digests** (mutable tags can move mid-job) and pass them to `--server-image` so the report
   records exactly what ran.
2. **Record once** (offline, deterministic): `benchmark synthetic record --op all --config
   .github/synthetic-workload.toml --out-dir rec` (read ops only; writes aren't recordable or
   comparable across versions).
3. **Measure each build sequentially** — start → prep → measure → **stop** before the next:
   - `docker run -d -p 6379:6379 <digest>`; **poll readiness** (`redis-cli ping`); **set
     `GRAPH.CONFIG SET MAX_QUEUED_QUERIES 1000`** (the C=32 uncached sweep trips the default queue
     limit — same fix as `synthetic-verify`) and any other controlled DB settings;
     `benchmark synthetic run --recording rec --endpoint falkor://127.0.0.1:6379 --label pr
     --server-image pr@sha256:… --concurrency 1,2,4,8,16,32 --cache both --samples 200 --warmup 50
     --out pr.json`; **`docker rm -f`** (guaranteed cleanup via trap).
   - repeat for `main` (`--label main`) and, if present, `release` (`--label "release X.Y.Z"`).
4. **Diff + color (non-fatal):** `benchmark synthetic report --regression --baseline main.json
   --candidate pr.json --thresholds .github/synthetic-thresholds.toml --out syn-main.md`, and
   likewise for release. Each call catches failure → "unavailable" fragment.
5. **Publish (non-blocking):** assemble one body (arch-specific marker — see B3). **Both** the
   job-summary step and the sticky-PR-comment step (`pull-requests: write`) are guarded by
   `if: always()`; a missing report, a comment-write failure, or a read-only fork token (fork PRs
   get a read-only `GITHUB_TOKEN` regardless of the declared permission) is handled by **warning and
   exiting 0**, so publication can never stop the non-blocking job.

### B3. Trigger, serialization, and the tool ref

- **PR-triggered only.** Runs with the A/B benchmark (same `benchmark-*` labels / `/benchmark` /
  PR-scoped dispatch). Skip on plain `workflow_dispatch` with no PR (no PR number/image). Because it
  has its **own** uniquely-labelled VM and runs containers strictly sequentially, it satisfies
  "≤ 1 benchmark per machine" without contending with A/B variant VMs.
- **Arch-specific markers** to avoid x86/arm comment races (mirroring the A/B's
  `-arm` marker): `<!-- synthetic-benchmark -->` / `<!-- synthetic-benchmark-arm -->`.
- **Separate `SYNTHETIC_BENCHMARK_REF`** pinned to an **immutable commit SHA** (with a `# vX.Y.Z`
  comment, like `BENCHMARK_REF = deca7752 # v2.2`) and a **separate checkout**, so the A/B's
  `BENCHMARK_REF=v2.2` (coupled to the legacy `ab-compare` vendor mode + its trend continuity) is
  left completely untouched.

### B4. "No release yet" handling

The job resolves the highest semver tag under `ghcr.io/falkordb/falkordb-server`; if none, it runs
and reports **PR-vs-main only** (today's state). When releases begin, PR-vs-release appears
automatically with the correct `release X.Y.Z` name. An image that's too old to load the recorded
bundle is reported **unavailable**, not compared.

## 6. Example rendered comment (illustrative)

```markdown
<!-- synthetic-benchmark -->
### 🧪 Synthetic per-op regression — PR vs main (same machine)

Identical recorded workload, each build measured back-to-back on one VM (one container at a time).
🟢 = faster or within budget · 🔴 = slower than budget **or** results differ · N/A = no perf verdict
(results differ or not comparable). Non-blocking.

**pr vs main** — 🔴 2 of 108 cells over budget (match_by_index @ C=16); 1 op with differing results

| op | C | cache | main p50 (ms) | pr p50 (ms) | Δp50 | verdict |
|---|--:|---|--:|--:|--:|:--:|
| match_by_index | 16 | cached | 2.21 | 2.65 | +19.9% | 🔴 |
| match_by_index | 32 | cached | 5.15 | 4.58 | −11.0% | 🟢 |
| expand_hops_5 | 8 | cached | 1.90 | 1.88 | _(−1%)_ | 🔴 N/A ⚠ results differ |
| … | | | | | | |
```

## 7. Risks / mitigations

- **Same-machine noise vs a 10 % budget** (we measured ±10–13 % run-to-run on the *fastest* op at
  C=32): the single default budget could be red-happy. Mitigations baked in: (a) p50 (tail-robust);
  (b) an **absolute `floor_ms`** so sub-millisecond ops aren't flagged on relative jitter alone;
  (c) **per-op / per-op×concurrency budgets** for known-noisy cells. **Recommended calibration
  step** before turning it on: run a few **A/A** comparisons (same image twice) per arch and set
  `budget_pct`/`floor_ms` from the observed noise, so a red means signal, not jitter. Optionally
  repeat AB/BA to cancel fixed-order bias (future).
- **Result divergence across versions** (a next-gen build may legitimately differ from main): the
  split guard makes it **per-op N/A + correctness-🔴**, never an abort or a bogus latency verdict.
- **Mutable tags / wrong build:** resolved to **digests up front**, and the RC image is waited on
  for the **exact head SHA** — so a moved `edge-rs`/`rc-pr-<N>` can't silently swap the build.
- **Tool/infra failure blocking the PR:** the **job** is allowed-to-fail and always posts a result;
  the tool keeps surfacing real failures (no silent hiding).
- **`rc` image not ready:** reuse `wait-for-rc-image` (proven, 85-min timeout).
- **Legacy A/B breakage:** avoided by a **separate `SYNTHETIC_BENCHMARK_REF`** — `BENCHMARK_REF`
  stays at `v2.2`.
- **Cost:** one extra VM per benchmark run — acceptable (label-triggered, like the A/B).
- **`edge-rs` freshness:** it is the last *merged* main, which can lag the PR base by a few commits;
  acceptable for a per-PR trend signal (and it's exactly what the A/B uses as variant B).

## 8. Deliverables checklist (on approval)

**`FalkorDB/benchmark`:** `run --label`; `report --regression` (non-fatal, colored 🟢/🔴/N/A,
p50=total-median verdict with `budget_pct` + `floor_ms`); **split guard** on a **comparability
manifest** (`workload_hash` + `generator_version` + samples/warmup + sweep + applied server
settings, recorded in the report → global not-comparable on mismatch; per-op digest mismatch =
per-op N/A, no abort — strict `--diff` unchanged); threshold TOML parsing + validation + precedence;
unit tests (≥ 90 % patch); docs (readme/copilot-instructions/cookbook); **new tag + its immutable
commit SHA** for `SYNTHETIC_BENCHMARK_REF`.

**`FalkorDB/falkordb-rs-next-gen`:** `.github/synthetic-thresholds.toml` +
`.github/synthetic-workload.toml`; a new **synthetic-A/B job** in the benchmark pipeline (own
uniquely-labelled VM, allowed-to-fail; wait-for-rc-by-SHA → resolve images to **digests** →
`record` once → **sequential** per-build measure with `MAX_QUEUED_QUERIES` raised + readiness poll +
guaranteed cleanup → per-baseline `report --regression` → arch-marked sticky comment + summary,
non-blocking); `SYNTHETIC_BENCHMARK_REF` pin (separate from `BENCHMARK_REF`); PR-vs-release
auto-enabled when a release image exists. **Calibrate** `budget_pct`/`floor_ms` from A/A runs before
enabling.

## 9. Out of scope (for this iteration)

- Gating/failing the PR on regression (kept informational).
- A combined multi-baseline `report --regression` subcommand (two `report --diff` calls for now).
- Write-op comparison (reads only; writes aren't recordable/deterministically comparable).
- gh-pages trend storage for the synthetic numbers (the A/B dashboard remains the trend home).
