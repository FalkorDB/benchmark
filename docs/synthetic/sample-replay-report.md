# Synthetic per-operation benchmark

| field | value |
|---|---|
| tool | v0.1.0 |
| endpoint / graph | `falkor://127.0.0.1:6379` / `tutorial_demo` |
| FalkorDB module | 4.20.1 |
| redis | 8.6.3 |
| CACHE_SIZE | 25 |
| client host | macOS 26.5.2 Tahoe · Apple M1 Pro (10c/10t) · 16.0 GiB · arm64 |
| samples / warmup | 500 / 100 |
| concurrency | 1, 4 |
| cache seed | 42 |
| connection | pool(size=1) per worker |
| dataset | seed 42 · 1000 nodes · 5000 edges |
| corpus_hash | `sha256:57ec47dffa81b5be2d39fba9b634ea93e79020e2a0ba9f578692d72b351c7606` |

## `aggregate_count`

_cached — plan reused, execution only_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2598 | 0.080 / 0.096 / 0.102 / 0.117 | 0.362 / 0.423 / 0.445 / 0.492 | 0.0 |  |
| 4 | 8171 | 0.092 / 0.137 / 0.153 / 0.180 | 0.474 / 0.569 / 0.595 / 0.664 | 0.0 | ⬅ knee |

_uncached — plan-cache miss each run, execution + compilation_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2319 | 0.132 / 0.155 / 0.163 / 0.179 | 0.416 / 0.466 / 0.491 / 0.524 | 100.0 |  |
| 4 | 7135 | 0.171 / 0.236 / 0.263 / 0.320 | 0.545 / 0.652 / 0.682 / 0.761 | 100.0 | ⬅ knee |

compilation_ms (median uncached − cached server time):

| C | compilation_ms |
|---:|---:|
| 1 | 0.052 |
| 4 | 0.079 |

## `expand_1_hop`

_cached — plan reused, execution only_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2498 | 0.073 / 0.087 / 0.094 / 0.104 | 0.379 / 0.429 / 0.446 / 0.507 | 0.0 |  |
| 4 | 7590 | 0.086 / 0.130 / 0.148 / 0.184 | 0.495 / 0.625 / 0.673 / 0.756 | 0.0 | ⬅ knee |

_uncached — plan-cache miss each run, execution + compilation_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2241 | 0.122 / 0.143 / 0.154 / 0.171 | 0.431 / 0.477 / 0.508 / 0.552 | 100.0 |  |
| 4 | 6899 | 0.157 / 0.218 / 0.242 / 0.293 | 0.556 / 0.671 / 0.712 / 0.803 | 100.0 | ⬅ knee |

compilation_ms (median uncached − cached server time):

| C | compilation_ms |
|---:|---:|
| 1 | 0.049 |
| 4 | 0.072 |

## `match_by_index`

_cached — plan reused, execution only_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2973 | 0.036 / 0.043 / 0.046 / 0.049 | 0.324 / 0.373 / 0.389 / 0.414 | 0.0 |  |
| 4 | 8747 | 0.043 / 0.079 / 0.092 / 0.115 | 0.430 / 0.550 / 0.592 / 0.722 | 0.0 | ⬅ knee |

_uncached — plan-cache miss each run, execution + compilation_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2483 | 0.066 / 0.080 / 0.085 / 0.097 | 0.392 / 0.433 / 0.445 / 0.483 | 100.0 |  |
| 4 | 8221 | 0.088 / 0.138 / 0.154 / 0.188 | 0.468 / 0.571 / 0.600 / 0.662 | 100.0 | ⬅ knee |

compilation_ms (median uncached − cached server time):

| C | compilation_ms |
|---:|---:|
| 1 | 0.030 |
| 4 | 0.045 |
