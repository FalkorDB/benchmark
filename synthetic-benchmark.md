# Synthetic per-operation benchmark — design

Status: **draft for review** (no code yet). Implementation follows only after approval (repo
convention: design first, rubber-duck it, then implement). This revision (v3) adds a **concurrency
sweep** (measure latency vs throughput per operation), incorporates a rubber-duck review, and
resolves the AI-review feedback on the PR. Correcting-detail notes from review are marked **[review]**.

## 1. Goal

Measure the latency of individual Cypher **operations in isolation**, and how that latency behaves as
**throughput scales up**. For each operation, on **every measured invocation** we capture a *paired*
pair of numbers:

- **server time** — the database's own reported execution time, read from the query response
  (FalkorDB's *internal execution time*). Excludes client, driver-parsing and network cost.
- **total time** — wall-clock from *just before* the client sends the request to *just after* the
  client has received the response and drained every row.

Because both numbers come from the **same invocation**, we report a *paired* residual
**`non_internal_ms = total − server`**. **[review]** This is *not* "pure network overhead": it also
includes server queue/wait, the module's own reporting, connection selection and driver
deserialization — hence the neutral name.

**Concurrency is a first-class knob.** Each operation is measured at a configurable **concurrency
level `C`** (number of simultaneous in-flight requests): `C = 1` is the isolated single-flight
latency; `C > 1` (any positive integer) loads the server with `C` concurrent copies of the *same*
operation. Running a **sweep** of `C` values (e.g. `1, 2, 4, 8, 16, …`) traces, per operation, how
**latency (incl. tail) changes as achieved throughput rises** — the classic latency-vs-throughput
curve and its saturation "knee".

Measurements are cleaned of outliers using **Criterion.rs's methodology** (Tukey fences + bootstrap
confidence intervals).

### Decisions locked for this iteration (from review)

| Decision | Choice |
| --- | --- |
| Operation granularity | High-level Cypher **primitives**, each as one isolated query |
| Vendors | **FalkorDB only** (kept vendor-agnostic so Neo4j/Memgraph can follow) |
| Dataset | **Dedicated synthetic dataset**, seeded + reproducible, size knobs |
| Write isolation | **Steady-state**, per-worker scratch namespace, deterministic keys, periodic reset |
| Cold vs warm | **Warm / steady-state** after warm-up (plan cache primed) |
| Concurrency | First-class `--concurrency` level (default `1`, any positive int) + **sweep** for latency-vs-throughput |
| Output | Standalone **JSON + Markdown/console report** (UI later) |
| Statistics | **Criterion-like** (warm-up, sampling, Tukey outliers, bootstrap CIs) + latency percentiles |
| Harness | **In-house concurrency engine (primary; required for `C > 1`)**; Criterion optional for the `C = 1` headline + version baselines — see §4 |

### Future (designed for, not built now)

1. **Compare op latency between FalkorDB versions** — Criterion baselines for single-flight; our JSON
   (with the FalkorDB version + corpus hash embedded) for every metric/level.
2. **Open-loop (fixed arrival-rate) load with coordinated-omission correction** — the closed-loop
   sweep in this design answers "latency as `C` rises"; a rigorous *offered-throughput* curve needs an
   open-loop driver (see §4.2 and §8). This complements the repo's existing coordinated-omission work.

## 2. Where the two timings come from (verified against `vendor/falkordb-rs`)

- **server time** — `QueryResult::get_internal_execution_time() -> Option<f64>` (milliseconds),
  parsed from the response stats *"Query internal execution time: N milliseconds"*
  (`vendor/falkordb-rs/src/response/mod.rs`). **[review]** Verified present; value is in ms with
  ~6 decimal places from a high-resolution timer (precision caveat in §10).
- **total time** — an `Instant` around `graph.ro_query/query(q).with_timeout(ms).execute().await`
  **plus draining the result set**. **[review]** The async result is **not network-lazy** — rows are
  eagerly parsed and buffered before `execute()` returns
  (`vendor/falkordb-rs/src/response/row_stream.rs`). Network receipt completes at `execute()`; we
  still drain every row to model client consumption and to surface row-decode errors.
