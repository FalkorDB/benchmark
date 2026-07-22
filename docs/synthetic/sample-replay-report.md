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
| 1 | 2805 | 0.077 / 0.081 / 0.084 / 0.088 | 0.353 / 0.375 / 0.382 / 0.405 | 0.0 |  |
| 4 | 8281 | 0.091 / 0.132 / 0.148 / 0.179 | 0.462 / 0.555 / 0.586 / 0.664 | 0.0 | ⬅ knee |

_uncached — plan-cache miss each run, execution + compilation_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2388 | 0.126 / 0.141 / 0.144 / 0.151 | 0.407 / 0.433 / 0.440 / 0.460 | 100.0 |  |
| 4 | 7268 | 0.163 / 0.222 / 0.242 / 0.284 | 0.530 / 0.642 / 0.679 / 0.754 | 100.0 | ⬅ knee |

compilation_ms (median uncached − cached server time):

| C | compilation_ms |
|---:|---:|
| 1 | 0.049 |
| 4 | 0.071 |

## `expand_1_hop`

_cached — plan reused, execution only_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2688 | 0.071 / 0.075 / 0.076 / 0.083 | 0.367 / 0.400 / 0.413 / 0.431 | 0.0 |  |
| 4 | 8034 | 0.084 / 0.124 / 0.140 / 0.168 | 0.480 / 0.584 / 0.624 / 0.686 | 0.0 | ⬅ knee |

_uncached — plan-cache miss each run, execution + compilation_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2280 | 0.119 / 0.136 / 0.143 / 0.153 | 0.423 / 0.462 / 0.482 / 0.528 | 100.0 |  |
| 4 | 6917 | 0.156 / 0.218 / 0.241 / 0.281 | 0.550 / 0.667 / 0.713 / 0.827 | 100.0 | ⬅ knee |

compilation_ms (median uncached − cached server time):

| C | compilation_ms |
|---:|---:|
| 1 | 0.048 |
| 4 | 0.071 |

## `match_by_index`

_cached — plan reused, execution only_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2830 | 0.035 / 0.037 / 0.039 / 0.040 | 0.348 / 0.373 / 0.380 / 0.392 | 0.0 |  |
| 4 | 9246 | 0.042 / 0.072 / 0.087 / 0.106 | 0.414 / 0.507 / 0.539 / 0.595 | 0.0 | ⬅ knee |

_uncached — plan-cache miss each run, execution + compilation_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2473 | 0.064 / 0.076 / 0.081 / 0.087 | 0.387 / 0.417 / 0.436 / 0.463 | 100.0 |  |
| 4 | 8362 | 0.084 / 0.123 / 0.140 / 0.169 | 0.456 / 0.564 / 0.607 / 0.701 | 100.0 | ⬅ knee |

compilation_ms (median uncached − cached server time):

| C | compilation_ms |
|---:|---:|
| 1 | 0.029 |
| 4 | 0.041 |
