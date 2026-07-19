# Synthetic per-operation benchmark — design

Status: **draft for review** (no code yet). Implementation follows only after approval (repo
convention: design first, rubber-duck it, then implement). This revision incorporates a rubber-duck
review that corrected several early assumptions (called out inline as **[review]**).

## 1. Goal

Measure the latency of individual Cypher **operations in isolation**. For each operation, on **every
measured invocation** we capture a *paired* pair of numbers:

- **server time** — the database's own reported execution time, read from the query response
  (FalkorDB's *internal execution time*). Excludes client, driver-parsing and network cost.
- **total time** — wall-clock from *just before* the client sends the request to *just after* the
  client has received the response and drained every row.

Because the two numbers come from the **same invocation**, we can report a *paired*
`total − server` residual. **[review]** That residual is **not** "pure network overhead": it also
includes server queue/wait, the module's own reporting, connection selection and driver
deserialization. We therefore name it **`non_internal_ms`** (everything outside the DB's internal
execution timer), not "overhead".

Measurements are cleaned of outliers using **Criterion.rs's methodology** (Tukey fences +
bootstrap confidence intervals). **The runner is sequential for now** (a single connection, one
operation at a time).

### Decisions locked for this iteration (from review)

| Decision | Choice |
| --- | --- |
| Operation granularity | High-level Cypher **primitives**, each as one isolated query |
| Vendors | **FalkorDB only** (kept vendor-agnostic so Neo4j/Memgraph can follow) |
| Dataset | **Dedicated synthetic dataset**, seeded + reproducible, size knobs |
| Write isolation | **Steady-state** on a pre-sized graph; scratch namespace, periodic reset |
| Cold vs warm | **Warm / steady-state** after warm-up (plan cache primed) |
| Output | Standalone **JSON + Markdown/console report** (UI later) |
| Statistics | **Criterion-like** (warm-up, sampling, Tukey outliers, bootstrap CIs) |
| Harness | **Criterion.rs for total-time + version baselines; a thin in-house pass for the paired server-time/residual** — see §4 |

### Future (designed for, not built now)

1. **Compare op latency between FalkorDB versions** — Criterion's saved **baselines** cover this for
   the total-time metric; our JSON (with the FalkorDB version embedded) covers the server metric.
2. **Latency vs throughput as parallelism scales** — *not* Criterion's model; §8 defines the seam.

## 2. Where the two timings come from (verified against `vendor/falkordb-rs`)

- **server time** — `QueryResult::get_internal_execution_time() -> Option<f64>` (milliseconds),
  parsed from the response stats *"Query internal execution time: N milliseconds"*
  (`vendor/falkordb-rs/src/response/mod.rs`). **[review]** Verified present; value is in ms with
  ~6 decimal places from a high-resolution timer (see precision caveat in §10).
- **total time** — an `Instant` around `graph.ro_query/query(q).with_timeout(ms).execute().await`
  **plus draining the result set**. **[review]** Important correction: the async result is **not
  network-lazy** — rows are eagerly parsed and buffered before `execute()` returns
  (`vendor/falkordb-rs/src/response/row_stream.rs`). So network receipt completes at `execute()`;
  we still drain every row to (a) model real client consumption and (b) surface row-decode errors,
  but we no longer claim draining is "when the bytes arrive".
- **cache state** — `QueryResult::get_cached_execution() -> Option<bool>`. **[review]** This means a
  cached **execution plan/context**, not cached query *results*. We report the share of measured
  invocations with `cached == false` and assert the plan cache is warm during measurement.

Reads use `ro_query`, writes use `query` (mirroring `QueryType::{Read, Write}` in
`src/queries_repository.rs`).

**[review] Connection & deadline caveats**
- The async client **defaults to 8 multiplexed sockets** (`vendor/falkordb-rs/src/client/builder.rs`).
  For honest single-flight latency we must **explicitly configure a single connection** and record
  the strategy in the report.
