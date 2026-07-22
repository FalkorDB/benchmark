# Synthetic benchmark diff — A → B

| field | A (baseline) | B (candidate) |
|---|---|---|
| FalkorDB module | 4.20.1 | 4.20.1 |
| server image | — | — |
| endpoint / graph | `falkor://127.0.0.1:6379` / `tutorial_demo` | `falkor://127.0.0.1:6379` / `tutorial_demo` |
| workload_hash | `sha256:57638f31c17b7be582ca3c15f7e3c4ad050189e4200f5af478f6711658dd5eb5` | `sha256:57638f31c17b7be582ca3c15f7e3c4ad050189e4200f5af478f6711658dd5eb5` |
| samples / warmup | 500 / 100 | 500 / 100 |

_Δ is 100·(B−A)/A. **Latency: lower is better** (a positive Δ = slower / regressed); **throughput: higher is better**. `—` = not measured in that run._

> ⚠ baseline and candidate ran the same FalkorDB module version (4.20.1) — there is no version delta to measure

## `aggregate_count`

_cached (plan reused — execution only)_

| C | A total p50/p90/p95/p99 (ms) | B total p50/p90/p95/p99 (ms) | Δp50 | A tput (ops/s) | B tput (ops/s) | Δtput |
|---:|---|---|---:|---:|---:|---:|
| 1 | 0.357 / 0.384 / 0.395 / 0.431 | 0.357 / 0.383 / 0.397 / 0.421 | +0.2% | 2719 | 2718 | -0.0% |
| 4 | 0.464 / 0.570 / 0.601 / 0.686 | 0.485 / 0.617 / 0.667 / 0.779 | +4.5% | 8276 | 7745 | -6.4% |

_uncached (forced plan-cache miss — execution + compilation)_

| C | A total p50/p90/p95/p99 (ms) | B total p50/p90/p95/p99 (ms) | Δp50 | A tput (ops/s) | B tput (ops/s) | Δtput |
|---:|---|---|---:|---:|---:|---:|
| 1 | 0.404 / 0.428 / 0.437 / 0.452 | 0.412 / 0.440 / 0.452 / 0.475 | +2.0% | 2444 | 2396 | -1.9% |
| 4 | 0.528 / 0.623 / 0.655 / 0.719 | 0.540 / 0.638 / 0.673 / 0.742 | +2.2% | 7329 | 7222 | -1.5% |

## `expand_1_hop`

_cached (plan reused — execution only)_

| C | A total p50/p90/p95/p99 (ms) | B total p50/p90/p95/p99 (ms) | Δp50 | A tput (ops/s) | B tput (ops/s) | Δtput |
|---:|---|---|---:|---:|---:|---:|
| 1 | 0.359 / 0.392 / 0.403 / 0.427 | 0.407 / 0.470 / 0.504 / 0.574 | +13.4% | 2739 | 2351 | -14.2% |
| 4 | 0.476 / 0.574 / 0.612 / 0.678 | 0.520 / 0.662 / 0.718 / 0.825 | +9.4% | 8090 | 7226 | -10.7% |

_uncached (forced plan-cache miss — execution + compilation)_

| C | A total p50/p90/p95/p99 (ms) | B total p50/p90/p95/p99 (ms) | Δp50 | A tput (ops/s) | B tput (ops/s) | Δtput |
|---:|---|---|---:|---:|---:|---:|
| 1 | 0.406 / 0.446 / 0.461 / 0.495 | 0.471 / 0.551 / 0.595 / 0.652 | +16.0% | 2373 | 2029 | -14.5% |
| 4 | 0.549 / 0.665 / 0.707 / 0.806 | 0.559 / 0.680 / 0.724 / 0.803 | +1.8% | 6994 | 6874 | -1.7% |

## `match_by_index`

_cached (plan reused — execution only)_

| C | A total p50/p90/p95/p99 (ms) | B total p50/p90/p95/p99 (ms) | Δp50 | A tput (ops/s) | B tput (ops/s) | Δtput |
|---:|---|---|---:|---:|---:|---:|
| 1 | 0.310 / 0.336 / 0.344 / 0.362 | 0.313 / 0.339 / 0.350 / 0.370 | +0.9% | 3182 | 3147 | -1.1% |
| 4 | 0.420 / 0.514 / 0.535 / 0.604 | 0.442 / 0.547 / 0.583 / 0.657 | +5.2% | 9229 | 8616 | -6.6% |

_uncached (forced plan-cache miss — execution + compilation)_

| C | A total p50/p90/p95/p99 (ms) | B total p50/p90/p95/p99 (ms) | Δp50 | A tput (ops/s) | B tput (ops/s) | Δtput |
|---:|---|---|---:|---:|---:|---:|
| 1 | 0.344 / 0.378 / 0.396 / 0.418 | 0.358 / 0.416 / 0.433 / 0.497 | +4.3% | 2766 | 2638 | -4.6% |
| 4 | 0.461 / 0.574 / 0.619 / 0.709 | 0.480 / 0.602 / 0.655 / 0.783 | +4.2% | 8277 | 7829 | -5.4% |
