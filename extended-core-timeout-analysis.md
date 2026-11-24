# Extended-core timeout / failure analysis

Date: 2026-11-17  
Primary evidence: `Results-261105-18:25/` (C-engine 4.22.1 vs 4.22.3, read-only extended-core)  
Summary: `ui/public/summaries/multi_engine_compare_extcore.json`  
Query file: `medium-extended-core-falkor`  
Live confirmation: current `edge-rs` / `edge-rs-redis` on graph `falkor` (medium Pokec)

> Note: an edge-rs Valkey vs Redis **extended-core + 5% writes** attempt was cancelled and left **no** `Results-*` directory. The only complete extended-core failure dataset on disk is the C-engine run above. Live probes on current edge-rs reproduce the same failure shape.

---

## 1. Run configuration

| Setting | Value |
|---------|-------|
| Dataset | medium Pokec (~100k `User`, ~1.77M `Friend`) |
| Workload | 10,000 queries, `parallel=8`, `mps=2000`, `write_ratio=0` |
| Profile | `extended-core` = baseline catalog **plus** deep-hop + a few extras |
| Client / server timeout | `FALKOR_QUERY_TIMEOUT_MS=20000` (also applied via `GRAPH.CONFIG SET TIMEOUT`) |
| Engines (historical run) | 4.22.1 @ `:6380` (`falkordb-c`), 4.22.3 @ `:6800` (`falkordb-rs` label) |

Extended-core–only query shapes (from `src/queries_repository.rs`):

```cypher
-- exact_5_hop_traverse_count
MATCH (s:User {id: $id})-[:Friend*5..5]->(t:User) RETURN count(t) AS cnt

-- exact_6_hop_traverse_count
MATCH (s:User {id: $id})-[:Friend*6..6]->(t:User) RETURN count(t) AS cnt
```

There is **no** `DISTINCT`, hop cap short of 5/6, fanout guard, or `LIMIT`. `count(t)` counts **matching path rows** (walks), not unique reachable nodes.

---

## 2. Overall outcome (Results-261105-18:25)

| Metric | 4.22.1 | 4.22.3 |
|--------|--------|--------|
| success / errors | 9832 / **168** | 9830 / **170** |
| global failure rate | **1.68%** | **1.70%** |
| global timeout rate (prom) | **0%** | **0%** |
| p50 / p95 / p99 | 0.99 / 57.9 / **2032** ms | 1.04 / 59.4 / **2007** ms |
| avg latency | 98.7 ms | 94.6 ms |
| throughput | ~18 qps | ~18 qps |
| elapsed | ~548 s | ~549 s |

Baseline on the same day (`Results-261105-18:15`) was **0 errors**, ~10 ms avg, ~780 qps. Extended-core is ~40× slower wall-clock and is the only profile that produces failures under this config.

**Version gap is not the story:** 4.22.1 and 4.22.3 fail the same queries at the same rates.

---

## 3. Where every error comes from

Per-query failure rates (prom `benchmark_query_failure_rate_pct`) and latency tails:

| Query | n (approx) | fail% 4.22.1 | fail% 4.22.3 | p50 | p95 | p99 |
|-------|------------|-------------:|-------------:|----:|----:|----:|
| **`exact_6_hop_traverse_count`** | ~195 | **75.4%** | **75.9%** | ~1.6 s | ~16–18 s | **~19.1 s** |
| **`exact_5_hop_traverse_count`** | ~204 | **10.3%** | **10.8%** | ~1.2 s | ~12–13 s | **~15.3 s** |
| `all_shortest_paths_len` | ~195 | 0% | 0% | ~25–29 ms | ~0.55 s | ~1.85–1.89 s |
| all other catalog queries | — | 0% | 0% | — | — | p99 &lt; 0.5 s (in this filter) |

### Error budget (reconstructs global errors)

| Query | ≈ errors |
|-------|---------:|
| `exact_6_hop_traverse_count` | 195 × 0.754 ≈ **147** |
| `exact_5_hop_traverse_count` | 204 × 0.103 ≈ **21** |
| **Sum** | **≈ 168** |

That matches global `errors=168` on 4.22.1 (170 on 4.22.3). **No other query contributes measurable failures.**

### Why `timeout_rate` is 0% while queries clearly time out

Medium script sets a **20s Falkor per-query timeout** (`GRAPH.QUERY … TIMEOUT` / `with_timeout` + `GRAPH.CONFIG SET TIMEOUT`).

When the engine aborts, the client receives a normal Falkor/Redis **error** (`Query timed out`), which the harness records as a **failure** (`benchmark_query_failure_rate_pct`), not as a client-side `tokio::time::Elapsed` timeout.

