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
| corpus_hash | `sha256:57638f31c17b7be582ca3c15f7e3c4ad050189e4200f5af478f6711658dd5eb5` |

## `aggregate_count`

_cached — plan reused, execution only_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2719 | 0.077 / 0.085 / 0.091 / 0.094 | 0.357 / 0.384 / 0.395 / 0.431 | 0.0 |  |
| 4 | 8276 | 0.091 / 0.127 / 0.140 / 0.165 | 0.464 / 0.570 / 0.601 / 0.686 | 0.0 | ⬅ knee |

_uncached — plan-cache miss each run, execution + compilation_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2444 | 0.125 / 0.135 / 0.138 / 0.143 | 0.404 / 0.428 / 0.437 / 0.452 | 100.0 |  |
| 4 | 7329 | 0.162 / 0.221 / 0.246 / 0.294 | 0.528 / 0.623 / 0.655 / 0.719 | 100.0 | ⬅ knee |

compilation_ms (median uncached − cached server time):

| C | compilation_ms |
|---:|---:|
| 1 | 0.048 |
| 4 | 0.071 |

## `expand_1_hop`

_cached — plan reused, execution only_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2739 | 0.071 / 0.078 / 0.082 / 0.086 | 0.359 / 0.392 / 0.403 / 0.427 | 0.0 |  |
| 4 | 8090 | 0.081 / 0.113 / 0.122 / 0.141 | 0.476 / 0.574 / 0.612 / 0.678 | 0.0 | ⬅ knee |

_uncached — plan-cache miss each run, execution + compilation_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2373 | 0.118 / 0.134 / 0.142 / 0.153 | 0.406 / 0.446 / 0.461 / 0.495 | 100.0 |  |
| 4 | 6994 | 0.152 / 0.205 / 0.228 / 0.267 | 0.549 / 0.665 / 0.707 / 0.806 | 100.0 | ⬅ knee |

compilation_ms (median uncached − cached server time):

| C | compilation_ms |
|---:|---:|
| 1 | 0.047 |
| 4 | 0.070 |

## `match_by_index`

_cached — plan reused, execution only_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 3182 | 0.036 / 0.039 / 0.041 / 0.043 | 0.310 / 0.336 / 0.344 / 0.362 | 0.0 |  |
| 4 | 9229 | 0.043 / 0.077 / 0.092 / 0.114 | 0.420 / 0.514 / 0.535 / 0.604 | 0.0 | ⬅ knee |

_uncached — plan-cache miss each run, execution + compilation_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2766 | 0.065 / 0.073 / 0.077 / 0.082 | 0.344 / 0.378 / 0.396 / 0.418 | 100.0 |  |
| 4 | 8277 | 0.084 / 0.132 / 0.152 / 0.180 | 0.461 / 0.574 / 0.619 / 0.709 | 100.0 | ⬅ knee |

compilation_ms (median uncached − cached server time):

| C | compilation_ms |
|---:|---:|
| 1 | 0.029 |
| 4 | 0.041 |
