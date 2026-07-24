# Design — cover the A/B benchmark's write shapes in the synthetic check (Phase 7)

**Status:** proposal for review — **not implemented**. Draft for maintainer review before any code
(per the workflow). **Rubber-duck reviewed**; this revision folds in the review's corrections — the
first draft's "counters are deterministic" thesis was wrong (see §11). Follows the reads-scope work
(design [`synthetic-cover-ab-query-shapes.md`](./synthetic-cover-ab-query-shapes.md), Phases 1–5,
merged in PRs #240–#250) and is the sibling of the algorithms design (Phase 6). The parent design
deferred writes to "their own state-isolation design" (§3.3) — this is that design.

**Headline (corrected):** the A/B **write** shapes are **fundamentally harder** than reads and there is
**no cheap deterministic correctness signal**. They mutate **base** `:User`/`:Friend`, carry server
non-determinism (`timestamp()`/`date()`/server `rand()`), and — critically — FalkorDB's mutation
**counters count *actual* value/topology changes**, so they depend on accumulated state, MERGE
create-vs-match order, and prior writes in the cycled corpus. So Phase 7 splits into an **achievable
latency-only tier** (the A/B trend goal) and a **harder, partial correctness tier** built on an
online-recorded per-command outcome oracle — both **opt-in / nightly**, **never** in the per-PR gate.

**Legend:** ✅ implemented & merged · 🚧 in progress · ⛔ deferred.

## 1. Scope: the **10** write shapes (corrected inventory)

Every `QueryType::Write` in `queries_repository` (`src/queries_repository.rs:446-475,893-940`) — the
first draft listed 8 and mis-described several:

| Shape | Cypher gist | Mutation | Determinism hazard |
| --- | --- | --- | --- |
| `single_vertex_write` | `CREATE (:User{id})` (existing id) | +1 node (dup id) | grows the graph (drift); counter fixed |
| `single_vertex_update` | `MATCH (:User{id}) SET rpc_social_credit` | `properties_set` 0/1 | **value-change counted** (repeat same value → 0) |
| `single_edge_update` | `MATCH ()-[e:Friend]->() ORDER BY rand() LIMIT 1 SET color,bench_capacity` | var | **server `rand()`** target + value-change |
| `single_edge_write` | `MERGE ()-[:Friend]->() ON CREATE… ON MATCH date()` | create *or* match | edge existence + parallel edges; `date()` |
| `merge_user_insert_path` | `MERGE (:User{id=vertices+r}) ON CREATE timestamp()` | create then match | **order-dependent** (creates once, then matches); `timestamp()` |
| `merge_user_upsert_existing` | `MERGE (:User{id=r}) ON MATCH age,last_seen=timestamp()` | match; props | `timestamp()`; value-change |
| `merge_friend_edge_upsert` | `MERGE ()-[:Friend]->() ON CREATE date()… ON MATCH date()` | create *or* match | edge existence; `date()` |
| `detach_delete_user` | `MATCH (:User{id}) DETACH DELETE u` | −1 node, −**degree** edges | **variable count**; no-op on repeat; needs `relationships_deleted` |
| `remove_user_property_and_label` | `MATCH (:User{id}) REMOVE prop, :Label` | props/labels removed | **no-op unless prepared**; needs removed counters |
| `foreach_loop_mutation` | `FOREACH SET loop_counter=1,2,3` | `properties_set=3` | fixed 3 (updates **one** User thrice — not bulk) |

## 2. What is already in place (and why it is not enough)

The synthetic **live** path benchmarks *synthetic-owned* writes with isolation
(`src/synthetic/writes.rs`, `catalog.rs:272-370`), but the primitives **do not** transfer cleanly:

- **`ExpectedMutation`** is **5 rigid unit variants** and **`MutationStats` has only 4 counters**
  (`writes.rs:221-287`): `nodes_created`/`nodes_deleted`/`relationships_created`/`properties_set` —
  **no** `relationships_deleted`, `properties_removed`, or `labels_removed` (the client exposes them,
  `vendor/falkordb-rs/src/response/mod.rs:109-156`). So `detach_delete_user` and
  `remove_user_property_and_label` are **unrepresentable** today, and `NodeMatched` (which requires
  `properties_set == 0`) cannot model an upsert that matches *and* updates.
- **`verify_mutation` is value-dependent, not value-independent** — FalkorDB counts *actual* changes,
  so `single_vertex_update`/`merge_user_upsert_existing`/`single_edge_update` flap between
  `properties_set` 1 and 0 when a repeated value is already set (observed even at C=1).
- **`WriteScratch`** isolates via a per-worker scratch label + disjoint key band; the A/B shapes hit
  **base** labels at **seeded** ids — no scratch isolation applies.
- **`ResetSchedule`** resets between multi-invocation windows — but the recorded corpus is **256
  commands, cycled** (`catalog.rs:19-22`, `mod.rs:947-954`), and at the default 200 warm-up + 1000
  samples with a large cadence the same command repeats **without** resetting, so MERGE create-vs-match
  and delete no-ops make even the "fixed-count" shapes **non-constant**.

## 3. The gap (five blockers)

### 3.1 Replay is read-only by construction
Recording rejects any `QueryType::Write` (`recording.rs:285-289,359-365`); every recorded command is
tagged `"kind":"read"` (`recording.rs:430-445`); replay renders scratch writes rather than replaying
recorded commands (`mod.rs:947-989`), runs every reference command through `GRAPH.RO_QUERY`
(`op_runner.rs:56,141`), fails closed on writes (`replay.rs:79`), and always measures with
`MeasureTarget::read()` (`replay.rs:222`). **Fix:** a **recorded-write worker source**, a
`GRAPH.QUERY` write measurement path, and a **versioned bundle** carrying the write kind (currently op
kind is *excluded* from `workload_hash`, `recording.rs:62-80,212-230`).

### 3.2 Counters are state/value/order-dependent — no cheap deterministic oracle
Per §2, a constant `ExpectedMutation` is wrong. Deterministic verification needs a **per-command,
per-invocation expected outcome** that accounts for accumulated state. That outcome is only knowable
by **executing the exact command sequence from a known pristine base** — i.e. an **online-recorded
oracle** (record captures each command's *actual* full mutation stats + result, in order). Recording
is currently **offline** (`shapes.rs` renders without a server), so this is a real architectural add.

### 3.3 Base-state isolation needs per-invocation (not per-window) pristine state
Because outcomes accumulate (create-once-then-match, delete-then-no-op), a deterministic oracle needs
the base restored **before each measured invocation** — reloading `graph.jsonl`
(`replay.rs:281-312`, `dataset.rs:403-475`) or `GRAPH.COPY` (present on `falkordb/falkordb:latest`).
That is **expensive** and, since resets run inside `invoke`, would land in **reported throughput but
not sample latency** (`engine.rs:127-135`). **Fix:** per-invocation restore for the correctness tier
(bounded, C=1); latency tier uses a cheaper periodic reset and asserts nothing.

### 3.4 Irreducible server non-determinism
`single_edge_update` picks its target with server `rand()` → the affected edge (and its
value-change counter) is **not reproducible** between record and replay. `timestamp()`/`date()` write
non-reproducible values (though not always non-reproducible *counters*). **Fix:** exclude
`single_edge_update` from the correctness tier (latency-only); for `timestamp()`/`date()` shapes,
verify only the reproducible parts of the outcome.

### 3.5 Restore safety & verification are load-bearing
`--no-load` verifies only node/edge **counts** (`replay.rs`), missing property/label corruption; a
failed write run must **restore on both success and failure**; `workload_hash` hashes bundle files,
not live state. **Fix:** error-safe final restore, forbid `--no-load` for writes, and verify graph
**content** (not just counts) after a write run so a later read recording is not silently polluted.

## 4. Approach — two tiers, latency-first

1. **Latency tier (achievable, primary goal):** a write-capable record/replay path (`GRAPH.QUERY`,
   recorded-write worker, versioned bundle) that **measures write latency/throughput** with periodic
   base-graph reset to bound drift, and **asserts no correctness** (result + counters both untracked).
   This alone delivers per-op **trend** coverage for all 10 write shapes — the A/B trend goal.
2. **Correctness tier (harder, partial, deferred):** an **online-recorded per-command outcome oracle**
   (full `MutationStats` incl. deleted/removed counters) + **per-invocation pristine restore** + C=1,
   for the **deterministic subset only** — excludes `single_edge_update` (server `rand()`) and defers
   `remove_user_property_and_label` (needs prepared state) until the counter model is generalized.
   Replaces the 5-variant `ExpectedMutation` with a **generalized per-invocation expected outcome**.

Selection is an **orthogonal** `--repo-writes` axis (like Phase 6's `--repo-algorithms`), initially
**mutually exclusive** with `--repo-reads` (replay has one global concurrency sweep, `replay.rs:39-57`,
so a mixed bundle cannot express C=1 writes alongside C=1,8 reads).

## 5. Scope: in / out
- **In (latency tier):** all 10 shapes, latency/throughput, periodic reset, opt-in nightly.
- **In (correctness tier, staged):** the deterministic fixed-outcome subset (the two plain
  create/update, the create-once MERGEs, `foreach_loop_mutation`) via the online oracle at C=1.
- **Deferred:** `single_edge_update` (server `rand()`), `remove_user_property_and_label` (prepared
  state), `detach_delete_user`'s variable counts until `relationships_deleted` is added; C>1 writes.
- **Out:** Neo4j/Memgraph variants; any digest gating of write results; any change to the per-PR read
  gate or the A/B `--query-profile`.

## 6. Phasing (each its own PR)
1. **Write-capable record/replay (latency-only):** versioned bundle with hashed write kind,
   recorded-write worker, `GRAPH.QUERY` measure path, periodic base reset. Latency tier for all 10.
2. **Generalized outcome model + full `MutationStats`** (`relationships_deleted`/`properties_removed`/
   `labels_removed`); per-invocation restore primitive.
3. **Online outcome oracle** at record time (capture per-command stats+result), C=1 correctness tier
   for the deterministic subset.
4. **Prepared-state + removal shapes** (`remove_user_property_and_label`) and variable-count
   `detach_delete_user`.
5. **Concurrency** — decide C>1 (per-worker id partitioning) or keep C=1 for correctness.
6. **Docs.**

## 7. Risks & open questions
1. **Reset cost / throughput accounting (§3.3)** — per-invocation restore is expensive and pollutes
   throughput; is the correctness tier worth it, or is latency-only + a separate lightweight
   invariant check enough?
2. **Online recording (§3.2)** — moving writes to online recording is a real departure from the
   offline read recorder; scope it carefully.
3. **Value-change counters (§2)** — even the "deterministic subset" needs the oracle to record the
   *actual* per-command counters, because SET-same-value yields 0.
4. **Server `rand()` (§3.4)** — `single_edge_update` is latency-only forever unless the shape changes.
5. **Bundle/version compatibility** — adding write kind + expected outcomes must not break existing
   read bundles or the `workload_hash` of reads.

## 8. Acceptance
Latency tier: opt-in record + replay of all 10 write shapes on the FalkorDB per-PR image with periodic
base reset, off the per-PR read gate, no correctness assertion. Correctness tier (staged): the
deterministic subset verified at C=1 against an online-recorded per-command outcome oracle with
per-invocation restore; `single_edge_update` and the removal/variable-count shapes explicitly
deferred. A drift-guard binds the shape table to `queries_repository`'s 10 write names.

## 9. Rollout
Land the phases behind `--repo-writes` in `FalkorDB/benchmark`; `falkordb-rs-next-gen` picks each up on
the next `SYNTHETIC_BENCHMARK_REF` bump. The per-PR `synthetic-verify` gate stays reads-only; a
nightly/on-demand job exercises the write shapes.

## 10. What the rubber-duck corrected (so reviewers can trust this revision)
1. **Counters are not value-independent** — FalkorDB counts *actual* changes, so update/upsert
   `properties_set` flaps 1↔0 on repeat even at C=1 (§2/§3.2).
2. **Per-window reset is insufficient** — the 256-command corpus cycles without resetting, so MERGE
   create-vs-match and delete no-ops make even "fixed-count" shapes non-constant; needs
   per-invocation state (§3.3).
3. **There are 10 writes, not 8** — added `merge_user_upsert_existing` and
   `remove_user_property_and_label`; corrected `single_vertex_write` (plain `CREATE`, no `timestamp`),
   `single_edge_write` (a MERGE), and `foreach_loop_mutation` (fixed 3 sets, one User) (§1).
4. **The counter model is too small** — 5 rigid variants + 4 counters cannot express DETACH DELETE or
   REMOVE; needs a generalized outcome + `relationships_deleted`/`properties_removed`/`labels_removed`
   (§2).
5. **Write replay is real architecture** — RO_QUERY-only measurement, scratch-rendering worker,
   `"kind":"read"` records, op kind excluded from `workload_hash`; needs a recorded-write worker +
   versioned bundle (§3.1).
6. **Restore safety & content verification** — final restore on success/failure, forbid `--no-load`
   for writes, verify content not just counts (§3.5). Result policy should be per-shape, not blanket
   N/A.