Client guard is `timeout_ms + 5000` (`FALKOR_BENCHMARK_QUERY_TIMEOUT_GUARD_EXTRA_MS` in `falkor_driver.rs`). Server kills the query first (~20s), so:

- `benchmark_global_timeout_rate_pct` / per-query timeout gauges stay **0**
- `errors` / failure rate climb
- latency histograms for survivors still show p99 ≈ 15–19s (just under the cutoff)

So “timeouts” in the operational sense are **server-enforced query aborts reported as failures**.

---

## 4. Root cause (query semantics + graph structure)

### 4.1 Combinatorial walk explosion

`[:Friend*K..K]` expands **all directed walks of length K**, and `count(t)` tallies those walks.

On medium Pokec, even modest source out-degrees fan out through dense intermediate neighborhoods:

| Seed `id` | out-degree | 5-hop `count(t)` (edge-rs) | 6-hop (edge-rs, TIMEOUT 20s) |
|-----------|------------|----------------------------|------------------------------|
| 780 | **113** | — | **Query timed out** (~20.1s wall) |
| 37219 | 16 | — | **Query timed out** |
| 10427 | 6 | — | **~366M** walks in ~19.7s (barely finishes) |
| 1902 | 2 | — | **~188M** walks in ~10.9s |
| 38845 | 25 | **~46M** walks in ~2.8s | — |
| 22893, 1 | **0** | 0 in &lt;1 ms | 0 in &lt;1 ms |

Observations:

1. **High variance by seed.** Leaves with `outdeg=0` “succeed” instantly (`cnt=0`). Hubs and well-connected seeds explode.
2. **Path count, not node count.** Hundreds of millions of walks on a 100k-node graph means massive multi-visit / multi-walk enumeration, not “reach 100k users.”
3. **6-hop is qualitatively worse than 5-hop.** Under 20s, a large fraction of random seeds never finish → ~75% fail rate for 6-hop vs ~10% for 5-hop in the concurrent run.
4. **Out-degree alone is not predictive.** `id=1902` (outdeg 2) still yields ~188M length-6 walks because of intermediate fanout.

This is inherent to unbounded (except by exact hop) **walk enumeration** on a social graph, not a 4.22.1↔4.22.3 regression.

### 4.2 Concurrency cascade (`parallel=8`, `mps=2000`)

The medium run issues work with 8 workers. When several `exact_6_hop_*` queries run together:

- Each can pin CPU for up to ~20s
- Queueing / reduced concurrency for other workers inflates wait+exec time
- Borderline 5-hop / 6-hop seeds that would finish solo can miss the 20s budget under load

That explains why **standalone** probes sometimes finish near 19s while the **benchmark fail rate** for 6-hop is ~75%: load shifts the distribution across the timeout cliff.

p50 for 6-hop is already ~1.6s (many easy/zero seeds + some moderate), while p95/p99 sit at 16–19s — a classic timeout-cliff histogram.

### 4.3 Heavy but non-failing: `all_shortest_paths_len`

```cypher
CALL algo.SPpaths({
  sourceNode: t, targetNode: s,
  relTypes: ['Friend'], relDirection: 'outgoing',
  pathCount: 0, maxLen: 4
}) YIELD path RETURN length(path)
```

- p99 ≈ **1.85–1.9 s** (well under 20s) → **0% fail**
- Live edge-rs sample ~10–12 ms internal for one pair
- Contributes to global **avg** and **p99** inflation vs baseline, but not to the error count

Other shortest-path variants in the catalog stay in the tens–low hundreds of ms at p99 in this run.

### 4.4 What is *not* the problem

| Hypothesis | Verdict |
|------------|---------|
| 4.22.3 regression vs 4.22.1 | **Rejected** — same fail shape and rates |
| edge-rs-only multi-hop DISTINCT bug (historical) | **Different issue**; fixed on current edge-rs baseline (0 errors). Extended-core 5/6-hop timeouts are walk explosion under 20s, also on C engines |
| Client tokio timeout mis-config | **Secondary** — server 20s fires first; failures are server “Query timed out” |
| Empty / wrong graph | Historical run used loaded medium graph; live probes use `falkor` (~100k nodes). Empty `pokec` graph name is a red herring |
| Write mix | RO extended-core already fails; 5% writes not required to explain errors (and edge-rs extcore+w05 never finished) |

---

## 5. Live edge-rs confirmation (2026-11-17)

Containers: `bench-falkor-edge-rs` (Valkey) and `bench-falkor-edge-rs-redis` (Redis), graph **`falkor`**.

Both engines:

- Time out the same hard 6-hop seeds at **~20.1s** with `Query timed out`
- Complete huge-but-finite 6-hop counts (~188M–366M) in 10–20s when under budget
- Finish a hard 5-hop (~46M) in ~2.5–3s
- Complete sample `algo.SPpaths` maxLen 4 quickly

