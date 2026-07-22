[![Cargo Build & Test](https://github.com/FalkorDB/benchmark/actions/workflows/ci.yml/badge.svg)](https://github.com/FalkorDB/benchmark/actions/workflows/ci.yml)
[![Code Coverage](https://github.com/FalkorDB/benchmark/actions/workflows/coverage.yml/badge.svg)](https://github.com/FalkorDB/benchmark/actions/workflows/coverage.yml)
[![codecov](https://codecov.io/gh/FalkorDB/benchmark/graph/badge.svg)](https://codecov.io/gh/FalkorDB/benchmark)
[![License](https://img.shields.io/github/license/falkordb/benchmark.svg)](https://github.com/falkordb/benchmark/blob/master/LICENSE)
[![Discord](https://img.shields.io/discord/1146782921294884966.svg?style=social&logo=discord)](https://discord.com/invite/99y2Ubh6tg)
[![Twitter](https://img.shields.io/twitter/follow/falkordb?style=social)](https://twitter.com/falkordb)

## See the benchmarks ([Click here](https://benchmark.falkordb.com/))
# Key Benchmark Takeaways
## Navigation
- [About the benchmarks](#about-the-benchmarks)
- [System Requirements](#system-requirements)
  - [Prerequisites](#prerequisites)
  - [Installation Steps](#installation-steps)
- [Development](#development)
- [Run the benchmark](#run-the-benchmark)
  - [Run via helper scripts](#run-via-helper-scripts)
  - [CLI workflow](#cli-workflow)
  - [Multi-vendor reports](#multi-vendor-runs-and-per-vendor-comparison-reports-ui)
  - [Per-query latency tracking](#per-query-latency-tracking-for-the-single-view)
  - [Query explanations and samples](#query-explanations-and-samples)
  - [Simulation mode](#run-simulation-to-see-that-the-benchmark-itself-can-sustain-specific-mps-given-a-fixed-latency-on-that-hardware)
- [Data](#data)
- [FAQ](#faq)
- [Grafana and Prometheus](#grafana-and-prometheus)

Get mission-critical performance even under extreme workloads, with response times staying under 140ms at p99, while
competitors struggle with multi-second latencies. Reduce infrastructure costs and improve user experience with
FalkorDB's superior performance profile, requiring fewer resources to handle peak workloads.

| Percentile       | FalkorDB (ms) | Neo4j (ms) | Performance Difference |
|------------------|---------------|------------|------------------------|
| **p50 (median)** | 55.0          | 577.5      | 10.5x faster           |
| **p90**          | 108.0         | 4784.1     | 44.3x faster           |
| **p99**          | 136.2         | 46923.8    | 344.5x faster          |

## About the benchmarks

This benchmark provides comprehensive performance comparisons between FalkorDB and Neo4j graph databases. This benchmark
specifically focuses on aggregate expansion operations, a common workload in graph database applications. The results
indicate FalkorDB's particular strength in maintaining consistent performance under varying workload conditions,
especially crucial for production environments where predictable response times are essential.

## System Requirements

### Prerequisites

- Ubuntu
- Redis server
- build-essential, cmake, m4, automake
- libtool, autoconf, python3
- libomp-dev, libssl-dev
- pkg-config
- Rust toolchain
- SDKman
- unzip, zip

Installation Steps
==================

#### install redis server

```bash
sudo apt-get install lsb-release curl gpg
curl -fsSL https://packages.redis.io/gpg | sudo gpg --dearmor -o /usr/share/keyrings/redis-archive-keyring.gpg
sudo chmod 644 /usr/share/keyrings/redis-archive-keyring.gpg
echo "deb [signed-by=/usr/share/keyrings/redis-archive-keyring.gpg] https://packages.redis.io/deb $(lsb_release -cs) main" | sudo tee /etc/apt/sources.list.d/redis.list
sudo apt-get update
sudo apt-get install redis
```

- stop the redis server `sudo systemctl stop redis-server`
- disable the redis server `sudo systemctl disable redis-server`
- check the redis server status `sudo systemctl status redis-server`

#### install sdkman

- install unzip `sudo apt install unzip zip -y`
- `curl -s "https://get.sdkman.io" | bash`
- load sdkman in the current shell `source "$HOME/.sdkman/bin/sdkman-init.sh"`

#### build falkordb from source

- `git clone --recurse-submodules -j8 https://github.com/FalkorDB/FalkorDB.git`
- `sudo apt install build-essential cmake m4 automake libtool autoconf python3 libomp-dev libssl-dev`
- install rust `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- from FalkorDB root dir run `make`

#### build the benchmark from source

from `~/`

- install pkg-config `sudo apt install pkg-config -y`
- `git clone git@github.com:FalkorDB/benchmark.git`
- `cd benchmark`
- `sdk env install`
- download and unpack neo4j `./scripts/download-neo4j.sh`
- build the benchmark `cargo build --release`
- enable autocomplete `source <(./target/release/benchmark generate-auto-complete bash)`
- copy the falkor shared lib to `cp ~/FalkorDB/bin/linux-x64-release/src/falkordb.so .`

## Development

Automation for this repo is driven by [`just`](https://github.com/casey/just) — run `just --list`
to see every recipe. CI installs `just` and runs these same recipes, so whatever CI checks you can
reproduce locally with the identical command.

Install `just` (`cargo install just` or `brew install just`) and, for coverage,
[`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov) (`cargo install cargo-llvm-cov`).
Building the Rust crate also needs `protoc` (`sudo apt-get install -y protobuf-compiler` or
`brew install protobuf`).

### Build, lint and test

```bash
just build            # build all targets/features
just clippy           # strict clippy (warnings denied)
just test             # run the unit + integration test suite
just test-one query_builder  # run a single test by name filter
just ci               # everything the Rust CI runs: build + clippy + test
```

### Documentation checks

Markdown docs are validated in CI (the `Docs validation` workflow) and locally with the same recipe:

```bash
just doc-check        # all doc checks: links + shell examples
just doc-links        # offline broken-link + anchor check (lychee) over tracked *.md
just doc-shell        # bash -n syntax-check the shell (bash/sh) examples in the docs
```

`just doc-links` runs [`lychee`](https://github.com/lycheeverse/lychee) in `--offline` mode, so it
verifies relative and same-file anchor links without network access (external URLs are skipped to
keep CI stable). It needs the `lychee` binary on your `PATH` — install it locally with
`cargo install lychee` or `brew install lychee` (CI installs it automatically via
`taiki-e/install-action`). Rust code blocks in the docs are compiled as doctests by `just test`
(via `src/doc_examples.rs`). Because doctests are run as well as compiled, fence an example that
should type-check but not execute with `rust,no_run`, and one that should not compile at all with
`rust,ignore` (or a non-Rust language) so it is skipped.

### Code coverage

```bash
just coverage-local   # spin up a Docker FalkorDB, generate codecov.json, tear it down
just coverage         # generate codecov.json via cargo-llvm-cov (needs a reachable FalkorDB)
just coverage-html    # open a browsable HTML coverage report (needs a reachable FalkorDB)
```

`just coverage` runs the unit tests **and** the `#[ignore]`d integration tests
(`--include-ignored`) so server-backed code is measured, so it needs a reachable FalkorDB — set
`FALKORDB_HOST`/`FALKORDB_PORT`, or use `just coverage-local` to spin one up in Docker. The
`coverage` CI job provides a FalkorDB service container.

Coverage is uploaded to [Codecov](https://codecov.io/gh/FalkorDB/benchmark) by the `coverage`
workflow. Please cover new code with tests and keep coverage high — **patch coverage must stay
≥ 90%** (enforced by `codecov.yml`).

### Synthetic per-operation benchmark (experimental)

Measures a curated suite of **read and write operations in isolation** — one at a time, selectable —
capturing on every invocation both the **server time** (FalkorDB's reported internal execution time)
and the **total time** (end-to-end client round-trip), then summarizing them with severe-outlier
removal (Tukey fences, like Criterion.rs) and writing a JSON report with one block per operation. For
each operation it sweeps a list of **concurrency levels**, so the report traces how latency
(including the p99 tail) changes as achieved throughput rises — the latency-vs-throughput curve and
its saturation "knee". This is Part 5 of a larger tool (see the design epic
[#200](https://github.com/FalkorDB/benchmark/issues/200)); it can **generate its own reproducible
dataset**, sweep concurrency, and measure **write operations** with steady-state isolation (see
[Write operations](#write-operations-steady-state-isolation) below).

Each operation is measured under two plan-cache conditions so you can see the cost of expression
**compilation** separately from execution:

- **cached** — the plan is reused (warm cache), so only execution is measured;
- **uncached** — every invocation is forced to miss the plan cache (a unique query-text token), so
  it recompiles each time, exposing compilation cost.

`compilation_ms ≈ uncached − cached` server time. (FalkorDB's `CACHE_SIZE` can't be set to `0` and
is load-time only, so the uncached condition is produced client-side and verified via the response's
`cached_execution` flag; the server's actual `CACHE_SIZE` is recorded in the report.) Use `--cache
cached|uncached|both` (default `both`).

#### Concurrency sweep (latency vs throughput)

Every operation is measured at each level of a configurable **concurrency sweep** (`--concurrency`,
default `1,2,4,8,16,32`). Each level `C` runs a **closed-loop** engine: `C` worker tasks, each with
its own dedicated connection, fire one query, await it to completion (row draining included), then
immediately fire the next — so there are at most `C` requests in flight (one outstanding per active
worker). After a discarded
warm-up window, every worker measures in a shared window; the level reports the pooled latency
percentiles (p50/p90/p95/p99) and the **achieved throughput** (`completed ÷ window`, ops/sec).

Because a new request is issued only after the previous one *completes*, the reported throughput is
**achieved, not offered** — it can never exceed the server's own service rate. The measured
latencies therefore describe behaviour *at that achieved rate*: a closed loop does not model a fixed
external arrival rate, so it neither reproduces nor corrects for
[coordinated omission](https://www.scylladb.com/2021/04/22/on-coordinated-omission/) — quantifying
the tail under a target offered load needs open-loop / arrival-rate testing (future work). Read the
curve by following latency as `C` (and throughput) rise: throughput climbs until the server
saturates, after which extra concurrency mostly inflates the tail — the highest-throughput level is
flagged as the `<- knee`. A single-level sweep (`--concurrency 1`) reproduces the classic
single-connection latency measurement plus its achieved throughput.

See [`synthetic-benchmark.md`](synthetic-benchmark.md) for the concurrency model, the report schema
(`operations[].levels[]`), and how to read the curve in depth.

#### Operation catalog

Select operations with `--op <name>` (repeatable and comma-separated) or `--all-reads`. `--all-reads`
selects every **read** op; **write** ops (`create_node`, `merge_miss`, `create_edge`, `set_property`,
`delete_node`, `merge_hit`) are opt-in via `--op` so a
sweep never mutates a graph unless you ask. All read ops except `return_const` target the benchmark's
`:User {id, age}` / `(:User)-[:Friend]->(:User)` schema (with an index on `:User(id)`). Most draw
their parameters from `:User` ids sampled out of
`--graph` (`shortest_path` needs two, `match_by_index`/`expand_*`/`aggregate_*`/`property_projection`
one); `return_const` and `match_by_label_scan` need no seed ids (they vary a constant / a scan
modulus). Write ops need no seed ids either — they target their own scratch namespace (see
[Write operations](#write-operations-steady-state-isolation)). Either point `--graph` at a graph that
already holds that schema, or let the tool **generate a reproducible one** (see
[Generating a dataset](#generating-a-reproducible-dataset) below). Every query projects **scalars**
(never whole nodes) and is parameterized; the corpus is seeded (`--seed`) so the same seed yields an
identical corpus.

| Operation | What it measures | Cypher (body) |
|---|---|---|
| `return_const` | round-trip / parse+exec baseline (no dataset) | `RETURN $i AS x` |
| `match_by_index` | point lookup on the `:User(id)` index | `MATCH (n:User {id: $id}) RETURN n.id` |
| `match_by_label_scan` | full `:User` label scan (non-indexable predicate) | `MATCH (n:User) WHERE n.id % $modulus = 0 RETURN count(n) AS c` |
| `expand_1_hop` | 1-hop `:Friend` expansion | `MATCH (s:User {id: $id})-[:Friend]->(n:User) RETURN n.id` |
| `expand_hops_5` | fixed 5-hop `:Friend` expansion | `MATCH (s:User {id: $id})-[:Friend*5..5]->(n:User) RETURN DISTINCT n.id LIMIT 100` |
| `aggregate_count` | count a node's 1-hop neighbours | `MATCH (s:User {id: $id})-[:Friend]->(n:User) RETURN count(n) AS c` |
| `aggregate_group` | group neighbours by age with counts | `MATCH (s:User {id: $id})-[:Friend]->(n:User) RETURN n.age AS age, count(*) AS c ORDER BY c DESC LIMIT 10` |
| `shortest_path` | bounded shortest `:Friend` path between two nodes | `MATCH (s:User {id: $from}), (t:User {id: $to}) WITH shortestPath((s)-[:Friend*1..6]->(t)) AS p RETURN coalesce(length(p), -1) AS len` |
| `property_projection` | project scalar properties of an indexed node | `MATCH (n:User {id: $id}) RETURN n.id, n.age` |
| `create_node` *(write)* | create a fresh scratch node each invocation | `CREATE (n:BenchScratch_<run> {id: $id}) RETURN n.id` |
| `merge_miss` *(write)* | `MERGE` a fresh scratch node (always misses → creates) | `MERGE (n:BenchScratch_<run> {id: $id}) RETURN n.id` |
| `create_edge` *(write)* | create a fresh edge between two scratch nodes | `MATCH (a:BenchScratch_<run> {id: $src}), (b:BenchScratch_<run> {id: $dst}) CREATE (a)-[:BenchEdge]->(b)` |
| `set_property` *(write)* | set one property on a pre-created scratch node | `MATCH (n:BenchScratch_<run> {id: $id}) WHERE n.touched IS NULL SET n.touched = $id` |
| `delete_node` *(write)* | delete a pre-created scratch node | `MATCH (n:BenchScratch_<run> {id: $id}) DELETE n` |
| `merge_hit` *(write)* | `MERGE` an existing scratch node (always hits) | `MERGE (n:BenchScratch_<run> {id: $id}) RETURN n.id` |

Because `OpName` is a clap `ValueEnum`, `--op <TAB>` completes the operation names once you've
installed completion (`benchmark generate-auto-complete <shell>`), and `just synthetic-ops` (or
`benchmark synthetic list-ops`) prints the catalog.

Set up and run, start to end:

```bash
# 1. start a FalkorDB server (use a tagged image, not :edge, for meaningful version numbers)
docker run -d --rm -p 6379:6379 falkordb/falkordb:latest

# 2. build the tool (needs protoc; see Prerequisites)
just build

# 3. list the available operations
just synthetic-ops

# 4. run the probe over several ops (records the exact server image so results are reproducible)
IMAGE=$(docker inspect --format '{{index .RepoDigests 0}}' falkordb/falkordb:latest)
just synthetic-bench --endpoint falkor://127.0.0.1:6379 --graph main \
    --op match_by_index,expand_1_hop,aggregate_count --samples 500 --warmup 100 \
    --concurrency 1,4,16,32 --cache both --seed 42 --server-image "$IMAGE" \
    --out synthetic-report.json
# ...or measure the whole read catalog at once:
just synthetic-bench --graph main --all-reads --samples 500
# ...or sweep a single operation (uses the default 1,2,4,8,16,32 concurrency sweep):
just synthetic-bench-one match_by_index
```

Sample output (one block per selected op; one table row per concurrency level, per cache mode):

```text
synthetic benchmark — endpoint falkor://127.0.0.1:6379  graph main  samples 500  warmup 100  concurrency [1,4,16,32]  seed 42  connection pool(size=1) per worker
server — falkordb module ver 4.20.1  redis 8.6.3  CACHE_SIZE 25
server image: falkordb/falkordb@sha256:9042fdc4...
client host — bench-01 · Linux 6.8 Ubuntu 24.04 · Intel(R) Xeon(R) (8c/16t) · 32.0 GiB · x86_64

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

expand_1_hop
  ...
report written to synthetic-report.json
markdown written to synthetic-report.md
```

Alongside the JSON, the tool writes a **PR-pasteable Markdown report** (`<out>.md`, e.g.
`synthetic-report.md`) — a metadata table plus the same per-op latency-vs-throughput tables, ready
to drop into a pull request.

The report's `meta.server` block records the FalkorDB module version, `redis_version`/`build_id`,
`CACHE_SIZE`, and the operator-supplied `--server-image` (FalkorDB does not expose a graph-module
git SHA to clients, so the image digest is the reproducible build identity). A `:edge` image reports
a `999999` placeholder version and the tool warns you to use a tagged image for comparisons.
`meta.host` records the **client** machine that ran the probe (OS, CPU, cores, memory, arch, via
`sysinfo`); the hostname is kept in the JSON/console but omitted from the Markdown.

#### Write operations (steady-state isolation)

Write ops measure mutation latency/throughput without the graph drifting between samples, so write
numbers stay comparable across a long sweep. Six ops are available: `create_node`, `merge_miss`,
`create_edge`, `set_property`, `delete_node`, and `merge_hit`. Only the operation itself is inside
the timer; setup/reset/cleanup run in **untimed** hooks that abort the sample on failure. The
isolation model:

- **Scratch namespace.** A run writes only to a **run-unique label** `BenchScratch_<run_token>` (a
  random per-run hex nonce), so a sweep never touches your real data or another run's scratch. The
  label is shared by all workers of a run (keeping the plan cache warm), and the run's scratch is
  dropped (`DETACH DELETE`, edges included) on a fresh connection after each level — even if the
  level errored — so nothing leaks into the next op/level.
- **Disjoint per-worker keys.** At concurrency `C`, worker `w` owns the key band
  `[w·reset_every, (w+1)·reset_every − 1]` and uses `window_key = w·reset_every + (seq mod
  reset_every)` for invocation `seq`. Bands never overlap, so concurrent writers never collide, and
  within a window every key is unique — so `merge_miss` always misses (creates), `delete_node`
  deletes each node exactly once, `create_edge` never duplicates an edge, and identities never
  repeat. Keys are **run-independent** (only the label carries the nonce), so the workload stays
  comparable across runs; `(w+1)·reset_every` must fit `i32` (FalkorDB params), which bounds
  `reset_every × C`.
- **Empty- vs populated-band ops.** `create_node`/`merge_miss` keep their band **empty** (they
  create into it). The ops that need existing targets — `create_edge`, `set_property`, `delete_node`,
  `merge_hit` — keep their band **populated**: an untimed setup pre-creates `reset_every` nodes (one
  per key) so every invocation has a target.
- **Run-level reset (sawtooth).** Every `reset_every` operations — counted over the **global**
  warm-up + measured sequence — each worker runs an untimed reset before its band is reused, bounding
  write drift to one sawtooth window. Empty-band ops clear the band; populated-band ops clear **and
  refill** it with `reset_every` fresh clean nodes (so `delete_node` gets its nodes back and
  `set_property` always writes a brand-new property). `merge_hit` never mutates, so its band is set
  up once and its reset is a no-op. Tune the cadence with `--reset-every N` (config `reset_every`,
  default 50000); a smaller `N` keeps the graph tighter but spends more time in setup/reset.
- **Per-sample verification.** Each write checks FalkorDB's mutation counters against the operation's
  intent (`create_node`/`merge_miss` ⇒ one node created; `create_edge` ⇒ one relationship;
  `set_property` ⇒ one property set; `delete_node` ⇒ one node deleted; `merge_hit` ⇒ **no** mutation
  — a pure match), so a silent no-op fails loudly rather than producing a fast, misleading sample.
  `set_property`'s `WHERE n.touched IS NULL` makes this self-checking: a broken reset would match
  nothing and fail verification instead of silently measuring a redundant write.

**Limitations.**
- **Steady state, not a fixed arrival rate.** The reset produces a sawtooth in graph size within each
  window; it bounds drift, it does not eliminate it.
- **Reset is untimed but not free.** At high `C` a worker's reset (which can delete/recreate a whole
  window of nodes) contends with other workers on the server and counts toward achieved throughput,
  so keep `reset_every` modest for write sweeps.
- **Scratch lookups are unindexed.** The populated-band ops (and `merge_miss`) match on the
  run-unique label without an index, so they include a label-scoped lookup cost; a coordinated
  run-level index is a possible future refinement.

```bash
# measure the write ops over a concurrency sweep, resetting each worker's scratch every 5k ops
just synthetic-bench --graph bench \
    --op create_node,merge_miss,create_edge,set_property,delete_node,merge_hit \
    --concurrency 1,8,32 --reset-every 5000 --samples 500
```

#### Generating a reproducible dataset

Instead of measuring whatever graph the endpoint already holds, the tool can **generate its own**
seeded `:User {id, age}` / `(:User)-[:Friend]->(:User)` graph so results are controlled and
comparable across runs, machines and FalkorDB versions. Pass `--generate` with `--nodes`/`--edges`:

```bash
just synthetic-bench --graph bench --generate --nodes 100000 --edges 1000000 \
    --op match_by_index,expand_hops_5,aggregate_count --samples 500 --seed 42
```

- `--generate` is **destructive**: it drops and rewrites `--graph` (so it's opt-in on the CLI and is
  never authorized by a config file alone). It creates the `:User(id)` index, then bulk-loads the
  nodes and edges via `UNWIND` batches.
- The graph is generated deterministically from `--seed`: `edges` must be `≥ nodes` (a ring backbone
  guarantees connectivity for expansions/shortest paths); `edges` counts relationships. The same
  seed + knobs reproduce the exact same graph and the same operation corpora everywhere.
- When a dataset is generated, the report's `meta.dataset` records `{seed, nodes, edges,
  workload_hash}`. **`workload_hash`** (`sha256:…`) is a stable fingerprint of the whole workload —
  the dataset knobs, the selected operations (in order) and their query bodies, and the sampled input
  pools. **Only compare runs whose `workload_hash` matches**; a different hash means a different
  workload. (For an externally-supplied graph the tool can't fingerprint the data, so no
  `workload_hash` is emitted. Older reports used the field name `corpus_hash`, still accepted on
  read.)

#### Config file (`synthetic-bench.toml`)

For a growing knob set you can put the configuration in a `synthetic-bench.toml` file (auto-detected
in the working directory, or pass `--config <path>`). Any CLI flag **overrides** the file, which in
turn overrides the built-in defaults; generation still requires the explicit `--generate` flag.

```toml
# synthetic-bench.toml
seed = 42
nodes = 100000
edges = 1000000
operations = ["match_by_index", "expand_hops_5", "aggregate_count"]
samples = 500
concurrency = [1, 4, 16, 32]   # closed-loop worker counts to sweep (default 1,2,4,8,16,32)
cache = "both"           # cached | uncached | both
reset_every = 50000      # write-op scratch reset cadence (ops per sawtooth window); read ops ignore it
# endpoint / graph / warmup / server_timeout_ms / client_deadline_ms / out are all optional
```

```bash
just synthetic-bench --generate     # reads synthetic-bench.toml, builds the dataset, runs the ops
```

Unknown keys and misspelled operation names are rejected with a clear error, and operation names use
the same spelling as `--op` (e.g. `expand_1_hop`).

To run the integration test against a live server:

```bash
just synthetic-it     # uses FALKORDB_HOST/FALKORDB_PORT, default 127.0.0.1:6379
```

#### Record / run / report: the same workload across versions (recommended)

For a rigorous comparison of two FalkorDB versions, **record the workload once** — the dataset
load-script *and* the measured commands — then **run that identical bundle** against each version and
**diff the reports**. Unlike the Criterion baselines below (which regenerate the graph and re-derive
the commands each run), `run --recording` loads the recorded graph and measures the recorded commands
through the closed-loop engine (the full concurrency sweep + cached/uncached modes), so the only
variable is the FalkorDB version. See the full walkthrough in the
[synthetic benchmark tutorial](docs/synthetic-benchmark-tutorial.md), and task-oriented recipes in
the [synthetic benchmark cookbook](docs/synthetic-benchmark-cookbook.md).

```bash
# 1. record a bundle OFFLINE (no server) into recordings/demo/
just synthetic-record demo --graph tutorial_demo \
  --op match_by_index,expand_1_hop,aggregate_count --seed 42 --nodes 1000 --edges 5000
# 2. compare version A (:6379) vs version B (:6380) on that identical bundle
just synthetic-compare-versions demo falkor://127.0.0.1:6379 falkor://127.0.0.1:6380
```

- **`just synthetic-record <name> [flags]`** writes `recordings/<name>/` = `manifest.json` +
  `graph.jsonl` (load statements) + `commands/<op>.jsonl`, plus a length-framed **`workload_hash`**
  over the graph *and* the commands (so any later edit is detected on load). It is **offline** — a
  pure function of the seed + knobs — and reads `synthetic-bench.toml` for defaults. Select ops with
  `--op <names>`, or **`--op all`** (or `--op '*'`) for every read operation.
- **`benchmark synthetic run --recording <dir> [--concurrency … --cache …]`** drops + loads +
  **count-verifies** the recorded graph, then measures the recorded commands across the concurrency
  sweep + cache modes, writing a report plus a per-op **`result_digest`** (a hash of the result
  values). It also **verifies results are identical at the highest concurrency** (an untimed
  concurrent pass) so a wrong result under concurrency is a hard fail. `--no-load` skips the reload
  for a load-once / run-many flow (still count-verifying first). `just synthetic-replay <name>
  <endpoint>` wraps this.
- **`benchmark synthetic report --diff <A.json> <B.json> [--out diff.md]`** **guards** the pair (it
  aborts unless the `workload_hash` **and** every op's `result_digest` match, so a version returning
  wrong/empty results faster can't masquerade as an improvement — the version difference itself is
  expected and recorded), then writes a **Markdown diff** across every op × cache-mode × concurrency
  level (throughput + total-latency p50/p90/p95/p99 with deltas). `just synthetic-compare-versions`
  runs `run --recording` against both endpoints then `report --diff`.
- **`just synthetic-sanity`** self-checks the tool: it records the same workload twice (asserting an
  identical `workload_hash` — deterministic recording), then `run --recording` at C=1,4 + `report
  --diff` (incl. the C>1 result verification) against a throwaway Docker FalkorDB. Latency is not
  asserted (it is environment-dependent noise — see the tutorial).
- **`just synthetic-verify`** is the CI **non-divergence gate**: it records **all** read ops
  (`--op all`, medium dataset) and runs `run --recording` **twice** against the same throwaway
  FalkorDB across the full concurrency sweep + both cache modes, failing if `report --diff` finds a
  different `workload_hash` or any per-op result digest — i.e. the two runs on one machine must not
  diverge. Latency is not asserted.
- `recordings/` is git-ignored (regenerable bundles).

#### Version-comparison baselines (Criterion, C=1)

To track a **read** operation's latency **between FalkorDB versions**, save a
[Criterion](https://github.com/bheisler/criterion.rs) C=1 single-flight baseline on one version and
compare against it on another. The workload (dataset + operations) comes from `synthetic-bench.toml`,
so both runs measure exactly the same thing — and `synthetic-compare` **guards** that with the
`workload_hash` before it will compare, refusing to put mismatched workloads side by side. (This path
regenerates per run; for a rigorous cross-version comparison prefer **record / replay** above.)

```bash
# on FalkorDB version A (needs a synthetic-bench.toml with nodes/edges/operations)
just synthetic-baseline v4.2.1
# ...upgrade FalkorDB, then on version B:
just synthetic-compare v4.2.1
```

```text
⚠ server image changed: falkordb@sha256:aaa… → falkordb@sha256:bbb…
baseline guard: OK — same workload, safe to compare
synthetic/match_by_index/total_ms
    time:   [297 µs 303 µs 310 µs]
    change: [-9.4% -8.1% -6.7%] (p = 0.00 < 0.05)   Performance has improved.
```

How it works:

- **`just synthetic-baseline <name>`** (re)generates the dataset from `synthetic-bench.toml`, captures
  that run's `workload_hash` + FalkorDB module version into `baselines/<name>.json`, then saves the
  Criterion baseline `<name>` (single-flight C=1 read latencies + browsable HTML plots under
  `target/criterion/`).
- **`just synthetic-compare <name>`** captures the current run's identity, runs the **guard**
  (`benchmark synthetic report --diff`) — which **aborts** if the `workload_hash` differs (or is
  absent, i.e. an external, unfingerprintable graph) — then runs Criterion against the saved baseline.
- The **FalkorDB version is the subject** of the comparison, so a version change is *recorded and
  displayed*, never a reason to abort; the guard only warns when the two versions are identical (no
  delta to measure) or the dev `999999` placeholder (use tagged images). The **workload** is the hard
  gate. Baselines therefore require a **generated** dataset (so the workload is fingerprintable);
  write ops are out of scope (their per-invocation reset lifecycle doesn't fit Criterion's model).
- `synthetic-bench.toml` and `baselines/` are git-ignored (per-user config + local baselines).

### UI dashboard (`ui/`)

```bash
just ui-install       # npm ci
just ui-lint          # lint
just ui-build         # production build
just ui-dev           # start the dev server
just ui-smoke         # Playwright smoke test (starts its own dev server)
```

#### run the benchmark
##### run via helper scripts

Use the wrapper scripts in `scripts/` for the fastest end-to-end benchmark activation:

- `scripts/run_small_benchmark.sh`
- `scripts/run_medium_benchmark.sh`
- `scripts/run_large_benchmark.sh`

Each script handles the full pipeline for its dataset size:
1. clears and loads enabled vendors
2. generates vendor-specific query files
3. runs benchmark workloads
4. writes results into a shared `RESULTS_DIR`
5. aggregates UI-ready summaries

Quick start:

```bash
./scripts/run_small_benchmark.sh
./scripts/run_medium_benchmark.sh
./scripts/run_large_benchmark.sh
```

Run only Falkor primary + secondary comparison:

```bash
RUN_FALKOR=1 RUN_FALKOR_2=1 RUN_NEO4J=0 RUN_MEMGRAPH=0 ./scripts/run_medium_benchmark.sh
```

Override workload shape:

```bash
QUERIES_COUNT=25000 WRITE_RATIO=0.05 PARALLEL=10 MPS=3000 ./scripts/run_medium_benchmark.sh
```

Point wrappers to external endpoints:

```bash
FALKOR_ENDPOINT=falkor://127.0.0.1:6379 \
FALKOR_ENDPOINT_2=falkor://127.0.0.1:6800 \
NEO4J_ENDPOINT=neo4j://127.0.0.1:7687 \
MEMGRAPH_ENDPOINT=bolt://127.0.0.1:17687 \
./scripts/run_small_benchmark.sh
```

Common environment knobs:
- vendor toggles: `RUN_FALKOR`, `RUN_FALKOR_2`, `RUN_NEO4J`, `RUN_MEMGRAPH`
- workload controls: `BATCH_SIZE`, `PARALLEL`, `MPS`, `QUERIES_COUNT`, `WRITE_RATIO`, `QUERIES_FILE`
- algorithm toggles: `ENABLE_ALGO_PAGERANK`, `ENABLE_ALGO_MAX_FLOW`, `ENABLE_ALGO_MSF`, `ENABLE_ALGO_HARMONIC`
- output folder: `RESULTS_DIR`
- Falkor timeout tuning (medium/large wrappers): `FALKOR_QUERY_TIMEOUT_MS`

##### CLI workflow

##### Generate the docker compose for prometheus and grafana

```bash
./generate_docker_compose.sh
```

##### Run the docker compose

```bash
docker-compose up
```

The benchmark is a cli tool that can be used to run the benchmarks

```text
➜  cargo run  --bin benchmark -- --help                                                                  git:(prometheus|✚7…3
    
Usage: benchmark <COMMAND>

Commands:
  generate-auto-complete
  load                    load data into the database
  generate-queries        generate a set of queries and store them in a file to be used with the run command
  run                     run the queries generated by the GenerateQueries command against the chosen vendor
  help                    Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

##### load the data

- `cargo run --release --bin benchmark -- load --vendor falkor -s small`
- `cargo run --release --bin benchmark -- load --vendor neo4j -s small`
- `cargo run --release --bin benchmark -- load --vendor memgraph -s small`

NOTE: It is possible to use the load command with externally run vendor endpoint:
- `cargo run --release --bin benchmark -- load --vendor falkor -s small --endpoint falkor://127.0.0.1:6379`
- `cargo run --release --bin benchmark -- load --vendor neo4j -s small --endpoint neo4j://neo4j:benchmark123@127.0.0.1:7687`
- `cargo run --release --bin benchmark -- load --vendor memgraph -s small --endpoint bolt://127.0.0.1:7687`

Profile-aware loading (runs additional fixture/index setup when required):
- `cargo run --release --bin benchmark -- load --vendor neo4j -s small --query-profile fixture-dependent`
- `cargo run --release --bin benchmark -- load --vendor memgraph -s small --query-profile fixture-dependent`
- `cargo run --release --bin benchmark -- load --vendor falkor -s small --query-profile fixture-dependent`

##### create a set of queries to be used with the run command

-

`cargo run --release --bin benchmark -- generate-queries  -s10000000 --dataset small --name=small-readonly --write-ratio 0.0`

NOTE: preparing a smaller run of 1,000,000 queries:

`cargo run --release --bin benchmark -- generate-queries  -s1000000 --dataset small --name=small-readonly --write-ratio 0.0`

Generate with a broader coverage profile:

- `cargo run --release --bin benchmark -- generate-queries -s1000000 --dataset small --name=small-extended --write-ratio 0.0 --vendor neo4j --query-profile extended-core`
- `cargo run --release --bin benchmark -- generate-queries -s1000000 --dataset small --name=small-fixtures --write-ratio 0.0 --vendor memgraph --query-profile fixture-dependent`

##### run the benchmarks

- `cargo run --release --bin benchmark run --vendor falkor --name small-readonly -p40 --mps 4000`
- `cargo run --release --bin benchmark run --vendor neo4j --name small-readonly -p40 --mps 4000`
- `cargo run --release --bin benchmark run --vendor memgraph --name small-readonly -p40 --mps 4000`

NOTE: It is possible to use the run command externally run vendor endpoint:
- `cargo run --release --bin benchmark run --vendor falkor --name small-readonly -p40 --mps 4000 --endpoint falkor://127.0.0.1:6379`
- `cargo run --release --bin benchmark run --vendor neo4j --name small-readonly -p40 --mps 4000 --endpoint neo4j://neo4j:benchmark123@127.0.0.1:7687`
- `cargo run --release --bin benchmark run --vendor memgraph --name small-readonly -p40 --mps 4000 --endpoint bolt://127.0.0.1:7687`

##### multi-vendor runs and per-vendor comparison reports (UI)

The benchmark is designed to run the same workload against multiple vendors and then generate a **pairwise comparison report**.

1) Run each vendor into the same results directory (so it contains `Results-.../<vendor>/{meta.json,metrics.prom}`):

- `cargo run --release --bin benchmark -- run --vendor falkor --name small-readonly -p40 --mps 4000 --results-dir Results-YYMMDD-HH:MM`
- `cargo run --release --bin benchmark -- run --vendor neo4j --name small-readonly -p40 --mps 4000 --results-dir Results-YYMMDD-HH:MM`
- `cargo run --release --bin benchmark -- run --vendor memgraph --name small-readonly -p40 --mps 4000 --results-dir Results-YYMMDD-HH:MM`

2) Aggregate into UI-ready JSON summaries:

- `cargo run --release --bin benchmark -- aggregate --results-dir Results-YYMMDD-HH:MM --out-dir ui/public/summaries`

This produces:

- `ui/public/summaries/neo4j_vs_falkordb.json`
- `ui/public/summaries/memgraph_vs_falkordb.json`

AWS instance comparisons (e.g. Graviton vs Intel for FalkorDB runs stored under `aws-tests/`):

- `cargo run --release --bin benchmark -- aggregate-aws-tests --aws-tests-dir aws-tests --out-path ui/public/summaries/aws_tests_falkor_graviton_vs_intel.json`

3) Open the UI:

- `cd ui && npm install && npm run dev`

The comparison pages load only the relevant vendor pair:

- `/neo4j` compares Neo4j vs FalkorDB
- `/memgraph` compares Memgraph vs FalkorDB

##### per-query latency tracking (for the "single" view)

Workloads generated by `generate-queries` embed a stable `q_id` and a query catalog (mapping id -> query name). During `run`, the benchmark exports per-query latency percentiles (P10..P99) into `metrics.prom` and the aggregator emits them under `result.histogram_for_type`.

Important: if you change the query set/metrics, regenerate the workload file before running:

- `cargo run --release --bin benchmark -- generate-queries --dataset small -s1000000 --name small-readonly --write-ratio 0.0`

##### helper script

For convenience wrappers that load data, regenerate queries, run workloads, and aggregate UI summaries, see:

- `scripts/run_small_benchmark.sh`
- `scripts/run_medium_benchmark.sh`
- `scripts/run_large_benchmark.sh`

##### query explanations and samples

For the maintained query catalog guide (including phase-1 additions and sample Cypher), see:

- `QUERY_EXPLANATIONS_AND_SAMPLES.md`

##### run simulation to see that the benchmark itself can sustain specific mps given a fixed latency on that hardware

For example, simulate 40 clients that send at 5000 messages per seconds with latency of one millisecond per call.

- `cargo run --release --bin benchmark run --vendor falkor --name small -p40 --mps 5000 --simulate 1`

### Data

The data is based on https://www.kaggle.com/datasets/wolfram77/graphs-snap-soc-pokec
licensed: https://creativecommons.org/licenses/by/4.0/

## FAQ

### System Requirements

**Q: What are the minimum system requirements?**  
A: FalkorDB requires a Linux/Unix system with 4GB RAM minimum. For production environments, 16GB RAM is recommended.

### Installation & Setup

**Q: Can I run FalkorDB without Redis?**  
A: No, FalkorDB requires Redis 6.2 or higher as it operates as a Redis module.

### Development

**Q: Which query language does FalkorDB use?**  
A: FalkorDB uses the Cypher query language, similar to Neo4j, making migration straightforward.

### Data Management

**Q: Does FalkorDB support data persistence?**  
A: Yes, through Redis persistence mechanisms (RDB/AOF). Additional persistence options are in development.

### Integration

**Q: Does FalkorDB support common programming languages?**  
A: Yes, through FalkorDB has set of clients in all these programming langauges and more
see [official clients](https://docs.falkordb.com/clients.html)

### Production Use

**Q: Is FalkorDB production-ready?**  
A: Yes, FalkorDB is stable for production use, being a continuation of the battle-tested RedisGraph codebase.

### Troubleshooting

**Q: What should I do if I get "libgomp.so.1: cannot open shared object file"?**  
A: Install OpenMP:

- Ubuntu: `apt-get install libgomp1`
- RHEL/CentOS: `yum install libgomp`
- OSX: `brew install libomp`

### Migration

**Q: Can I migrate from Neo4j to FalkorDB?**  
A: Yes, FalkorDB supports the Cypher query language, making migration from Neo4j straightforward. Migration tools are in
development.

### Grafana and Prometheus

- Accessing grafana http://localhost:3000
- Accessing prometheus http://localhost:9090
- sum by (vendor, spawn_id)  (rate(operations_total{vendor="falkor"}[1m]))
  redis
- rate(redis_commands_processed_total{instance=~"redis-exporter:9121"}[1m])
- redis_connected_clients{instance=~"redis-exporter:9121"}
- topk(5, irate(redis_commands_total{instance=~"redis-exporter:9121"} [1m]))
- redis_blocked_clients
- redis_commands_total
- redis_commands_failed_calls_total
- redis_commands_latencies_usec_count
- redis_commands_rejected_calls_total
- redis_io_threaded_reads_processed
- redis_io_threaded_writes_processed
- redis_io_threads_active
- redis_memory_max_bytes
- redis_memory_used_bytes
- redis_memory_used_peak_bytes
- redis_memory_used_vm_total
- redis_process_id
  =======


