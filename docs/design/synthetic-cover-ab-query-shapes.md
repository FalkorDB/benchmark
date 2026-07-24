# Design — cover the A/B benchmark's query shapes in the synthetic per-op check

**Status:** **reads-scope implemented and merged** — Phases 1–5 landed in PRs #240–#250; the per-PR
non-divergence gate is being hardened to mirror the CI matrix (#251, in flight). **Writes (Phase 7)**
and **algorithms (Phase 6)** are **deferred** by the reads-first decision. This is now a **living
design + status record**, kept in sync as work lands — see the status table (§0) and the per-phase
markers in §3/§7. §2/§3 describe the **pre-implementation baseline** that motivated the work; the
✅/⛔ markers track current state. Rubber-duck reviewed; this revision folds in the review's five
blocking corrections (the first draft materially understated the work — see §11).

**Legend:** ✅ implemented & merged · 🚧 in progress · ⛔ deferred (own design).
**Goal:** let the **synthetic** per-op regression check probe the query shapes the **A/B benchmark**
measures (the 64 named shapes in `src/queries_repository.rs`, surfaced in the
[per-query trend](https://falkordb.github.io/falkordb-rs-next-gen/benchmark/trend/)), so a per-op
regression on those shapes is caught deterministically per-PR — not just the ~9 hand-curated synthetic
ops we probe today.
**Reality check up front:** this is **not** a small "reuse the query list" change. The synthetic
pipeline is `OpName`-enum-centric, deterministic-by-construction, read-only, and against a minimal
fixture; the A/B repo is name-based, random, read+write, against a richer fixture. Closing that gap is
a **multi-PR effort with three hard prerequisites** (dynamic op identity, result canonicalization,
per-op runtime budgets) that must land **before** any shapes are added.

## 0. Implementation status (kept in sync)

| Phase | What | Status |
| --- | --- | --- |
| 1a | String-keyed dynamic op identity (`OpKey` / `OpKey::dynamic`) + name-derived salt | ✅ #241 |
| 1b | Recursive `FalkorValue` result canonicalization (`canonical_row` / `canonical_value`) | ✅ #240 |
| 1c | Per-op runtime budgets (`OpBudget`) + core/nightly tiers (`Tier`, `--tier`) + lean machine-usable summary | ✅ #243, #244, #245 (report size: #238) |
| 2 | Seedable named generation API (`generate_with_rng(&mut dyn Rng)`) | ✅ #242 |
| 3 | Baseline fixture parity (age index + deterministic `bench_capacity`) + string-`OpKey` record/replay plumbing + the ~46 Baseline reads via `--repo-reads` | ✅ #246 (3a), #247 (3b), #248 (B2) |
| 4 | ExtendedCore `temporal_spatial_roundtrip` read | ✅ #249 |
| 5 | Fulltext/vector fixture + fixture-dependent reads | ✅ #250 |
| — | Per-PR non-divergence gate mirrors the CI matrix (uncached, C=1 & C=8) | 🚧 #251 |
| 6 | Algorithms (capability-gated, per-op budgets, determinism exclusions) | ⛔ deferred |
| 7 | Writes (base-fixture mutation + non-determinism; needs a state-isolation design) | ⛔ deferred (own design) |

**Decisions (locked):** **reads-first** (writes/algorithms deferred to their own designs);
non-canonicalizable LIMIT/top-k/float shapes are **excluded from strict result gating** (still timed,
digest `N/A`); offline capability gaps are **recorded and skipped as `N/A`**; per-PR core-subset
wall-clock budget target **≈5 min**; coupling = **bridge + port fallback** (reuse `queries_repository`
as one source of truth, port only the few shapes it can't render deterministically); the per-PR gate
runs **uncached at C=1 and C=8**.

## 1. What the trend report contains

`trend.json` lists **92 entries**: **64 named query shapes** (defined by 64 `add_query(name, kind,
|RandomUtil, Flavour| -> Query)` calls in `src/queries_repository.rs`) plus **28 raw Cypher strings**.
The 28 are **not one bucket** (§6): loader DML (`UNWIND [{src:…}]` edge-import batches,
`src/falkor/falkor_driver.rs:903-927`), fixture DDL/DML (`CREATE …INDEX`, `SET u.ft_text/…`),
readiness/capability probes (`db.indexes`, `db.meta.stats`, `dbms.procedures`), and driver schema
refreshes (`DB.LABELS/PROPERTYKEYS/RELATIONSHIPTYPES`,
`vendor/falkordb-rs/src/graph_schema/mod.rs:15-20`). **The 64 named shapes are the workload source of
truth**; the 28 are infrastructure, handled per-category, not turned into ops.

## 2. Today: two divergent query systems

| | A/B benchmark | Synthetic check |
| --- | --- | --- |
| Shapes | `queries_repository.rs` — **64** via `add_query(…)` call sites (`:385-1073`) | `synthetic/catalog.rs` — **9 read + 6 write** static `OperationSpec`s (`:134-467`) |
| Identity | query **name** (+ builder-assigned `u16` id, unstable — see §3.1) | **`OpName` enum** + fixed `salt()` (`synthetic/mod.rs:83-194,170-173`); CLI/replay/thresholds all key on `OpName` (`cli.rs:9-40`, `recording.rs:361-365`, `thresholds.rs:149-195`) |
| RNG | `RandomUtil` — **not seeded** (`rand::random_range`, `:231-243`) | `StdRng::seed_from_u64(seed ^ salt)` + `splitmix64` dataset (`recording.rs:203-210`, `dataset.rs:32-66`) — deterministic |
| Reads/writes | both (server `rand()`/`timestamp()`, `DETACH DELETE`, `:404-407,839-880`) | reads replayed; writes **rejected** by record/replay (`recording.rs:237-241`, `replay.rs:79-89`); write ops use run-unique scratch labels that never touch base data (`writes.rs:1-18`) |
| Fixture | `:User(id)` **and** `:User(age)` indexes + `bench_capacity` (ensured **unconditionally**) + fulltext/vector indexes + `ft_text`/`embedding` (`main.rs:1158-1168,1378-1394`, `falkor_driver.rs:651-842`) | only `:User{id,age}` + `:Friend` + **id index**, property-less edges (`dataset.rs:258-320`) |
| Gate | aggregate trend, random sampling | `workload_hash` + per-op `result_digest`, byte-stable corpus (the `synthetic-verify` non-divergence gate) |

## 3. The gap (five blockers, verified)

### 3.1 Synthetic op identity is static `OpName`, not dynamic names
> **✅ Resolved — #241** (string-keyed `OpKey` / `OpKey::dynamic`, name-derived salt).
Ops are an enum; the CLI selector, recording lookup, and thresholds all key on `OpName`
(`synthetic/mod.rs:83-194`, `cli.rs:9-40`, `recording.rs:361-365`, `thresholds.rs:149-195`). A repo
name absent from `OpName` **cannot be selected, recorded, thresholded, or replayed**. The repo's
`u16` ids are also assigned **after** conditional inserts (algorithms are added conditionally before
later shapes, `queries_repository.rs:181-184,686-838`), so they're **not stable** — unrelated repo
edits would shift ids and invalidate baselines, violating the synthetic fixed-salt invariant
(`mod.rs:170-173`). ⇒ **There is no "automatic flow"** (the first draft's central claim was wrong).
**Fix:** a **dynamic op descriptor keyed by the stable query name**, with a name-derived salt and
explicit `kind`/`profile`/`capability` metadata; make thresholds + recording lookups **string-keyed**.

### 3.2 Determinism: RNG at record time; results already sorted; the real blocker is `FalkorValue`
> **✅ Resolved — #240** (recursive `FalkorValue` canonicalization); non-canonicalizable LIMIT/top-k/
> float shapes are excluded from strict result gating (still timed, digest `N/A`) per the locked decision.
`synthetic-verify` records **once** and replays the recorded **strings** twice (`Justfile:314-321`,
`replay.rs:78-151`) — so seeded-RNG determinism matters **at record time only**, not replay. Result
rows are **already canonicalized + sorted before hashing** (`op_runner.rs:129-165`), so the first
draft's "add `ORDER BY`" idea is **wrong** — it would change the measured shape. The **actual**
blocker: non-scalar values (nodes/edges/paths) fall back to `Debug` (`op_runner.rs:193-201`), whose
`HashMap` property order is **process-unstable** (`vendor/falkordb-rs/src/value/graph_entities.rs`),
and some shapes return an **unordered subset** (`algo_pagerank_summary`/`entity_path_introspection`
`LIMIT 1` without `ORDER BY`, vector/fulltext top-10, float algo scores —
`queries_repository.rs:687-710,989-1095`). **Fix:** recursive, key-sorted canonicalization for
**every** `FalkorValue` variant (sort map/property keys, preserve array/path order); a per-shape
result policy; fix or exclude the inherently non-deterministic `LIMIT`/top-k/float shapes (a
synthetic-only `ORDER BY` would no longer measure the same shape).

### 3.3 Repo writes are incompatible with the current write isolation
> **⛔ Deferred** — writes are out of the reads-first scope; they need their own state-isolation design (Phase 7).
Record/replay reject writes (`recording.rs:237-241`, `replay.rs:79-89`); synthetic write isolation is
run-unique scratch that must **never** touch base `:User` data (`writes.rs:1-18`,
`catalog.rs:229-467`). Repo writes instead mutate/delete the **base fixture** and carry
non-deterministic state — server `rand()` (`:404-407`), `timestamp()`/`date()` (`:839-862`),
`detach_delete_user` (`:866-870`), and `remove_user_property_and_label` which is a **no-op unless
specially prepared** (`:872-880`). **Fix:** a **separate write design** (per-shape setup/reset/expected
mutation/restore, likely snapshot or reload); until then the acceptance criterion is **reads only**,
not "all 64".

### 3.4 Fixture parity is bigger than one index, and the A/B fixture isn't directly reusable
> **✅ Resolved — #246** (baseline fixture parity: age index + deterministic `bench_capacity`),
> **#249** (ExtendedCore), **#250** (fulltext/vector fixture; offline capability gaps recorded as `N/A`).
The A/B baseline has **both** `:User(id)` and `:User(age)` indexes and ensures `bench_capacity`
**unconditionally** (`main.rs:1158-1168,1378-1394`; readiness checks both, `falkor_driver.rs:260-304`).
A Baseline shape, `var_len_with_edge_where_filter`, filters `bench_capacity` (`:919-936`) — it *runs*
on today's synthetic graph but always returns **zero**, so it doesn't exercise the intended shape. The
fixture builder is also **not drop-in reusable**: `Falkor::client()` hardcodes graph `"falkor"`
(`:357-375`) and the fixture method bundles index creation + up to **90 s** of readiness probing
(`:651-842`), while synthetic loading has only Index/Nodes/Edges phases and count checks
(`dataset.rs:225-255`). And recording is **offline**, so capability-gated selection can't happen at
record time. **Fix:** add the age index + deterministic `bench_capacity` (reuse `data_prep`) to the
baseline generator; extract **graph-name-agnostic** fixture/probe/readiness helpers over `AsyncGraph`;
add fixture load + validation phases; add the missing **ExtendedCore** phase
(`temporal_spatial_roundtrip`, `:995-1020`); decide offline-capability behavior (fail vs record-and-
skip-as-N/A vs server-aware recording).

### 3.5 Runtime is the dominant cost and has no budget knobs
> **✅ Resolved — #244** (per-op `OpBudget`) + **#243** (core/nightly `Tier` + `--tier`) +
> **#245/#238** (lean machine-usable summary + collapsed report sections).
`samples`/`warmup` are **per worker** (`engine.rs:116-131`) and each replay executes every corpus
command once for reference **plus** once per max-concurrency worker (`replay.rs:133-199,299-323`). At
today's verify settings (corpus 256, concurrency sum 63, samples 200, warmup 50, 2 modes) that's
**~39,948 queries/op/replay ≈ 79,896 across a record+replay pair** — i.e. **~3.7 M executions for 46
baseline reads, ~4.3 M for all 54 reads**. Sweeps are applied **per op** (`mod.rs:788-837`). There is
**no per-op sample/timeout/concurrency/cache-mode/verification budget** today, so "smaller algorithm
samples" is impossible without new machinery. **Report size** compounds it: 46–54 reads × 6
concurrencies × 2 cache modes = **552–648 cells** (×768 if writes+algos), which blows GitHub's **65 KB**
sticky-comment cap even with #238's collapsed `<details>`; and CI currently posts the **whole body**
and deletes the detail files on success (`ci.yml:123-132`, `Justfile:347-349`). **Fix (prerequisite,
not phase 6):** add **per-op execution policy** (samples/timeout/concurrency/cache/verify) + explicit
**core vs nightly** tier metadata; switch the sticky comment to **lean (summary + 🔴/N/A ops) with the
full report in the job summary + artifact**.

## 4. Approach — reuse `queries_repository` behind a deterministic, string-keyed synthetic op source

Two ways to get the 64 shapes in:

- **(Recommended) Bridge + seed + generalize.** Make repo generation seedable, introduce a
  **string-keyed dynamic synthetic op** over the repo catalog, canonicalize results, and add per-op
  runtime budgets + a lean comment. Keeps **one workload source of truth** (`queries_repository`), but
  — corrected from the first draft — this is **explicit plumbing**, not automatic.
- **(Alternative) Port** all 64 into `catalog.rs` as `OperationSpec`s. Native fit for the synthetic
  model, but duplicates 64 bodies → guaranteed drift. Kept only as a **per-shape fallback** for shapes
  the bridge can't render deterministically (§3.2).

### 4.1 Seedable RNG seam (feasible; adjust the seam)
Verified blast radius: 64 closures (50 use `random`, 14 don't), 56 `RandomUtil` accesses, `RandomUtil`
confined to `queries_repository.rs`. A bare `&mut dyn Rng` loses `vertices`/`random_vertex`/
`random_path`, so the lower-churn seam is **`RandomUtil<'a> { rng: &'a mut dyn Rng, vertices, edges }`**
+ a `generate_with_rng(&mut dyn Rng)` entry, keeping today's entropy-seeded `generate()` as the
compatibility path. If we *also* want reproducible **A/B** streams, the seed must additionally cover
pool/read-write/algorithm selection (`:263-298,344-371`); note Bolt param serialization isn't sorted
(`query.rs:19-31`) and `StdRng` isn't stable across `rand` versions (`recording.rs:7-11`) — so the
synthetic corpus must be pinned by the **rendered Cypher+params text**, not by trusting cross-version
RNG.

## 5. Determinism & the `synthetic-verify` gate
Holds iff: record-time corpus is byte-identical for a seed (§4.1); dataset/fixture is byte-identical
(`splitmix64`, already deterministic); **and** every shape's result canonicalizes stably (§3.2 —
recursive `FalkorValue` canonicalization + per-shape policy, not `ORDER BY`). Shapes that can't be
stabilized are excluded from strict result gating with a noted reason (still timed, digest marked
N/A).

## 6. Scope: in / out
- **Workload source of truth:** the 64 named shapes. **Reads first** (§3.3 defers writes).
- **Not ops:** loader DML, fixture DDL/DML, readiness/capability probes, driver schema refreshes (the
  28 raw entries, classified per §1 — *not* all "fixture").
- **Gated/opt-in/coarse:** algorithms (capability-gated, per-op budget, excluded from `--all-reads`);
  writes (separate design); introspection shapes (optional).

## 7. Phasing (reordered per review — prerequisites first, each its own PR; status per item)
1. ✅ **(#241, #240, #243, #244, #245)** **Foundations (no new shapes):** (a) dynamic **string-keyed op
   identity/metadata** + name-derived salt; (b) recursive **`FalkorValue` result canonicalization**;
   (c) **per-op runtime/report policy** (budgets + core/nightly tiers + lean comment/artifact).
2. ✅ **(#242)** **Seedable named generation API** in `queries_repository` (§4.1); prove byte-identical
   rendered corpus for a seed; A/B behavior unchanged.
3. ✅ **(#246, #247, #248)** **Baseline fixture parity** (age index + deterministic `bench_capacity`)
   **then** the ~46 Baseline non-algorithm reads.
4. ✅ **(#249)** **ExtendedCore** (`temporal_spatial_roundtrip`).
5. ✅ **(#250)** **Fulltext/vector fixture** (graph-name-agnostic helpers + readiness + capability
   policy) → the fixture-dependent reads.
6. ⛔ **(deferred)** **Algorithms** — capability-gated, per-op budgets, determinism exclusions.
7. ⛔ **(deferred, own design)** **Writes** — under a **separate** state-isolation design (§3.3).

## 8. Risks & open questions
1. **Runtime/size (§3.5)** — biggest: **core-subset per-PR + full nightly** (recommended) vs full set
   every PR. Needs a wall-clock budget and the lean-comment/artifact change **before** Phase 3.
2. **Result determinism (§3.2)** — canonicalization must cover every `FalkorValue`; a few LIMIT/top-k/
   float shapes may be excluded from strict gating.
3. **Dynamic-op refactor (§3.1)** — touches CLI/recording/thresholds; must stay compatible with the
   existing `OpName` ops (they keep working; new ops are string-keyed).
4. **Fixture parity (§3.4)** — reuse the A/B FalkorDB fixture *statements* verbatim via shared helpers;
   avoid a second, divergent fixture; handle offline capability gating.
5. **Writes (§3.3)** — out of "reads" scope; separate design; don't claim "all 64" until then.
6. **Coupling** — after Phase 2 the synthetic op set is *derived from* the repo catalog but still
   needs explicit metadata (kind/profile/capability/budget) per shape — confirm that curated coupling
   is desired over full automation.

## 9. Acceptance (revised)
Per-PR gate covers a **core read subset**; the **full read set** runs nightly/opt-in; **writes and
algorithms are opt-in** behind their own designs. "All 64 every PR" is explicitly **not** the initial
target (runtime/size + writes/algos determinism).
> **✅ Met for reads** — the core read subset gates every PR (uncached, C=1 & C=8) and the full read
> set is tier-selectable (`--tier`); writes (Phase 7) and algorithms (Phase 6) remain deferred.

## 10. Rollout
Land the phases in `FalkorDB/benchmark`; `falkordb-rs-next-gen` picks each up on the next
`SYNTHETIC_BENCHMARK_REF` bump. The per-PR synthetic job stays non-blocking; `synthetic-verify` guards
every phase.
> **Status:** Phases 1–5 landed (#240–#250); the `synthetic-verify` non-divergence gate now mirrors the
> per-PR matrix (`--repo-reads full` × C=1,8 × uncached) — #251, in flight.

## 11. What the rubber-duck corrected (so reviewers can trust this revision)
1. **No "automatic flow"** — synthetic is `OpName`-static; needs a string-keyed dynamic op (§3.1).
2. **Determinism was wrong** — RNG matters at record not replay; rows are already sorted; the real
   blocker is `FalkorValue` `Debug`/`HashMap` + unstable `LIMIT`/top-k/float, needing canonicalization,
   not `ORDER BY` (§3.2).
3. **Writes don't fit** current isolation and are non-deterministic — separate design; reads first
   (§3.3).
4. **Fixture parity** is age-index + unconditional `bench_capacity` + non-reusable graph-hardcoded
   loader + offline capability gating + missing ExtendedCore (§3.4).
5. **Runtime/size** are the dominant cost (per-worker samples → millions of executions; 65 KB cap) and
   must be **prerequisites**, not a final phase (§3.5).
