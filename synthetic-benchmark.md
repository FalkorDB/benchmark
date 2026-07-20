# Synthetic benchmark — concurrency sweep (latency vs throughput)

Companion documentation for the synthetic per-operation benchmark's **concurrency sweep engine**
(Part 4 of the design epic [#200](https://github.com/FalkorDB/benchmark/issues/200)). For the
operation catalog, the reproducible dataset generator, and the plan-cache (cached-vs-uncached)
comparison, see the [Synthetic per-operation benchmark section of the README](readme.md#synthetic-per-operation-benchmark-experimental).

## What & why

The headline capability is showing, **per operation, how latency (including the p99 tail) changes as
achieved throughput rises** — the latency-vs-throughput curve and its saturation "knee" — by
sweeping a configurable concurrency level `C`.

## How it works

Each operation is measured at every level of a configurable **concurrency sweep** (`--concurrency`,
default `1,2,4,8,16,32`). A level `C` runs a **closed-loop** engine:

- `C` worker tasks, each owning its **own dedicated connection** (an independent single-socket
  client — not a clone of one handle, which would share a schema cache).
- Each worker fires one query, **awaits it to completion** (row draining included), then immediately
  fires the next from its pre-generated sequence — so there are at most **`C` requests in flight**
  (one outstanding request per active worker; the exactly-`C` test asserts the *maximum* observed
  concurrency equals `C`).
- After a discarded **warm-up** window, all workers cross a barrier together so the measurement
  window opens with every worker active; each records when its first measured invocation *started*
  and its last one *completed*.
- The level's window is `max(last_completed) − min(first_started)`, and the **achieved throughput**
  is `completed ÷ window` (ops/sec). Latency percentiles (p50/p90/p95/p99) are pooled across all
  workers, after severe-outlier removal.

### Achieved vs offered throughput (coordinated-omission caveat)

Because a new request is issued only after the previous one *completes*, the reported throughput is
**achieved, not offered** — it can never exceed the server's own service rate. The measured
latencies therefore describe behaviour **at that achieved rate**: a closed loop does **not** model a
fixed external arrival rate, so it neither reproduces nor corrects for
[coordinated omission](https://www.scylladb.com/2021/04/22/on-coordinated-omission/). Quantifying the
tail under a target offered load needs open-loop / arrival-rate testing, which is **out of scope**
(future work), along with writes.

### How to read the curve

Follow latency as `C` (and throughput) rise: throughput climbs until the server saturates, after
which extra concurrency mostly inflates the tail. The highest-throughput level is flagged with
`<- knee` in the console table. A single-level sweep (`--concurrency 1`) reproduces the classic
single-connection latency measurement plus its achieved throughput.

### Cache modes per level

Every level is still measured under **both** plan-cache conditions, with the derived compilation
cost — the epic's standing mandate:

- **cached** — the plan is reused (warm cache) → execution only;
- **uncached** — a unique per-invocation query-text token forces a plan-cache miss every run →
  execution + compilation.

`compilation_ms ≈ uncached − cached` server time, computed **per level** (so you can see how
compilation cost itself behaves under concurrency). Uncached query text stays globally unique across
the whole sweep (every worker at every level claims a disjoint block of invocation ids), so no two
invocations are ever served from a previous one's cached plan.

## Usage

Sweep one operation over the default curve, or pass explicit levels:

```bash
# one operation over the default 1,2,4,8,16,32 sweep
just synthetic-bench-one match_by_index

# explicit levels + more flags forwarded to the probe
just synthetic-bench-one match_by_index -- --concurrency 1,4,16,32 --samples 500

# the general recipe also takes --concurrency
just synthetic-bench --graph main --op match_by_index --concurrency 1,4,16,32 --samples 500
```

Or set it in the config file (`synthetic-bench.toml`; any CLI flag overrides it):

```toml
concurrency = [1, 4, 16, 32]   # closed-loop worker counts to sweep (default 1,2,4,8,16,32)
```

## Console output

One block per operation; one table row per concurrency level, per cache mode; the knee is flagged:

```text
match_by_index
  [cached — plan reused, execution only]
    C    throughput(ops/s)   server p50/p90/p95/p99             total p50/p90/p95/p99              miss%
    1              2950     0.081 / 0.130 / 0.150 / 0.200     0.330 / 0.470 / 0.500 / 0.900     0.0
    4             11800     0.090 / 0.160 / 0.170 / 0.260     0.340 / 0.500 / 0.520 / 1.100     0.0
   16             30400     0.180 / 0.700 / 0.900 / 1.900     0.600 / 1.800 / 2.100 / 4.200     0.0
   32             41200     0.350 / 1.400 / 1.700 / 3.400     0.780 / 3.400 / 3.900 / 7.900     0.0  <- knee
  [uncached — plan-cache miss each run, execution + compilation]
    C    throughput(ops/s)   server p50/p90/p95/p99             total p50/p90/p95/p99              miss%
    1              2100     0.106 / 0.160 / 0.180 / 0.240     0.360 / 0.500 / 0.540 / 0.960   100.0
   ...
  compilation_ms (median uncached-cached server time) by level:
    C=1    0.025
    C=32   0.040
```

## Report schema

The JSON report groups each operation's measurements by concurrency level. Abridged example (one
level shown; `levels[]` has one entry per swept concurrency, and each `metrics.*_ms` summary carries
the full `min`/`mean`/`median`/`p90`/`p95`/`p99`/`max`/`stddev` set):

```json
{
  "schema_version": 2,
  "meta": { "concurrency": [1, 4, 16, 32] },
  "operations": {
    "match_by_index": {
      "levels": [
        {
          "concurrency": 1,
          "cached": {
            "throughput_ops_per_sec": 2950.0,
            "metrics": {
              "server_ms": { "n": 487, "removed": 13, "median": 0.081, "p90": 0.130, "p95": 0.150, "p99": 0.200 },
              "total_ms": { "median": 0.330, "p95": 0.500, "p99": 0.900 },
              "non_internal_ms": { "median": 0.249 },
              "cached_false_rate": 0.0,
              "cached_unknown": 0
            }
          },
          "uncached": {
            "throughput_ops_per_sec": 2100.0,
            "metrics": {
              "server_ms": { "median": 0.106, "p95": 0.180, "p99": 0.240 },
              "cached_false_rate": 1.0,
              "cached_unknown": 0
            }
          },
          "compilation_ms_median": 0.025
        }
      ]
    }
  }
}
```

The top-level report also records `schema_version` (`2` since Part 4) and `meta.concurrency` (the
sweep that was run).

> [!NOTE]
> The Part 4 report shape is a **breaking change** from earlier versions: an operation's stats moved
> from top-level `cached`/`uncached` fields to `levels[]`. A pre-Part-4 (v1) report that contains
> operation data will not deserialize into the new shape, so tooling should read `schema_version`
> from the raw JSON first and migrate on it.

## What the tests guarantee

- **Peak concurrency reaches `C`** — an instrumented fake worker asserts the observed *maximum*
  in-flight count equals `C` (a barrier makes it deterministic, not scheduler-dependent), confirming
  the closed loop actually saturates all `C` workers even though the steady-state invariant is *at
  most* `C`.
- **Throughput math** — with a fake fixed-latency op under a paused clock, `throughput ≈ C / latency`.
- **Warm-up excluded & pooled** — measured samples pool across workers with warm-up invocations
  omitted.
- **Fail-fast** — a worker error *or* panic aborts the level and surfaces the error rather than
  reporting a partial, misleading throughput (no deadlock on the warm-up barrier).
- **Integration (docker)** — a real sweep over `[1, 4, 8]` asserts every level reports positive
  throughput with an ordered p50 ≤ p90 ≤ p95 ≤ p99, and that throughput rises above the `C=1`
  baseline.
