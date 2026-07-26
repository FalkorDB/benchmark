//! Bridge the A/B benchmark's `queries_repository` **read shapes** into the synthetic
//! record/replay pipeline (design §3.4 / Phases 3–5).
//!
//! The synthetic check historically probes a small, hand-curated catalog ([`catalog`]). This module
//! lets it also record the A/B benchmark's **non-algorithm read shapes** — the `Baseline` reads
//! (Phase 3), the `ExtendedCore` `temporal_spatial_roundtrip` (Phase 4), and the `FixtureDependent`
//! fulltext/vector reads (Phase 5). The op *set* is **auto-discovered** from [`queries_repository`]
//! (proven by the drift-guard tests), and this module adds the explicit synthetic metadata each shape
//! carries (coverage profile + tier + result policy + capability — the *derive-with-annotation*
//! model, Decision 3).
//!
//! ## Determinism (record-once → replay-verbatim)
//! Each shape's corpus is rendered **once at record time** from a fixed per-shape seed
//! (`corpus_seed ^ salt`, mirroring how [`catalog`] ops seed) via the seedable
//! [`UsersQueriesRepository::render_read_with_rng`] entry (design §4.1), and the concrete Cypher is
//! recorded verbatim. Replay never touches the RNG — it replays the recorded strings — so the
//! `workload_hash` is byte-identical across replay endpoints (the A/B compares two FalkorDB
//! versions/images, not different databases) and the non-divergence gate stays meaningful.
//! The FixtureDependent reads additionally need a fulltext/vector **fixture** (index DDL + seed data)
//! in the graph; it is baked into the recorded bundle **once** (design §3.4 /
//! [`fixture_statements`](crate::synthetic::dataset::fixture_statements)) and replayed verbatim into
//! every endpoint, so the fixture never diverges either. (The fixture DDL/queries are
//! FalkorDB-specific, so these shapes are for FalkorDB-vs-FalkorDB A/B, not cross-database runs.)
//!
//! ## Result policy (Decision 4)
//! Most baseline reads project byte-stable results and are result-**gated**. Shapes whose result set
//! isn't byte-stable — `LIMIT` without `ORDER BY`, or the fulltext/vector **top-k** reads (ties and
//! ordering are non-deterministic) — are recorded and timed but marked result-**N/A**
//! ([`ResultPolicy::NotApplicable`]) so a benign result difference never fails the gate. We do **not**
//! add `ORDER BY` to the shared repo queries (that would change the shape).
//!
//! [`catalog`]: crate::synthetic::catalog
//! [`queries_repository`]: crate::queries_repository
//! [`UsersQueriesRepository::render_read_with_rng`]: crate::queries_repository::UsersQueriesRepository::render_read_with_rng

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::queries_repository::{
    AlgorithmQuerySelection, Flavour, QueryCoverageProfile, QueryType, UsersQueriesRepository,
};
use crate::synthetic::catalog::{OpBudget, CORPUS_SIZE};
use crate::synthetic::recording::RecordedOp;
use crate::synthetic::{CacheSelection, OpKey, Tier};
use rand::rngs::StdRng;
use rand::SeedableRng;
use std::collections::BTreeSet;

/// Whether a shape's result set is byte-stable across runs/replays, and so whether replay gates its
/// result digest (design §3.2 / Decision 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultPolicy {
    /// Byte-stable result: replay computes and compares a `result_digest`.
    Gated,
    /// Result excluded from strict gating (still recorded + timed) — the shape's result set isn't
    /// byte-stable (e.g. `LIMIT` without `ORDER BY`). Carries a human-readable reason.
    NotApplicable(&'static str),
}

impl ResultPolicy {
    /// Whether replay should gate (compute + compare) this shape's result digest.
    pub fn is_gated(self) -> bool {
        matches!(self, ResultPolicy::Gated)
    }
}

/// The engine procedure an **algorithm shape** requires beyond plain Cypher (design §3.5): each
/// names its `algo.*` procedure. Read shapes — including the fulltext/vector fixture reads — are
/// capability-free: their fixture DDL runs at graph-load time, **before** any probe could skip
/// them, so annotating them would break the per-PR `--repo-reads full` gate on engines lacking the
/// indexes rather than skip cleanly (capability-aware *loading* is future work, not built here).
///
/// Recording persists [`Self::procedure`] on each shape's manifest entry
/// ([`crate::synthetic::recording::OpEntry::capability`]), and replay probes the engine's
/// procedure registry **before** the reference capture (design §3.5): an op whose required
/// procedure is absent is *skipped* — reported, but never executed — instead of failing the whole
/// replay. Capability-free shapes need nothing (`capability = None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeCapability {
    /// `algo.pageRank` — whole-graph PageRank (Phase 6, per-procedure per design §3.5).
    AlgoPageRank,
    /// `algo.maxFlow` — single-pair max flow over `bench_capacity` (Phase 6).
    AlgoMaxFlow,
    /// `algo.MSF` — minimum spanning forest over `bench_capacity` (Phase 6).
    AlgoMsf,
    /// `algo.HarmonicCentrality` — whole-graph harmonic centrality (Phase 6).
    AlgoHarmonic,
}

impl ShapeCapability {
    /// The procedure name this capability requires, exactly as the engine's `dbms.procedures()`
    /// registry spells it (matching is case-insensitive at probe time). Recorded on the shape's
    /// manifest entry so replay can probe-and-skip on an engine that lacks the procedure.
    pub fn procedure(self) -> &'static str {
        match self {
            ShapeCapability::AlgoPageRank => "algo.pageRank",
            ShapeCapability::AlgoMaxFlow => "algo.maxFlow",
            ShapeCapability::AlgoMsf => "algo.MSF",
            ShapeCapability::AlgoHarmonic => "algo.HarmonicCentrality",
        }
    }
}

/// The **synthetic-only** coverage family a shape belongs to (design Phase 6 §3.3): which curated
/// annotation table defines it and which record-time selector picks it up.
///
/// Deliberately **not** [`QueryCoverageProfile`] — that enum is the global A/B `--query-profile`
/// CLI surface (`clap::ValueEnum`), so an `Algorithm` member there would expose a misleading
/// `--query-profile algorithm` on the A/B benchmark that still wouldn't enable algorithm queries
/// (that's [`AlgorithmQuerySelection`]). This family axis exists only inside the synthetic check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageFamily {
    /// A non-algorithm repo read, tagged with the A/B [`QueryCoverageProfile`] that introduced it
    /// (`Baseline` Phase 3, `ExtendedCore` Phase 4, `FixtureDependent` Phase 5). Selected by
    /// `--repo-reads <tier>`.
    Reads(QueryCoverageProfile),
    /// An opt-in whole-graph algorithm read (Phase 6) — selected **only** by `--repo-algorithms`,
    /// never by `--repo-reads`/tier, never in the per-PR gate, and absent from the A/B
    /// `--query-profile`.
    Algorithm,
    /// An opt-in mutation shape (Phase 7 §1) — selected **only** by `--repo-writes`, never by
    /// `--repo-reads`/tier, never in the per-PR gate. Latency tier: measured via `GRAPH.QUERY`
    /// with periodic base-graph resets, result + counters untracked (design §4.1).
    Write,
}

/// One repo read shape's synthetic metadata: its stable [`queries_repository`] name, coverage
/// **family** (non-algorithm read tagged with its A/B profile, or opt-in algorithm — Phase 6),
/// coverage **tier** (Decision 1), result
/// policy (Decision 4), optional **capability** (Phase 5 fulltext/vector), and its record-time
/// **corpus size** + replay **budget** (design §3.4).
///
/// Kind is always `Read` for this table. Every current repo read renders a full
/// [`CORPUS_SIZE`]-command corpus and inherits the global runtime knobs
/// ([`OpBudget::INHERIT`] — pinned by a drift-guard test); the fields exist so heavier future
/// shapes (whole-graph algorithms, ~40–80 ms/call) can record a small corpus and dial their own
/// samples/concurrency/timeouts down without perturbing the rest of the bundle. The Baseline and
/// ExtendedCore reads need no capability (`capability = None`); the FixtureDependent fulltext/vector
/// reads carry the [`ShapeCapability`] they exercise — see the module docs.
///
/// [`queries_repository`]: crate::queries_repository
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapeSpec {
    /// The shape's stable `queries_repository` read name (also the recorded op's key).
    pub name: &'static str,
    /// The synthetic-only [`CoverageFamily`] the shape belongs to: a non-algorithm read tagged
    /// with the [`QueryCoverageProfile`] that introduced it (Phases 3–5), or an opt-in
    /// [`CoverageFamily::Algorithm`] shape (Phase 6).
    pub family: CoverageFamily,
    /// Coverage tier: [`Tier::Core`] gates every PR; [`Tier::Full`] runs nightly/on-demand.
    pub tier: Tier,
    /// Whether replay gates this shape's result digest ([`ResultPolicy`]).
    pub result_policy: ResultPolicy,
    /// The engine capability this shape requires, or `None` for plain-Cypher reads ([`ShapeCapability`]).
    pub capability: Option<ShapeCapability>,
    /// How many commands to render into the recorded corpus (must be ≥ 1). [`CORPUS_SIZE`] for
    /// every current repo read; a parameterless or heavy shape can record fewer.
    pub corpus_size: usize,
    /// Per-op runtime budget recorded into the bundle ([`OpBudget`], design §3.4) and overlaid on
    /// the global config at replay. [`OpBudget::INHERIT`] for every current repo read.
    pub budget: OpBudget,
    /// Whether `record --oracle` captures this shape's per-command mutation outcomes into the
    /// bundle (Phase 7 §6.3) — [`OracleEligibility::Eligible`] only for the deterministic write
    /// subset; every read/algorithm shape and every excluded write carries the design-cited
    /// reason. Declared per row so adding a shape forces an explicit eligibility decision.
    pub oracle: OracleEligibility,
}

