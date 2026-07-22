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
| 1 | 0.352 / 0.397 / 0.420 / 0.445 | 0.367 / 0.418 / 0.439 / 0.500 | +4.1% | 2759 | 2648 | -4.0% |
| 4 | 0.498 / 0.631 / 0.691 / 0.835 | 0.478 / 0.581 / 0.615 / 0.707 | -4.0% | 7570 | 8115 | +7.2% |

_uncached (forced plan-cache miss — execution + compilation)_

| C | A total p50/p90/p95/p99 (ms) | B total p50/p90/p95/p99 (ms) | Δp50 | A tput (ops/s) | B tput (ops/s) | Δtput |
|---:|---|---|---:|---:|---:|---:|
| 1 | 0.409 / 0.464 / 0.491 / 0.538 | 0.425 / 0.487 / 0.519 / 0.571 | +4.1% | 2337 | 2261 | -3.3% |
| 4 | 0.541 / 0.645 / 0.676 / 0.746 | 0.560 / 0.689 / 0.732 / 0.860 | +3.5% | 7190 | 6690 | -6.9% |

## `expand_1_hop`

_cached (plan reused — execution only)_

| C | A total p50/p90/p95/p99 (ms) | B total p50/p90/p95/p99 (ms) | Δp50 | A tput (ops/s) | B tput (ops/s) | Δtput |
|---:|---|---|---:|---:|---:|---:|
| 1 | 0.400 / 0.523 / 0.568 / 0.632 | 0.585 / 0.906 / 1.061 / 1.400 | +46.4% | 2055 | 1458 | -29.0% |
| 4 | 0.508 / 0.658 / 0.714 / 0.863 | 0.520 / 0.675 / 0.737 / 0.858 | +2.4% | 6503 | 7119 | +9.5% |

_uncached (forced plan-cache miss — execution + compilation)_

| C | A total p50/p90/p95/p99 (ms) | B total p50/p90/p95/p99 (ms) | Δp50 | A tput (ops/s) | B tput (ops/s) | Δtput |
|---:|---|---|---:|---:|---:|---:|
| 1 | 0.457 / 0.543 / 0.571 / 0.648 | 0.562 / 0.786 / 0.855 / 1.007 | +22.9% | 2115 | 1588 | -24.9% |
| 4 | 0.581 / 0.741 / 0.813 / 0.964 | 0.572 / 0.694 / 0.742 / 0.830 | -1.6% | 6115 | 6721 | +9.9% |

## `match_by_index`

_cached (plan reused — execution only)_

| C | A total p50/p90/p95/p99 (ms) | B total p50/p90/p95/p99 (ms) | Δp50 | A tput (ops/s) | B tput (ops/s) | Δtput |
|---:|---|---|---:|---:|---:|---:|
| 1 | 0.351 / 0.451 / 0.484 / 0.573 | 0.331 / 0.420 / 0.459 / 0.508 | -5.6% | 2635 | 2870 | +8.9% |
| 4 | 0.439 / 0.550 / 0.586 / 0.657 | 0.451 / 0.578 / 0.624 / 0.733 | +2.6% | 8708 | 8132 | -6.6% |

_uncached (forced plan-cache miss — execution + compilation)_

| C | A total p50/p90/p95/p99 (ms) | B total p50/p90/p95/p99 (ms) | Δp50 | A tput (ops/s) | B tput (ops/s) | Δtput |
|---:|---|---|---:|---:|---:|---:|
| 1 | 0.404 / 0.485 / 0.519 / 0.584 | 0.372 / 0.445 / 0.486 / 0.559 | -8.0% | 2382 | 2497 | +4.9% |
| 4 | 0.477 / 0.609 / 0.655 / 0.764 | 0.535 / 0.704 / 0.781 / 0.937 | +12.1% | 7861 | 6728 | -14.4% |