- `with_timeout(ms)` is a **server-side** guard; the client-side response timeout is disabled by
  default (`vendor/falkordb-rs/src/client/mod.rs`). We add a **Tokio client deadline** around each
  call, mirroring `src/falkor/falkor_driver.rs` (`query_timeout_guard`), so a stuck socket can't hang
  the run.

## 3. What counts as an "operation" (the catalog)

A curated catalog of **primitive** operations, each one parameterized Cypher statement against a
known graph shape. Initial set:

**Reads**
- `match_by_index` — point lookup on an indexed property (`MATCH (n:Label {id:$id}) RETURN n`).
- `match_by_label_scan` — label scan + filter on an **un-indexed** property.
- `expand_1_hop` — traverse one relationship type from a seed node.
- `expand_n_hop` — variable-length traverse. **[review]** Bounds must be **literals** in
  FalkorDB/openCypher, so the depth is fixed per benchmark (e.g. `*1..5`) and encoded in the
  benchmark id (`expand_hops_5`), **not** a `$k` parameter.
- `aggregate_count`, `aggregate_group` — count / group-by aggregation.
- `shortest_path` — `shortestPath` between two connected seed nodes.
- `property_projection` — return a few properties for a bounded match (`LIMIT $n`).

**Writes** (steady-state, §5.3)
- `create_node`, `create_edge`, `set_property`, `delete_node`.
- **[review]** `merge_node` is split into two benchmarks — **`merge_hit`** (key exists → match path)
  and **`merge_miss`** (key absent → create path) — because they exercise different plans.

Catalog entry:

```
OperationSpec {
    name:  &'static str,                 // stable id == Criterion benchmark id
    kind:  QueryType,                    // Read | Write -> ro_query vs query
    // inputs are PRE-GENERATED into a fixed, seeded corpus (see §5.2) rather than built
    // inside the timing loop, so the workload is identical across samples/metrics/versions:
    corpus: fn(&mut Rng, &DatasetHandle) -> Vec<Query>,
    write:  Option<WritePlan>,           // scratch namespace, setup/reset/cleanup hooks
}
```

Queries are built with the existing `crate::query::{Query, QueryBuilder, QueryParam}` and use
**parameters** (not inlined literals) so FalkorDB's plan cache stays warm and stable.

## 4. Harness: what Criterion can and cannot do for us

The user asked: *"can we use Criterion instead of writing the stats logic?"* The honest, corrected
answer after review:

**[review] Criterion cannot give us two *paired* metrics.** Criterion analyzes exactly **one scalar
per sample** (it divides each sample's aggregate by `iters`); it has no notion of a second,
response-reported number, and two separate benchmark ids would measure two **different** invocation
populations — so `total − server` computed across them would not be paired. Confirmed against
`criterion`'s analysis code.

So we split responsibilities:

- **Total time → Criterion (native).** One `iter_custom` benchmark per op measures end-to-end
  wall-clock. Criterion owns warm-up, sampling, Tukey outlier classification, bootstrap CIs, HTML
  plots, and — crucially — **version baselines** (`--save-baseline` / `--baseline`). Zero stats code
  from us for this metric.
- **Server time + `non_internal_ms` → thin in-house pass (paired).** The *same* `iter_custom` loop
  records, per invocation, the paired `(server_ms, total_ms)` into a side buffer. We then compute
  robust statistics **ourselves** over that buffer, reusing Criterion's *methodology* (Tukey-fence
  outlier removal + bootstrap CI) — and, since the user asked for outliers **removed**, we actually
  drop severe (>3×IQR) outliers from our headline server/residual numbers.

