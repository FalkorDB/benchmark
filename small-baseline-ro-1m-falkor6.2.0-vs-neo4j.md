# Small dataset: FalkorDB 6.2.0 vs Neo4j 2026.10.2 — baseline RO 1M

Date: 2026-11-23  
Dataset: **small** Pokec (10,000 nodes / 121,716 `Friend` edges)  
Profile: **baseline**, **read-only** (`write_ratio=0`)  
Queries: **1,000,000** each (`small-baseline-ro-1m-falkor` / `small-baseline-ro-1m-neo4j`)  
Target scheduler rate: **mps=8000**, **parallel=64**  
Timeout: `FALKOR_QUERY_TIMEOUT_MS=20000`  
Results: `Results-261123-small-baseline-ro-1m-falkor6.2.0-vs-neo4j/`

| Engine | Version | Endpoint |
|--------|---------|----------|
| FalkorDB | **6.2.0** (module 60200, Redis 7.2.4) | `falkor://127.0.0.1:6800` (`bench-falkor-6.2.0`) |
| Neo4j | **Community 2026.10.2** | `neo4j://127.0.0.1:7687` (local tarball) |

Algos disabled in query generation. Sequential runs (Falkor first, then Neo4j).

---

## Overall

| Metric | FalkorDB 6.2.0 | Neo4j 2026.10.2 | Neo vs Falkor |
|--------|----------------|-----------------|---------------|
| success / errors | **1,000,000 / 0** | **1,000,000 / 0** | same |
| fail / timeout % | 0 / 0 | 0 / 0 | same |
| **elapsed** | **211.3 s** | 592.2 s | **+180%** (Neo slower) |
| **achieved QPS** | **4732** | 1689 | Neo **−64%** |
| target mps | 8000 | 8000 | neither fully saturated target |
| **avg latency** | **13.5 ms** | 37.9 ms | Neo **+180%** |
| **p50** | 12.7 ms | **12.0 ms** | Neo slightly better |
| **p95** | **20.7 ms** | 166.9 ms | Neo **+705%** |
| **p99** | **29.2 ms** | 397.3 ms | Neo **+1261%** |

### Verdict

On small baseline RO at high concurrency (p=64, target 8k mps):

- **Both complete 1M queries with zero errors.**
- **FalkorDB 6.2.0 is ~2.8× faster wall-clock** and delivers **~2.8× higher achieved QPS**.
- Medians are similar (~12–13 ms); **Neo4j tails are much heavier** (p95/p99).
- Neither side sustained the full **8000** scheduled mps end-to-end (Falkor ~4.7k, Neo4j ~1.7k achieved)—limited by query latency × worker concurrency, not by the scheduler target alone.

---

## Memory (Falkor multi-layer, end of run)

| Layer | Falkor 6.2.0 (small) |
|-------|----------------------|
| GRAPH.MEMORY | ~7 MB |
| Redis RSS peak | ~184 MiB |
| Container peak | ~257 MiB |

(Neo4j store/JVM metrics: JMX collect warned once during run; not used for primary comparison.)

---

## Heaviest query tails (p99)

**Falkor (tight tails):** `shortest_path*` ~40 ms p99; most catalog queries ~24–28 ms p99.

**Neo4j (heavy tails):** `shortest_path_with_filter` p99 **~2.2 s** (p50 still ~12 ms); many others p99 ~385–436 ms while p50 stays ~11–21 ms — classic concurrency/tail behavior under p=64.

---

## Artifacts

| Path | Role |
|------|------|
| `Results-261123-small-baseline-ro-1m-falkor6.2.0-vs-neo4j/falkordb-6.2.0/` | Falkor metrics |
| `Results-261123-small-baseline-ro-1m-falkor6.2.0-vs-neo4j/neo4j/` | Neo4j metrics |
| `small-baseline-ro-1m-falkor` / `…-neo4j` | 1M query streams |

---

## Bottom line

**FalkorDB 6.2.0 >> Neo4j 2026.10.2** on this small baseline RO 1M workload for throughput and tail latency, with equal reliability (0 errors). Neo4j p50 is competitive; p95/p99 and sustained QPS are not.