- **cache state** — `QueryResult::get_cached_execution() -> Option<bool>` (a cached **execution
  plan/context**, not cached results). **[review]** A *missing* stat is reported as an explicit
  `cached_unknown` count — it is **not** folded into "not cached" (that would distort
  `cached_false_rate`).

Reads use `ro_query`, writes use `query` (mirroring `QueryType::{Read, Write}` in
`src/queries_repository.rs`).

**[review] Connection & deadline**
- The async client **defaults to 8 multiplexed sockets** (`vendor/falkordb-rs/src/client/builder.rs`).
  For honest measurement we configure the connection explicitly and record it: **a single dedicated
  connection at `C = 1`, and a pool of exactly `C` connections** for `C > 1` (one per worker).
- `with_timeout(ms)` is a **server-side** guard; the client-side response timeout is disabled by
  default (`vendor/falkordb-rs/src/client/mod.rs`). We add a **Tokio client deadline** per call,
  mirroring `src/falkor/falkor_driver.rs` (`query_timeout_guard`), so a stuck socket can't hang a run.

## 3. What counts as an "operation" (the catalog)

A curated catalog of **primitive** operations, each one parameterized Cypher statement against a known
graph shape. Initial set:

**Reads**
- `match_by_index` — point lookup on an indexed property (`MATCH (n:Label {id:$id}) RETURN n`).
- `match_by_label_scan` — label scan + filter on an **un-indexed** property.
- `expand_1_hop` — traverse one relationship type from a seed node.
- `expand_hops_5` — variable-length traverse. **[review]** Bounds must be **literals** in
  FalkorDB/openCypher, so the depth is fixed per benchmark (`*1..5`) and encoded in the benchmark id,
  **not** a `$k` parameter.
- `aggregate_count`, `aggregate_group` — count / group-by aggregation.
- `shortest_path` — `shortestPath` between two connected seed nodes.
- `property_projection` — return a few properties for a bounded match (`LIMIT $n`).

**Writes** (steady-state, §5.3)
- `create_node`, `create_edge`, `set_property`, `delete_node`.
- **[review]** `merge_node` is split into **`merge_hit`** (key exists → match path) and **`merge_miss`**
  (key absent → create path) — different plans.

Catalog entry — **[review]** inputs are a **fully pre-generated, seeded corpus** (a deterministic,
per-worker sequence), not built from a live RNG inside the timing loop, and `RngCore` is used as an
object-safe trait (`Rng` is not a valid function-pointer parameter type):

```rust
struct OperationSpec {
    name:  &'static str,                 // stable id
    kind:  QueryType,                    // Read | Write -> ro_query vs query
    // deterministic corpus for worker `w` of `n` workers; the full set of sequences (all workers,
    // all keys) is hashed into the run's corpus_hash (§5.2), so the workload is identical across
    // samples, concurrency levels and versions:
    corpus: fn(&mut dyn rand::RngCore, &DatasetHandle, worker: usize, workers: usize) -> Vec<Query>,
    write:  Option<WritePlan>,           // per-worker scratch namespace + untimed hooks
}
```

Queries are built with the existing `crate::query::{Query, QueryBuilder, QueryParam}` and use
**parameters** (not inlined literals) so FalkorDB's plan cache stays warm and stable.

## 4. Harness

### 4.1 What we use, and why the concurrency requirement reshapes it

**[review]** The v2 plan leaned on Criterion for everything. Criterion is **single-flight**: it runs
one closure to completion, then the next, and analyzes **one aggregate scalar per sample**. It cannot
drive `C > 1` in-flight requests, and it cannot analyze a *second, paired* per-invocation metric
(server time) — two Criterion benchmark ids would measure two **different** invocation populations,
and a per-invocation side buffer's `n` would **not** match Criterion's sample population. Confirmed
against Criterion's `iter_custom` semantics (Criterion controls sample count and hands each closure an
`iters`; the closure returns one aggregate `Duration`).