/// Whether the §6.3 online outcome oracle applies to a shape (Phase 7 §5): `record --oracle`
/// captures per-command [`MutationStats`](crate::synthetic::writes::MutationStats) only for
/// **eligible** writes — the deterministic fixed-outcome subset whose counters are reproducible
/// from a pristine base at C=1 — and replay re-verifies each recorded outcome per invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleEligibility {
    /// In the oracle-eligible subset (§6.3 deterministic writes + the §6.4
    /// prepared-state/variable-count writes): oracle-captured at record time, re-verified at
    /// replay.
    Eligible,
    /// Never oracle-captured, with the design-cited reason.
    Excluded(&'static str),
}

/// The [`OracleEligibility::Excluded`] annotation every non-write shape carries: the oracle
/// captures **mutation** counters, which reads and algorithm procedures never produce.
const ORACLE_NOT_A_WRITE: OracleEligibility =
    OracleEligibility::Excluded("not a write — no mutation outcome to capture (§6.3)");

/// The names of the oracle-eligible write shapes (the §6.3 deterministic subset plus the §6.4
/// prepared-state/variable-count shapes) — the single source of truth for which ops a format-v4
/// bundle **must** carry outcomes for: capture targets exactly this set, and `recording::load` +
/// replay enforce it exactly (no subset, no strays), so oracle coverage can never silently shrink.
/// Frozen legacy v3 bundles are checked against the recording module's frozen seven-op §6.3 list
/// instead.
pub fn oracle_eligible_names() -> std::collections::BTreeSet<&'static str> {
    write_shapes()
        .iter()
        .filter(|s| s.oracle == OracleEligibility::Eligible)
        .map(|s| s.name)
        .collect()
}

/// The curated annotation for the **46 baseline non-algorithm read shapes** (design §3.4).
///
/// The op *set* is auto-discovered from [`queries_repository`] — the drift-guard test asserts this
/// table's names are **exactly** [`UsersQueriesRepository::non_algorithm_read_names`] for the
/// `Baseline` profile, so adding/removing a baseline read there fails the build until this table is
/// updated. Order mirrors the `queries_repository` definition order.
///
/// A small [`Tier::Core`] subset gates every PR (cheap, deterministic, representative of distinct
/// plan shapes — point lookup, label scan, 1–2-hop expansion, aggregation, index filter, hash
/// join); everything else is [`Tier::Full`] (nightly/on-demand).
///
/// [`queries_repository`]: crate::queries_repository
pub fn baseline_read_shapes() -> Vec<ShapeSpec> {
    use ResultPolicy::{Gated, NotApplicable};
    use Tier::{Core, Full};
    // `s(name, tier, policy)` keeps the table dense and readable — every row is a `Baseline` read
    // with a full corpus and an inherited (global) runtime budget.
    fn s(
        name: &'static str,
        tier: Tier,
        result_policy: ResultPolicy,
    ) -> ShapeSpec {
        ShapeSpec {
            name,
            family: CoverageFamily::Reads(QueryCoverageProfile::Baseline),
            tier,
            result_policy,
            capability: None,
            corpus_size: CORPUS_SIZE,
            budget: OpBudget::INHERIT,
            oracle: ORACLE_NOT_A_WRITE,
        }
    }
    vec![
        s("single_vertex_read", Core, Gated),
        s("aggregate_expansion_1", Core, Gated),
        s("aggregate_expansion_1_with_filter", Core, Gated),
        s("aggregate_expansion_2", Full, Gated),
        s("aggregate_expansion_2_with_filter", Full, Gated),
        s("aggregate_expansion_3", Full, Gated),
        s("aggregate_expansion_3_with_filter", Full, Gated),
        s("aggregate_expansion_4", Full, Gated),
        s("aggregate_expansion_4_with_filter", Full, Gated),
        s("aggregate_age", Core, Gated),
        s("aggregate_age_distinct", Full, Gated),
        s("aggregate_age_filtered", Full, Gated),
        s("aggregate_count_users", Core, Gated),
        s("aggregate_age_min_max_avg", Full, Gated),
        s("neighbours_2", Core, Gated),
        s("neighbours_2_with_filter", Full, Gated),
        s("neighbours_2_with_data", Full, Gated),
        s("neighbours_2_with_data_and_filter", Full, Gated),
        s("shortest_path", Full, Gated),
        s("shortest_path_with_filter", Full, Gated),
        s("pattern_cycle", Full, Gated),
        s("pattern_long", Full, Gated),
        s("pattern_short", Full, Gated),
        s("vertex_on_label_property", Full, Gated),
        s("vertex_on_label_property_index", Core, Gated),
        s("vertex_on_property", Core, Gated),
        s("value_join", Full, Gated),
        s("value_join_cnt", Core, Gated),
        s("order_by_age", Full, Gated),
        s("unwind_rows", Full, Gated),
        s("var_len_friends", Full, Gated),
        s("optional_friend", Full, Gated),
        s("call_subquery", Full, Gated),
        s("id_seek", Core, Gated),
        s("id_range_scan", Full, Gated),
        s("union_all_ids", Full, Gated),
        s("union_distinct_ids", Full, Gated),
        s("all_shortest_paths_len", Full, Gated),
        s("var_len_with_edge_where_filter", Full, Gated),
        s("exact_5_hop_traverse_count", Full, Gated),
        s("exact_6_hop_traverse_count", Full, Gated),
        s("count_users_plain", Core, Gated),
        s("count_friend_edges_plain", Core, Gated),
        s("indexed_or_predicate", Full, Gated),
        s("indexed_in_list_predicate", Full, Gated),
        s(
            "entity_path_introspection",
            Full,
            NotApplicable("LIMIT without ORDER BY returns an unordered subset"),
        ),
    ]
}

/// The curated annotation for the **ExtendedCore** read shapes (design §3.4 / Phase 4): today just
/// `temporal_spatial_roundtrip`, which round-trips deterministic temporal (`date`/`localtime`/
/// `duration`) and spatial (`point`/`distance`) values.
///
/// It binds **no random params**, so every render is byte-identical; its result canonicalizes stably
/// ([`op_runner`] handles `Date`/`Time`/`Duration`/`Point` and bit-patterns floats), so it is
/// result-**gated**. The `ExtendedCore` profile is unavailable on Memgraph, but the synthetic record
/// path is FalkorDB-only, so the shape is always present there (no capability gate needed).
///
/// Auto-discovered like the baseline set: the drift-guard test asserts these names are **exactly** the
/// reads the `ExtendedCore` profile adds over `Baseline`.
///
/// [`op_runner`]: crate::synthetic::op_runner
pub fn extended_core_read_shapes() -> Vec<ShapeSpec> {
    vec![ShapeSpec {
        name: "temporal_spatial_roundtrip",
        family: CoverageFamily::Reads(QueryCoverageProfile::ExtendedCore),
        tier: Tier::Core,
        result_policy: ResultPolicy::Gated,
        capability: None,
        corpus_size: CORPUS_SIZE,
        budget: OpBudget::INHERIT,
        oracle: ORACLE_NOT_A_WRITE,
    }]
}

/// The curated annotation for the **FixtureDependent** fulltext/vector read shapes (design §3.4 /
/// Phase 5): the vector smoke read plus the two fulltext (node + relationship) smoke reads.
///
/// Each requires the post-load fixture ([`fixture_statements`](crate::synthetic::dataset::fixture_statements))
/// — the fulltext/vector index DDL and seed data — baked into the recorded graph, so the record path
/// records them via [`record_rendered_with_fixture`](crate::synthetic::recording::record_rendered_with_fixture)
/// (record-once → replay-verbatim: every replay endpoint gets the identical fixture). They bind **no random
/// params** (byte-identical renders), but their result set is **top-k** (ties/ordering are
/// non-deterministic), so all three are result-**N/A** ([`ResultPolicy::NotApplicable`], Decision 4) —
/// we do not add `ORDER BY` to force determinism. All are **capability-free** (`capability = None`,
/// like every read shape — see [`ShapeCapability`]): their index DDL is part of the graph load,
/// which runs before any probe, so the per-PR `--repo-reads full` gate must never probe. All are
/// [`Tier::Full`]: fixture-dependent shapes stay out of the always-on core subset.
///
/// Auto-discovered like the other sets: the drift-guard test asserts these names are **exactly** the
/// reads the `FixtureDependent` profile adds over `ExtendedCore`.
pub fn fixture_dependent_read_shapes() -> Vec<ShapeSpec> {
    // Every row is a FixtureDependent, Full-tier, result-N/A, capability-free read with a full
    // corpus and an inherited (global) runtime budget; only the name differs.
    fn s(name: &'static str) -> ShapeSpec {
        ShapeSpec {
            name,
            family: CoverageFamily::Reads(QueryCoverageProfile::FixtureDependent),
            tier: Tier::Full,
            result_policy: ResultPolicy::NotApplicable(
                "vector/fulltext top-k ordering is non-deterministic",
            ),
            capability: None,
            corpus_size: CORPUS_SIZE,
            budget: OpBudget::INHERIT,
            oracle: ORACLE_NOT_A_WRITE,
        }
    }
    vec![
        s("vector_query_nodes_smoke"),
        s("fulltext_query_nodes_smoke"),
        s("fulltext_query_relationships_smoke"),
    ]
}

/// Every repo read shape the synthetic check records: the [`baseline_read_shapes`] (Phase 3), then
/// the [`extended_core_read_shapes`] (Phase 4), then the [`fixture_dependent_read_shapes`] (Phase 5),
/// in `queries_repository` definition order (the record order that feeds `workload_hash`).
pub fn repo_read_shapes() -> Vec<ShapeSpec> {
    let mut shapes = baseline_read_shapes();
    shapes.extend(extended_core_read_shapes());
    shapes.extend(fixture_dependent_read_shapes());
    shapes
}

/// The coverage [`Tier`] of the recorded shape named `name` — **family-agnostic**: repo read
/// shapes, the Phase 6 algorithm shapes and the Phase 7 write shapes alike — or `None` when no
/// shape has that name. Lets string-keyed consumers (thresholds validation, report tier rollups)
/// resolve a dynamic recorded op exactly like a static catalog op. Selection stays per-family
/// ([`record_repo_reads`] / [`record_algorithm_reads`] / [`record_repo_writes`]) — this is a
/// lookup, not a selector.
pub fn shape_tier(name: &str) -> Option<Tier> {
    // Chain the component lists (same order as `repo_read_shapes`, then the algorithm family,
    // then the write family)
    // instead of materializing the combined Vec on every lookup;
    // `shape_tier_covers_every_shape` guards drift.
    baseline_read_shapes()
        .into_iter()
        .chain(extended_core_read_shapes())
        .chain(fixture_dependent_read_shapes())
        .chain(algorithm_read_shapes())
        .chain(write_shapes())
        .find(|shape| shape.name == name)
        .map(|shape| shape.tier)
}

