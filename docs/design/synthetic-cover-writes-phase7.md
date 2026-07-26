# Design — cover the A/B benchmark's write shapes in the synthetic check (Phase 7)

**Status:** §6.1 **implemented** (write-capable record/replay, latency tier for all 10 write
shapes — recording format v2 with kind-bound `workload_hash`, `--repo-writes` selector,
`GRAPH.QUERY` C=1 measure path with per-cell base reset + verified error-safe final restore);
§6.2 **implemented** (generalized `ExpectedOutcome` model, full 7-counter `MutationStats`,
`restore_base` per-invocation restore primitive); §6.3 **implemented** (online outcome oracle:
`record --oracle <endpoint>` captures per-command `MutationStats` for the oracle-eligible shapes
over each eligible op's **complete command corpus** — twice, determinism proven at record time —
into **recording format v3** with the outcomes bound into the `workload_hash`; a v3 bundle must
carry the oracle for **exactly** the eligible set, full corpus per op — enforced at load, attach
and replay — and the replay report attests the verified coverage (`meta.oracle_verified`); replay
re-verifies every recorded outcome against the engine as an untimed C=1 correctness pass and
hard-fails on divergence, `--require-oracle` refuses oracle-less write bundles (oracle→v2
downgrade guard) and the diff/regression guards treat a one-sided or differing attestation as
not-comparable; **stats only** — write result *values* stay un-captured per §3.4/§5-Out);
§6.4 **implemented** (prepared state as a recorded load phase + `detach_delete_user` /
`remove_user_property_and_label` oracle-eligible — 9 of the 10 shapes covered by the correctness
tier — minting **recording format v4**; **amendment (post-review):** v4 requires the prepared
phase and the nine-op exact set, while **format v3 is frozen** as the §6.3-era layout — the
seven-op eligible set, no prepared phase — so pre-§6.4 v3 bundles keep loading and replaying
under their own exact-set rule; cross-version rehash flips v3↔v4 are refused); §6.5 (concurrency) **not implemented**. **Rubber-duck reviewed**; this revision folds in the review's corrections — the
first draft's "counters are deterministic" thesis was wrong (see §10). Follows the reads-scope work
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

Every `QueryType::Write` in `queries_repository` (`src/queries_repository.rs:446-472,893-942`) — the
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
(`src/synthetic/writes.rs`, `src/synthetic/catalog.rs:272-370`), but the primitives **do not** transfer cleanly:

- **`ExpectedMutation`** *was* **5 rigid unit variants** and **`MutationStats` had only 4 counters**:
  `nodes_created`/`nodes_deleted`/`relationships_created`/`properties_set` —
  **no** `relationships_deleted`, `properties_removed`, or `labels_removed` (the client exposes them,
  `vendor/falkordb-rs/src/response/mod.rs:109-156`). So `detach_delete_user` and
  `remove_user_property_and_label` were **unrepresentable**, and `NodeMatched` (which required
  `properties_set == 0`) could not model an upsert that matches *and* updates. *Resolved by §6.2
  (phasing item 2): `MutationStats` now carries all 7 counters and the generalized `ExpectedOutcome`
  (`src/synthetic/writes.rs`) replaces the variants with per-counter `Exactly(n)`/`Any` expectations.*
- **`verify_mutation` is value-dependent, not value-independent** — FalkorDB counts *actual* changes,
  so `single_vertex_update`/`merge_user_upsert_existing`/`single_edge_update` flap between
  `properties_set` 1 and 0 when a repeated value is already set (observed even at C=1).
- **`WriteScratch`** isolates via a per-worker scratch label + disjoint key band; the A/B shapes hit
  **base** labels at **seeded** ids — no scratch isolation applies.
- **`ResetSchedule`** resets between multi-invocation windows — but the recorded corpus is **256
  commands, cycled** (`src/synthetic/catalog.rs:19-22`, `src/synthetic/mod.rs:947-954`), and at the default 200 warm-up + 1000
  samples with a large cadence the same command repeats **without** resetting, so MERGE create-vs-match
  and delete no-ops make even the "fixed-count" shapes **non-constant**.

## 3. The gap (five blockers)

### 3.1 Replay is read-only by construction
Recording rejects any `QueryType::Write` (`src/synthetic/recording.rs:285-289,359-365`); every recorded command is
tagged `"kind":"read"` (`src/synthetic/recording.rs:430-445`); writes exist only in the **live
`synthetic run` write worker**, which renders scratch writes from templates — never from recorded
commands (`src/synthetic/mod.rs:947-989`); replay runs every reference command through `GRAPH.RO_QUERY`
(`src/synthetic/op_runner.rs:56,141`), fails closed on writes (`src/synthetic/replay.rs:79`), and always measures with
`MeasureTarget::read()` (`src/synthetic/replay.rs:222`). **Fix:** a **recorded-write worker source**, a
`GRAPH.QUERY` write measurement path, and a **versioned bundle** carrying the write kind (currently op
kind is *excluded* from `workload_hash`, `src/synthetic/recording.rs:62-80,212-230`).

