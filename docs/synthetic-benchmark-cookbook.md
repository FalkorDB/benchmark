# Synthetic benchmark cookbook — recipes by user story

Task-oriented recipes for the **synthetic per-operation benchmark**: each story is a self-contained
scenario with its **goal**, the **exact commands**, **what each step does**, and **real captured
output**. For the conceptual walkthrough (why record/replay, and why run-to-run latency is *noise*,
not a workload difference) read the [synthetic benchmark tutorial](synthetic-benchmark-tutorial.md)
first; this cookbook is the "how do I do X" companion.

All commands are driven through [`just`](../Justfile) recipes (the same ones CI uses) or the
`benchmark synthetic` subcommands directly. The three verbs are:

- **`record`** — write a workload bundle **offline** (no server): the dataset load-script + the
  measured commands, plus a `workload_hash` over both. A pure function of the seed + knobs.
- **`run`** — load a graph and measure operations across a **concurrency sweep** × **cache modes**,
  writing a JSON + Markdown report. `run --recording <dir>` measures a recorded bundle; `run
  --generate` builds a fresh dataset and probes it live.
- **`report`** — re-render a saved report, or **`report --diff A B`** guard-and-diff two reports.

> Prerequisites: a FalkorDB to measure against, e.g. `docker run -d -p 6379:6379
> falkordb/falkordb:latest`, and the Rust toolchain (see the repo [readme](../readme.md)). Outputs
> below were captured on a 1000-node / 5000-edge dataset for brevity; host names are shown as
> `bench-host` and tracing lines are omitted.

