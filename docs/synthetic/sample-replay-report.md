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
| workload_hash | `sha256:57ec47dffa81b5be2d39fba9b634ea93e79020e2a0ba9f578692d72b351c7606` |

## `aggregate_count`

_cached — plan reused, execution only_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2759 | 0.079 / 0.094 / 0.101 / 0.111 | 0.352 / 0.397 / 0.420 / 0.445 | 0.0 |  |
| 4 | 7570 | 0.095 / 0.150 / 0.170 / 0.204 | 0.498 / 0.631 / 0.691 / 0.835 | 0.0 | ⬅ knee |

_uncached — plan-cache miss each run, execution + compilation_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2337 | 0.133 / 0.156 / 0.167 / 0.184 | 0.409 / 0.464 / 0.491 / 0.538 | 100.0 |  |
| 4 | 7190 | 0.166 / 0.218 / 0.239 / 0.278 | 0.541 / 0.645 / 0.676 / 0.746 | 100.0 | ⬅ knee |

compilation_ms (median uncached − cached server time):

| C | compilation_ms |
|---:|---:|
| 1 | 0.053 |
| 4 | 0.071 |

## `expand_1_hop`

_cached — plan reused, execution only_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2055 | 0.081 / 0.107 / 0.125 / 0.147 | 0.400 / 0.523 / 0.568 / 0.632 | 0.0 |  |
| 4 | 6503 | 0.088 / 0.131 / 0.147 / 0.179 | 0.508 / 0.658 / 0.714 / 0.863 | 0.0 | ⬅ knee |

_uncached — plan-cache miss each run, execution + compilation_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2115 | 0.130 / 0.161 / 0.175 / 0.200 | 0.457 / 0.543 / 0.571 / 0.648 | 100.0 |  |
| 4 | 6115 | 0.164 / 0.233 / 0.262 / 0.310 | 0.581 / 0.741 / 0.813 / 0.964 | 100.0 | ⬅ knee |

compilation_ms (median uncached − cached server time):

| C | compilation_ms |
|---:|---:|
| 1 | 0.049 |
| 4 | 0.075 |

## `match_by_index`

_cached — plan reused, execution only_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2635 | 0.040 / 0.058 / 0.065 / 0.073 | 0.351 / 0.451 / 0.484 / 0.573 | 0.0 |  |
| 4 | 8708 | 0.044 / 0.082 / 0.099 / 0.121 | 0.439 / 0.550 / 0.586 / 0.657 | 0.0 | ⬅ knee |

_uncached — plan-cache miss each run, execution + compilation_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2382 | 0.077 / 0.102 / 0.112 / 0.128 | 0.404 / 0.485 / 0.519 / 0.584 | 100.0 |  |
| 4 | 7861 | 0.088 / 0.140 / 0.156 / 0.198 | 0.477 / 0.609 / 0.655 / 0.764 | 100.0 | ⬅ knee |

compilation_ms (median uncached − cached server time):

| C | compilation_ms |
|---:|---:|
| 1 | 0.037 |
| 4 | 0.045 |
