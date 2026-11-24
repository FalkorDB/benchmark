# FalkorDB edge-rs timeouts vs Rust client — investigation summary

Date: 2026-11-15  
Context: medium baseline benchmarks comparing `falkordb/falkordb:edge-rs` (Valkey host) and `falkordb/falkordb:edge-rs-redis` (Redis host), using the benchmark’s Rust stack (`falkordb-rs` v0.9.0 vendored + `benchmark` driver).

---

## Executive summary

**The timeout storm is not primarily a Rust-client ↔ edge / Valkey compatibility bug.**

- The same multi-hop baseline queries **time out on edge-rs via `redis-cli`**, with no Rust client involved.
- **C engine 4.22.1** finishes the same Cypher in **~80 ms**.
- **edge-rs** and **edge-rs-redis** both hit **server-side `Query timed out`** (20–60s).
- The Rust client correctly surfaces that as `RedisError("timed out")`. Under `parallel=8` and a **20s** script timeout, slow queries block pool workers and **cascade**.

**Valkey vs Redis as the host is not the differentiator** for this failure mode.

---

## Environment

| Role | Image | Port | Host | Graph module |
|------|--------|------|------|--------------|
| edge-rs | `falkordb/falkordb:edge-rs` | 6380 | **Valkey** 7.2.4 | ver 999999 |
| edge-rs-redis | `falkordb/falkordb:edge-rs-redis` | 6800 | **Redis** 8.10.2 | ver 999999 |
| C reference | `falkordb/falkordb:v4.22.1` | 6900 | Redis 8.10.2 | ver 42201 |

Dataset: medium Pokec (~100k `User` nodes, ~1.77M `Friend` edges), graph name `falkor`.

Client stack:

- `falkordb = { path = "vendor/falkordb-rs" }` (tag v0.9.0 lineage)
- Reads: `GRAPH.RO_QUERY` + `--compact` + per-query `timeout <ms>`
- Writes: `GRAPH.QUERY` + same
- Pool: `ConnectionPool { pool_size: 8 }`
- redis-rs: `response_timeout` explicitly set to **`None`** in falkordb-rs (no short socket timeout)
- Benchmark also wraps calls in `tokio::time::timeout(query_timeout + 5s)`

Medium script defaults (important):

```bash
FALKOR_QUERY_TIMEOUT_MS=${FALKOR_QUERY_TIMEOUT_MS:-20000}  # 20s
# also applied as GRAPH.CONFIG SET TIMEOUT
PARALLEL=8
MPS=2000
```

Driver fallback if env unset in other paths: **180s** — much higher than the medium script’s 20s.

---

## Repro Cypher (definitive)

From the baseline workload (`aggregate_expansion_3_with_filter`):

```cypher
CYPHER id = 99884
MATCH (s:User {id: $id})-->()-->()-->(n:User)
WHERE n.age >= 18
RETURN DISTINCT n.id
```

### Single-shot via `redis-cli` (no Rust)

| Engine | Command | Result |
|--------|---------|--------|
| **C 4.22.1** | `GRAPH.RO_QUERY ... TIMEOUT 60000` | **~82 ms** internal execution |
| **edge-rs (Valkey)** | same | **`Query timed out`** at 20s |
| **edge-rs-redis** | same | **`Query timed out`** at 20s |

Conclusion: failure is on the **edge-rs engine path**, not client protocol/encoding unique to Rust.

---

## Rust client behavior

### What works

- Connect (`falkor://` → `redis://`), `PING`, simple `RETURN` / `count(n)`.
- `GRAPH.RO_QUERY` / `GRAPH.QUERY` both present on edge images.
- Compact protocol path is used as designed.
- Light query families (e.g. `neighbours_2` only) complete quickly with the Rust client:
  - 40 queries, parallel=1: **~303 ms** total
  - 40 queries, parallel=8: **~229 ms** total
  - No timeouts when pathological multi-hop shapes are absent

### What fails under medium baseline RO

Short probe: **201** mixed baseline RO queries, parallel=8, `FALKOR_QUERY_TIMEOUT_MS=20000`:

| Target | Failures | Notes |
|--------|----------|--------|
| edge-rs (Valkey) | ~**4.5%** ops | 100% fail on expansion_3/4 with filter |
| edge-rs-redis | ~**4.5%** ops | Same pattern |

Error form in logs:

```text
Error executing query: ..., the error is: RedisError("timed out")
```

Prometheus often records these as **failures** (not always `timeout_rate`), depending on how `is_timeout_error` classifies the redis error string — but the underlying event is still a timeout.