| # | User story | Verbs |
|---|---|---|
| [1](#story-1) | Compare two FalkorDB versions on **every** operation across **every** concurrency level | `record` → `run` ×2 → `report --diff` |
| [2](#story-2) | Profile **one** version's latency/throughput curve (concurrency × cached/uncached) | `run` |
| [3](#story-3) | Capture a **reproducible** workload offline to share or archive | `record` |
| [4](#story-4) | Re-render or diff **previously saved** reports (no server) | `report` |
| [5](#story-5) | **Self-check** the tool (determinism + run-to-run non-divergence) | `record` / `just synthetic-verify` |

---

<a id="story-1"></a>
## Story 1 — Compare two FalkorDB versions on every operation across every concurrency level

**Goal.** "Is version B faster than version A?" — measured fairly on the *identical* graph and the
*identical* commands, for **all** read operations and **all** concurrency levels, so the only
variable is the FalkorDB version.

```bash
# 1. Record every read op ONCE, offline (--op all is the magic selector; --op '*' also works).
benchmark synthetic record --op all --nodes 1000 --edges 5000 --seed 7 --out-dir rec

# 2. Run that identical bundle against each version, across the full concurrency sweep + both
#    cache modes. Point --endpoint at your two FalkorDB builds (here: ports 6381 and 6382).
benchmark synthetic run --recording rec --endpoint falkor://127.0.0.1:6381 \
  --concurrency 1,2,4,8,16,32 --cache both --out runA.json \
  --server-image "falkordb/falkordb:v4.2.0"
benchmark synthetic run --recording rec --endpoint falkor://127.0.0.1:6382 \
  --concurrency 1,2,4,8,16,32 --cache both --out runB.json \
  --server-image "falkordb/falkordb:v4.2.1"

# 3. Guard + diff. report --diff ABORTS unless both reports measured the same workload_hash AND
#    every op's result_digest matches — so a version that returns wrong/empty results faster
#    cannot masquerade as an improvement.
benchmark synthetic report --diff runA.json runB.json --out diff.md
```

**What each step does.**

- `record --op all` writes `rec/` = `manifest.json` + `graph.jsonl` + `commands/<op>.jsonl` for all
  nine read ops, with a `workload_hash` over the graph *and* the commands. It touches no server.
- Each `run --recording` drops + loads + **count-verifies** the recorded graph, then measures the
  recorded commands through the closed-loop engine. It also **verifies results are identical at the
  highest concurrency** (an untimed concurrent pass), so a wrong result under concurrency is a hard
  fail — this is the "results agree when concurrency > 1" guarantee.
- `report --diff` is **fail-closed**: it refuses to compare two runs of different workloads, then
  emits a Markdown table per op × cache-mode × concurrency (throughput + total p50/p90/p95/p99 with
  deltas).

**What you get** (`diff.md`, header + one operation):

```markdown
# Synthetic benchmark diff — A → B

| field | A (baseline) | B (candidate) |
|---|---|---|
| FalkorDB module | 4.20.1 | 4.20.1 |
| server image | falkordb/falkordb:v4.2.0 | falkordb/falkordb:v4.2.1 |
| workload_hash | `sha256:7be5c44c…3058e2` | `sha256:7be5c44c…3058e2` |
| samples / warmup | 100 / 50 | 100 / 50 |

## `aggregate_count`

_cached (plan reused — execution only)_

| C | A total p50/p90/p95/p99 (ms) | B total p50/p90/p95/p99 (ms) | Δp50 | A tput (ops/s) | B tput (ops/s) | Δtput |
|---:|---|---|---:|---:|---:|---:|
| 1  | 0.496 / 0.708 / 0.793 / 0.968 | 0.426 / 0.536 / 0.565 / 0.602 | -14.2% | 1860 | 2179 | +17.1% |
| 8  | 0.734 / 0.976 / 1.078 / 1.272 | 0.719 / 0.907 / 0.977 / 1.177 | -2.0%  | 10065 | 10609 | +5.4% |
| 32 | 1.768 / 2.467 / 2.694 / 3.194 | 1.632 / 2.442 / 2.886 / 3.640 | -7.7%  | 16830 | 17250 | +2.5% |
```

> **The `workload_hash` matching is the whole point** — it proves A and B ran byte-identical
> graphs and commands. When both endpoints are the *same* build (as above), the diff even prints
> `⚠ baseline and candidate ran the same FalkorDB module version … there is no version delta to
> measure`, and the small ±deltas you see are pure environment noise. Point the two `--endpoint`s
> at genuinely different versions to read a real delta. See
> [`docs/synthetic/sample-diff.md`](synthetic/sample-diff.md) for a full example.

`just synthetic-compare-versions A=falkor://…:6381 B=falkor://…:6382` wraps the two runs + the diff
into a single recipe.

---

<a id="story-2"></a>
## Story 2 — Profile one version's latency/throughput curve

**Goal.** For a single FalkorDB, see how each operation scales: throughput and tail latency across
the concurrency sweep, and the extra cost of a cold plan cache (compilation).

```bash
# Reuse a recorded bundle (from Story 3) and measure it live — the report shape is identical
# whether the graph came from a recording or from --generate.
benchmark synthetic run --recording rec --endpoint falkor://127.0.0.1:6381 \
  --concurrency 1,2,4,8,16,32 --cache both --out profile.json

# Or profile a fresh, self-generated dataset with no recording step (destructive: drops + rewrites):
benchmark synthetic run --generate --nodes 1000 --edges 5000 --op all \
  --endpoint falkor://127.0.0.1:6381 --concurrency 1,2,4,8,16,32 --cache both --out profile.json
```

**What each step does.** `run` measures every op through the closed-loop engine at each concurrency
level, in both `cached` (warm plan, execution only) and `uncached` (forced plan-cache miss,
execution + compilation) modes, and derives the per-level **compilation cost** from the difference.
`--recording` loads a frozen bundle; `--generate` builds a reproducible dataset into the graph first
(destructive: it drops + rewrites).

**What you get** (console, header + `aggregate_count`):

```text
synthetic benchmark — endpoint falkor://127.0.0.1:6381  graph falkor  samples 100  warmup 50  concurrency [1,2,4,8,16,32]  seed 7  connection pool(size=1) per worker
server — falkordb module ver 4.20.1  redis 8.6.3  CACHE_SIZE 25
client host — bench-host · macOS · Apple M1 Pro (10c/10t) · 16.0 GiB · arm64
dataset — seed 7  nodes 1000  edges 5000  workload_hash sha256:7be5c44c…3058e2

aggregate_count
  [cached — plan reused, execution only]
    C    throughput(ops/s)   server p50/p90/p95/p99             total p50/p90/p95/p99              miss%
    1              1860     0.102 / 0.158 / 0.180 / 0.210     0.496 / 0.708 / 0.793 / 0.968     0.0
    8             10065     0.113 / 0.211 / 0.254 / 0.319     0.734 / 0.976 / 1.078 / 1.272     0.0
   32             16830     0.099 / 0.209 / 0.263 / 0.347     1.768 / 2.467 / 2.694 / 3.194     0.0  <- knee
  [uncached — plan-cache miss each run, execution + compilation]
    C    throughput(ops/s)   server p50/p90/p95/p99             total p50/p90/p95/p99              miss%
    1              1788     0.167 / 0.211 / 0.226 / 0.255     0.526 / 0.660 / 0.715 / 0.812   100.0
   32             12299     0.608 / 1.412 / 1.763 / 2.399     2.361 / 3.355 / 3.788 / 4.671   100.0  <- knee
  compilation_ms (median uncached-cached server time) by level:
    C=1    0.065
    C=32   0.509
```

The `<- knee` marker flags the concurrency where throughput stops scaling (the saturation point).
`miss%` confirms the cache condition (0 % cached, 100 % uncached). A full three-op report is saved
in [`docs/synthetic/sample-replay-report.md`](synthetic/sample-replay-report.md).

---

<a id="story-3"></a>
## Story 3 — Capture a reproducible workload offline

**Goal.** Freeze an exact graph + command set into a portable bundle you can commit, share, or hand
to another machine — with a hash that detects any later tampering.

```bash
# No server required. Same seed + same tool build ⇒ byte-identical bundle every time.
benchmark synthetic record --op all --nodes 1000 --edges 5000 --seed 7 --out-dir rec
ls rec rec/commands
```

**What each step does.** `record` writes the dataset load-script (`graph.jsonl`), one command file
per op (`commands/<op>.jsonl`), and a `manifest.json` describing the bundle, sealed by a
`workload_hash` computed over the graph *and* every command. Because it is a pure function of the
seed and the knobs, it is fully offline and reproducible.

**What you get:**

```text
recorded 9 op(s) into rec (workload_hash sha256:7be5c44c…3058e2)
rec: commands  graph.jsonl  manifest.json
rec/commands: aggregate_count.jsonl aggregate_group.jsonl expand_1_hop.jsonl
  expand_hops_5.jsonl match_by_index.jsonl match_by_label_scan.jsonl
  property_projection.jsonl return_const.jsonl shortest_path.jsonl
```

`manifest.json` (excerpt):

```json
{
  "format_version": 1,
  "generator_version": "synthbench/v3",
  "dataset": { "seed": 7, "nodes": 1000, "edges": 5000 },
  "ops": [ { "name": "return_const", "count": 256 }, { "name": "match_by_index", "count": 256 } ],
  "workload_hash": "sha256:7be5c44c…3058e2"
}
```

A full manifest is in
[`docs/synthetic/sample-recording-manifest.json`](synthetic/sample-recording-manifest.json).
`just synthetic-record <name>` wraps this and writes under `recordings/<name>/` (git-ignored).

---

<a id="story-4"></a>
## Story 4 — Re-render or diff previously saved reports

**Goal.** You already have report JSON(s) from an earlier run — inspect or compare them again with
**no server** and no re-measuring.

```bash
# Re-print the console summary of one saved report (add --out report.md to also write Markdown):
benchmark synthetic report runA.json

# Diff two saved reports (guards workload_hash + result digests, then writes Markdown):
benchmark synthetic report --diff runA.json runB.json --out diff.md
```

**What each step does.** `report <json>` deserializes a saved run and re-renders the exact console
summary (percentiles, throughput, compilation cost) — handy for turning an old JSON into a Markdown
artifact without re-running the benchmark. `report --diff` is the same guard-and-diff used in
[Story 1](#story-1), but on **already-captured** JSON, so it is instant and server-free.

**What you get:** the same console table as [Story 2](#story-2) (for re-render), or the
[Story 1](#story-1) `diff.md` (for `--diff`). If the two reports measured different workloads, the
diff **aborts** rather than printing a misleading comparison:

```text
cannot diff — workload_hash mismatch — the workload changed since the baseline was saved
(baseline sha256:…a1, candidate sha256:…b2); re-save the baseline for the current workload
```

---

<a id="story-5"></a>
## Story 5 — Self-check the tool (determinism + non-divergence)

**Goal.** Trust the tool before trusting its numbers: prove that (a) recording is **deterministic**
and (b) two runs of the same workload against the same FalkorDB **do not diverge** in results.

```bash
# (a) Determinism: record the same workload twice, compare the workload_hash — must be identical.
benchmark synthetic record --op all --nodes 1000 --edges 5000 --seed 7 --out-dir rec
benchmark synthetic record --op all --nodes 1000 --edges 5000 --seed 7 --out-dir rec_again
# → both print the SAME `workload_hash sha256:7be5c44c…3058e2`

# (b) Non-divergence (the CI gate): record all ops, run twice against ONE FalkorDB across the full
#     sweep + both cache modes, and fail if any result digest differs. Spins its own throwaway
#     Docker FalkorDB and tears it down.
just synthetic-verify

# A lighter, faster self-check (records twice + a C=1,4 diff):
just synthetic-sanity
```

**What each step does.** The two `record`s producing an identical `workload_hash` proves the bundle
is a pure function of the seed + tool build. `just synthetic-verify` — the **`Synthetic
non-divergence` CI gate** — records every read op, then runs the recorded bundle **twice** against
the same server across `--concurrency 1,2,4,8,16,32 --cache both`; the final `report --diff` fails
non-zero if the `workload_hash` or **any** per-op result digest differs between the two runs.
Because it compares deterministic result digests (not latency), it is **not** flaky.

**What you get:**

```text
first : sha256:7be5c44c…3058e2
again : sha256:7be5c44c…3058e2      ← identical ⇒ deterministic

# …and the final line from `just synthetic-verify`:
synthetic-verify OK — no divergence across all ops × concurrency 1,2,4,8,16,32 × cached/uncached
```

---

## See also

- [Synthetic benchmark tutorial](synthetic-benchmark-tutorial.md) — the conceptual walkthrough and
  the latency-noise explainer.
- [`readme.md`](../readme.md) — the record / run / report reference and the full `just` recipe list.
- [`docs/synthetic/`](synthetic/) — full sample outputs (`sample-console.txt`, `sample-diff.md`,
  `sample-replay-report.md`, `sample-recording-manifest.json`).
- `just synthetic-ops` lists every operation; `benchmark synthetic run --help` documents every flag.
