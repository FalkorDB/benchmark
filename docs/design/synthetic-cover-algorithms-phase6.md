# Design — cover the A/B benchmark's algorithm shapes in the synthetic check (Phase 6)

**Status:** proposal for review — **not implemented**. Draft for maintainer review before any code
(per the workflow). **Rubber-duck reviewed**; this revision folds in the review's corrections — the
first draft understated the work (see §11). Follows the reads-scope work (design
[`synthetic-cover-ab-query-shapes.md`](./synthetic-cover-ab-query-shapes.md), Phases 1–5, merged in
PRs #240–#250): the per-op non-divergence gate now covers the **50 A/B read shapes** but deliberately
**excludes the 4 capability-gated algorithm reads**. This is Phase 6 — cover those 4, **opt-in and
nightly**, so the algorithm latency the A/B
[per-query trend](https://falkordb.github.io/falkordb-rs-next-gen/benchmark/trend/) tracks is probed
per-op, and the deterministic ones gate a `result_digest`.

**Legend:** ✅ implemented & merged · 🚧 in progress · ⛔ deferred.

## 1. Scope: the 4 algorithm shapes

`queries_repository` defines exactly four, listed in `ALGORITHM_QUERY_NAMES`
(`src/queries_repository.rs:23-28`) and **each independently gated** by `AlgorithmQuerySelection`
(`:31-46`, all four enabled by default):

| Shape | FalkorDB Cypher (`src/queries_repository.rs`) | Returns | Whole-graph? |
| --- | --- | --- | --- |
| `algo_pagerank_summary` | `CALL algo.pageRank('User', null) YIELD node, score RETURN score LIMIT 1` (`:748`) | one `score` (float) | yes |
| `algo_max_flow_single_pair` | `CALL algo.maxFlow({… capacityProperty:'bench_capacity'}) YIELD maxFlow` (`:784`) | one `max_flow` (float) for a seeded `(s,t)` pair | no (single pair) |
| `algo_msf_summary` | `CALL algo.MSF({weightAttribute:'bench_capacity'}) YIELD edges RETURN size(edges), reduce(… total_weight)` (`:827`) | `edge_count` (int) + `total_weight` (float) | yes |
| `algo_harmonic_summary` | `CALL algo.HarmonicCentrality() YIELD node, score RETURN count, avg, max` (`:869`) | `node_count` (int) + `avg_score`/`max_score` (float) | yes |

Synthetic records the **FalkorDB flavour only** (`read_shapes_repository` builds `Flavour::FalkorDB`,
`src/synthetic/shapes.rs`), so the Neo4j `gds.*`/projected-`benchmark_algo_graph` and Memgraph
variants are out of scope — only FalkorDB's `algo.*` procedures matter.

## 2. What is already in place (built by Phases 1–5)

- **String-keyed ops** (`OpKey`, #241) and the **`result_policy`** mechanism
  (`ResultPolicy::{Gated, NotApplicable(reason)}`, #240) — the gate/N-A machinery an algorithm op
  needs already exists (Decision 4). Canonicalization hashes floats via `f64::to_bits()`
  (`src/synthetic/op_runner.rs:193-206`), so a **gated** algorithm's float digest is exact — the only
  question is run-to-run *value* stability (§6), not formatting.
- **Coverage tiers** (`Tier::{Core, Full}`, #243) — algorithms must never enter the always-on Core
  per-PR subset. **But `Tier::Full` alone is not enough to keep them out of the per-PR gate** (§3.2).
- **Capability annotation** (`ShapeCapability`, Phase 5) — an extensible, machine-readable label; a
  per-procedure algorithm capability slots in (§3.5).
- **`bench_capacity`** is written deterministically on **every** generated `:Friend` edge
  (`src/synthetic/dataset.rs:311-330`), exactly what `algo.maxFlow` (`capacityProperty`) and
  `algo.MSF` (`weightAttribute`) read — so **pageRank / Harmonic / MSF need no new fixture**. `maxFlow`
  is the exception (§3.3).

## 3. The gap (five blockers, verified)

### 3.1 `algo.maxFlow` needs a **simple** graph; the synthetic generator does not guarantee one
The generator forbids self-loops but **not parallel edges** — `edges_are_deterministic_and_never_self_loops`
(`src/synthetic/dataset.rs:680-684`) asserts only `a != b`, and `edge_at` (`:119-150`) can emit
duplicate `(src,dst)` pairs. On a multigraph FalkorDB's `algo.maxFlow` errors
(`relationship type must not contain multi-edges (tensors)`), and the exact CI oracle fixture
(`seed=7`, 1000/5000, `Justfile:322-324`) contains duplicate endpoint pairs. So "no new fixture" is
**false for maxFlow**. **Fix:** a deterministic **simple-graph guarantee** for the algorithm fixture
(dedupe `(src,dst)` at generation, keeping the count/`bench_capacity` deterministic) or an
algorithm-specific simple fixture; add a "no parallel `:Friend` edges" test + a live maxFlow smoke
test. *(Empirically confirm the dup-pair count and the tensors error first.)*

### 3.2 `Tier::Full` cannot double as "algorithm opt-in"
`--repo-reads` is `Option<Tier>` (`src/cli.rs:542`); selection filters **only** by tier
(`shapes.rs:selected_shapes`); `Tier::Full` is the *complete read set* and `Core ⊆ Full`
(`mod.rs:340-352`); and CI already runs **`--repo-reads full`** (`Justfile:323-324`). So adding
Full-tier algorithms to `repo_read_shapes()` would **silently pull them into every PR**, while keeping
them separate makes them unreachable by tier. **Fix:** an **orthogonal** selection dimension — a
`--repo-algorithms` flag (or `RepoShapeSelection::{Reads(Tier), Algorithms}`) — leaving
`repo_read_shapes()` and `--repo-reads full` as **exactly** today's 50 reads.

### 3.3 The record path hard-excludes algorithms, and the profile enum is shared with the A/B CLI
`read_shapes_repository()` always builds with **`no_algorithms()`** and `record_selected_shapes()`
always builds a `FixtureDependent` repo and validates only **`non_algorithm_read_names()`**
(`src/synthetic/shapes.rs`); `ShapeSpec.profile` is never consulted for repo construction. Worse,
`QueryCoverageProfile` is the **global A/B `--query-profile` enum** (`queries_repository.rs:64`, used
at `cli.rs:205-208,264-271`) — so a `QueryCoverageProfile::Algorithm` would expose a misleading
`--query-profile algorithm` on the A/B benchmark **and still wouldn't enable algorithms** (that's
`AlgorithmQuerySelection`). **Fix:** a **synthetic-only** shape family (not `QueryCoverageProfile`), a
separate `record_algorithm_reads()` that builds an **all-algorithms-enabled** repository with its
**own** drift-guard, and a new `algorithm_read_names()` accessor on both the inner repo
(`queries_repository.rs:274-279`) and the wrapper (`:372-376`).

### 3.4 Per-op budget + corpus size are not wired for **recorded** (dynamic) shapes
`OpBudget` is applied only to catalog `OpName`s (`src/synthetic/mod.rs:671-705`); `ShapeSpec`,
`RecordedOp`, and `OpEntry` carry **no** budget (`shapes.rs`, `recording.rs`), and replay applies one
global config and **captures all `CORPUS_SIZE` (256) commands before checking `result_policy`**
(`replay.rs:133-151`). Whole-graph algorithms are ~40–80 ms/call locally, so a 256-command reference
pass is ~11–20 s **per op** — untenable. **Fix (prerequisite):** wire a per-shape **budget + corpus
size** into the recorded-shape path (1 command for the parameterless pageRank/Harmonic/MSF; a small
seeded pair set for maxFlow) and **skip the full reference capture for result-N/A shapes**.

### 3.5 One coarse capability is wrong, and N/A does not skip execution
FalkorDB capabilities are detected **per procedure** (`src/falkor/falkor_driver.rs:520-550`), and
Harmonic is independently optional (`src/main.rs:1171-1183`) — a single `GraphAlgorithms` label can't
represent a partially-supported engine. And `ResultPolicy::NotApplicable` only disables *digest
comparison*; it does **not** skip execution, so an engine lacking a procedure still runs (and fails)
the query. **Fix:** **per-procedure** capability variants (`PageRank`/`MaxFlow`/`Msf`/`Harmonic`),
**probe-before-capture**, and a defined representation for a **skipped** op in the report/diff.

## 4. Approach — a synthetic-only algorithm family, opt-in, behind prerequisites

Mirror the reads' **derive-with-annotation** coupling (one source of truth in `queries_repository`,
curated metadata in `shapes.rs`), but on a **separate axis** from reads:

1. **Prerequisites first:** the simple-graph algorithm fixture (§3.1) and the recorded-shape
   budget/corpus plumbing (§3.4).
2. A **synthetic-only** `CoverageFamily` (or decouple `ShapeSpec.profile` from the A/B enum) with an
   `Algorithm` member — no change to the A/B `--query-profile`.
3. `algorithm_read_shapes() -> Vec<ShapeSpec>` (4 shapes, per-procedure `capability`, `result_policy`
   per §6, per-op budget/corpus) + `algorithm_read_names()` accessor + a drift-guard asserting the
   table equals the repo's algorithm names.
4. `record_algorithm_reads()` rendering from an **all-algorithms-enabled** repository (not
   `no_algorithms()`), against the **simple-graph** fixture.
5. An **orthogonal** opt-in selector (`--repo-algorithms`); algorithms never in `--repo-reads full`
   nor the per-PR gate; documented in cookbook + readme.

## 5. Scope: in / out
- **In:** the 4 FalkorDB `algo.*` shapes; the simple-graph fixture, recorded-shape budget/corpus
  plumbing, per-procedure capability labels, per-shape result policy; opt-in (nightly / on-demand)
  record + replay.
- **Out:** Neo4j `gds.*` + projected `benchmark_algo_graph`, Memgraph variants; writes (Phase 7); any
  change to the default per-PR read gate or the A/B `--query-profile`.

## 6. Determinism per shape
Floats are digested by `f64::to_bits()` (exact), so the question is **value** stability run-to-run on
the same image:

| Shape | Proposed `result_policy` | Why |
| --- | --- | --- |
| `algo_pagerank_summary` | **N/A** | `RETURN score LIMIT 1` (no `ORDER BY`) — arbitrary single float. |
| `algo_harmonic_summary` | **N/A** (pending) | `avg`/`max` over all nodes; iterative/aggregation value stability unproven. |
| `algo_max_flow_single_pair` | **Gated (candidate)** | Max-flow of a fixed simple graph + capacities + seeded pair is a unique integral value coerced to exact `f64`. |
| `algo_msf_summary` | **Gated (candidate)** | `edge_count` and MSF `total_weight` are unique (tie-breaking cannot change the minimum total). |

Default **all four to N/A**; promote `max_flow`/`msf` to `Gated` **only after** confirming byte-stable
digests across ≥2 runs on the per-PR image (the reads' bar). Never add a synthetic-only `ORDER BY`.

## 7. Phasing (prerequisites first, each its own PR)
1. **Simple-graph algorithm fixture** (§3.1) — deterministic, no parallel `:Friend` edges + tests.
2. **Recorded-shape budget/corpus plumbing** (§3.4) — per-shape corpus size + budget on the dynamic
   path; N/A shapes skip full reference capture.
3. **Annotation + selection (all N/A):** synthetic-only family, `algorithm_read_shapes()`,
   `algorithm_read_names()` + drift-guard, `record_algorithm_reads()`, `--repo-algorithms`. Record +
   replay the 4 shapes end-to-end, opt-in.
4. **Per-procedure capability** (§3.5) — probe-before-capture + skipped-op reporting.
5. **Promote deterministic shapes:** flip `max_flow`/`msf` to `Gated` once verified byte-stable.
6. **Docs:** cookbook + readme.

## 8. Risks & open questions
1. **maxFlow topology (§3.1)** — biggest: confirm the dup-pair count + tensors error, and that a
   deduped simple graph keeps `workload_hash`/digests deterministic.
2. **Float/iteration value stability (§6)** — whether `max_flow`/`msf` digests are byte-stable
   run-to-run; if not they stay N/A (latency-only), like `pagerank`/`harmonic`.
3. **Value of N/A-only coverage** — N/A shapes add latency/trend coverage + exercise the `algo.*`
   paths but nothing to the divergence gate; acceptable because algorithms are opt-in, not per-PR.
4. **Budget/corpus plumbing is real work (§3.4)**, not annotation — it is a prerequisite, and it also
   benefits any future heavy read.
5. **Skipped-op semantics (§3.5)** — how a capability-absent op appears in `report --diff` needs
   defining so it neither reads as a pass nor a divergence.

## 9. Acceptance
Opt-in record + replay of the 4 algorithm shapes end-to-end on the FalkorDB per-PR image against the
simple-graph fixture, off the per-PR read gate and the A/B `--query-profile`; `pagerank`/`harmonic`
result-N/A, `max_flow`/`msf` gated **iff** verified byte-stable; a drift-guard binds the table to
`queries_repository`'s algorithm names. "Algorithms every PR" is explicitly **not** the target.

## 10. Rollout
Land the phases behind `--repo-algorithms` in `FalkorDB/benchmark`; `falkordb-rs-next-gen` picks each
up on the next `SYNTHETIC_BENCHMARK_REF` bump. The per-PR `synthetic-verify` gate stays reads-only; a
nightly/on-demand job exercises the algorithm shapes.

## 11. What the rubber-duck corrected (so reviewers can trust this revision)
1. **maxFlow needs a simple graph** — the generator only forbids self-loops, so the oracle fixture has
   parallel edges and `algo.maxFlow` errors on tensors; "no new fixture" was wrong (§3.1).
2. **`Tier::Full` ≠ opt-in** — `--repo-reads full` already means "all reads" and runs every PR; a
   separate selection axis is required so algorithms don't leak into the gate (§3.2).
3. **The record path hard-excludes algorithms and the profile enum is the A/B CLI's** — needs a
   synthetic-only family + `record_algorithm_reads()` + `algorithm_read_names()`, not
   `QueryCoverageProfile::Algorithm` (§3.3).
4. **Per-op budget/corpus is unwired for recorded shapes** and replay captures all 256 commands before
   checking policy — a real prerequisite, not annotation (§3.4).
5. **Capability must be per-procedure**, and result-N/A does **not** skip execution — needs
   probe-before-capture + skipped-op reporting (§3.5).
6. **Floats are fine to gate** once deterministic — canonicalization hashes `f64::to_bits()`, so
   `max_flow`/`msf` are sound `Gated` candidates after the topology fix (§6).
