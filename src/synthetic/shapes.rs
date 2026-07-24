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
use crate::synthetic::catalog::CORPUS_SIZE;
use crate::synthetic::recording::RecordedOp;
use crate::synthetic::{OpKey, Tier};
use rand::rngs::StdRng;
use rand::SeedableRng;
use std::collections::BTreeSet;

/// Whether a shape's result set is byte-stable across runs/engines, and so whether replay gates its
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

/// The engine capability a fixture-dependent read requires beyond plain Cypher (design §3.4). The
/// fulltext/vector smoke reads name the specific index procedure they exercise.
///
/// Today this is **annotation only** — a stable, machine-readable label carried on the [`ShapeSpec`]
/// so a future capability-gating pass (record-and-skip-as-N/A on an engine that lacks the capability)
/// can key off it without a bundle-format change. It is not yet consulted at record or replay time
/// (the per-PR A/B images are modern FalkorDB with all three capabilities). Non-fixture reads need
/// nothing (`capability = None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeCapability {
    /// `db.idx.vector.queryNodes` — a vector index over `:User(embedding)`.
    VectorQueryNodes,
    /// `db.idx.fulltext.queryNodes` — a fulltext index over `:User(ft_text)`.
    FulltextQueryNodes,
    /// `db.idx.fulltext.queryRelationships` — a fulltext index over `:Friend(ft_text)`.
    FulltextQueryRelationships,
}

/// One repo read shape's synthetic metadata: its stable [`queries_repository`] name, coverage
/// **profile** (Baseline / ExtendedCore / FixtureDependent), coverage **tier** (Decision 1), result
/// policy (Decision 4), and optional **capability** (Phase 5 fulltext/vector).
///
/// Kind is always `Read` for this table; per-op **budget** is deferred to its phase. The Baseline and
/// ExtendedCore reads need no capability (`capability = None`); the FixtureDependent fulltext/vector
/// reads carry the [`ShapeCapability`] they exercise — see the module docs.
///
/// [`queries_repository`]: crate::queries_repository
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapeSpec {
    /// The shape's stable `queries_repository` read name (also the recorded op's key).
    pub name: &'static str,
    /// Coverage profile the shape belongs to: [`QueryCoverageProfile::Baseline`] (Phase 3),
    /// [`QueryCoverageProfile::ExtendedCore`] (Phase 4), or [`QueryCoverageProfile::FixtureDependent`]
    /// (Phase 5).
    pub profile: QueryCoverageProfile,
    /// Coverage tier: [`Tier::Core`] gates every PR; [`Tier::Full`] runs nightly/on-demand.
    pub tier: Tier,
    /// Whether replay gates this shape's result digest ([`ResultPolicy`]).
    pub result_policy: ResultPolicy,
    /// The engine capability this shape requires, or `None` for plain-Cypher reads ([`ShapeCapability`]).
    pub capability: Option<ShapeCapability>,
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
    // `s(name, tier, policy)` keeps the table dense and readable — every row is a `Baseline` read.
    fn s(
        name: &'static str,
        tier: Tier,
        result_policy: ResultPolicy,
    ) -> ShapeSpec {
        ShapeSpec {
            name,
            profile: QueryCoverageProfile::Baseline,
            tier,
            result_policy,
            capability: None,
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
        profile: QueryCoverageProfile::ExtendedCore,
        tier: Tier::Core,
        result_policy: ResultPolicy::Gated,
        capability: None,
    }]
}

