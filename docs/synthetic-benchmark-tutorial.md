# Synthetic benchmark tutorial: comparing FalkorDB versions on an identical workload

This tutorial shows how to use the **synthetic per-operation benchmark** to compare two FalkorDB
versions fairly, using the **record / replay** workflow. It walks through recording a workload,
replaying it, comparing two versions, and the built-in self-sanity check — and explains the
run-to-run latency noise you will see and why it is *not* a workload difference.

All commands are driven through [`just`](../Justfile) recipes (the same ones CI uses). Every step
links its **expected output** under [`docs/synthetic/`](synthetic/).

## Why record / replay

To compare two FalkorDB versions, the **graph** and the **measured commands** must be *identical* —
otherwise you are comparing two different workloads, not two versions. The older
`just synthetic-baseline` / `synthetic-compare` recipes **re-generate the graph and re-derive the
commands on every run**. The dataset itself is generated from a portable, version-stable mixer, but
the measured command corpus is drawn with an RNG whose exact sequence is not guaranteed stable
across tool rebuilds — and regenerating the graph each run also makes the server redo heavy write
work and land in a slightly different state. Both are avoidable sources of noise.

**Record / run / report splits the benchmark into three phases and reuses the same artifacts:**

1. **record** — `synthetic record` writes the dataset load-script *and* the measured commands to a
   **bundle** on disk (offline; no server needed).
2. **run** — `synthetic run --recording <dir>` drops + loads + **verifies** the recorded graph, then
   measures the recorded commands through the closed-loop engine (concurrency sweep + cache modes),
   verifying results are unchanged under concurrency.
3. **report** — `synthetic report --diff A.json B.json` guards the two runs measured the same
   workload, then writes a Markdown diff across every op / cache-mode / concurrency level.

Because both versions load the same recorded graph and run the same recorded commands, the only
variable left is the FalkorDB version.

## Prerequisites

- A reachable FalkorDB (e.g. `docker run -d -p 6379:6379 falkordb/falkordb:latest`).
- The toolchain the repo uses (`just`, a Rust toolchain, `protoc`). See the [README](../readme.md).

## Step 1 — Record the workload (offline)

Recording contacts **no server** — it is a pure function of the dataset knobs and the seed:

```bash
just synthetic-record demo --graph tutorial_demo \
  --op match_by_index,expand_1_hop,aggregate_count \
  --seed 42 --nodes 1000 --edges 5000
```

This writes a bundle to `recordings/demo/`:

```text
recordings/demo/
├── manifest.json              # versions, knobs, ops, and the workload_hash
├── graph.jsonl                # the ordered load statements (index + node/edge UNWIND batches)
└── commands/
    ├── match_by_index.jsonl   # the fully-rendered measured queries, one per line
    ├── expand_1_hop.jsonl
    └── aggregate_count.jsonl
```

Expected `manifest.json`: [`docs/synthetic/sample-recording-manifest.json`](synthetic/sample-recording-manifest.json).
The `workload_hash` is a length-framed SHA-256 over the header, **every graph statement**, and
**every command** — so any later edit to the graph *or* the commands is detected when the bundle is
loaded. `just synthetic-record` reads `synthetic-bench.toml` for defaults, so once you have a config
you can simply run `just synthetic-record demo`.

## Step 2 — Run one version against the recorded bundle

`run --recording` loads the recorded graph (drop + load + count-verify) and then measures the
recorded commands through the closed-loop engine across a concurrency sweep and cache modes:

```bash
just synthetic-replay demo falkor://127.0.0.1:6379 -- --concurrency 1,4 --samples 500 --warmup 100
# (a thin wrapper for: benchmark synthetic run --recording recordings/demo --endpoint … --concurrency 1,4)
```

Expected report:

- Markdown (PR-pasteable): [`docs/synthetic/sample-replay-report.md`](synthetic/sample-replay-report.md)
- JSON (full detail, incl. per-op `result_digest`): [`docs/synthetic/sample-replay-report.json`](synthetic/sample-replay-report.json)

Each operation is measured at every requested concurrency level, under **cached** (plan reused) and
**uncached** (forced plan-cache miss) modes. The JSON carries a per-op **`result_digest`** — a hash
of the result *values* across the recorded commands — and the run **verifies results are identical at
the highest concurrency** (an untimed concurrent pass): a version returning wrong/empty results under
concurrency is a hard failure, not a faster number.