Because arbitrary concurrency is now a first-class requirement, the **primary measurement path is a
small in-house engine** (§4.2) that already must exist for `C > 1`; we reuse it for `C = 1` too so a
single latency-vs-throughput curve is produced by one code path. **Criterion is kept as an optional,
complementary path** for the `C = 1` single-flight headline: its warm-up/Tukey/bootstrap machinery,
HTML plots and — most valuably — **version baselines** (`--save-baseline` / `--baseline`) are worth
keeping for regression tracking of single-flight latency. (See open question §10.5.)

### 4.2 The in-house concurrency engine (closed-loop)

For an operation at concurrency `C`, the engine runs a **closed-loop** load: `C` worker tasks, each on
its own connection, each firing a request, awaiting the response (drain included), then immediately
firing the next from its **pre-generated deterministic sequence**. A warm-up window (discarded) primes
the plan cache; then a measurement window records, per invocation, the **paired** `(server_ms,
total_ms)`.

```rust
async fn measure(op, c: usize, ds, warmup, window) -> LevelResult {
    let conns = pool_of(c);                       // exactly `c` connections
    let workers = (0..c).map(|w| tokio::spawn(async move {
        let corpus = op.corpus(&mut seeded_rng(w), &ds, w, c);   // fixed, untimed, per-worker
        let mut buf = Vec::new();
        for q in corpus.iter().cycle() {
            if stop.load() { break; }
            op.write.as_ref().map(|p| p.before(&mut state))?;     // [review] untimed hook -> Result;
                                                                  // propagate; do NOT time-on-failure
            let t0 = Instant::now();
            let r = run_and_drain(&mut conns[w], op.kind, q).await?;  // fatal on error (§5.1)
            buf.push(Sample { server_ms: r.server_ms, total_ms: t0.elapsed().as_secs_f64()*1e3,
                              phase: phase_now() });
        }
        buf
    }));
    // run warmup then window; a SHARED run-level counter (not a per-callback index) drives the
    // periodic write reset cadence so `reset_every` means "every N operations", not "per sample".
    let samples = join(workers).measured_phase_only();
    LevelResult {
        concurrency: c,
        throughput_ops_s: samples.len() as f64 / window.as_secs_f64(),   // ACHIEVED throughput
        server_ms: robust(samples.map(|s| s.server_ms)),                 // Tukey removal + percentiles
        total_ms:  robust(samples.map(|s| s.total_ms)),
        non_internal_ms: paired(samples.map(|s| s.total_ms - s.server_ms)),
    }
}
```

A **sweep** calls `measure(op, c, …)` for each `c` in the configured list and assembles the
latency-vs-throughput curve. **[review]** This yields **achieved** throughput (closed-loop); an
**offered**-throughput curve (fixed arrival rate) and coordinated-omission correction are the §8
open-loop extension.

**[review] Failure handling.** `get_internal_execution_time()` returning `None` must **not** become
`NaN` (`Duration::from_secs_f64(NaN)` panics; a zero duration also makes Criterion abandon analysis).
An invocation that errors, times out, or lacks server stats is **benchmark-fatal by default**;
opt-in `--tolerate-failures` retries until the window has enough *successes* and reports
`attempts`/`failures` separately (never silently averaged in). Setup/reset hook failures propagate the
same way — the operation is **not** timed if its setup failed.

**[review] Criterion has no custom flags.** Dataset/concurrency knobs come from an **env/config file**
read by the harness, not `--nodes`/`--concurrency` passed to Criterion. We **pin `criterion`** because
the optional `C = 1` exporter reads its output schema.

## 5. Architecture

Self-contained; nothing in the existing run/scheduler path changes.

```text
src/synthetic/
  mod.rs          // config, orchestration, Report
  catalog.rs      // OperationSpec catalog (§3)              -- engine-agnostic
  dataset.rs      // synthetic dataset + DatasetHandle (§5.2) -- engine-agnostic
  op_runner.rs    // run_and_drain(): one op -> { server_ms, total_ms, rows, cached } -- engine-agnostic
  engine.rs       // closed-loop concurrency engine + sweep (§4.2)
  stats.rs        // Tukey removal + percentiles + bootstrap CI (paired)
  report.rs       // JSON + Markdown; (optionally) merges Criterion C=1 output
benches/
  synthetic_ops.rs // OPTIONAL Criterion harness for the C=1 single-flight headline + baselines
```