/// The curated annotation for the **FixtureDependent** fulltext/vector read shapes (design §3.4 /
/// Phase 5): the vector smoke read plus the two fulltext (node + relationship) smoke reads.
///
/// Each requires the post-load fixture ([`fixture_statements`](crate::synthetic::dataset::fixture_statements))
/// — the fulltext/vector index DDL and seed data — baked into the recorded graph, so the record path
/// records them via [`record_rendered_with_fixture`](crate::synthetic::recording::record_rendered_with_fixture)
/// (record-once → replay-verbatim: every engine replays the identical fixture). They bind **no random
/// params** (byte-identical renders), but their result set is **top-k** (ties/ordering are
/// non-deterministic), so all three are result-**N/A** ([`ResultPolicy::NotApplicable`], Decision 4) —
/// we do not add `ORDER BY` to force determinism. Each is annotated with the [`ShapeCapability`] it
/// exercises (metadata for a future capability-gating pass — see [`ShapeCapability`]). All are
/// [`Tier::Full`]: capability-gated shapes stay out of the always-on core subset.
///
/// Auto-discovered like the other sets: the drift-guard test asserts these names are **exactly** the
/// reads the `FixtureDependent` profile adds over `ExtendedCore`.
pub fn fixture_dependent_read_shapes() -> Vec<ShapeSpec> {
    use ShapeCapability::{FulltextQueryNodes, FulltextQueryRelationships, VectorQueryNodes};
    // Every row is a FixtureDependent, Full-tier, result-N/A read; only the name + capability differ.
    fn s(
        name: &'static str,
        capability: ShapeCapability,
    ) -> ShapeSpec {
        ShapeSpec {
            name,
            profile: QueryCoverageProfile::FixtureDependent,
            tier: Tier::Full,
            result_policy: ResultPolicy::NotApplicable(
                "vector/fulltext top-k ordering is non-deterministic",
            ),
            capability: Some(capability),
        }
    }
    vec![
        s("vector_query_nodes_smoke", VectorQueryNodes),
        s("fulltext_query_nodes_smoke", FulltextQueryNodes),
        s("fulltext_query_relationships_smoke", FulltextQueryRelationships),
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
        .any(|shape| shape.profile == QueryCoverageProfile::FixtureDependent)
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
/// Each shape's corpus is [`CORPUS_SIZE`] renders drawn from a fixed per-shape seed
/// (`corpus_seed ^ salt`, the op's [`OpKey::salt`]), so a given seed yields a byte-identical corpus
/// (record-once → replay-verbatim). [`Tier::Full`] selects every repo read; [`Tier::Core`] selects
/// only the core subset. Returns an error if the annotation table names a shape that isn't an
/// auto-discovered `queries_repository` non-algorithm read (annotation drift).
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

    let mut ops = Vec::with_capacity(shapes.len());
    for shape in shapes {
        if !available.contains(shape.name) {
            return Err(OtherError(format!(
                "repo read shape '{}' is annotated but not a queries_repository non-algorithm read \
                 (annotation drift — update src/synthetic/shapes.rs)",
                shape.name
            )));
        }
        let key = OpKey::dynamic(shape.name.to_string(), QueryType::Read);
        let mut rng = StdRng::seed_from_u64(corpus_seed ^ key.salt());
        let mut commands = Vec::with_capacity(CORPUS_SIZE);
        for _ in 0..CORPUS_SIZE {
            let prepared = repo.render_read_with_rng(shape.name, &mut rng).ok_or_else(|| {
                OtherError(format!("repo read shape '{}' failed to render", shape.name))
            })?;
            commands.push(prepared.cypher);
        }
        ops.push(RecordedOp {
            key,
            result_gated: shape.result_policy.is_gated(),
            commands,
        });
    }
    Ok(ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The set of names in the annotation table.
    fn annotated_names() -> BTreeSet<&'static str> {
        baseline_read_shapes().iter().map(|s| s.name).collect()
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
            assert_eq!(shape.profile, QueryCoverageProfile::ExtendedCore);
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
        // Every fixture shape is FixtureDependent, Full-tier, result-N/A, and carries a capability.
        for shape in fixture_dependent_read_shapes() {
            assert_eq!(shape.profile, QueryCoverageProfile::FixtureDependent);
            assert_eq!(shape.tier, Tier::Full);
            assert!(!shape.result_policy.is_gated(), "top-k reads are result-N/A");
            assert!(shape.capability.is_some(), "fixture reads carry a capability");
        }
        // The capabilities map 1:1 to the three index procedures.
        let caps: Vec<Option<ShapeCapability>> =
            fixture_dependent_read_shapes().iter().map(|s| s.capability).collect();
        assert_eq!(
            caps,
            vec![
                Some(ShapeCapability::VectorQueryNodes),
                Some(ShapeCapability::FulltextQueryNodes),
                Some(ShapeCapability::FulltextQueryRelationships),
            ]
        );
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
            shapes.iter().filter(|s| s.profile == QueryCoverageProfile::ExtendedCore).count(),
            1,
            "exactly one extended-core read"
        );
        assert_eq!(
            shapes.iter().filter(|s| s.profile == QueryCoverageProfile::FixtureDependent).count(),
            3,
            "exactly three fixture-dependent reads"
        );
        // `temporal_spatial_roundtrip` is ExtendedCore, Core-tier, and result-gated.
        let ts = shapes.iter().find(|s| s.name == "temporal_spatial_roundtrip").unwrap();
        assert_eq!(ts.profile, QueryCoverageProfile::ExtendedCore);
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
            assert_eq!(s.profile, QueryCoverageProfile::FixtureDependent);
            assert_eq!(s.tier, Tier::Full);
            assert!(!s.result_policy.is_gated());
            assert!(s.capability.is_some());
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
            profile: QueryCoverageProfile::Baseline,
            tier: Tier::Full,
            result_policy: ResultPolicy::Gated,
            capability: None,
        }];
        let err = record_selected_shapes(&bogus, 1000, 5000, 1).unwrap_err();
        assert!(
            format!("{err}").contains("annotation drift"),
            "unexpected error: {err}"
        );
    }
}