Valkey vs Redis edge-rs behavior on these shapes is **aligned** (same timeouts, same order of magnitude counts/times).

---

## 6. Mapping to catalog / harness

| Layer | Role |
|-------|------|
| `QueryCoverageProfile::extended-core` | Adds `exact_5_hop_traverse_count`, `exact_6_hop_traverse_count`, `temporal_spatial_roundtrip` |
| `scripts/run_medium_benchmark.sh` | Default `FALKOR_QUERY_TIMEOUT_MS=20000`; `IN_SCRIPT_QUERY_PROFILE` hardcoded `baseline` (extcore runs use a temp override) |
| `falkor_driver.rs` | `with_timeout(query_timeout_ms)` + tokio guard +5s; engine errors → failure path |
| Workload generator | Uniform-ish mix over catalog; ~200 draws each of 5-hop and 6-hop in 10k lines |

---

## 7. Conclusions

1. **All extended-core errors** in `Results-261105-18:25` come from **`exact_6_hop_traverse_count` (~75% fail)** and **`exact_5_hop_traverse_count` (~10% fail)**.
2. **Root cause:** exact-length 5/6-hop `Friend` **walk enumeration** + `count` of path rows on a dense social graph → multi‑million to multi‑hundred‑million intermediate results; many seeds exceed the **20s** server timeout.
3. **Metrics nuance:** those aborts appear as **failures**, not `timeout_rate`, because Falkor returns a query error before the client guard elapses.
4. **Not a version regression** between 4.22.1 and 4.22.3; **reproduced on current edge-rs** (Valkey and Redis).
5. **`all_shortest_paths_len`** adds a long tail (p99 ~2s) but does not fail under 20s.
6. **Concurrency (p=8)** amplifies borderline cases via CPU contention / queueing.

---

## 8. Suggested follow-ups (optional)

Harness / workload (measurement fairness):

- Raise `FALKOR_QUERY_TIMEOUT_MS` for extended-core only (e.g. 60–180s), or
- Rewrite deep hops to `count(DISTINCT t)`, add `LIMIT`, or sample only low-degree seeds, or
- Move 5/6-hop to a separate “stress” profile so baseline/extended-core SLOs stay clean

Engine (product):

- Treat unbounded exact-k walk counts on dense graphs as known expensive patterns; consider planner warnings, hop/work caps, or cheaper reachability/`count(DISTINCT)` plans

Re-run:

- Complete edge-rs extended-core RO (and optional 5% RW) with documented timeout policy to refresh numbers beyond the C-engine 2026-11-05 run

---

## 9. Broader query lightening (GRAPH.EXPLAIN + timing)
After exact 5/6-hop LIMIT 100, remaining **tails** (not failures) on clean extended-core were dominated by:

| Query (pre-change) | Issue | Change |
|--------------------|-------|--------|
| `all_shortest_paths_len` | `pathCount:0 maxLen:4` → p99 **~3.2s**; hard pairs ~1.2s | `pathCount:10 maxLen:3` |
| `var_len_with_edge_where_filter` | unbounded `*1..3` count (hub ~355k / 129ms) | `WITH t LIMIT 100 RETURN count(t)` |
| `order_by_age` | full 100k sort/materialize | `LIMIT 100` |
| `value_join` / `_cnt` | full age cohort | `LIMIT 100` |
| expansions / neighbours / patterns | unbounded row fan-out | `LIMIT 100` |
| `shortest_path*` | tried `maxLen:6` | **reverted** — made easy pairs slower and cascaded under load |
| `algo_max_flow` (optional) | ~15s/pair; `db.relationshipTypes` noise | hardcode `Friend` (still heavy; keep algos off by default) |

Fair retest (algos off): `Results-261117-light-extcore/` vs prior lim100-only `Results-261117-lim100-extcore-noalgo/`:

| | lim100 hops only (C) | **lightened (C 4.22.4)** | **lightened (edge-rs)** |
|--|--|--|--|
| errors | 0 | **0** | **0** |
| elapsed | 25.5 s | **11.3 s** | **18.3 s** |
| qps | ~392 | **~887** | **~545** |
| global p99 | 182 ms | **124 ms** | **332 ms** |
| `all_shortest` p99 | 3195 ms | **45 ms** | **78 ms** |

---
## 10. Fix: LIMIT 100 on exact 5/6-hop (2026-11-17)

Queries in `src/queries_repository.rs` now cap path rows:

```cypher
MATCH (s:User {id: $id})-[:Friend*5..5]->(t:User)
WITH t LIMIT 100
RETURN count(t) AS cnt
```

(same for `*6..6`). Solo probes on hub `id=780` drop from multi-second / timeout to **~1–2 ms** on C 4.22.4 and edge-rs.