Keeping `catalog` / `dataset` / `op_runner` free of engine- and Criterion-specific types is the seam
that lets both the sweep engine and the optional Criterion path reuse them.

### 5.1 Op-runner (`op_runner.rs`)

```rust
async fn run_and_drain(conn, kind, query) -> Result<OpSample> {
    let started = Instant::now();
    let result  = with_client_deadline(                       // Tokio guard, §2
        match kind { Read => conn.ro_query(q), Write => conn.query(q) }
            .with_timeout(server_timeout_ms).execute()).await?;
    let cached    = result.get_cached_execution();            // Option<bool>: None -> cached_unknown
    let server_ms = result.get_internal_execution_time().ok_or(MissingServerStats)?;  // no NaN
    let rows = drain(result.data).await?;                     // consume + surface row errors
    Ok(OpSample { server_ms, total_ms: started.elapsed().as_secs_f64()*1e3, rows, cached })
}
```

### 5.2 Synthetic dataset (`dataset.rs`)

A dedicated, **seeded, reproducible** graph built once per run, sized by knobs (env/config, §4.2):

- `nodes`, `edges`/avg-degree, label & relationship-type counts.
- An **indexed** `id` (for `match_by_index`, `merge_*`, `set_property`), an **un-indexed** property
  (for `match_by_label_scan`), and enough connectivity for `expand_*` / `shortest_path`.
- Creates required **indexes** up front.
- `DatasetHandle` exposes **seeded pools** of valid inputs (existing ids, connected pairs).
- **[review] Write determinism.** For write ops the **entire key/identity sequence is pre-generated,
  per worker**, from the seed — never a mutable counter sampled at execution time (warm-up, varying
  `iters`, retries and resets would otherwise change the real workload while the corpus hash stayed
  the same). The `corpus_hash` covers **all workers' full pre-generated sequences** (read params *and*
  write keys/identities), so a run — and any Criterion baseline — is only compared against a
  byte-identical workload.
- Reuses existing load utilities (`src/data_prep.rs`, the Falkor batch-load path).

### 5.3 Warm / steady-state and write isolation

- **Warm:** an explicit warm-up per (op, `C`) primes the plan cache; we assert `cached == true` during
  measurement and report `cached_false_rate` and `cached_unknown`.
- **Reads** are naturally repeatable; the fixed corpus varies parameters over the seed pool.
- **[review] Writes** are timed so only the operation is inside the timer — **setup/reset/cleanup run
  in untimed hooks that return `Result` and abort the sample on failure**:
  - **per-worker scratch namespace** (its own label / rel-type / key range) so concurrent workers
    never collide and cleanup is one cheap delete that never touches the read fixture;
  - `create_node` / `merge_miss` consume the worker's **pre-generated fresh-key sequence**; growth is
    bounded by a **periodic reset** hook driven by a **shared run-level operation count** (so
    `reset_every = N` means every `N` operations, independent of Criterion/engine sampling);
  - **[review] `create_edge`** uses **fresh, deterministic edge identities** per invocation (from the
    pre-generated sequence) with untimed cleanup/reset, so repeated invocations don't accumulate
    duplicate edges or trip uniqueness — later samples still measure the same graph shape;
  - `set_property` / `merge_hit` operate on **pre-created scratch seeds**;
  - `create_node` + `delete_node` can be paired to keep net size ~constant.
- **[review]** Reset is a bounded sawtooth in scratch size; it does not perfectly restore
  allocator/index/cache state (documented limitation).

## 6. Methodology / statistics

- **Per (operation, concurrency level)** we report: **achieved throughput** (ops/s) and, for both
  `server_ms` and `total_ms`, the latency distribution — **median, p90, p95, p99, mean** and a
  bootstrap CI. Tail percentiles matter most as `C` rises. `non_internal_ms` is the paired
  `total − server`.