/// The repo read shapes the given `tier` selects, in record order: [`Tier::Full`] selects every repo
/// read; [`Tier::Core`] selects only the core subset. Shared by [`record_repo_reads`] and
/// [`repo_reads_need_fixture`] so the two agree on what a tier records.
fn selected_shapes(tier: Tier) -> Vec<ShapeSpec> {
    repo_read_shapes()
        .into_iter()
        .filter(|shape| tier.includes(shape.tier))
        .collect()
}

/// Whether the `tier`'s selection includes any [`QueryCoverageProfile::FixtureDependent`] shape, so
/// the record path must bake the fulltext/vector fixture into the recorded graph
/// ([`record_rendered_with_fixture`](crate::synthetic::recording::record_rendered_with_fixture))
/// instead of the plain [`record_rendered`](crate::synthetic::recording::record_rendered). The
/// fixture shapes are all [`Tier::Full`], so `Tier::Core` never needs the fixture.
pub fn repo_reads_need_fixture(tier: Tier) -> bool {
    selected_shapes(tier)
        .iter()
        .any(|shape| shape.family == CoverageFamily::Reads(QueryCoverageProfile::FixtureDependent))
}

/// The concurrency sweep every algorithm shape measures: a single closed-loop worker. Whole-graph
/// algorithms saturate the engine by themselves, so a concurrency curve adds runtime without
/// information.
static ALGORITHM_SWEEP: [usize; 1] = [1];

/// The corpus size for `algo_max_flow_single_pair` — a small seeded set of **distinct**
/// `(source, target)` pairs (design §3.4; duplicate draws re-render, bounded), instead of the
/// [`CORPUS_SIZE`]-command corpus a cheap read records.
const MAX_FLOW_CORPUS_SIZE: usize = 4;

/// The per-op budget every algorithm shape records (design §3.4): whole-graph algorithms are
/// ~40–80 ms/call, so they dial samples/concurrency down and their timeouts up instead of
/// inheriting the global read-tuned knobs. Recorded into the bundle ([`RecordedBudget`]) and
/// overlaid on the global config at replay; the resolved effective policy is persisted per op and
/// guarded by `report --diff`.
///
/// [`RecordedBudget`]: crate::synthetic::catalog::RecordedBudget
const ALGORITHM_BUDGET: OpBudget = OpBudget {
    samples: Some(25),
    warmup: Some(2),
    concurrency: Some(&ALGORITHM_SWEEP),
    cache: Some(CacheSelection::Cached),
    server_timeout_ms: Some(60_000),
    client_deadline_ms: Some(75_000),
};

/// The curated annotation for the **4 opt-in whole-graph algorithm read shapes** (design Phase 6
/// §7.3), in `queries_repository` definition order. Selected **only** by `--repo-algorithms` —
/// never by `--repo-reads`/tier, never in the per-PR gate ([`repo_read_shapes`] is exactly the 50
/// non-algorithm reads).
///
/// The op *set* is auto-discovered: the drift-guard test asserts this table's names are **exactly**
/// [`UsersQueriesRepository::algorithm_read_names`] of an all-algorithms-enabled repository, so
/// adding/removing an algorithm read there fails the build until this table is updated.
///
/// Result policies follow the design's §6 determinism table: `max_flow`/`msf` are **Gated** —
/// their values are unique (a max-flow of a fixed simple graph + capacities + seeded pair; an
/// MSF's `edge_count`/minimum `total_weight`, which tie-breaking cannot change) and their digests
/// were verified byte-stable across 3 independent replays on the same image, including across a
/// server restart (the design §7.5 evidence); `record_and_replay_algorithm_shapes_end_to_end`
/// re-verifies two-replay digest stability (same server process) on every coverage run.
/// `pagerank`/`harmonic` stay **N/A** — arbitrary/iterative float values.
/// Never force determinism with a synthetic-only `ORDER BY`. Each shape carries the
/// per-procedure [`ShapeCapability`] replay probes before capture (design §3.5).
pub fn algorithm_read_shapes() -> Vec<ShapeSpec> {
    use ShapeCapability::{AlgoHarmonic, AlgoMaxFlow, AlgoMsf, AlgoPageRank};
    // Every row is an Algorithm-family, Full-tier shape with the algorithm budget; only the
    // corpus size (1 for parameterless shapes, a small seeded pair set for maxFlow), capability
    // and result policy vary.
    fn s(
        name: &'static str,
        capability: ShapeCapability,
        corpus_size: usize,
        result_policy: ResultPolicy,
    ) -> ShapeSpec {
        ShapeSpec {
            name,
            family: CoverageFamily::Algorithm,
            tier: Tier::Full,
            result_policy,
            capability: Some(capability),
            corpus_size,
            budget: ALGORITHM_BUDGET,
            oracle: ORACLE_NOT_A_WRITE,
        }
    }
    vec![
        s(
            "algo_pagerank_summary",
            AlgoPageRank,
            1,
            ResultPolicy::NotApplicable(
                "RETURN score LIMIT 1 without ORDER BY — arbitrary single float",
            ),
        ),
        s(
            "algo_max_flow_single_pair",
            AlgoMaxFlow,
            MAX_FLOW_CORPUS_SIZE,
            ResultPolicy::Gated,
        ),
        s("algo_msf_summary", AlgoMsf, 1, ResultPolicy::Gated),
        s(
            "algo_harmonic_summary",
            AlgoHarmonic,
            1,
            ResultPolicy::NotApplicable(
                "avg/max over all nodes — iterative float value stability unproven",
            ),
        ),
    ]
}

/// The per-op replay budget every write shape records (Phase 7 §4.1): **C=1 only** — decided
/// permanent by §6.5 (a recorded corpus is replayed verbatim on one shared graph: duplicate
/// targets and cross-band edges make any worker split race, and interleaved mutations change what
/// each invocation does, so a C>1 latency number would not be comparable across versions) — with a modest
/// samples/warmup so the drift a cell accumulates between base resets stays small (~110
/// invocations/cell). Cache modes + timeouts inherit the run's global knobs: the write shapes are
/// single-entity point mutations, as cheap as the point reads, and their uncached-vs-cached
/// compilation split is as meaningful as for reads. Recorded into the bundle ([`RecordedBudget`])
/// and overlaid at replay; the resolved effective policy is persisted per op and guarded by
/// `report --diff`.
///
/// [`RecordedBudget`]: crate::synthetic::catalog::RecordedBudget
const WRITE_SWEEP: [usize; 1] = [1];
const WRITE_BUDGET: OpBudget = OpBudget {
    samples: Some(100),
    warmup: Some(10),
    concurrency: Some(&WRITE_SWEEP),
    cache: None,
    server_timeout_ms: None,
    client_deadline_ms: None,
};

/// The curated annotation for the **10 opt-in write shapes** (Phase 7 §1), in `queries_repository`
/// definition order. Selected **only** by `--repo-writes` — never by `--repo-reads`/tier, never in
/// the per-PR gate ([`repo_read_shapes`] stays exactly the 50 non-algorithm reads).
///
/// The op *set* is auto-discovered: the drift-guard test asserts this table's names are **exactly**
/// [`UsersQueriesRepository::write_names`], so adding/removing a write shape there fails the build
/// until this table is updated.
///
/// Every shape is **latency-tier** (design §4.1): replayed via `GRAPH.QUERY` with periodic
/// base-graph resets, and `ResultPolicy::NotApplicable` — mutation outcomes are state- and
/// value-dependent (MERGE create-vs-match, SET-same-value counting 0, DETACH DELETE no-ops on
/// repeat — §2/§10.1), `timestamp()`/`date()`/`rand()` values are non-reproducible (§3.4), and no
/// **statically modelled** counter expectation exists. The §6.3 + §6.4 **correctness tier**
/// instead records each command's *actual* outcome online (`record --oracle`) for the
/// oracle-eligible set — the nine [`OracleEligibility::Eligible`] rows below — and replay
/// re-verifies those recorded outcomes per invocation from a pristine base; the one excluded row
/// carries the design-cited reason (§3.4 server `rand()`).
/// No capability: all plain Cypher.
pub fn write_shapes() -> Vec<ShapeSpec> {
    use OracleEligibility::{Eligible, Excluded};
    // Every row is a Write-family, Full-tier, result-N/A shape with the write budget and a full
    // corpus; only the name, the N/A reason and the §6.3 + §6.4 oracle eligibility vary.
    fn s(
        name: &'static str,
        why_na: &'static str,
        oracle: OracleEligibility,
    ) -> ShapeSpec {
        ShapeSpec {
            name,
            family: CoverageFamily::Write,
            tier: Tier::Full,
            result_policy: ResultPolicy::NotApplicable(why_na),
            capability: None,
            corpus_size: CORPUS_SIZE,
            budget: WRITE_BUDGET,
            oracle,
        }
    }
    vec![
        s(
            "single_vertex_write",
            "latency tier — plain CREATE grows the graph (duplicate ids); outcome untracked",
            Eligible,
        ),
        s(
            "single_vertex_update",
            "latency tier — SET counters are value-dependent (1↔0 on repeated value, §10.1)",
            Eligible,
        ),
        s(
            "single_edge_update",
            "latency tier — server rand() picks the target edge; never correctness-verifiable (§3.4)",
            Excluded("server rand() picks the target edge — outcome not reproducible (§3.4)"),
        ),
        s(
            "single_edge_write",
            "latency tier — MERGE create-vs-match depends on accumulated state; date() non-reproducible",
            Eligible,
        ),
        s(
            "merge_user_insert_path",
            "latency tier — create-once-then-match ordering; timestamp() non-reproducible (§10.2)",
            Eligible,
        ),
        s(
            "merge_user_upsert_existing",
            "latency tier — ON MATCH SET counters value-dependent; timestamp() non-reproducible",
            Eligible,
        ),
        s(
            "merge_friend_edge_upsert",
            "latency tier — MERGE create-vs-match depends on accumulated state; date() non-reproducible",
            Eligible,
        ),
        s(
            "detach_delete_user",
            "latency tier — deletes are state-dependent (no-op on repeat) with variable counts",
            Eligible,
        ),
        s(
            "remove_user_property_and_label",
            "latency tier — REMOVE outcome depends on prepared state consumed by earlier repeats",
            Eligible,
        ),
        s(
            "foreach_loop_mutation",
            "latency tier — SET counters value-dependent on repeat against the same User",
            Eligible,
        ),
    ]
}