Under concurrency, queries that *do* complete among heavy families can show **multi-second** client-observed latency (order of ~10–13s), while the same *simple* multi-hops via redis-cli stay in milliseconds when they are cheap shapes. The catastrophic case is specifically **3–4 hop + filter + `DISTINCT n.id`** (and similar), which never finish within 20s on edge-rs.

### Cascade mechanism

1. Worker issues heavy RO query with server `TIMEOUT 20000`.
2. edge-rs runs until timeout → server returns timeout / connection stays busy for ~20s.
3. With **8 workers** and pool size **8**, stuck queries occupy connections.
4. Other workers wait → more wall-clock pressure → more timeouts.
5. If the process is OOM-killed (exit **137** observed when running two full medium graphs dual-box), clients then see **`NoConnection`**.

So the Rust stack **amplifies** an engine slowness into a timeout storm; it does not invent the root timeout.

---

## What we ruled out (mostly)

| Hypothesis | Verdict |
|------------|---------|
| Rust cannot talk to edge / Valkey | **No** — basic ops and light queries work |
| Valkey-only protocol bug | **No** — edge-rs-redis fails the same Cypher |
| redis-rs default short `response_timeout` | **No** — falkordb-rs sets it to `None` |
| Client using `GRAPH.QUERY` for reads | **No** — reads use `ro_query` → `GRAPH.RO_QUERY` |
| Wrong param encoding (`CYPHER id = …`) | **Unlikely** — same string times out on redis-cli |

### Secondary (amplifier, not root)

- Medium script **20s** timeout is aggressive vs driver 180s default and vs edge-rs cost on hard queries.
- Pool size 8 + parallel 8 + long queries ⇒ queueing / head-of-line effects.
- Large `RETURN n.id` / `DISTINCT n.id` result materialization can add client CPU, but does **not** explain redis-cli 20s server timeout on edge vs 82ms on C.

---

## Dual-run operational notes

Attempts to run **both** edge-rs and edge-rs-redis medium loads concurrently on Docker Desktop often ended with:

- container **exit 137** (SIGKILL / OOM), and/or  
- mass `timed out` then `NoConnection`

Recommendation for host comparisons until memory/engine issues settle:

- Run **one engine at a time** (sequential), **or**
- Raise container memory and keep a single medium graph resident, **and**
- Raise `FALKOR_QUERY_TIMEOUT_MS` substantially (e.g. `300000`) **or** exclude known-bad query shapes.

---

## Recommendations

### For edge-rs engine work

1. Treat as **perf/timeout bug** on multi-hop patterns like:
   - `MATCH (s:User {id:$id})-->()-->()-->(n:User) WHERE … RETURN DISTINCT n.id`
   - related `aggregate_expansion_3/4*_with_filter` baseline queries.
2. Compare plan/execution to C 4.22.1 on the same graph (C ~tens of ms, edge >20s).
3. Confirm whether `DISTINCT`, intermediate cartesian behavior, or filter placement differs in the Rust engine.

### For benchmark / Rust client usage against edge

1. **Do not** treat current medium baseline RO @ 20s / p=8 as a fair Valkey-vs-Redis client comparison until edge multi-hop costs are fixed.
2. For smoke tests:
   - `FALKOR_QUERY_TIMEOUT_MS=300000` (or higher), and/or  
   - filter out expansion_3/4 with filter families, and/or  
   - start with parallel=1.
3. Optionally improve timeout classification so `RedisError("timed out")` always increments timeout metrics (ops clarity only).

### For Valkey vs Redis baseline (Rust engine)

Once edge multi-hop is healthy (or queries are curated):

- Same image lineage: `edge-rs` vs `edge-rs-redis`
- Same workload file, sequential or dual with enough RAM
- Report host (`server_name` / `executable`) from `INFO server` in results identity

Until then, any “Valkey slower than Redis” signal is confounded by **shared edge-rs query timeouts**.

---

## Bottom line

| Question | Answer |
|----------|--------|
| Is the Rust client incompatible with edge? | **No strong evidence of protocol incompatibility.** |
| Why so many timeouts? | **edge-rs is extremely slow / times out on some baseline multi-hop + filter + DISTINCT queries** that C handles quickly; the client reports server/guard timeouts and concurrent workers amplify them. |
| Valkey vs Redis? | **Same edge failure on both hosts** for the smoking-gun Cypher. |
| Where to fix first? | **edge-rs query execution** for those shapes; secondarily benchmark timeout/concurrency settings when testing edge. |
