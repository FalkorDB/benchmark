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
| 1 | 2672 | 0.079 / 0.095 / 0.101 / 0.110 | 0.366 / 0.412 / 0.433 / 0.459 | 0.0 |  |
| 4 | 8043 | 0.092 / 0.135 / 0.154 / 0.180 | 0.470 / 0.573 / 0.617 / 0.703 | 0.0 | ⬅ knee |

_uncached — plan-cache miss each run, execution + compilation_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2315 | 0.131 / 0.157 / 0.164 / 0.183 | 0.417 / 0.471 / 0.497 / 0.532 | 100.0 |  |
| 4 | 7242 | 0.166 / 0.221 / 0.244 / 0.285 | 0.534 / 0.641 / 0.675 / 0.745 | 100.0 | ⬅ knee |

compilation_ms (median uncached − cached server time):

| C | compilation_ms |
|---:|---:|
| 1 | 0.052 |
| 4 | 0.074 |

## `expand_1_hop`

_cached — plan reused, execution only_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2593 | 0.073 / 0.086 / 0.092 / 0.101 | 0.376 / 0.422 / 0.441 / 0.477 | 0.0 |  |
| 4 | 7596 | 0.087 / 0.131 / 0.151 / 0.181 | 0.498 / 0.621 / 0.662 / 0.774 | 0.0 | ⬅ knee |

_uncached — plan-cache miss each run, execution + compilation_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2268 | 0.120 / 0.139 / 0.149 / 0.163 | 0.426 / 0.475 / 0.495 / 0.539 | 100.0 |  |
| 4 | 7018 | 0.157 / 0.214 / 0.236 / 0.275 | 0.555 / 0.658 / 0.693 / 0.772 | 100.0 | ⬅ knee |

compilation_ms (median uncached − cached server time):

| C | compilation_ms |
|---:|---:|
| 1 | 0.048 |
| 4 | 0.070 |

## `match_by_index`

_cached — plan reused, execution only_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2771 | 0.036 / 0.043 / 0.045 / 0.050 | 0.351 / 0.389 / 0.407 / 0.430 | 0.0 |  |
| 4 | 8675 | 0.042 / 0.075 / 0.088 / 0.102 | 0.438 / 0.557 / 0.604 / 0.683 | 0.0 | ⬅ knee |

_uncached — plan-cache miss each run, execution + compilation_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2434 | 0.070 / 0.088 / 0.093 / 0.106 | 0.399 / 0.444 / 0.463 / 0.498 | 100.0 |  |
| 4 | 8369 | 0.087 / 0.139 / 0.157 / 0.189 | 0.463 / 0.565 / 0.602 / 0.665 | 100.0 | ⬅ knee |

compilation_ms (median uncached − cached server time):

| C | compilation_ms |
|---:|---:|
| 1 | 0.034 |
| 4 | 0.045 |