/// The algorithm selection [`record_algorithm_reads`] uses: **all four** enabled, so the
/// repository registers every `algo_*` read and the drift-guard sees the complete set.
fn all_algorithms() -> AlgorithmQuerySelection {
    AlgorithmQuerySelection {
        pagerank: true,
        max_flow: true,
        msf: true,
        harmonic: true,
    }
}

/// The algorithm selection the baseline-read source uses: **none**. Algorithm reads are opt-in and
/// capability-gated (Phase 6), so they're excluded from the auto-discovered baseline read set.
fn no_algorithms() -> AlgorithmQuerySelection {
    AlgorithmQuerySelection {
        pagerank: false,
        max_flow: false,
        msf: false,
        harmonic: false,
    }
}

/// Build the `queries_repository` handle the read shapes render from: `FalkorDB` flavour, no
/// algorithms, at the given coverage `profile`. `vertices` must match the recorded graph's `:User`
/// count (ids `1..=vertices`) so each shape's random params address real nodes. `FixtureDependent`
/// is a superset of `ExtendedCore` (itself a superset of `Baseline`) for non-algorithm reads — the
/// lower-profile shapes render identically under it — so the record path builds one
/// `FixtureDependent` repository to render every phase's shapes.
fn read_shapes_repository(
    profile: QueryCoverageProfile,
    vertices: i32,
    edges: i32,
) -> UsersQueriesRepository {
    UsersQueriesRepository::new(vertices, edges, Flavour::FalkorDB, no_algorithms(), profile)
}

/// Render the selected `tier`'s repo read shapes into [`RecordedOp`]s, ready for
/// [`record_rendered`](crate::synthetic::recording::record_rendered) — **offline**, no server.
///
/// Each shape's corpus is `corpus_size` renders ([`CORPUS_SIZE`] for every current repo read) drawn
/// from a fixed per-shape seed (`corpus_seed ^ salt`, the op's [`OpKey::salt`]), so a given seed
/// yields a byte-identical corpus (record-once → replay-verbatim); its [`OpBudget`] is recorded
/// alongside so replay applies the same per-op overrides. [`Tier::Full`] selects every repo read;
/// [`Tier::Core`] selects only the core subset. Returns an error if the annotation table names a
/// shape that isn't an auto-discovered `queries_repository` non-algorithm read (annotation drift).
pub fn record_repo_reads(
    tier: Tier,
    vertices: i32,
    edges: i32,
    corpus_seed: u64,
) -> BenchmarkResult<Vec<RecordedOp>> {
    record_selected_shapes(&selected_shapes(tier), vertices, edges, corpus_seed)
}

/// Render the given `shapes` into [`RecordedOp`]s against a fresh repository. Split out of
/// [`record_repo_reads`] so the annotation-drift guard is unit-testable with a bogus shape.
fn record_selected_shapes(
    shapes: &[ShapeSpec],
    vertices: i32,
    edges: i32,
    corpus_seed: u64,
) -> BenchmarkResult<Vec<RecordedOp>> {
    // `FixtureDependent` covers every recordable read (the Baseline/ExtendedCore shapes render
    // identically under it), so a single repository renders every phase's shapes.
    let repo = read_shapes_repository(QueryCoverageProfile::FixtureDependent, vertices, edges);
    let available: BTreeSet<&str> = repo
        .non_algorithm_read_names()
        .iter()
        .map(String::as_str)
        .collect();
    render_shapes(&repo, &available, "non-algorithm read", shapes, vertices, corpus_seed)
}

/// Render the 4 opt-in algorithm read shapes ([`algorithm_read_shapes`]) into [`RecordedOp`]s —
/// **offline**, no server (design Phase 6 §7.3, selected by `--repo-algorithms`).
///
/// Renders from an **all-algorithms-enabled** repository (design §3.3 — the read-shape path builds
/// with [`no_algorithms`], which never registers the `algo_*` reads) and validates the annotation
/// table against [`UsersQueriesRepository::algorithm_read_names`] (annotation drift fails loudly).
/// The rendered corpora address the plain generated dataset: since `synthbench/v5` every generated
/// graph is **simple** (no parallel `:Friend` edges — `algo.maxFlow` rejects multi-edges) and every
/// `:Friend` edge carries `bench_capacity` (maxFlow's `capacityProperty`, MSF's `weightAttribute`),
/// so no extra fixture is baked.
pub fn record_algorithm_reads(
    vertices: i32,
    edges: i32,
    corpus_seed: u64,
) -> BenchmarkResult<Vec<RecordedOp>> {
    let repo = UsersQueriesRepository::new(
        vertices,
        edges,
        Flavour::FalkorDB,
        all_algorithms(),
        QueryCoverageProfile::Baseline,
    );
    let available: BTreeSet<&str> = repo
        .algorithm_read_names()
        .iter()
        .map(String::as_str)
        .collect();
    render_shapes(
        &repo,
        &available,
        "algorithm read",
        &algorithm_read_shapes(),
        vertices,
        corpus_seed,
    )
}

/// Render the 10 opt-in write shapes ([`write_shapes`]) into [`RecordedOp`]s — **offline**, no
/// server (Phase 7 §3.1, selected by `--repo-writes`).
///
/// The write pool is profile-independent (registered unconditionally in `queries_repository`), so
/// a plain Baseline repository sees all 10; the annotation table is validated against
/// [`UsersQueriesRepository::write_names`] (annotation drift fails loudly). The rendered corpora
/// address the plain generated dataset — seeded ids within `1..=vertices` (plus the disjoint
/// `vertices + id` insert band `merge_user_insert_path` uses), no fixture. Replay measures them
/// via `GRAPH.QUERY` with periodic base resets (latency tier, design §4.1).
pub fn record_repo_writes(
    vertices: i32,
    edges: i32,
    corpus_seed: u64,
) -> BenchmarkResult<Vec<RecordedOp>> {
    let repo = UsersQueriesRepository::new(
        vertices,
        edges,
        Flavour::FalkorDB,
        AlgorithmQuerySelection::default(),
        QueryCoverageProfile::Baseline,
    );
    let available: BTreeSet<&str> = repo.write_names().iter().map(String::as_str).collect();
    render_shapes(&repo, &available, "write", &write_shapes(), vertices, corpus_seed)
}