**[review] So the answer is "mostly yes":** Criterion eliminates the stats code for the headline
total-time metric and gives version-comparison for free; we write a small, well-scoped robust-stats
helper (~150–200 lines, or reuse `criterion`'s public `stats` utilities) only for the paired
server-time and residual — the parts Criterion structurally cannot produce.

### Corrected measurement sketch (illustrative)

**[review]** The earlier async-closure sketch would not compile: `AsyncGraph::query/ro_query` take
`&mut self`, and a `FnMut` async closure can neither hold a `&mut` borrow across the returned future
nor `move` the driver/RNG out of a repeatedly-called closure. We therefore use a **synchronous**
`iter_custom` that `block_on`s the async work, borrowing the driver mutably only *inside* the block
(no borrow escapes), and we consume **pre-generated** inputs instead of borrowing an RNG:

```rust
let rt = tokio::runtime::Runtime::new()?;
let corpus = op.corpus(&mut seeded_rng, &ds);        // fixed inputs, generated ONCE, untimed
let sink = SampleSink::new(op.name);                 // paired (server_ms,total_ms) side buffer

group.bench_function(op.name, |b| {
    b.iter_custom(|iters| {
        rt.block_on(async {
            let mut wall = Duration::ZERO;
            for i in 0..iters {
                let q = &corpus[(i as usize) % corpus.len()];
                op.write.as_ref().map(|w| w.before(i));      // untimed setup/reset hook
                let t0 = Instant::now();
                let r = run_and_drain(&mut driver, op.kind, q).await;  // fatal on error (§ below)
                let total = t0.elapsed();
                wall += total;
                sink.push(r.server_ms, total.as_secs_f64() * 1e3);     // paired record
            }
            wall                                        // Criterion analyzes total wall-clock
        })
    });
});
```

**[review] Failure handling.** `get_internal_execution_time()` returning `None` must **not** become
`NaN` (`Duration::from_secs_f64(NaN)` panics, and a zero duration makes Criterion abandon analysis).
A measured invocation that errors, times out, or lacks server stats is **benchmark-fatal by
default**; optionally a `--tolerate-failures` mode retries until exactly `iters` *successes* and
reports `attempts`/`failures` separately (never silently averaged in).

**[review] Criterion CLI has no custom flags.** Criterion rejects unknown flags, so dataset knobs
(`nodes`, `edges`, seed, …) come from an **env/config file** read by the bench, not `--nodes`. We
also **pin the `criterion` version**, because the JSON exporter (§7) reads Criterion's output schema.

## 5. Architecture

Self-contained; nothing in the existing run/scheduler path changes.

```
src/synthetic/
  mod.rs          // config, orchestration, Report
  catalog.rs      // OperationSpec catalog (§3)   -- Criterion-agnostic
  dataset.rs      // synthetic dataset + DatasetHandle (§5.2)  -- Criterion-agnostic
  op_runner.rs    // run_and_drain(): one op -> { server_ms, total_ms, rows, cached } -- Criterion-agnostic
  stats.rs        // Tukey removal + bootstrap CI for the paired server/residual metrics
  report.rs       // JSON + Markdown; merges Criterion's total-time output with our server stats
benches/
  synthetic_ops.rs // Criterion harness wiring the catalog (§4)
```

Keeping `catalog`/`dataset`/`op_runner` free of Criterion types is the seam that lets the future
throughput driver (§8) reuse them.

### 5.1 Op-runner (`op_runner.rs`)

```
async fn run_and_drain(driver, kind, query) -> Result<OpSample> {
    let started = Instant::now();
    let result  = with_client_deadline(                       // Tokio guard, §2
        match kind { Read => graph.ro_query(q), Write => graph.query(q) }
            .with_timeout(server_timeout_ms).execute()).await?;
    let cached    = result.get_cached_execution().unwrap_or(false);
    let server_ms = result.get_internal_execution_time().ok_or(MissingServerStats)?;  // no NaN
    let rows = drain(result.data).await?;                     // consume + surface row errors
    Ok(OpSample { server_ms, total_ms: started.elapsed().as_secs_f64()*1e3, rows, cached })
}
```

Single connection is configured explicitly (§2). Reused verbatim by the future concurrency layer.

### 5.2 Synthetic dataset (`dataset.rs`)

A dedicated, **seeded, reproducible** graph built once per run, sized by knobs (env/config, §4):

- `nodes`, `edges`/avg-degree, label & relationship-type counts.
- An **indexed** `id` (for `match_by_index`, `merge_*`, `set_property`), an **un-indexed** property
  (for `match_by_label_scan`), and enough connectivity for `expand_*` / `shortest_path`.
- Creates required **indexes** up front.
- `DatasetHandle` exposes **pre-generated, seeded pools** of valid inputs (existing ids, connected
  pairs) so each op's `corpus` is fixed and identical across samples, metric passes and versions.
- Reuses existing load utilities (`src/data_prep.rs`, the Falkor batch-load path) rather than
  reinventing loading.
- The report embeds a **config/corpus hash** (seed + knobs + catalog) so runs — and Criterion
  baselines — are only compared when the workload truly matches. **[review]** Criterion does not
  validate corpus equivalence itself.

### 5.3 Warm / steady-state and write isolation

- **Warm:** an explicit warm-up per op primes the plan cache before measurement; we assert
  `cached == true` during measurement and report any `cached == false` rate.
- **Reads** are naturally repeatable; the fixed corpus varies parameters over the seed pool.
- **[review] Writes** are timed with the same `iter_custom` loop so that only the operation itself
  is inside the timer — **setup/reset/cleanup run in untimed hooks**:
  - dedicated **scratch namespace** (its own label/rel-type) so cleanup is one cheap delete and never
    touches the read fixture;
  - `create_node`/`merge_miss` use a **monotonic counter** for fresh keys; growth is bounded by a
    **periodic reset** hook (untimed) that recreates the scratch namespace from a snapshot —
    acknowledging this yields a sawtooth in scratch size and does not perfectly restore allocator/
    index state (documented limitation);
  - `set_property`/`create_edge`/`merge_hit` operate on **pre-created scratch seeds**;
  - `create_node` + `delete_node` can be paired to keep net size ~constant.

## 6. Methodology / statistics

- **Total-time (Criterion):** warm-up ~3s, then ≥100 samples or ~5s (whichever larger). Criterion
  **classifies** Tukey outliers (mild 1.5×IQR / severe 3×IQR) and reports mean + bootstrap CI.
  **[review]** Criterion does *not delete* outliers from its headline estimate — it labels them and
  its point estimate is mean/regression-slope over all samples. Baselines enable version diffs.
- **Server-time & `non_internal_ms` (in-house `stats.rs`):** over the paired side buffer we **remove
  severe (>3×IQR) outliers** (what the user asked for), then report n, removed-count, median, mean,
  std-dev/MAD and a bootstrap CI. Because the samples are paired, `non_internal_ms` is a true
  per-invocation `total − server`.
- Every knob (dataset seed/size, warm-up, sample size, server timeout, client deadline, reset
  interval, connection strategy, corpus hash) is recorded in the report header for reproducibility.

## 7. Output

- **Primary:** `synthetic-report.json` — run metadata (git SHA, FalkorDB/module version via
  `CALL dbms.version()` or the module list, dataset seed+knobs+corpus hash, host info via
  `sysinfo`, connection strategy), then per op: `server_ms {median,mean,ci,stddev,n,removed}`,
  `total_ms {…, from Criterion}`, `non_internal_ms {…}`, and `cached_false_rate`. Plus a rendered
  **Markdown/console table**.
- **Secondary:** Criterion's own `target/criterion/**` (HTML, violin plots, version comparison) for
  the total-time metric — free.
- Shape kept close to `src/aggregator.rs` output so UI wiring later is small.

## 8. Designing for the future extensions

**(a) Version comparison.** Total-time: Criterion `--save-baseline <A>` on version A, then
`--baseline <A>` on the next version prints per-op deltas + significance. **[review]** (Compare
against the *saved* name `A`, not a new name.) Server-time: diff two `synthetic-report.json` files
(FalkorDB version embedded). Guard both with the corpus hash so only like-for-like is compared.

**(b) Latency vs throughput under parallelism.** Criterion is single-flight, so a **separate
driver** (new CLI subcommand, e.g. `synthetic-throughput`) reuses the Criterion-agnostic
`catalog.rs` / `dataset.rs` / `op_runner.rs` unchanged, driving the op across increasing
concurrency (reusing `src/scheduler.rs`/pool patterns). **[review] Caveats to honor there:**
- a fixed worker-count sweep measures **achieved** throughput; an **offered**-load curve needs an
  **open-loop, arrival-rate** driver (workers can't just spin) — decide which we want;
- each worker needs its **own scratch namespace** to stay isolated;
- **cloned `AsyncGraph`s share a schema write-lock during response parsing** — a real
  cross-worker coupling to account for when interpreting concurrency results.

## 9. Tooling: `just` + CI

Per repo convention (drive everything through `just`; keep CI identical):

- `just synthetic-bench` → `cargo bench --bench synthetic_ops` (needs a reachable FalkorDB; `protoc`
  already required to build). Dataset knobs via env/config file (§4), not bench flags.
- `just synthetic-bench-one <op>` → Criterion name filter.
- `just synthetic-baseline <name>` / `just synthetic-compare <name>` → save/compare baselines.
- **Not a CI gate** (numbers are machine-dependent) — matches how `falkordb-rs` treats benches. A
  tiny `synthetic-bench-smoke` (one op, minimal samples, tolerate-failures off) can later run in CI
  purely to keep the harness compiling.
- New dev-dependency: **pinned** `criterion` with `async_tokio`; `[[bench]]` with `harness = false`.
  No change to the release binary.

## 10. Open questions / risks

1. **Outliers: remove vs report.** We *report* Criterion's classification for total-time but
   *remove* severe outliers for the in-house server/residual headline. If you also want total-time
   physically de-outliered, we post-process Criterion's `sample.json` (times ÷ iters) and recompute —
   note **individual** invocation outliers can't be recovered from Criterion's per-sample file. OK
   with the split, or force removal on both?
2. **Server-time precision.** FalkorDB reports ms with ~6 decimals from a hi-res timer; for very fast
   ops the value may still quantize. Summing/averaging reduces noise but can't recover lost
   resolution — we'll measure empirical resolution and handle zero/near-zero values. Acceptable?
3. **Dataset size defaults.** Proposed ~100k nodes / ~1M edges, or a small/medium/large preset
   aligned with the existing `Size` enum — which?
4. **Failure policy.** Default benchmark-fatal on error/timeout/missing-stats; opt-in
   `--tolerate-failures` retry-to-success with separate failure reporting. Good?
5. **How much Criterion.** Confirm the split in §4 (Criterion for total-time + baselines; in-house
   thin pass for paired server-time/residual). The alternative is a fully in-house harness that uses
   Criterion's *ideas* but not the crate — more code, but paired stats and a single report for both
   metrics, and no dependency on Criterion's output schema.

## 11. Rollout (phased, each independently reviewable)

1. **Scaffold:** pinned `criterion` dev-dep, `benches/synthetic_ops.rs`, `src/synthetic/` skeleton,
   `just` recipes, single-connection driver + client deadline, one read op (`match_by_index`) with
   paired capture + `stats.rs` + JSON report.
2. **Dataset generator** (knobs, indexes, seeded corpus pools, config/corpus hash).
3. **Full read catalog.**
4. **Write catalog** with scratch namespace + untimed setup/reset hooks + `merge_hit`/`merge_miss`.
5. **Report polish** (Markdown, metadata, `non_internal_ms`, cached-rate) + baseline `just` recipes.
6. *(future)* Throughput/parallelism driver reusing catalog/dataset/runner.

## 12. Worked example (input → output)

### 12.1 Input — configuration (env / config file, not Criterion flags)

```toml
# synthetic-bench.toml
seed               = 42
nodes              = 100_000
edges              = 1_000_000
labels             = ["Person", "Movie"]
rel_types          = ["ACTED_IN", "KNOWS"]
warmup_secs        = 3
sample_size        = 200        # min measured invocations per op
server_timeout_ms  = 5_000      # FalkorDB-side guard (with_timeout)
client_deadline_ms = 6_000      # Tokio-side guard (§2)
connection         = "single"   # single | multiplexed
reset_every        = 5_000      # write-scratch reset interval (untimed)
operations         = ["match_by_index", "expand_hops_5", "create_node", "merge_miss"]
```

Run it:

```bash
just synthetic-bench                     # all configured ops
just synthetic-bench-one match_by_index  # a single op
just synthetic-baseline v4.2.1           # save a baseline (version comparison)
just synthetic-compare  v4.2.1           # compare the current build against it
```

### 12.2 Input — an operation and its pre-generated corpus

```rust
OperationSpec {
    name: "match_by_index",
    kind: Read,
    // generated ONCE, seeded, untimed — identical across samples/metrics/versions:
    corpus: |rng, ds| ds.sample_ids("Person", 1000).into_iter().map(|id|
        QueryBuilder::new()
            .text("MATCH (p:Person {id: $id}) RETURN p")
            .param("id", id)
            .build()
    ).collect(),
    write: None,
}
// corpus (first rows): {id: 5123}, {id: 88134}, {id: 240}, {id: 61802}, …
```

A write op carries a `WritePlan` (scratch namespace + untimed hooks) instead:

```rust
OperationSpec {
    name: "create_node", kind: Write,
    corpus: |rng, ds| (0..1000).map(|_| /* props from seeded rng */ …).collect(),
    write: Some(WritePlan { scratch_label: "_bench_scratch",
                            key: KeySource::MonotonicCounter,   // fresh key per invocation
                            reset: ResetEvery(5_000) }),        // untimed
}
```

### 12.3 Output — console (per op)

```
match_by_index
  total_ms   [criterion] mean 0.34  median 0.33  95% CI [0.32, 0.36]   outliers 6/200 (3%)
  server_ms  [in-house ] median 0.081  mean 0.084  95% CI [0.079, 0.089]   removed 4 severe
  non_internal_ms (paired total−server)  median 0.256
  cached_execution=false: 0.5%
```

### 12.4 Output — `synthetic-report.json` (excerpt)

```json
{
  "meta": {
    "git_sha": "cd0e073",
    "falkordb_version": "4.2.1",
    "dataset": { "seed": 42, "nodes": 100000, "edges": 1000000, "corpus_hash": "9f3a1c…" },
    "connection": "single",
    "host": { "cpu": "…", "mem_gb": 32 },
    "warmup_secs": 3, "sample_size": 200
  },
  "operations": {
    "match_by_index": {
      "server_ms":       { "median": 0.081, "mean": 0.084, "ci": [0.079, 0.089], "stddev": 0.02, "n": 200, "removed": 4 },
      "total_ms":        { "median": 0.33,  "mean": 0.34,  "ci": [0.32, 0.36],   "stddev": 0.05, "n": 200, "outliers": 6 },
      "non_internal_ms": { "median": 0.256 },
      "cached_false_rate": 0.005
    }
  }
}
```

### 12.5 Output — Markdown table (for humans / PRs)

| op | server median (ms) | total median (ms) | non_internal (ms) | n | removed |
|----|-----:|-----:|-----:|--:|--:|
| match_by_index | 0.081 | 0.33 | 0.256 | 200 | 4 |
| expand_hops_5  | 2.41  | 3.02 | 0.61  | 200 | 7 |
| create_node    | 0.12  | 0.40 | 0.28  | 200 | 3 |
| merge_miss     | 0.15  | 0.44 | 0.29  | 200 | 5 |

### 12.6 Output — version comparison (total-time, Criterion baseline)

```
$ just synthetic-compare v4.2.1
match_by_index/total_ms
  time:   [0.302 ms 0.311 ms 0.320 ms]
  change: [-9.4% -8.1% -6.7%] (p = 0.00 < 0.05)   Performance has improved.
```

## 13. Trade-offs of each option

Each row is **chosen → alternative**, with the trade-off. The "chosen" column reflects the
decisions locked in §1.

### Harness — *Criterion + thin in-house pass* → fully in-house
- **Chosen:** Criterion owns total-time (free warm-up, Tukey outliers, bootstrap CI, HTML plots,
  **version baselines**); ~150 LOC in-house computes the paired server-time/residual.
  - *Pros:* least code; free version comparison + plots; matches FalkorDB-org convention.
  - *Cons:* two stats paths in one report; depends on Criterion's output schema (so we pin it); the
    server metric isn't in Criterion's HTML.
- **Alt — fully in-house:** one harness, one report, everything paired.
  - *Pros:* single coherent report; full control; trivial to reuse for the concurrency phase.
  - *Cons:* we re-implement bootstrap CIs, baselines, plots, regression — more code + maintenance.

### Operation granularity — *primitives* → plan-operators (`GRAPH.PROFILE`)
- **Primitives:** one Cypher statement per op; simple, stable, maps to user-facing operations.
  - *Con:* `server_ms` is whole-query, not per-operator.
- **PROFILE:** per-operator timings (Label Scan, Conditional Traverse, Aggregate…).
  - *Pro:* true per-operator server cost. *Con:* more complex, PROFILE has its own overhead, hard to
    isolate a single operator — deferred.

### Dataset — *synthetic, seeded* → existing fixtures (IMDB/Pokec)
- **Synthetic:** reproducible, size knobs, controlled distributions + indexes → comparable across
  versions/machines. *Con:* must build/maintain the generator; may not mirror real-world skew.
- **Existing:** realistic shape, already loaded. *Con:* not reproducible/parameterizable; harder to
  isolate; fixture drift breaks version comparison.

### Write isolation — *steady-state* → reset-each-sample
- **Steady-state:** scratch namespace + fresh keys + periodic (untimed) reset; fast → high sample
  counts. *Con:* sawtooth scratch size; doesn't perfectly restore allocator/index/cache state.
- **Reset-each:** snapshot-restore before every sample; most isolated. *Con:* reset dominates
  wall-time, tiny sample counts — usually impractical for ms-scale ops.

### Cold vs warm — *warm* → cold / both
- **Warm:** steady-state after warm-up; low variance; reflects hot-path production latency.
  *Con:* hides first-execution/plan-compile cost.
- **Both:** also measure cold (cache miss) to capture compile cost. *Con:* high variance, needs a
  cache flush between samples, more runtime — deferred.

### Outliers — *remove for server/residual, report for total* → remove-both / report-both
- **Chosen:** total-time keeps Criterion's "report-and-resist"; server/residual physically drop
  severe (>3×IQR). *Pro:* matches "outliers removed" for our metric while keeping Criterion's
  headline standard. *Con:* two conventions in one report (documented).
- **Remove-both:** also post-process Criterion's `sample.json`. *Con:* diverges from Criterion norms;
  per-invocation outliers can't be recovered from its per-sample file.

### Vendors — *FalkorDB-only* → all three now
- **FalkorDB-only:** focused, one server-time source, fastest to ship.
- **All:** cross-engine comparison, but each engine reports timing differently (Neo4j
  `resultConsumedAfter`, Memgraph metadata…), 3× surface — deferred behind the vendor-agnostic runner.

### Connection — *single* → multiplexed (client default is 8 sockets)
- **Single:** honest single-flight latency (no client-side multiplexing/queuing effects).
- **Multiplexed:** matches some production clients, but confounds *isolated* per-op latency — that
  belongs in the throughput phase (§8).

### Failure policy — *fatal* → tolerate-failures
- **Fatal:** a timeout / missing server stats aborts the op → nothing silently biased into the mean.
- **Tolerate (opt-in):** retry to N successes, report attempts/failures separately — for flaky envs.

---

### Appendix: why not a fully bespoke stats implementation?

Criterion gives identical statistics for total-time **plus** baselines, async support and HTML with
zero maintenance, matching FalkorDB-org convention. We only hand-write the paired server-time /
residual robust-stats pass — the part Criterion structurally cannot produce — plus the
domain-specific catalog, synthetic dataset and JSON/Markdown exporter.
