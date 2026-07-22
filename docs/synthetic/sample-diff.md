# Synthetic benchmark diff — A → B

| field | A (baseline) | B (candidate) |
|---|---|---|
| FalkorDB module | 4.20.1 | 4.20.1 |
| server image | — | — |
| endpoint / graph | `falkor://127.0.0.1:6379` / `tutorial_demo` | `falkor://127.0.0.1:6379` / `tutorial_demo` |
| workload_hash | `sha256:57ec47dffa81b5be2d39fba9b634ea93e79020e2a0ba9f578692d72b351c7606` | `sha256:57ec47dffa81b5be2d39fba9b634ea93e79020e2a0ba9f578692d72b351c7606` |
| samples / warmup | 500 / 100 | 500 / 100 |

_Δ is 100·(B−A)/A. **Latency: lower is better** (a positive Δ = slower / regressed); **throughput: higher is better**. `—` = not measured in that run._

> ⚠ baseline and candidate ran the same FalkorDB module version (4.20.1) — there is no version delta to measure

## `aggregate_count`

_cached (plan reused — execution only)_

| C | A total p50/p90/p95/p99 (ms) | B total p50/p90/p95/p99 (ms) | Δp50 | A tput (ops/s) | B tput (ops/s) | Δtput |
|---:|---|---|---:|---:|---:|---:|
| 1 | 0.362 / 0.423 / 0.445 / 0.492 | 0.366 / 0.408 / 0.424 / 0.455 | +1.1% | 2598 | 2651 | +2.0% |
| 4 | 0.474 / 0.569 / 0.595 / 0.664 | 0.480 / 0.574 / 0.604 / 0.673 | +1.1% | 8171 | 8096 | -0.9% |

_uncached (forced plan-cache miss — execution + compilation)_

| C | A total p50/p90/p95/p99 (ms) | B total p50/p90/p95/p99 (ms) | Δp50 | A tput (ops/s) | B tput (ops/s) | Δtput |
|---:|---|---|---:|---:|---:|---:|
| 1 | 0.416 / 0.466 / 0.491 / 0.524 | 0.414 / 0.457 / 0.475 / 0.520 | -0.4% | 2319 | 2334 | +0.7% |
| 4 | 0.545 / 0.652 / 0.682 / 0.761 | 0.534 / 0.640 / 0.682 / 0.758 | -2.0% | 7135 | 7151 | +0.2% |

## `expand_1_hop`

_cached (plan reused — execution only)_

| C | A total p50/p90/p95/p99 (ms) | B total p50/p90/p95/p99 (ms) | Δp50 | A tput (ops/s) | B tput (ops/s) | Δtput |
|---:|---|---|---:|---:|---:|---:|
| 1 | 0.379 / 0.429 / 0.446 / 0.507 | 0.380 / 0.425 / 0.442 / 0.491 | +0.2% | 2498 | 2550 | +2.1% |
| 4 | 0.495 / 0.625 / 0.673 / 0.756 | 0.492 / 0.618 / 0.664 / 0.783 | -0.5% | 7590 | 7710 | +1.6% |

_uncached (forced plan-cache miss — execution + compilation)_

| C | A total p50/p90/p95/p99 (ms) | B total p50/p90/p95/p99 (ms) | Δp50 | A tput (ops/s) | B tput (ops/s) | Δtput |
|---:|---|---|---:|---:|---:|---:|
| 1 | 0.431 / 0.477 / 0.508 / 0.552 | 0.432 / 0.493 / 0.522 / 0.556 | +0.1% | 2241 | 2245 | +0.2% |
| 4 | 0.556 / 0.671 / 0.712 / 0.803 | 0.553 / 0.661 / 0.692 / 0.759 | -0.5% | 6899 | 7009 | +1.6% |

## `match_by_index`

_cached (plan reused — execution only)_

| C | A total p50/p90/p95/p99 (ms) | B total p50/p90/p95/p99 (ms) | Δp50 | A tput (ops/s) | B tput (ops/s) | Δtput |
|---:|---|---|---:|---:|---:|---:|
| 1 | 0.324 / 0.373 / 0.389 / 0.414 | 0.311 / 0.390 / 0.429 / 0.534 | -4.0% | 2973 | 3072 | +3.3% |
| 4 | 0.430 / 0.550 / 0.592 / 0.722 | 0.430 / 0.552 / 0.605 / 0.734 | +0.1% | 8747 | 8656 | -1.0% |

_uncached (forced plan-cache miss — execution + compilation)_

| C | A total p50/p90/p95/p99 (ms) | B total p50/p90/p95/p99 (ms) | Δp50 | A tput (ops/s) | B tput (ops/s) | Δtput |
|---:|---|---|---:|---:|---:|---:|
| 1 | 0.392 / 0.433 / 0.445 / 0.483 | 0.348 / 0.388 / 0.409 / 0.446 | -11.1% | 2483 | 2775 | +11.8% |
| 4 | 0.468 / 0.571 / 0.600 / 0.662 | 0.463 / 0.572 / 0.602 / 0.671 | -1.0% | 8221 | 8319 | +1.2% |