- **Outliers:** Tukey fences (mild 1.5×IQR / severe 3×IQR). We **remove severe outliers** from the
  reported latency estimates (the user's ask). **[review]** For the optional `C = 1` Criterion path,
  Criterion *classifies and resists* outliers (mean/regression over all samples) rather than deleting
  them — we keep its convention there and note the difference.
- Every knob (dataset seed/size + `corpus_hash`, warm-up, window, concurrency list, server timeout,
  client deadline, reset interval, connection strategy) is recorded in the report header.

## 7. Output

- **Primary:** `synthetic-report.json` — run metadata (git SHA, FalkorDB/module version via
  `CALL dbms.version()`/module list, dataset seed+knobs+`corpus_hash`, host info via `sysinfo`,
  connection strategy), then per operation an array of **per-concurrency-level** results. Plus a
  rendered **Markdown/console** latency-vs-throughput table.
- **Secondary (optional):** Criterion's `target/criterion/**` (HTML, plots, version comparison) for
  the `C = 1` metric.

```json
{
  "meta": { "falkordb_version": "4.2.1",
            "dataset": {"seed":42,"nodes":100000,"edges":1000000,"corpus_hash":"9f3a…"},
            "warmup_secs":3, "window_secs":10 },
  "operations": {
    "match_by_index": {
      "connection": "pool",
      "levels": [
        { "concurrency":1,  "throughput_ops_s":2950,  "server_ms":{"p50":0.081,"p99":0.20},
          "total_ms":{"p50":0.33,"p99":0.9}, "non_internal_ms":{"p50":0.25}, "cached_false_rate":0.005 },
        { "concurrency":8,  "throughput_ops_s":18100, "server_ms":{"p50":0.10,"p99":0.6},
          "total_ms":{"p50":0.44,"p99":2.1}, "non_internal_ms":{"p50":0.34} },
        { "concurrency":32, "throughput_ops_s":41200, "server_ms":{"p50":0.35,"p99":3.4},
          "total_ms":{"p50":0.78,"p99":7.9}, "non_internal_ms":{"p50":0.43} }
      ]
    }
  }
}
```

## 8. Future extensions

- **Version comparison.** Single-flight: Criterion `--save-baseline <A>` then `--baseline <A>`
  (compare against the *saved* name). All levels/metrics: diff two `synthetic-report.json` files,
  guarded by `corpus_hash` so only like-for-like is compared.
- **Open-loop / offered throughput + coordinated omission.** The §4.2 closed-loop sweep measures
  **achieved** throughput and, by construction, under-measures the latency an offered rate "should"
  have seen when the server stalls (coordinated omission). A rigorous offered-load curve needs an
  **open-loop, arrival-rate driver** (requests dispatched on schedule regardless of completion) with
  CO correction — a natural next step that **reuses `catalog`/`dataset`/`op_runner`/`stats`
  unchanged**. **[review]** Also note: cloned `AsyncGraph`s **share a schema write-lock during response
  parsing**, a real cross-worker coupling to keep in mind when interpreting high-`C` results.

## 9. Tooling: `just` + CI (proposed — not yet implemented)

**[review]** These recipes are **proposed** (this PR is design-only); nothing here runs yet.

- `just synthetic-bench` → run the configured sweep (needs a reachable FalkorDB; `protoc` required to
  build). Dataset/concurrency knobs via env/config file (§4.2), not CLI flags.
- `just synthetic-bench-one <op>` → a single operation across the sweep.
- `just synthetic-baseline <name>` / `just synthetic-compare <name>` → save/compare the **optional
  Criterion `C = 1`** baseline. **[review]** `synthetic-compare` first checks the saved vs current
  `corpus_hash` (and FalkorDB version) and **aborts on mismatch** before invoking Criterion.
- **Not a CI gate** (numbers are machine-dependent) — matching how `falkordb-rs` treats benches. A
  tiny `synthetic-bench-smoke` (one op, `C = 1`, minimal window) can later run in CI only to keep the
  harness compiling.
- New **pinned** dev-dependency `criterion` (`async_tokio`) for the optional `C = 1` path; the sweep
  engine itself has no Criterion dependency.

## 10. Open questions / risks

1. **Outliers.** We remove severe (>3×IQR) outliers from the reported latency; the optional Criterion
   `C = 1` path keeps Criterion's report-and-resist convention. OK with that split, or force removal
   everywhere (post-processing Criterion's `sample.json`)?
2. **Server-time precision.** FalkorDB reports ms with ~6 decimals from a hi-res timer; very fast ops
   may still quantize. We measure empirical resolution and handle zero/near-zero values. Acceptable?
3. **Dataset size + concurrency sweep defaults.** Proposed dataset ~100k nodes / ~1M edges; default
   sweep `C = 1, 2, 4, 8, 16, 32`. Confirm, or pick presets aligned with the existing `Size` enum.
4. **Failure policy.** Fatal by default; opt-in `--tolerate-failures`. Good?
5. **How much Criterion, now that the in-house engine must exist for `C > 1`.** Keep Criterion as the
   optional `C = 1` baseline/plot path (proposed), or drop it and let the in-house engine own `C = 1`
   too (one report, one code path, no Criterion output-schema dependency — at the cost of
   re-implementing baseline comparison)?
6. **Closed-loop vs open-loop.** Ship the closed-loop achieved-throughput sweep now (answers "latency
   as `C` rises"); add the open-loop offered-rate + coordinated-omission driver later (§8) — agreed?

## 11. Rollout (phased, each independently reviewable)

1. **Scaffold:** `src/synthetic/` skeleton, connection pool + client deadline, `op_runner` with paired
   capture + `stats.rs`, the closed-loop `engine.rs` for a single op (`match_by_index`) at `C = 1`,
   JSON report, `just` recipes.
2. **Concurrency sweep** across a configurable `C` list + latency-vs-throughput table.
3. **Dataset generator** (knobs, indexes, seeded per-worker corpus + `corpus_hash`).
4. **Full read catalog.**
5. **Write catalog** with per-worker scratch namespaces, deterministic keys, `Result` hooks,
   run-level reset cadence, `merge_hit`/`merge_miss`, deterministic `create_edge` identities.
6. **Report polish** (Markdown, metadata, percentiles) + optional Criterion `C = 1` baseline path +
   `corpus_hash`-guarded `synthetic-compare`.
7. *(future)* Open-loop / coordinated-omission driver reusing catalog/dataset/runner/stats.

## 12. Worked example (input → output)

### 12.1 Input — configuration (env / config file, not Criterion flags)

```toml
# synthetic-bench.toml
seed               = 42
nodes              = 100_000
edges              = 1_000_000
warmup_secs        = 3
window_secs        = 10          # measurement window per (op, concurrency)
concurrency        = [1, 2, 4, 8, 16, 32]   # the sweep; use [1] for pure single-flight
server_timeout_ms  = 5_000
client_deadline_ms = 6_000
reset_every        = 50_000      # write-scratch reset, in OPERATIONS (run-level count)
operations         = ["match_by_index", "expand_hops_5", "create_node", "merge_miss"]
```

```bash
just synthetic-bench                     # the whole sweep, all configured ops
just synthetic-bench-one match_by_index  # one op across the sweep
just synthetic-baseline v4.2.1           # save the optional C=1 Criterion baseline
just synthetic-compare  v4.2.1           # compare current build vs it (corpus_hash-guarded)
```

### 12.2 Input — an operation + its pre-generated corpus

```rust
OperationSpec {
    name: "match_by_index", kind: Read,
    // deterministic per-worker slice of the id space, generated ONCE, untimed:
    corpus: |rng, ds, w, n| ds.sample_ids_for_worker("Person", w, n, 1000).into_iter().map(|id|
        QueryBuilder::new().text("MATCH (p:Person {id: $id}) RETURN p").param("id", id).build()
    ).collect(),
    write: None,
}
// worker 0 corpus: {id: 5123}, {id: 88134}, {id: 240}, … (disjoint from worker 1's slice)
```

### 12.3 Output — console (per op, latency vs throughput)

```text
match_by_index    (warm; cached_false 0.5%)
  C     throughput(ops/s)   server p50/p99 (ms)   total p50/p99 (ms)
  1              2,950         0.081 / 0.20          0.33 / 0.9
  8             18,100         0.100 / 0.60          0.44 / 2.1
 32             41,200         0.350 / 3.40          0.78 / 7.9        <- knee: throughput flattens, p99 climbs
```

### 12.4 Output — `synthetic-report.json`

See §7 (per-operation `levels[]`, each with `throughput_ops_s` + `server_ms`/`total_ms` percentiles).

### 12.5 Output — version comparison (optional Criterion, C=1 single-flight)

```text
$ just synthetic-compare v4.2.1
match_by_index/total_ms   change: [-9.4% -8.1% -6.7%] (p = 0.00 < 0.05)   Performance has improved.
```

## 13. Trade-offs of each option (chosen → alternative)

| Decision | Chosen | Alternative | Trade-off |
|---|---|---|---|
| **Harness** | In-house closed-loop engine (all `C`) + optional Criterion for `C = 1` | Criterion-only / fully in-house | Criterion can't do `C > 1` or paired metrics, so the engine must exist; keeping Criterion for `C = 1` gives free baselines/plots but adds a second stats path + a pinned schema dependency. Dropping it = one report/one path but we re-implement baseline comparison. |
| **Concurrency model** | Closed-loop `C`-in-flight **sweep** (achieved throughput) | Open-loop fixed arrival-rate (offered throughput) | Closed-loop is simple and directly answers "latency as `C` rises", but under-measures tail under stall (coordinated omission). Open-loop gives honest offered-load tails but needs rate control + CO correction — deferred (§8). |
| **Granularity** | Cypher **primitives** | Plan operators via `GRAPH.PROFILE` | Primitives are simple/stable but `server_ms` is whole-query; PROFILE gives per-operator cost but is complex and has its own overhead. |
| **Dataset** | **Synthetic, seeded** | Existing IMDB/Pokec fixtures | Synthetic is reproducible/parameterizable/comparable but must be built; existing is realistic but drifts and isn't reproducible. |
| **Write isolation** | **Steady-state**, per-worker scratch + deterministic keys + run-level reset | Reset-each-sample | Steady-state scales to high `C`/sample counts but sawtooths scratch size; reset-each is most isolated but its reset dominates runtime. |
| **Cold/warm** | **Warm** | Cold / both | Warm = low variance, hot-path latency, but hides plan-compile cost; both captures compile cost but is high-variance and slower. |
| **Outliers** | **Remove** severe for the reported latency | Report-and-resist (Criterion default) | Removal matches the "outliers removed" ask; the optional Criterion `C = 1` path keeps its own convention (documented). |
| **Vendors** | **FalkorDB only** | All three now | Focused/fast; multi-engine needs per-engine timing + 3× surface — deferred behind the vendor-agnostic runner. |
| **Connection** | **1 conn at `C = 1`, pool of `C` otherwise** | Multiplexed default (8 sockets) | Explicit pooling gives honest per-level behavior; the multiplexed default would confound isolation and per-`C` accounting. |
| **Failure policy** | **Fatal** on error/timeout/missing-stats | Opt-in tolerate-failures | Fatal avoids biased means; tolerate (retry-to-successes, report failures) helps flaky envs. |

---

### Appendix: why not a fully bespoke stats implementation everywhere?

The in-house engine already owns the concurrency sweep and the paired server/residual stats (Tukey +
bootstrap over its own samples). We keep the option of Criterion **only** for the `C = 1` single-flight
headline, because its saved **baselines** and HTML plots give version-comparison for free and match
FalkorDB-org convention. If §10.5 lands on "fully in-house", Criterion is dropped and the engine owns
`C = 1` too — everything else in this design is unchanged.
