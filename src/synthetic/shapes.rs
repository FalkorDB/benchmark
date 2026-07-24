//! Bridge the A/B benchmark's `queries_repository` **baseline read shapes** into the synthetic
//! record/replay pipeline (design §3.4 / Phase 3).
//!
//! The synthetic check historically probes a small, hand-curated catalog ([`catalog`]). This module
//! lets it also record the **baseline non-algorithm read shapes** the A/B benchmark measures — the
//! op *set* is **auto-discovered** from [`queries_repository`] (proven by the drift-guard test), and
//! this module adds the explicit synthetic metadata each shape carries (coverage tier + result
//! policy — the *derive-with-annotation* model, Decision 3).
//!
//! ## Determinism (record-once → replay-verbatim)
//! Each shape's corpus is rendered **once at record time** from a fixed per-shape seed
//! (`corpus_seed ^ salt`, mirroring how [`catalog`] ops seed) via the seedable
//! [`UsersQueriesRepository::render_read_with_rng`] entry (design §4.1), and the concrete Cypher is
//! recorded verbatim. Replay never touches the RNG — it replays the recorded strings — so the
//! `workload_hash` is byte-identical across engines and the A/B non-divergence gate stays meaningful.
//!
//! ## Result policy (Decision 4)
//! Most baseline reads project byte-stable results and are result-**gated**. A few shapes whose
//! result set isn't byte-stable (e.g. `LIMIT` without `ORDER BY`) are recorded and timed but marked
//! result-**N/A** ([`ResultPolicy::NotApplicable`]) so a benign result difference never fails the
//! gate — we do **not** add `ORDER BY` to the shared repo queries (that would change the shape).
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

