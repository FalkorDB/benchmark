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
| concurrency | 1 |
| cache seed | 42 |
| connection | pool(size=1) |
| dataset | seed 42 · 1000 nodes · 5000 edges |
| corpus_hash | `sha256:57638f31c17b7be582ca3c15f7e3c4ad050189e4200f5af478f6711658dd5eb5` |

## `aggregate_count`

_cached — plan reused, execution only_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 1778 | 0.094 / 0.123 / 0.133 / 0.158 | 0.523 / 0.668 / 0.734 / 0.823 | 0.0 | ⬅ knee |

## `expand_1_hop`

_cached — plan reused, execution only_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 1780 | 0.088 / 0.119 / 0.132 / 0.150 | 0.526 / 0.670 / 0.753 / 0.825 | 0.0 | ⬅ knee |

## `match_by_index`

_cached — plan reused, execution only_

| C | throughput (ops/s) | server p50/p90/p95/p99 (ms) | total p50/p90/p95/p99 (ms) | miss% | |
|---:|---:|---|---|---:|---|
| 1 | 2068 | 0.043 / 0.060 / 0.068 / 0.078 | 0.453 / 0.580 / 0.622 / 0.710 | 0.0 | ⬅ knee |