## Step 3 — Compare two versions and diff them

Start the two versions on different ports, record once, then compare:

```bash
# version A on :6379, version B on :6380
just synthetic-record demo --graph tutorial_demo \
  --op match_by_index,expand_1_hop,aggregate_count --seed 42 --nodes 1000 --edges 5000
just synthetic-compare-versions demo falkor://127.0.0.1:6379 falkor://127.0.0.1:6380
```

`synthetic-compare-versions` runs the **same bundle** against each endpoint (writing
`recordings/demo/version-a.json` and `version-b.json`), then `report --diff` **guards** the pair and
writes a Markdown diff. The guard:

- **aborts** unless the two runs' `workload_hash` match (they do, by construction — same bundle);
- **aborts** unless every op's `result_digest` matches (so a version that returns wrong or empty
  results faster can't look like an improvement);
- treats the FalkorDB **version** difference as expected (recorded, never rejected).

The diff (`recordings/demo/diff.md`) tabulates, per op × cache mode × concurrency level, the two
runs' throughput and total-latency p50/p90/p95/p99 with deltas. A sample diff is at
[`docs/synthetic/sample-diff.md`](synthetic/sample-diff.md), and a full record → run → report
transcript (hostname redacted to `bench-host`) is at
[`docs/synthetic/sample-console.txt`](synthetic/sample-console.txt).

## Step 4 — Self-sanity check the tool

`just synthetic-sanity` is a self-contained check of the tool itself. It spins up a throwaway Docker
FalkorDB, **records the same workload twice** and asserts the two `workload_hash`es are identical
(proving recording is deterministic), then `run --recording` at C=1,4 (load, then no-load) and
`report --diff` (which runs the C>1 result verification) the two reports:

```bash
just synthetic-sanity
# … → "deterministic recording OK: sha256:…"  then  "synthetic-sanity OK"
```

It passes iff recording is deterministic, results are unchanged under concurrency, and the pipeline
guards clean. It does **not** assert latencies (see the next section).

## Understanding the run-to-run latency noise

If you run the **same bundle against the same version twice**, the per-op latencies still wobble — on
a laptop with Docker Desktop, empirically ~±5–13%. This is **environment noise** (CPU-frequency
scaling, the Docker VM, background load), **not** a workload difference:

- the `workload_hash` is identical across the two runs (same graph + same commands), and
- every op's `result_digest` is identical (same results).

So the workload is provably identical; only the measured wall-clock varies. That is why the guard
gates on **workload identity + result correctness** (hard) and leaves **latency** advisory.

To get a trustworthy latency delta between versions:

- run on a **stable, dedicated host** (bare metal or a quiet VM), not a laptop under load;
- use **more samples** (`--samples`) to tighten the estimate;
- compare medians/p90 across the two `version-*.json` reports and treat sub-~10% deltas on noisy
  hardware as inconclusive.

## Reference

| Recipe / command | Purpose |
| --- | --- |
| `just synthetic-record <name> [flags]` | Record a workload bundle **offline** into `recordings/<name>/`. |
| `benchmark synthetic run --recording <dir> [--concurrency … --cache …]` | Load the recorded graph + measure the recorded commands (C sweep + cache modes) + verify results under concurrency. |
| `benchmark synthetic report --diff <A.json> <B.json> [--out diff.md]` | Guard two runs + write a Markdown diff across every op/cache-mode/concurrency. |
| `just synthetic-replay <name> <endpoint> [-- flags]` | Wrapper for `run --recording` against one endpoint. |
| `just synthetic-compare-versions <name> <A> <B>` | `run --recording` against two versions + `report --diff`. |
| `just synthetic-sanity` | Self-contained tool sanity (deterministic record + C=1,4 run + report --diff). |

The `just synthetic-baseline` / `synthetic-compare` recipes remain as a **single-version** local
latency tracker built on Criterion; they regenerate per run and are not the cross-version
comparator (Criterion adapts its iteration count to observed latency, so a faster/slower version
would run a different command sequence). Use record / run / report for comparing versions.

See the [README](../readme.md) for the operation catalog and `synthetic run` details.