/// Render each of `shapes` into a [`RecordedOp`] from `repo`, seeding every shape's corpus with
/// `corpus_seed ^ salt` (the op's [`OpKey::salt`]) so a given seed yields a byte-identical corpus
/// (record-once → replay-verbatim). `available` is the repository's auto-discovered name set for
/// the family being rendered; `kind` names it in the annotation-drift error. Shared by
/// [`record_selected_shapes`] (non-algorithm reads), [`record_algorithm_reads`] (Phase 6) and
/// [`record_repo_writes`] (Phase 7 — rendered through the repo's write seam, keyed `Write`).
fn render_shapes(
    repo: &UsersQueriesRepository,
    available: &BTreeSet<&str>,
    kind: &str,
    shapes: &[ShapeSpec],
    vertices: i32,
    corpus_seed: u64,
) -> BenchmarkResult<Vec<RecordedOp>> {
    let mut ops = Vec::with_capacity(shapes.len());
    for shape in shapes {
        if !available.contains(shape.name) {
            return Err(OtherError(format!(
                "shape '{}' is annotated but not a queries_repository {kind} \
                 (annotation drift — update src/synthetic/shapes.rs)",
                shape.name
            )));
        }
        if shape.corpus_size == 0 {
            return Err(OtherError(format!(
                "shape '{}' has corpus_size 0 — every shape must render at least one \
                 command (fix its ShapeSpec in src/synthetic/shapes.rs)",
                shape.name
            )));
        }
        // The op's key kind follows its family: Write-family shapes render through the repo's
        // write seam and record as write ops (format v2, kind hashed); everything else is a read.
        let key_kind = match shape.family {
            CoverageFamily::Write => QueryType::Write,
            _ => QueryType::Read,
        };
        let render = |rng: &mut StdRng| match shape.family {
            CoverageFamily::Write => repo.render_write_with_rng(shape.name, rng),
            _ => repo.render_read_with_rng(shape.name, rng),
        };
        let key = OpKey::dynamic(shape.name.to_string(), key_kind);
        let mut rng = StdRng::seed_from_u64(corpus_seed ^ key.salt());
        let mut commands = Vec::with_capacity(shape.corpus_size);
        // An Algorithm-family multi-command corpus is a seeded set of DISTINCT commands (the
        // maxFlow pair set — measuring the same pair twice adds runtime without coverage), so
        // duplicate draws re-render, bounded and deterministically (same seed ⇒ same skips ⇒ same
        // corpus). Read and write corpora keep plain sequential draws: duplicates there are
        // legitimate (a parameterless read renders CORPUS_SIZE identical commands by design).
        let need_distinct = shape.family == CoverageFamily::Algorithm && shape.corpus_size > 1;
        let mut seen = BTreeSet::new();
        let max_attempts = shape.corpus_size * 16;
        let mut attempts = 0usize;
        while commands.len() < shape.corpus_size && attempts < max_attempts {
            attempts += 1;
            let prepared = render(&mut rng).ok_or_else(|| {
                OtherError(format!("shape '{}' failed to render", shape.name))
            })?;
            if need_distinct && !seen.insert(prepared.cypher.clone()) {
                continue;
            }
            commands.push(prepared.cypher);
        }
        if commands.len() < shape.corpus_size {
            // Exhaustive fallback (the #259 dedup pattern): the bounded random draws above could
            // not fill the distinct corpus, so walk the entire ordered-pair space in canonical
            // order and take the first unused renders. This makes completion deterministic AND
            // total — it succeeds whenever enough distinct commands exist, so the error below
            // proves the space is genuinely too small (never draw luck). Only distinct corpora
            // can be short here: a plain corpus keeps every draw and fills within its budget.
            'walk: for source in 1..=vertices {
                for target in 1..=vertices {
                    if source == target {
                        continue;
                    }
                    let prepared = repo
                        .render_read_with_path(shape.name, &mut rng, (source, target))
                        .ok_or_else(|| {
                            OtherError(format!("shape '{}' failed to render", shape.name))
                        })?;
                    if !seen.insert(prepared.cypher.clone()) {
                        continue;
                    }
                    commands.push(prepared.cypher);
                    if commands.len() == shape.corpus_size {
                        break 'walk;
                    }
                }
            }
        }
        if commands.len() < shape.corpus_size {
            return Err(OtherError(format!(
                "shape '{}' can render only {} distinct command(s) of the {} its corpus needs — \
                 exhaustively verified over every (source, target) pair of the {vertices}-vertex \
                 dataset (shrink corpus_size in src/synthetic/shapes.rs or enlarge the dataset)",
                shape.name,
                commands.len(),
                shape.corpus_size
            )));
        }
        ops.push(RecordedOp {
            key,
            result_gated: shape.result_policy.is_gated(),
            budget: shape.budget.into(),
            capability: shape.capability.map(|c| c.procedure().to_string()),
            commands,
        });
    }
    Ok(ops)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::catalog::RecordedBudget;

    /// The set of names in the annotation table.
    fn annotated_names() -> BTreeSet<&'static str> {
        baseline_read_shapes().iter().map(|s| s.name).collect()
    }

    #[test]
    fn shape_tier_resolves_by_name() {
        // String-keyed tier lookup over the registry: a core read, a full read, an algorithm
        // shape (family-agnostic — algorithm ops roll up into tier buckets too), and a miss.
        assert_eq!(shape_tier("single_vertex_read"), Some(Tier::Core));
        assert_eq!(shape_tier("vector_query_nodes_smoke"), Some(Tier::Full));
        assert_eq!(shape_tier("algo_max_flow_single_pair"), Some(Tier::Full));
        assert_eq!(shape_tier("not_a_shape"), None);
    }

    #[test]
    fn shape_tier_covers_every_shape() {
        // Drift guard for the chained lookup: every shape in every family registry must resolve
        // to its own tier, so a new component list can't silently escape `shape_tier`.
        for shape in repo_read_shapes()
            .into_iter()
            .chain(algorithm_read_shapes())
            .chain(write_shapes())
        {
            assert_eq!(shape_tier(shape.name), Some(shape.tier), "{}", shape.name);
        }
    }

    #[test]
    fn baseline_shapes_match_the_auto_discovered_repository_reads() {
        // Derive-with-annotation (Decision 3): the annotation table must name EXACTLY the
        // auto-discovered baseline (non-algorithm) reads, IN THE SAME definition order — no more,
        // no fewer, no reordering. Order matters: it's the record order, which feeds `workload_hash`
        // (recording.rs). If `queries_repository` gains, drops, or reorders a baseline read, this
        // fails until `baseline_read_shapes()` is realigned.
        let repo = read_shapes_repository(QueryCoverageProfile::Baseline, 1000, 5000);
        let discovered: Vec<&str> =
            repo.non_algorithm_read_names().iter().map(String::as_str).collect();
        let annotated: Vec<&str> = baseline_read_shapes().iter().map(|s| s.name).collect();
        // Set diff first for the common "added/removed a shape" case (clearer than a raw seq diff)…
        let annotated_set: BTreeSet<&str> = annotated.iter().copied().collect();
        let discovered_set: BTreeSet<&str> = discovered.iter().copied().collect();
        assert_eq!(
            annotated_set, discovered_set,
            "annotation drift — annotated-only: {:?}; discovered-only: {:?}",
            annotated_set.difference(&discovered_set).collect::<Vec<_>>(),
            discovered_set.difference(&annotated_set).collect::<Vec<_>>()
        );
        // …then exact definition-order equality (the record order that determines `workload_hash`).
        assert_eq!(annotated, discovered, "baseline read shapes are out of definition order");
    }

    #[test]
    fn extended_core_adds_exactly_temporal_spatial_roundtrip_over_baseline() {
        // Derive-with-annotation for Phase 4: the reads the `ExtendedCore` profile adds over
        // `Baseline` must be EXACTLY the annotated extended-core shapes (today just
        // `temporal_spatial_roundtrip`). If `queries_repository` adds another ExtendedCore read, this
        // fails until `extended_core_read_shapes()` is updated.
        let baseline_repo = read_shapes_repository(QueryCoverageProfile::Baseline, 1000, 5000);
        let extended_repo = read_shapes_repository(QueryCoverageProfile::ExtendedCore, 1000, 5000);
        let baseline: BTreeSet<&str> =
            baseline_repo.non_algorithm_read_names().iter().map(String::as_str).collect();
        let extended: BTreeSet<&str> =
            extended_repo.non_algorithm_read_names().iter().map(String::as_str).collect();
        let added: BTreeSet<&str> = extended.difference(&baseline).copied().collect();
        let annotated: BTreeSet<&str> =
            extended_core_read_shapes().iter().map(|s| s.name).collect();
        assert_eq!(added, annotated, "ExtendedCore adds exactly the annotated extended-core reads");
        assert!(added.contains("temporal_spatial_roundtrip"));
        for shape in extended_core_read_shapes() {
            assert_eq!(shape.family, CoverageFamily::Reads(QueryCoverageProfile::ExtendedCore));
        }
    }

    #[test]
    fn fixture_dependent_adds_exactly_the_three_reads_over_extended_core() {
        // Derive-with-annotation for Phase 5: the reads the `FixtureDependent` profile adds over
        // `ExtendedCore` must be EXACTLY the annotated fixture-dependent shapes (the vector +
        // two fulltext smoke reads). If `queries_repository` adds another FixtureDependent read, this
        // fails until `fixture_dependent_read_shapes()` is updated.
        let extended_repo = read_shapes_repository(QueryCoverageProfile::ExtendedCore, 1000, 5000);
        let fixture_repo = read_shapes_repository(QueryCoverageProfile::FixtureDependent, 1000, 5000);
        let extended: BTreeSet<&str> =
            extended_repo.non_algorithm_read_names().iter().map(String::as_str).collect();
        let fixture: BTreeSet<&str> =
            fixture_repo.non_algorithm_read_names().iter().map(String::as_str).collect();
        let added: BTreeSet<&str> = fixture.difference(&extended).copied().collect();
        let annotated: BTreeSet<&str> =
            fixture_dependent_read_shapes().iter().map(|s| s.name).collect();
        assert_eq!(added, annotated, "FixtureDependent adds exactly the annotated fixture reads");
        assert_eq!(
            added,
            BTreeSet::from([
                "vector_query_nodes_smoke",
                "fulltext_query_nodes_smoke",
                "fulltext_query_relationships_smoke",
            ])
        );
        // Every fixture shape is FixtureDependent, Full-tier, result-N/A, and capability-free —
        // its index DDL loads with the graph, before any probe could skip it, so the per-PR
        // `--repo-reads full` gate must never probe (see ShapeCapability).
        for shape in fixture_dependent_read_shapes() {
            assert_eq!(
                shape.family,
                CoverageFamily::Reads(QueryCoverageProfile::FixtureDependent)
            );
            assert_eq!(shape.tier, Tier::Full);
            assert!(!shape.result_policy.is_gated(), "top-k reads are result-N/A");
            assert!(shape.capability.is_none(), "read shapes are capability-free");
        }
    }

    #[test]
    fn repo_read_shapes_match_the_fixture_dependent_discovery_in_order() {
        // The combined annotation (baseline ++ extended-core ++ fixture-dependent) must equal the
        // FixtureDependent-profile discovery, in definition order — the record order that feeds
        // `workload_hash`.
        let repo = read_shapes_repository(QueryCoverageProfile::FixtureDependent, 1000, 5000);
        let discovered: Vec<&str> =
            repo.non_algorithm_read_names().iter().map(String::as_str).collect();
        let annotated: Vec<&str> = repo_read_shapes().iter().map(|s| s.name).collect();
        assert_eq!(annotated, discovered, "repo read shapes drifted from FixtureDependent discovery");
    }

    #[test]
    fn algorithm_shapes_match_exactly_the_repo_algorithm_read_names() {
        // Derive-with-annotation for Phase 6: the annotation table must be EXACTLY the algorithm
        // reads an all-algorithms-enabled repository auto-discovers, in definition order. If
        // `queries_repository` adds/renames/removes an `algo_*` read, this fails until
        // `algorithm_read_shapes()` is updated.
        let repo = UsersQueriesRepository::new(
            1000,
            5000,
            Flavour::FalkorDB,
            all_algorithms(),
            QueryCoverageProfile::Baseline,
        );
        let discovered: Vec<&str> =
            repo.algorithm_read_names().iter().map(String::as_str).collect();
        let annotated: Vec<&str> = algorithm_read_shapes().iter().map(|s| s.name).collect();
        assert_eq!(annotated, discovered, "algorithm shapes drifted from repo algorithm reads");
        // The read-shape repository (no_algorithms) must discover NO algorithm reads — the family
        // is reachable only through record_algorithm_reads.
        let reads_repo = read_shapes_repository(QueryCoverageProfile::FixtureDependent, 1000, 5000);
        assert!(reads_repo.algorithm_read_names().is_empty());
    }

    #[test]
    fn write_shapes_match_exactly_the_repo_write_names() {
        // Derive-with-annotation for Phase 7 (§8 acceptance): the annotation table must be EXACTLY
        // the 10 writes the repository auto-discovers, in definition order (the record order that
        // feeds workload_hash). If `queries_repository` adds/renames/removes/reorders a write,
        // this fails until `write_shapes()` is realigned.
        let repo = UsersQueriesRepository::new(
            1000,
            5000,
            Flavour::FalkorDB,
            AlgorithmQuerySelection::default(),
            QueryCoverageProfile::Baseline,
        );
        let discovered: Vec<&str> = repo.write_names().iter().map(String::as_str).collect();
        let annotated: Vec<&str> = write_shapes().iter().map(|s| s.name).collect();
        assert_eq!(annotated.len(), 10, "Phase 7 §1 defines exactly 10 write shapes");
        assert_eq!(annotated, discovered, "write shapes drifted from repo write names");
    }

    #[test]
    fn write_shapes_are_uniformly_latency_tier_annotated() {
        // Phase 7 §4.1 (latency tier): every write shape is Write-family, Full-tier (never in the
        // per-PR Core gate), result-N/A (mutation outcomes are state/value-dependent — §2/§10),
        // plain Cypher (no capability), full corpus, and pinned to the C=1 write budget (§6.5:
        // recorded write replay is C=1 permanently).
        for shape in write_shapes() {
            assert_eq!(shape.family, CoverageFamily::Write, "{}", shape.name);
            assert_eq!(shape.tier, Tier::Full, "{}", shape.name);
            assert!(
                matches!(shape.result_policy, ResultPolicy::NotApplicable(_)),
                "'{}' must be result-N/A in the latency tier",
                shape.name
            );
            assert!(shape.capability.is_none(), "{}", shape.name);
            assert_eq!(shape.corpus_size, CORPUS_SIZE, "{}", shape.name);
            assert_eq!(shape.budget, WRITE_BUDGET, "{}", shape.name);
        }
        assert_eq!(WRITE_BUDGET.concurrency, Some(&WRITE_SWEEP[..]), "write replay is C=1 (§6.5)");
    }

    #[test]
    fn oracle_eligibility_names_the_eligible_set_exactly() {
        // §6.3 + §6.4 (design §5, phasing item 4): the oracle captures every write shape whose
        // outcome is reproducible from the restored base — the two plain create/update, the
        // create-once MERGEs, foreach_loop_mutation (§6.3), plus the prepared-state REMOVE and the
        // variable-count DETACH DELETE (§6.4: per-command capture + per-invocation restore make
        // both reproducible). Only the server-rand() shape (§3.4) stays excluded; every non-write
        // shape is excluded by construction (no mutation outcome exists to capture).
        let eligible: Vec<&str> = write_shapes()
            .iter()
            .filter(|s| s.oracle == OracleEligibility::Eligible)
            .map(|s| s.name)
            .collect();
        assert_eq!(
            eligible,
            vec![
                "single_vertex_write",
                "single_vertex_update",
                "single_edge_write",
                "merge_user_insert_path",
                "merge_user_upsert_existing",
                "merge_friend_edge_upsert",
                "detach_delete_user",
                "remove_user_property_and_label",
                "foreach_loop_mutation",
            ],
            "the oracle-eligible subset drifted"
        );
        let excluded: Vec<&str> = write_shapes()
            .iter()
            .filter(|s| matches!(s.oracle, OracleEligibility::Excluded(_)))
            .map(|s| s.name)
            .collect();
        assert_eq!(
            excluded,
            vec!["single_edge_update"],
            "the excluded set drifted"
        );
        for shape in repo_read_shapes().iter().chain(algorithm_read_shapes().iter()) {
            assert!(
                matches!(shape.oracle, OracleEligibility::Excluded(_)),
                "non-write shape '{}' must be oracle-excluded",
                shape.name
            );
        }
    }

    #[test]
    fn repo_read_selection_is_unchanged_by_the_write_family() {
        // --repo-reads must keep selecting EXACTLY the 50 non-algorithm reads: the write family is
        // reachable only through record_repo_writes (its own selector), never via tier selection.
        let selected = selected_shapes(Tier::Full);
        assert_eq!(selected.len(), 50);
        assert!(
            selected.iter().all(|s| !matches!(s.family, CoverageFamily::Write | CoverageFamily::Algorithm)),
            "tier selection must never pull in write or algorithm shapes"
        );
    }

    #[test]
    fn algorithm_shapes_gate_only_the_deterministic_pair() {
        // Design §6 determinism table + §7.5 promotion: max_flow/msf are Gated (byte-stability
        // verified across independent replays — the e2e live test re-verifies it continuously);
        // pagerank/harmonic stay result-N/A (arbitrary/iterative floats). §3.5: each carries its
        // per-procedure capability; §3.4: each records a reduced corpus (1, or the small seeded
        // maxFlow pair set) under the algorithm budget.
        let shapes = algorithm_read_shapes();
        assert_eq!(shapes.len(), 4);
        for shape in &shapes {
            assert_eq!(shape.family, CoverageFamily::Algorithm, "'{}'", shape.name);
            assert_eq!(shape.tier, Tier::Full, "'{}'", shape.name);
            assert_eq!(shape.budget, ALGORITHM_BUDGET, "'{}'", shape.name);
        }
        let gated: Vec<(&str, bool)> =
            shapes.iter().map(|s| (s.name, s.result_policy.is_gated())).collect();
        assert_eq!(
            gated,
            vec![
                ("algo_pagerank_summary", false),
                ("algo_max_flow_single_pair", true),
                ("algo_msf_summary", true),
                ("algo_harmonic_summary", false),
            ]
        );
        let caps: Vec<Option<ShapeCapability>> = shapes.iter().map(|s| s.capability).collect();
        assert_eq!(
            caps,
            vec![
                Some(ShapeCapability::AlgoPageRank),
                Some(ShapeCapability::AlgoMaxFlow),
                Some(ShapeCapability::AlgoMsf),
                Some(ShapeCapability::AlgoHarmonic),
            ]
        );
        let corpora: Vec<usize> = shapes.iter().map(|s| s.corpus_size).collect();
        assert_eq!(
            corpora,
            vec![1, MAX_FLOW_CORPUS_SIZE, 1, 1],
            "parameterless shapes render once; maxFlow renders its seeded pair set"
        );
    }

    #[test]
    fn capability_procedures_are_pinned() {
        // The exact procedure names replay probes for (via `dbms.procedures()`). Renaming one
        // changes which engines skip the shape — deliberate, so pin the full mapping.
        let expected: &[(ShapeCapability, &str)] = &[
            (ShapeCapability::AlgoPageRank, "algo.pageRank"),
            (ShapeCapability::AlgoMaxFlow, "algo.maxFlow"),
            (ShapeCapability::AlgoMsf, "algo.MSF"),
            (ShapeCapability::AlgoHarmonic, "algo.HarmonicCentrality"),
        ];
        for (cap, procedure) in expected {
            assert_eq!(cap.procedure(), *procedure);
        }
    }

    #[test]
    fn repo_read_shapes_exclude_algorithms_and_stay_exactly_todays_50_reads() {
        // Design §3.2: `repo_read_shapes()` / `--repo-reads full` remain EXACTLY today's 50
        // non-algorithm reads — algorithms are selected only by the orthogonal --repo-algorithms
        // (never by tier, never in the per-PR gate). The workload-hash golden in mod.rs pins the
        // recorded byte stream; this pins the selection sets.
        let read_names: BTreeSet<&str> = repo_read_shapes().iter().map(|s| s.name).collect();
        assert_eq!(read_names.len(), 50);
        let algo_names: BTreeSet<&str> = algorithm_read_shapes().iter().map(|s| s.name).collect();
        assert!(
            read_names.is_disjoint(&algo_names),
            "algorithm shapes must never enter repo_read_shapes()"
        );
        for name in &algo_names {
            assert_eq!(
                shape_tier(name),
                Some(Tier::Full),
                "'{name}' stays outside --repo-reads selection but must still resolve a tier \
                 for rollups/thresholds (family-agnostic shape_tier)"
            );
        }
    }

    #[test]
    fn record_algorithm_reads_renders_seeded_budgeted_corpora() {
        // The Phase 6 record path end-to-end (offline): 4 ops in definition order, each carrying
        // the algorithm budget and its reduced corpus, byte-identical for a fixed seed.
        let ops = record_algorithm_reads(1000, 5000, 7).expect("record algorithm reads");
        let names: Vec<&str> = ops.iter().map(|op| op.key.name()).collect();
        assert_eq!(
            names,
            vec![
                "algo_pagerank_summary",
                "algo_max_flow_single_pair",
                "algo_msf_summary",
                "algo_harmonic_summary"
            ]
        );
        for op in &ops {
            assert_eq!(
                op.budget,
                RecordedBudget::from(ALGORITHM_BUDGET),
                "'{}' must record the algorithm budget",
                op.key.name()
            );
        }
        // Gating mirrors the shape table (§6): the deterministic pair is digest-gated.
        let gated: Vec<bool> = ops.iter().map(|op| op.result_gated).collect();
        assert_eq!(gated, vec![false, true, true, false]);
        // Each op records its per-procedure capability string (design §3.5), so replay can
        // probe-and-skip on an engine that lacks the procedure.
        let capabilities: Vec<Option<&str>> =
            ops.iter().map(|op| op.capability.as_deref()).collect();
        assert_eq!(
            capabilities,
            vec![
                Some("algo.pageRank"),
                Some("algo.maxFlow"),
                Some("algo.MSF"),
                Some("algo.HarmonicCentrality"),
            ]
        );
        let corpora: Vec<usize> = ops.iter().map(|op| op.commands.len()).collect();
        assert_eq!(corpora, vec![1, MAX_FLOW_CORPUS_SIZE, 1, 1]);
        // The rendered Cypher exercises the real procedures…
        assert!(ops[0].commands[0].contains("algo.pageRank"), "{}", ops[0].commands[0]);
        assert!(ops[1].commands[0].contains("algo.maxFlow"), "{}", ops[1].commands[0]);
        assert!(ops[1].commands[0].contains("bench_capacity"), "{}", ops[1].commands[0]);
        assert!(ops[2].commands[0].contains("algo.MSF"), "{}", ops[2].commands[0]);
        assert!(ops[3].commands[0].contains("algo.HarmonicCentrality"), "{}", ops[3].commands[0]);
        // …the maxFlow corpus is a seeded set of distinct (source, target) pairs (deterministic
        // for the fixed seed, so distinctness is a stable fact)…
        let unique_pairs: BTreeSet<&str> =
            ops[1].commands.iter().map(String::as_str).collect();
        assert_eq!(unique_pairs.len(), MAX_FLOW_CORPUS_SIZE, "seeded pairs must differ");
        // …and the same seed reproduces the identical corpus (record-once → replay-verbatim).
        let again = record_algorithm_reads(1000, 5000, 7).expect("re-record");
        for (a, b) in ops.iter().zip(&again) {
            assert_eq!(a.commands, b.commands, "'{}' corpus must be seed-stable", a.key.name());
        }
    }

    #[test]
    fn record_repo_writes_renders_seeded_write_corpora() {
        // The Phase 7 record path end-to-end (offline): 10 write ops in definition order, each
        // keyed Write (kind feeds the format-v2 workload_hash), un-gated, budgeted C=1, with a
        // full seed-stable corpus.
        let ops = record_repo_writes(1000, 5000, 7).expect("record repo writes");
        let names: Vec<&str> = ops.iter().map(|op| op.key.name()).collect();
        let annotated: Vec<&str> = write_shapes().iter().map(|s| s.name).collect();
        assert_eq!(names, annotated, "record order must be the annotation (definition) order");
        for op in &ops {
            assert_eq!(op.key.kind(), QueryType::Write, "{}", op.key.name());
            assert!(!op.result_gated, "'{}' is latency-tier (result-N/A)", op.key.name());
            assert!(op.capability.is_none(), "{}", op.key.name());
            assert_eq!(
                op.budget,
                RecordedBudget::from(WRITE_BUDGET),
                "'{}' must record the write budget",
                op.key.name()
            );
            assert_eq!(op.commands.len(), CORPUS_SIZE, "{}", op.key.name());
        }
        // Spot-check rendered Cypher mutates (CREATE/SET/MERGE/DELETE/REMOVE/FOREACH)…
        assert!(ops[0].commands[0].contains("CREATE"), "{}", ops[0].commands[0]);
        assert!(ops[1].commands[0].contains("SET"), "{}", ops[1].commands[0]);
        // …and the same seed reproduces the identical corpora (record-once → replay-verbatim).
        let again = record_repo_writes(1000, 5000, 7).expect("re-record");
        for (a, b) in ops.iter().zip(&again) {
            assert_eq!(a.commands, b.commands, "'{}' corpus must be seed-stable", a.key.name());
        }
    }

    #[test]
    fn max_flow_corpus_stays_distinct_even_when_draws_collide() {
        // With 3 vertices there are only 6 ordered (source, target) pairs, so 4 seeded draws are
        // near-certain to collide — the bounded re-render must still deliver 4 DISTINCT maxFlow
        // commands (deterministically: same seed ⇒ same skips ⇒ same corpus).
        let ops = record_algorithm_reads(3, 6, 7).expect("record on a tiny pair space");
        let max_flow = &ops[1];
        assert_eq!(max_flow.key.name(), "algo_max_flow_single_pair");
        assert_eq!(max_flow.commands.len(), MAX_FLOW_CORPUS_SIZE);
        let unique: BTreeSet<&str> = max_flow.commands.iter().map(String::as_str).collect();
        assert_eq!(unique.len(), MAX_FLOW_CORPUS_SIZE, "corpus must be distinct despite collisions");
    }

    #[test]
    fn max_flow_distinct_corpus_is_total_when_the_pair_space_barely_fits() {
        // 3 vertices ⇒ EXACTLY 6 ordered (source, target) pairs. A corpus needing all 6 can never
        // be guaranteed by bounded random draws (coupon-collector tail) — the exhaustive fallback
        // must deliver every pair, for ANY seed, twice-identically. (The real maxFlow corpus is 4;
        // sizing it to the whole space exercises the totality property the random phase lacks.)
        let mut shape = algorithm_read_shapes().remove(1);
        assert_eq!(shape.name, "algo_max_flow_single_pair");
        shape.corpus_size = 6;
        let repo = UsersQueriesRepository::new(
            3,
            6,
            Flavour::FalkorDB,
            all_algorithms(),
            QueryCoverageProfile::Baseline,
        );
        let available: BTreeSet<&str> =
            repo.algorithm_read_names().iter().map(String::as_str).collect();
        for seed in [7u64, 0, 42, u64::MAX] {
            let shapes = std::slice::from_ref(&shape);
            let ops = render_shapes(&repo, &available, "algorithm read", shapes, 3, seed)
                .unwrap_or_else(|e| panic!("seed {seed}: the space suffices, must fill: {e}"));
            let unique: BTreeSet<&str> = ops[0].commands.iter().map(String::as_str).collect();
            assert_eq!(unique.len(), 6, "seed {seed}: all 6 ordered pairs — no draw luck");
            let again = render_shapes(&repo, &available, "algorithm read", shapes, 3, seed)
                .expect("re-render");
            assert_eq!(ops[0].commands, again[0].commands, "seed {seed}: deterministic");
        }
    }

    #[test]
    fn seed7_max_flow_corpus_renders_the_historical_pair_sequence() {
        // Golden for the exhaustive-fallback change: the bounded random phase stays
        // byte-compatible with the pre-fallback rejection loop, so the seed=7 1000/5000 oracle
        // corpus — `workload_hash`-relevant bytes in recorded bundles — must render exactly the
        // historical (source, target) sequence, in order.
        let ops = record_algorithm_reads(1000, 5000, 7).expect("record");
        let max_flow = &ops[1];
        assert_eq!(max_flow.key.name(), "algo_max_flow_single_pair");
        let expected = [(834, 998), (251, 200), (243, 109), (916, 109)];
        assert_eq!(max_flow.commands.len(), expected.len());
        for (cmd, (source, target)) in max_flow.commands.iter().zip(expected) {
            let prefix = format!("CYPHER source_id = {source} target_id = {target} ");
            assert!(cmd.starts_with(&prefix), "expected `{prefix}…`, got: {cmd}");
        }
    }

    #[test]
    fn algorithm_corpus_fails_clearly_when_the_pair_space_is_too_small() {
        // 2 vertices ⇒ only 2 ordered pairs < MAX_FLOW_CORPUS_SIZE: the bounded retry must give
        // up with an actionable error instead of looping forever or silently recording dups.
        let err = record_algorithm_reads(2, 2, 7).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("algo_max_flow_single_pair") && msg.contains("distinct"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn there_are_forty_six_baseline_reads_with_a_nonempty_core_subset() {
        let shapes = baseline_read_shapes();
        assert_eq!(shapes.len(), 46, "expected the 46 baseline reads (design §3.4)");
        // Names are unique.
        assert_eq!(annotated_names().len(), 46, "shape names must be unique");
        let core = shapes.iter().filter(|s| s.tier == Tier::Core).count();
        assert!(core > 0 && core < shapes.len(), "core is a small non-empty subset, got {core}");
    }

    #[test]
    fn repo_read_shapes_are_fifty_across_the_three_profiles() {
        // Baseline (46) + ExtendedCore (1) + FixtureDependent (3) = 50 unique reads across profiles.
        let shapes = repo_read_shapes();
        assert_eq!(shapes.len(), 50, "46 baseline + 1 extended-core + 3 fixture-dependent reads");
        let names: BTreeSet<&str> = shapes.iter().map(|s| s.name).collect();
        assert_eq!(names.len(), 50, "shape names must be unique across profiles");
        assert_eq!(
            shapes
                .iter()
                .filter(|s| s.family == CoverageFamily::Reads(QueryCoverageProfile::ExtendedCore))
                .count(),
            1,
            "exactly one extended-core read"
        );
        assert_eq!(
            shapes
                .iter()
                .filter(
                    |s| s.family == CoverageFamily::Reads(QueryCoverageProfile::FixtureDependent)
                )
                .count(),
            3,
            "exactly three fixture-dependent reads"
        );
        // `temporal_spatial_roundtrip` is ExtendedCore, Core-tier, and result-gated.
        let ts = shapes.iter().find(|s| s.name == "temporal_spatial_roundtrip").unwrap();
        assert_eq!(ts.family, CoverageFamily::Reads(QueryCoverageProfile::ExtendedCore));
        assert_eq!(ts.tier, Tier::Core);
        assert!(ts.result_policy.is_gated());
        assert_eq!(ts.capability, None);
        // The fixture reads are FixtureDependent, Full-tier, result-N/A, with a capability.
        for name in [
            "vector_query_nodes_smoke",
            "fulltext_query_nodes_smoke",
            "fulltext_query_relationships_smoke",
        ] {
            let s = shapes.iter().find(|s| s.name == name).unwrap();
            assert_eq!(
                s.family,
                CoverageFamily::Reads(QueryCoverageProfile::FixtureDependent)
            );
            assert_eq!(s.tier, Tier::Full);
            assert!(!s.result_policy.is_gated());
            assert!(s.capability.is_none());
        }
    }

    #[test]
    fn only_the_top_k_and_limit_shapes_are_result_na() {
        // The result-N/A reads are exactly `entity_path_introspection` (LIMIT without ORDER BY) and
        // the three fulltext/vector top-k reads; every other read is result-gated (Decision 4).
        let na: BTreeSet<&str> = repo_read_shapes()
            .iter()
            .filter(|s| !s.result_policy.is_gated())
            .map(|s| s.name)
            .collect();
        assert_eq!(
            na,
            BTreeSet::from([
                "entity_path_introspection",
                "vector_query_nodes_smoke",
                "fulltext_query_nodes_smoke",
                "fulltext_query_relationships_smoke",
            ]),
            "unexpected result-N/A set"
        );
    }

    #[test]
    fn record_repo_reads_full_covers_every_repo_read() {
        let ops = record_repo_reads(Tier::Full, 1000, 5000, 42).unwrap();
        let names: BTreeSet<&str> = ops.iter().map(|o| o.key.name()).collect();
        let expected: BTreeSet<&str> = repo_read_shapes().iter().map(|s| s.name).collect();
        assert_eq!(names, expected, "Full must record every repo read");
        // Every op renders a full corpus and is keyed by the shape name as a read.
        for op in &ops {
            assert_eq!(op.commands.len(), CORPUS_SIZE, "op '{}' short corpus", op.key.name());
            assert_eq!(op.key.kind(), QueryType::Read);
        }
        // `shortest_path` shares its name with a built-in `OpName`, so `OpKey::dynamic`
        // canonicalizes it to that built-in (by design — same name/kind/salt across every run);
        // every other repo read (incl. `temporal_spatial_roundtrip`) is a genuinely dynamic op.
        for op in &ops {
            if op.key.name() == "shortest_path" {
                assert!(op.key.is_named(), "shortest_path canonicalizes to the built-in OpName");
            } else {
                assert!(!op.key.is_named(), "'{}' should be a dynamic read", op.key.name());
            }
        }
        // The ExtendedCore shape is recorded and result-gated…
        let ts = ops.iter().find(|o| o.key.name() == "temporal_spatial_roundtrip").unwrap();
        assert!(ts.result_gated, "temporal_spatial_roundtrip is result-gated");
        // …and the result-N/A reads are recorded but not gated: the LIMIT-without-ORDER shape plus
        // the three fulltext/vector top-k reads.
        let na: BTreeSet<&str> =
            ops.iter().filter(|o| !o.result_gated).map(|o| o.key.name()).collect();
        assert_eq!(
            na,
            BTreeSet::from([
                "entity_path_introspection",
                "vector_query_nodes_smoke",
                "fulltext_query_nodes_smoke",
                "fulltext_query_relationships_smoke",
            ]),
            "exactly the LIMIT-without-ORDER and top-k reads are result-N/A"
        );
    }

    #[test]
    fn repo_reads_replay_never_probes_because_no_read_records_a_capability() {
        // The per-PR gate replays `--repo-reads full` bundles, and replay issues its one
        // `dbms.procedures()` probe **only** when ≥1 recorded op carries a capability. Fixture
        // (fulltext/vector) DDL loads with the graph, before any probe could skip its reads, so a
        // capability there would fail the load on an engine lacking the index instead of skipping
        // cleanly — reads must stay capability-free end-to-end (zero probes on the gate path).
        for tier in [Tier::Core, Tier::Full] {
            let ops = record_repo_reads(tier, 1000, 5000, 42).unwrap();
            for op in &ops {
                assert_eq!(
                    op.capability,
                    None,
                    "read '{}' records a capability — the --repo-reads {tier:?} replay would probe",
                    op.key.name()
                );
            }
        }
        // Drift guard for the annotation source: no read ShapeSpec carries a capability; the
        // algorithm family (opt-in, never on the gate path) is the only annotated one.
        assert!(
            repo_read_shapes().iter().all(|s| s.capability.is_none()),
            "read shapes must be capability-free"
        );
        assert!(
            algorithm_read_shapes().iter().all(|s| s.capability.is_some()),
            "algorithm shapes each name their procedure"
        );
    }

    #[test]
    fn full_records_the_fixture_dependent_reads_and_needs_the_fixture() {
        // The three fulltext/vector reads are recorded under Full as dynamic, result-N/A reads, and
        // the selection reports it needs the baked-in fixture (so the record path uses
        // `record_rendered_with_fixture`). Core omits them and needs no fixture.
        let full = record_repo_reads(Tier::Full, 1000, 5000, 42).unwrap();
        let full_names: BTreeSet<&str> = full.iter().map(|o| o.key.name()).collect();
        for name in [
            "vector_query_nodes_smoke",
            "fulltext_query_nodes_smoke",
            "fulltext_query_relationships_smoke",
        ] {
            assert!(full_names.contains(name), "Full must record '{name}'");
            let op = full.iter().find(|o| o.key.name() == name).unwrap();
            assert!(!op.key.is_named(), "'{name}' is a dynamic read");
            assert_eq!(op.key.kind(), QueryType::Read);
            assert!(!op.result_gated, "'{name}' is result-N/A (top-k)");
            assert_eq!(op.commands.len(), CORPUS_SIZE, "'{name}' short corpus");
        }
        assert!(repo_reads_need_fixture(Tier::Full), "Full selects fixture-dependent reads");

        let core = record_repo_reads(Tier::Core, 1000, 5000, 42).unwrap();
        let core_names: BTreeSet<&str> = core.iter().map(|o| o.key.name()).collect();
        for name in [
            "vector_query_nodes_smoke",
            "fulltext_query_nodes_smoke",
            "fulltext_query_relationships_smoke",
        ] {
            assert!(!core_names.contains(name), "Core must omit '{name}'");
        }
        assert!(!repo_reads_need_fixture(Tier::Core), "Core needs no fixture");
    }

    #[test]
    fn record_repo_reads_core_is_a_subset_of_full() {
        let core_ops = record_repo_reads(Tier::Core, 1000, 5000, 7).unwrap();
        let full_ops = record_repo_reads(Tier::Full, 1000, 5000, 7).unwrap();
        let core: BTreeSet<&str> = core_ops.iter().map(|o| o.key.name()).collect();
        let full: BTreeSet<&str> = full_ops.iter().map(|o| o.key.name()).collect();
        assert!(!core.is_empty() && core.len() < full.len());
        assert!(core.is_subset(&full), "core must be a subset of full");
    }

    #[test]
    fn record_repo_reads_is_byte_identical_for_a_fixed_seed() {
        // Record-once determinism: a fixed seed renders a byte-identical corpus for every shape, so
        // two records produce identical `RecordedOp`s — the comparability the A/B gate relies on.
        let a = record_repo_reads(Tier::Full, 2000, 8000, 12345).unwrap();
        let b = record_repo_reads(Tier::Full, 2000, 8000, 12345).unwrap();
        assert_eq!(a, b, "a fixed seed must render an identical corpus");
        // A different seed shifts the rendered params (the corpus is genuinely seed-sensitive).
        let c = record_repo_reads(Tier::Full, 2000, 8000, 9).unwrap();
        assert_ne!(a, c, "a different seed must render a different corpus");
    }

    #[test]
    fn baseline_reads_render_valid_in_range_params() {
        // Every rendered command binds `:User` ids within `[1, vertices]` (so it addresses real
        // recorded nodes) — a spot check that the seam wires `vertices` through correctly.
        let vertices = 500;
        let ops = record_repo_reads(Tier::Full, vertices, 2000, 3).unwrap();
        let single = ops.iter().find(|o| o.key.name() == "single_vertex_read").unwrap();
        for cmd in &single.commands {
            // `single_vertex_read` renders `CYPHER id = <n> MATCH (n:User {id : $id}) RETURN n`.
            let id: i32 = cmd
                .split("= ")
                .nth(1)
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| panic!("no id param in {cmd:?}"));
            assert!((1..=vertices).contains(&id), "id {id} out of range in {cmd:?}");
        }
    }

    #[test]
    fn record_selected_shapes_rejects_annotation_drift() {
        // A shape annotated but absent from the auto-discovered repository reads is rejected — the
        // safety net behind the derive-with-annotation model.
        let bogus = [ShapeSpec {
            name: "__not_a_repo_read__",
            family: CoverageFamily::Reads(QueryCoverageProfile::Baseline),
            tier: Tier::Full,
            result_policy: ResultPolicy::Gated,
            capability: None,
            corpus_size: CORPUS_SIZE,
            budget: OpBudget::INHERIT,
            oracle: ORACLE_NOT_A_WRITE,
        }];
        let err = record_selected_shapes(&bogus, 1000, 5000, 1).unwrap_err();
        assert!(
            format!("{err}").contains("annotation drift"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn every_current_repo_read_inherits_the_global_budget_and_full_corpus() {
        // Pin today's behaviour: no repo read overrides the global runtime knobs or records a
        // reduced corpus, so recorded bundles — and their pinned workload_hashes — are unchanged
        // by the budget/corpus plumbing (design §3.4). A future shape that needs a budget (e.g.
        // the Phase 6 algorithm reads) belongs in its own table, not here.
        for shape in repo_read_shapes() {
            assert_eq!(shape.budget, OpBudget::INHERIT, "'{}' must inherit", shape.name);
            assert_eq!(shape.corpus_size, CORPUS_SIZE, "'{}' must render a full corpus", shape.name);
        }
    }

    #[test]
    fn record_selected_shapes_honors_corpus_size_and_budget() {
        // A shape's `corpus_size` bounds its rendered corpus, its `budget` lands on the recorded
        // op (owned manifest form), and a truncated corpus is a strict prefix of the full render
        // (the corpus RNG stream is unchanged — smaller just stops earlier).
        static SWEEP: [usize; 1] = [1];
        let small = [ShapeSpec {
            name: "single_vertex_read",
            family: CoverageFamily::Reads(QueryCoverageProfile::Baseline),
            tier: Tier::Core,
            result_policy: ResultPolicy::Gated,
            capability: None,
            corpus_size: 3,
            budget: OpBudget {
                samples: Some(1),
                concurrency: Some(&SWEEP),
                ..OpBudget::INHERIT
            },
            oracle: ORACLE_NOT_A_WRITE,
        }];
        let ops = record_selected_shapes(&small, 1000, 5000, 42).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].commands.len(), 3, "corpus_size bounds the rendered corpus");
        assert_eq!(
            ops[0].budget,
            RecordedBudget {
                samples: Some(1),
                concurrency: Some(vec![1]),
                ..RecordedBudget::default()
            },
            "the shape's budget lands on the recorded op"
        );
        let full = record_repo_reads(Tier::Full, 1000, 5000, 42).unwrap();
        let full_svr = full.iter().find(|o| o.key.name() == "single_vertex_read").unwrap();
        assert_eq!(
            ops[0].commands.as_slice(),
            &full_svr.commands[..3],
            "a reduced corpus is a prefix of the full render"
        );
    }

    #[test]
    fn record_selected_shapes_rejects_a_zero_corpus_naming_the_shape() {
        let bogus = [ShapeSpec {
            name: "single_vertex_read",
            family: CoverageFamily::Reads(QueryCoverageProfile::Baseline),
            tier: Tier::Core,
            result_policy: ResultPolicy::Gated,
            capability: None,
            corpus_size: 0,
            budget: OpBudget::INHERIT,
            oracle: ORACLE_NOT_A_WRITE,
        }];
        let err = record_selected_shapes(&bogus, 1000, 5000, 42).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("single_vertex_read") && msg.contains("corpus_size 0"),
            "the error must name the shape and the zero corpus, got: {msg}"
        );
    }
}