### 3.2 Counters are state/value/order-dependent — no cheap deterministic oracle
Per §2, a constant `ExpectedMutation` is wrong. Deterministic verification needs a **per-command,
per-invocation expected outcome** that accounts for accumulated state. That outcome is only knowable
by **executing the exact command sequence from a known pristine base** — i.e. an **online-recorded
oracle** (record captures each command's *actual* full mutation stats + result, in order). Recording
is currently **offline** (`src/synthetic/shapes.rs` renders without a server), so this is a real architectural add.

### 3.3 Base-state isolation needs per-invocation (not per-window) pristine state
Because outcomes accumulate (create-once-then-match, delete-then-no-op), a deterministic oracle needs
the base restored **before each measured invocation** — reloading `graph.jsonl`
(`src/synthetic/replay.rs:281-312`, `src/synthetic/dataset.rs:403-475`) or `GRAPH.COPY` (present on `falkordb/falkordb:latest`).
That is **expensive** and, since resets run inside `invoke`, would land in **reported throughput but
not sample latency** (`src/synthetic/engine.rs:127-135`). **Fix:** per-invocation restore for the correctness tier
(bounded, C=1); latency tier uses a cheaper periodic reset and asserts nothing.

### 3.4 Irreducible server non-determinism
`single_edge_update` picks its target with server `rand()` → the affected edge (and its
value-change counter) is **not reproducible** between record and replay. `timestamp()`/`date()` write
non-reproducible values (though not always non-reproducible *counters*). **Fix:** exclude
`single_edge_update` from the correctness tier (latency-only); for `timestamp()`/`date()` shapes,
verify only the reproducible parts of the outcome.

### 3.5 Restore safety & verification are load-bearing
`--no-load` verifies only node/edge **counts** (`src/synthetic/replay.rs`), missing property/label corruption; a
failed write run must **restore on both success and failure**; `workload_hash` hashes bundle files,
not live state. **Fix:** error-safe final restore, forbid `--no-load` for writes, and verify graph
**content** (not just counts) after a write run so a later read recording is not silently polluted.

## 4. Approach — two tiers, latency-first

1. **Latency tier (achievable, primary goal):** a write-capable record/replay path (`GRAPH.QUERY`,
   recorded-write worker, versioned bundle) that **measures write latency/throughput** with periodic
   base-graph reset to bound drift, and **asserts no correctness** (result + counters both untracked).
   This alone delivers per-op **trend** coverage for all 10 write shapes — the A/B trend goal.
2. **Correctness tier (harder, partial, staged):** an **online-recorded per-command outcome oracle**
   (full `MutationStats` incl. deleted/removed counters) + **per-invocation pristine restore** + C=1,
   covering **9 of the 10 write shapes** — every shape except `single_edge_update` (server
   `rand()`, permanently excluded per §3.4). Delivered in two stages: §6.3 shipped the initial
   7-shape deterministic subset, then §6.4 (phasing item 4) added `remove_user_property_and_label`
   (via the prepared load phase) and `detach_delete_user` (variable counts, exact per-command).
   Replaces the 5-variant `ExpectedMutation` with a **generalized per-invocation expected outcome**.

Selection is an **orthogonal** `--repo-writes` axis (like Phase 6's `--repo-algorithms`), initially
**mutually exclusive** with `--repo-reads` (replay has one global concurrency sweep, `src/synthetic/replay.rs:39-57`,
so a mixed bundle cannot express C=1 writes alongside C=1,8 reads).

## 5. Scope: in / out
- **In (latency tier):** all 10 shapes, latency/throughput, periodic reset, opt-in nightly.
- **In (correctness tier, staged):** the deterministic fixed-outcome subset (the two plain
  create/update, the create-once MERGEs, `foreach_loop_mutation`) via the online oracle at C=1;
  since §6.4 also `remove_user_property_and_label` (against the recorded prepared state) and
  `detach_delete_user` (variable counts, reproducible per-command from the restored base).
- **Deferred:** `single_edge_update` (server `rand()`, §3.4 — outside any oracle); C>1 writes.
- **Out:** Neo4j/Memgraph variants; any digest gating of write results; any change to the per-PR read
  gate or the A/B `--query-profile`.

## 6. Phasing (each its own PR)
1. ✅ **Write-capable record/replay (latency-only):** versioned bundle with hashed write kind,
   recorded-write worker, `GRAPH.QUERY` measure path, periodic base reset. Latency tier for all 10.
2. ✅ **Generalized outcome model + full `MutationStats`** (`relationships_deleted`/`properties_removed`/
   `labels_removed`); per-invocation restore primitive (`replay::restore_base`).
3. ✅ **Online outcome oracle** at record time (capture per-command stats), C=1 correctness tier
   for the initial deterministic subset — 7 of the 10 shapes eligible at that stage; `single_edge_update` excluded
   (server `rand()`, §3.4), `detach_delete_user` + `remove_user_property_and_label` excluded
   until §6.4. The capture covers each eligible op's **complete command corpus** (per-command
   outcomes, no sampling), and a v3 bundle must carry the oracle for **exactly** the eligible
   set — full corpus per op, none anywhere else — enforced at load, attach and replay, with the
   replay report attesting verified coverage (`meta.oracle_verified`), so oracle coverage can
   never silently shrink. A re-hashed **v3→v2 downgrade** is byte-indistinguishable from a
   legitimate latency-tier v2 bundle (v2 hashes never covered oracle data), so it is refused by
   operator expectation instead: `run --recording --require-oracle` rejects (offline) any write
   bundle without an oracle, and the comparison side guards the attestation — `report --diff`
   (strict and `--regression`) treats a one-sided or differing `meta.oracle_verified` as
   not-comparable, renders the per-side attestation as an "outcome oracle" header row, and
   prominently warns when two un-attested runs measured oracle-eligible write ops. Capture is
   error-safe end-to-end: the *initial* setup load failure triggers one recovery restore (a
   combined error when that fails too), and the final restore is content-verified against the
   pristine digests. Capture is a record-time-only cost (~13½ min for the two full passes
   over the 1 000/5 000 repo-writes bundle on the pinned dev image). *Deviation from the sketch
   above:* per-command **result values** are **not** captured — §5-Out rules out digest-gating
   write results and §3.4 makes returned values irreproducible (`rand()`, engine-internal ids),
   so the oracle records the `MutationStats` counters only.
4. ✅ **Prepared-state + removal shapes** (`remove_user_property_and_label`) and variable-count
   `detach_delete_user` — both now **oracle-eligible** (9 of the 10 shapes; only
   `single_edge_update` remains excluded, §3.4). The prepared state is a **recorded load
   phase** (`prepared`, one deterministic constant statement appended to every `--repo-writes`
   `graph.jsonl`: every `User` gains `rpc_social_credit = id % 97` + `:TemporaryLabel`), so it is
   bound into the `workload_hash` and re-established by **every** base restore — each captured
   `REMOVE` performs a real removal (`properties_removed=1`, `labels_removed=1`) and each
   `DETACH DELETE` deletes the target plus its full degree (variable `relationships_deleted`
   recorded per command). *Interpretation note:* the design sketch said "prepared state" without
   fixing a mechanism; a load phase (mirroring §3.4's fixture precedent) was chosen over
   per-command setup statements because the §6.3 per-invocation restore already guarantees the
   state precedes every captured command, with zero new bundle machinery. Write-bundle
   `workload_hash`es change (the prepared statement is hashed); no committed bundle or golden
   pins one. **Format amendment (review round 2):** growing the eligible set in place would have
   re-defined what a valid v3 bundle *is* — a #267-era bundle (7 oracle ops, no prepared phase)
   would retroactively fail the nine-op exact-set check. So §6.4 bundles mint **recording format
   v4** (prepared phase **required**, nine-op exact set, `attach_oracle` upgrades v2→v4), and
   **v3 is frozen** as recorded history: the seven-op eligible set (`LEGACY_V3_ORACLE_OPS`), no
   prepared phase, still loading/replaying/verifying under its own exact-set rule and satisfying
   `--require-oracle`. A v3 bundle carrying a prepared phase, a v4 bundle lacking one, and
   rehashed v3↔v4 version flips are all rejected at load. This phase also root-caused and fixed
   a pre-existing capture/verify flake: the
   per-command loops opened two fresh TCP connections per command (~9 200 rapid connects per
   capture), which stalls macOS Docker port-forwarding into a spurious send timeout — the §6.3
   loops now reuse **one connection per pass** (`restore_base_on`).
5. ⛔ **Concurrency** — decide C>1 (per-worker id partitioning) or keep C=1 for correctness.
6. 🚧 **Docs** — folded into each phase's PR (doc sync is part of each phase's definition of
   done): §6.1's readme + cookbook updates shipped with phase 1.

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
oracle-eligible shapes verified at C=1 against an online-recorded per-command outcome oracle with
per-invocation restore; `single_edge_update` permanently excluded (§3.4), and the
removal/variable-count shapes — deferred by the initial §6.3 stage — delivered in §6.4
(9 of the 10). A drift-guard binds the shape table to `queries_repository`'s 10 write names.

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
5. **Write replay is real architecture** — RO_QUERY-only measurement, scratch rendering confined
   to the live-run write worker,
   `"kind":"read"` records, op kind excluded from `workload_hash`; needs a recorded-write worker +
   versioned bundle (§3.1).
6. **Restore safety & content verification** — final restore on success/failure, forbid `--no-load`
   for writes, verify content not just counts (§3.5). Result policy should be per-shape, not blanket
   N/A.
