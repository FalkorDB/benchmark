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

**Record / replay splits the benchmark into three phases and reuses the same artifacts everywhere:**

1. **generate input** — `synthetic record` writes the dataset load-script *and* the measured
   commands to a **bundle** on disk (offline; no server needed).
2. **load** — `synthetic replay` drops + loads + **verifies** the recorded graph into a server.
3. **run** — the same command replays with a **fixed-length, deterministic** runner.

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

## Step 2 — Replay against one version

Replaying loads the recorded graph (drop + load + count-verify) and then measures the recorded
commands with a fixed number of invocations:

```bash
just synthetic-replay demo falkor://127.0.0.1:6379 -- --samples 500 --warmup 100
```

Expected report:

- Markdown (PR-pasteable): [`docs/synthetic/sample-replay-report.md`](synthetic/sample-replay-report.md)
- JSON (full detail, incl. per-op `result_digest`): [`docs/synthetic/sample-replay-report.json`](synthetic/sample-replay-report.json)

Each operation reports a single `C=1` cached level (honest single-flight latency). The JSON also
carries a per-op **`result_digest`** — a hash of the result *cardinality* across the recorded
commands — used by the guard (Step 3) to reject a version that returns different results.

## Step 3 — Compare two versions

Start the two versions on different ports, record once, then compare:

```bash
# version A on :6379, version B on :6380
just synthetic-record demo --graph tutorial_demo \
  --op match_by_index,expand_1_hop,aggregate_count --seed 42 --nodes 1000 --edges 5000
just synthetic-compare-versions demo falkor://127.0.0.1:6379 falkor://127.0.0.1:6380
```

`synthetic-compare-versions` replays the **same bundle** against each endpoint (writing
`recordings/demo/version-a.json` and `version-b.json`) and then **guards** the comparison. The guard:

- **aborts** unless the two runs' `workload_hash` match (they do, by construction — same bundle);
- **aborts** unless every op's `result_digest` matches (so a version that returns wrong or empty
  results faster can't look like an improvement);
- treats the FalkorDB **version** difference as expected (recorded, never rejected).

A full record → replay → guard transcript (hostname redacted to `bench-host`) is in
[`docs/synthetic/sample-console.txt`](synthetic/sample-console.txt). Open the two `version-*.json`
reports (or their `.md` siblings) side by side to read the per-op latency delta.

## Step 4 — Self-sanity check the tool

`just synthetic-sanity` is a self-contained check of the tool itself. It spins up a throwaway Docker
FalkorDB, **records the same workload twice** and asserts the two `workload_hash`es are identical
(proving recording is deterministic), then replays it (load) and re-replays it (no-load) against the
same server and guards the two reports:

```bash
just synthetic-sanity
# … → "deterministic recording OK: sha256:…"  then  "synthetic-sanity OK"
```

It passes iff recording is deterministic and the replay pipeline completes and guards clean. It does
**not** assert latencies (see the next section).

## Understanding the run-to-run latency noise

If you replay the **same bundle against the same version twice**, the per-op latencies still wobble
— on a laptop with Docker Desktop, empirically ~±5–13%. This is **environment noise** (CPU-frequency
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

| Recipe | Purpose |
| --- | --- |
| `just synthetic-record <name> [flags]` | Record a workload bundle **offline** into `recordings/<name>/`. |
| `just synthetic-replay <name> <endpoint> [-- flags]` | Load the recorded graph + measure the recorded commands. |
| `just synthetic-compare-versions <name> <endpointA> <endpointB>` | Replay one bundle against two versions + guard. |
| `just synthetic-sanity` | Self-contained tool sanity (deterministic record + replay + guard). |

The `just synthetic-baseline` / `synthetic-compare` recipes remain as a **single-version** local
latency tracker built on Criterion; they regenerate per run and are not the cross-version
comparator (Criterion adapts its iteration count to observed latency, so a faster/slower version
would run a different command sequence). Use record / replay for comparing versions.

See the [README](../readme.md) for the operation catalog and `synthetic run` details.