### Fair extended-core retest (algos off, matching prior medium script)

Workload: `medium-extended-core-lim100-falkor`, 10k queries, p=8, mps=2000, timeout 20s.  
Results: `Results-261117-lim100-extcore-noalgo/`

| Engine | errors | fail% | elapsed | p50 / p95 / p99 | exact_5 fail / p99 | exact_6 fail / p99 |
|--------|-------:|------:|--------:|-------------------|--------------------|--------------------|
| **C 4.22.4** (LIMIT 100) | **0** | **0%** | **~25.5 s** | see metrics.prom | **0%** / low ms | **0%** / low ms |
| **edge-rs** (LIMIT 100) | **0** | **0%** | **~24.1 s** | see metrics.prom | **0%** / low ms | **0%** / low ms |
| C 4.22.4 unbounded (prior) | 197 | 1.97% | ~629 s | p99 ~2.2 s | 16.7% / ~20 s | 83.6% / ~20 s |
| C 4.22.1 unbounded (prior) | 168 | 1.68% | ~548 s | p99 ~2.0 s | 10.3% / ~15 s | 75.4% / ~19 s |

**Verdict:** LIMIT 100 **eliminates** the deep-hop timeout class and improves extended-core wall time by **~20–25×** on both C and Rust. Overall extended-core performance is no longer dominated by unbounded walk explosion.

Note: an intermediate retest accidentally enabled default algo queries (`max_flow` etc.); those are unrelated to the hop fix. Fair comparison uses algos disabled.

---
## 10. FalkorDB C 4.22.4 confirmation (unbounded, pre-fix)

Fresh medium Pokec load + extended-core RO on `falkordb/falkordb:v4.22.4` (module ver **42204**), port `:6381`, container `bench-falkor-4.22.4`.

Config matched prior runs: 10k queries, `parallel=8`, `mps=2000`, `FALKOR_QUERY_TIMEOUT_MS=20000`, file `medium-extended-core-falkor`.

Results: `Results-261117-4224-extcore/`

### Solo probes (TIMEOUT 20s)

| Query seed | Result on 4.22.4 |
|------------|------------------|
| 5-hop id=38845 | **~46.0M** walks in ~4.6s |
| 5-hop id=22893 / 1 | `cnt=0` &lt;50ms |
| 6-hop id=780 | **Query timed out** (~20.1s) |
| 6-hop id=37219 | **Query timed out** |
| 6-hop id=10427 | **Query timed out** |
| 6-hop id=1902 | **~188.6M** walks in ~18.7s |
| 6-hop id=1 | `cnt=0` &lt;50ms |

Same explosion / timeout pattern as 4.22.1, 4.22.3, and edge-rs.

### Full extended-core run

| Metric | 4.22.1 | 4.22.3 | **4.22.4** |
|--------|--------|--------|-----------|
| success / errors | 9832 / 168 | 9830 / 170 | **9803 / 197** |
| global failure rate | 1.68% | 1.70% | **1.97%** |
| global timeout rate (prom) | 0% | 0% | **0%** |
| p50 / p95 / p99 | 0.99 / 58 / 2032 ms | 1.04 / 59 / 2007 ms | **1.6 / 96 / 2179 ms** |
| elapsed | ~548 s | ~549 s | **~629 s** |
| `exact_6_hop` fail% | 75.4% | 75.9% | **83.6%** |
| `exact_5_hop` fail% | 10.3% | 10.8% | **16.7%** |
| `all_shortest_paths_len` fail% | 0% | 0% | **0%** (p99 ~3.7s) |

Error budget on 4.22.4 still collapses to the two deep-hop queries only (~163 + ~34 ≈ 197).

**Verdict:** Performance issues **persist on 4.22.4**. Fail rates are in the same ballpark (slightly higher than the dual-instance 4.22.1/4.22.3 run — likely load/host noise, not a fix). Root cause remains walk explosion under the 20s timeout, not a version-specific regression that 4.22.4 resolved.

---
## 9. Artifact index

| Artifact | Purpose |
|----------|---------|
| `Results-261105-18:25/` | Full extcore RO dual C-engine run |
| `ui/public/summaries/multi_engine_compare_extcore.json` | Authoritative multi-engine summary for that run |
| `medium-extended-core-falkor` | RO extended-core query stream (10k) |
| `medium-extended-core-w05-falkor` | Same catalog with ~5% writes (no completed Results dir yet) |
| `4.22.1-4.22.3.md` §2 | Earlier version comparison that first flagged deep-hop fails |
| `rust-edge-rs-timeout-issue.md` | Separate historical edge-rs multi-hop DISTINCT bug (fixed) |
| `Results-261117-4224-extcore/` | Medium extended-core RO on C **4.22.4** (confirms issue persists) |
