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
just test-one <name>  # run a single test by name filter
just ci               # everything the Rust CI runs: build + clippy + test
```

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

Measures a curated suite of **read operations in isolation** — one at a time, selectable — capturing
on every invocation both the **server time** (FalkorDB's reported internal execution time) and the
**total time** (end-to-end client round-trip), then summarizing them with severe-outlier removal
(Tukey fences, like Criterion.rs) and writing a JSON report with one block per operation. It uses a
single dedicated connection for honest single-flight latency. This is Part 2 of a larger tool (see
[`synthetic-benchmark.md`](synthetic-benchmark.md)); later parts add a generated synthetic dataset,
a concurrency sweep, and write operations.

Each operation is measured under two plan-cache conditions so you can see the cost of expression
**compilation** separately from execution:

- **cached** — the plan is reused (warm cache), so only execution is measured;
- **uncached** — every invocation is forced to miss the plan cache (a unique query-text token), so
  it recompiles each time, exposing compilation cost.

`compilation_ms ≈ uncached − cached` server time. (FalkorDB's `CACHE_SIZE` can't be set to `0` and
is load-time only, so the uncached condition is produced client-side and verified via the response's
`cached_execution` flag; the server's actual `CACHE_SIZE` is recorded in the report.) Use `--cache
cached|uncached|both` (default `both`).

#### Operation catalog

Select operations with `--op <name>` (repeatable and comma-separated) or `--all-reads`. All read
ops except `return_const` target the benchmark's `:User {id, age}` / `(:User)-[:Friend]->(:User)`
schema (with an index on `:User(id)`). Most draw their parameters from `:User` ids sampled out of
`--graph` (`shortest_path` needs two, `match_by_index`/`expand_*`/`aggregate_*`/`property_projection`
one); `return_const` and `match_by_label_scan` need no seed ids (they vary a constant / a scan
modulus). A reproducible generated dataset arrives in Part 3, so for now point `--graph` at a graph
that already holds that schema. Every query projects **scalars** (never whole nodes) and is
parameterized; the corpus is seeded (`--seed`) so the same seed yields an identical corpus.

| Operation | What it measures | Cypher (body) |
|---|---|---|
| `return_const` | round-trip / parse+exec baseline (no dataset) | `RETURN $i AS x` |
| `match_by_index` | point lookup on the `:User(id)` index | `MATCH (n:User {id: $id}) RETURN n.id` |
| `match_by_label_scan` | full `:User` label scan (non-indexable predicate) | `MATCH (n:User) WHERE n.id % $modulus = 0 RETURN count(n) AS c` |
| `expand_1_hop` | 1-hop `:Friend` expansion | `MATCH (s:User {id: $id})-[:Friend]->(n:User) RETURN n.id` |
| `expand_hops_5` | fixed 5-hop `:Friend` expansion | `MATCH (s:User {id: $id})-[:Friend*5..5]->(n:User) RETURN DISTINCT n.id LIMIT 100` |
| `aggregate_count` | count a node's 1-hop neighbours | `MATCH (s:User {id: $id})-[:Friend]->(n:User) RETURN count(n) AS c` |
| `aggregate_group` | group neighbours by age with counts | `MATCH (s:User {id: $id})-[:Friend]->(n:User) RETURN n.age AS age, count(*) AS c ORDER BY c DESC LIMIT 10` |
| `shortest_path` | bounded shortest `:Friend` path between two nodes | `MATCH (s:User {id: $from}),(t:User {id: $to}) WITH shortestPath((s)-[:Friend*1..6]->(t)) AS p RETURN coalesce(length(p), -1) AS len` |
| `property_projection` | project scalar properties of an indexed node | `MATCH (n:User {id: $id}) RETURN n.id, n.age` |

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
    --cache both --seed 42 --server-image "$IMAGE" --out synthetic-report.json
# ...or measure the whole read catalog at once:
just synthetic-bench --graph main --all-reads --samples 500
```

Sample output (one block per selected op):

```text
synthetic benchmark — endpoint falkor://127.0.0.1:6379  graph main  samples 500  warmup 100  seed 42  connection pool(size=1)
server — falkordb module ver 4.20.1  redis 8.6.3  CACHE_SIZE 25
server image: falkordb/falkordb@sha256:9042fdc4...

match_by_index
  [cached — plan reused, execution only]
    server_ms  median 0.033  mean 0.034  p99 0.059  (n=487, removed 13)
    total_ms   median 0.427  mean 0.440  p99 0.596  (n=487, removed 13)
    non_internal_ms (paired total-server)  median 0.394
    cached_execution=false: 0.0%  (unknown 0)
  [uncached — plan-cache miss each run, execution + compilation]
    server_ms  median 0.058  mean 0.063  p99 0.093  (n=497, removed 3)
    cached_execution=false: 100.0%  (unknown 0)
  compilation_ms (median uncached-cached server time)  0.025

expand_1_hop
  ...
report written to synthetic-report.json
```

The report's `meta.server` block records the FalkorDB module version, `redis_version`/`build_id`,
`CACHE_SIZE`, and the operator-supplied `--server-image` (FalkorDB does not expose a graph-module
git SHA to clients, so the image digest is the reproducible build identity). A `:edge` image reports
a `999999` placeholder version and the tool warns you to use a tagged image for comparisons.

To run the integration test against a live server:

```bash
just synthetic-it     # uses FALKORDB_HOST/FALKORDB_PORT, default 127.0.0.1:6379
```

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

```bash
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