/// One baseline read shape's synthetic metadata: its stable [`queries_repository`] name, coverage
/// tier (Decision 1), and result policy (Decision 4).
///
/// Kind is always `Read` and profile always `Baseline` for this table; **capability** (fulltext/
/// vector) and per-op **budget** are deferred to their phases (Phase 5 / Phase 6) — see the module
/// docs.
///
/// [`queries_repository`]: crate::queries_repository
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapeSpec {
    /// The shape's stable `queries_repository` read name (also the recorded op's key).
    pub name: &'static str,
    /// Coverage tier: [`Tier::Core`] gates every PR; [`Tier::Full`] runs nightly/on-demand.
    pub tier: Tier,
    /// Whether replay gates this shape's result digest ([`ResultPolicy`]).
    pub result_policy: ResultPolicy,
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
    // `s(name, tier, policy)` keeps the table dense and readable.
    fn s(
        name: &'static str,
        tier: Tier,
        result_policy: ResultPolicy,
    ) -> ShapeSpec {
        ShapeSpec {
            name,
            tier,
            result_policy,
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

/// Build the `queries_repository` handle the baseline read shapes render from: `FalkorDB` flavour,
/// no algorithms, `Baseline` coverage profile. `vertices` must match the recorded graph's `:User`
/// count (ids `1..=vertices`) so each shape's random params address real nodes.
fn baseline_repository(
    vertices: i32,
    edges: i32,
) -> UsersQueriesRepository {
    UsersQueriesRepository::new(
        vertices,
        edges,
        Flavour::FalkorDB,
        no_algorithms(),
        QueryCoverageProfile::Baseline,
    )
}

/// Render the selected `tier`'s baseline read shapes into [`RecordedOp`]s, ready for
/// [`record_rendered`](crate::synthetic::recording::record_rendered) — **offline**, no server.
///
/// Each shape's corpus is [`CORPUS_SIZE`] renders drawn from a fixed per-shape seed
/// (`corpus_seed ^ salt`, the op's [`OpKey::salt`]), so a given seed yields a byte-identical corpus
/// (record-once → replay-verbatim). [`Tier::Full`] selects every baseline read; [`Tier::Core`]
/// selects only the core subset. Returns an error if the annotation table names a shape that isn't
/// an auto-discovered `queries_repository` baseline read (annotation drift).
pub fn record_repo_reads(
    tier: Tier,
    vertices: i32,
    edges: i32,
    corpus_seed: u64,
) -> BenchmarkResult<Vec<RecordedOp>> {
    let selected: Vec<ShapeSpec> = baseline_read_shapes()
        .into_iter()
        // `Tier::Full` records everything; `Tier::Core` records only the core subset.
        .filter(|shape| tier == Tier::Full || shape.tier == Tier::Core)
        .collect();
    record_selected_shapes(&selected, vertices, edges, corpus_seed)
}

/// Render the given `shapes` into [`RecordedOp`]s against a fresh baseline repository. Split out of
/// [`record_repo_reads`] so the annotation-drift guard is unit-testable with a bogus shape.
fn record_selected_shapes(
    shapes: &[ShapeSpec],
    vertices: i32,
    edges: i32,
    corpus_seed: u64,
) -> BenchmarkResult<Vec<RecordedOp>> {
    let repo = baseline_repository(vertices, edges);
    let available: BTreeSet<&str> = repo
        .non_algorithm_read_names()
        .iter()
        .map(String::as_str)
        .collect();

    let mut ops = Vec::with_capacity(shapes.len());
    for shape in shapes {
        if !available.contains(shape.name) {
            return Err(OtherError(format!(
                "baseline read shape '{}' is annotated but not a queries_repository baseline read \
                 (annotation drift — update src/synthetic/shapes.rs)",
                shape.name
            )));
        }
        let key = OpKey::dynamic(shape.name.to_string(), QueryType::Read);
        let mut rng = StdRng::seed_from_u64(corpus_seed ^ key.salt());
        let mut commands = Vec::with_capacity(CORPUS_SIZE);
        for _ in 0..CORPUS_SIZE {
            let prepared = repo.render_read_with_rng(shape.name, &mut rng).ok_or_else(|| {
                OtherError(format!("baseline read shape '{}' failed to render", shape.name))
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
        // auto-discovered baseline (non-algorithm) reads — no more, no fewer. If `queries_repository`
        // gains or drops a baseline read, this fails until `baseline_read_shapes()` is updated.
        let repo = baseline_repository(1000, 5000);
        let discovered: BTreeSet<&str> =
            repo.non_algorithm_read_names().iter().map(String::as_str).collect();
        let annotated = annotated_names();
        assert_eq!(
            annotated, discovered,
            "annotation drift — annotated-only: {:?}; discovered-only: {:?}",
            annotated.difference(&discovered).collect::<Vec<_>>(),
            discovered.difference(&annotated).collect::<Vec<_>>()
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
    fn exactly_the_limit_without_order_shape_is_result_na() {
        // Only `entity_path_introspection` (LIMIT 1 without ORDER BY) is result-N/A among the
        // baseline reads; every other baseline read is result-gated (Decision 4).
        for shape in baseline_read_shapes() {
            let expected_gated = shape.name != "entity_path_introspection";
            assert_eq!(
                shape.result_policy.is_gated(),
                expected_gated,
                "unexpected result policy for '{}'",
                shape.name
            );
        }
    }

    #[test]
    fn record_repo_reads_full_covers_every_baseline_read() {
        let ops = record_repo_reads(Tier::Full, 1000, 5000, 42).unwrap();
        let names: BTreeSet<&str> = ops.iter().map(|o| o.key.name()).collect();
        assert_eq!(names, annotated_names(), "Full must record every baseline read");
        // Every op renders a full corpus and is keyed by the shape name as a read.
        for op in &ops {
            assert_eq!(op.commands.len(), CORPUS_SIZE, "op '{}' short corpus", op.key.name());
            assert_eq!(op.key.kind(), QueryType::Read);
        }
        // `shortest_path` shares its name with a built-in `OpName`, so `OpKey::dynamic`
        // canonicalizes it to that built-in (by design — same name/kind/salt across every run);
        // every other baseline read is a genuinely dynamic string-keyed op.
        for op in &ops {
            if op.key.name() == "shortest_path" {
                assert!(op.key.is_named(), "shortest_path canonicalizes to the built-in OpName");
            } else {
                assert!(!op.key.is_named(), "'{}' should be a dynamic read", op.key.name());
            }
        }
        // The result-N/A shape is recorded but not gated; the rest are gated.
        let na = ops.iter().find(|o| o.key.name() == "entity_path_introspection").unwrap();
        assert!(!na.result_gated, "the LIMIT-without-ORDER shape is result-N/A");
        assert!(
            ops.iter().filter(|o| !o.result_gated).count() == 1,
            "exactly one baseline read is result-N/A"
        );
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
            tier: Tier::Full,
            result_policy: ResultPolicy::Gated,
        }];
        let err = record_selected_shapes(&bogus, 1000, 5000, 1).unwrap_err();
        assert!(
            format!("{err}").contains("annotation drift"),
            "unexpected error: {err}"
        );
    }
}
