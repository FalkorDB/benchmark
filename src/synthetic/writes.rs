//! Write-operation lifecycle primitives: per-worker scratch isolation, a reset cadence, and
//! per-sample mutation verification. (Part 5 of the synthetic benchmark — see
//! [`synthetic-benchmark.md`](https://github.com/FalkorDB/benchmark/blob/master/synthetic-benchmark.md).)
//!
//! Write operations mutate the graph, so three problems must be solved before their latency is
//! meaningful:
//!
//! 1. **Isolation** — concurrent workers must not collide, and the benchmark must never touch a
//!    real user's data or another run's scratch. [`WriteScratch`] gives each worker a **run-unique**
//!    label (`BenchScratch_<run_token>`) plus a disjoint per-worker key band, so a reset only ever
//!    deletes this worker's rows and setup/cleanup can safely wipe by label.
//! 2. **Drift** — repeated `create`/`merge_miss` grow the graph unboundedly. [`ResetSchedule`] fires
//!    an (untimed) reset every `reset_every` operations, counted over the **global** invocation
//!    sequence (warm-up included), bounding accumulation to one sawtooth window.
//! 3. **Silent no-ops** — a `delete` with no target, or a `merge` that hit when it should have
//!    missed, would benchmark the wrong thing. [`verify_mutation`] checks FalkorDB's reported
//!    mutation counters against the operation's [`ExpectedOutcome`] on every sample.
//!
//! These primitives are deliberately pure (no I/O), so the tricky invariants — reset cadence, key
//! disjointness, mutation checks — are unit-tested in isolation before being wired into the engine.

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::query::Query;
use serde::{Deserialize, Serialize};

/// Fires a reset every `reset_every` operations, counted over the **global** invocation sequence
/// (warm-up + measured), so scratch that warm-up mutated is bounded too.
///
/// A reset runs *between* windows — before invocations `reset_every`, `2·reset_every`, … — never at
/// `seq == 0` (the initial state is established by setup, not a reset). Within a window, an
/// operation's identity comes from [`ResetSchedule::window_pos`] so keys reused after a reset don't
/// accumulate duplicates.
#[derive(Debug, Clone, Copy)]
pub struct ResetSchedule {
    reset_every: usize,
}

impl ResetSchedule {
    /// Create a schedule with a positive cadence. `reset_every == 0` is rejected (a zero cadence
    /// would divide by zero and never bound drift).
    pub fn new(reset_every: usize) -> BenchmarkResult<Self> {
        if reset_every == 0 {
            return Err(OtherError(
                "reset_every must be >= 1 (0 would never bound write drift)".to_string(),
            ));
        }
        Ok(Self { reset_every })
    }

    /// Whether a reset must run *before* invocation `seq` — true exactly at the window boundaries
    /// `reset_every`, `2·reset_every`, …, and never at `seq == 0`.
    pub fn should_reset(
        &self,
        seq: u64,
    ) -> bool {
        seq > 0 && seq.is_multiple_of(self.reset_every as u64)
    }

    /// The 0-based position of `seq` within its reset window (`seq % reset_every`) — the index a
    /// write op uses to pick a within-window-unique key/identity.
    pub fn window_pos(
        &self,
        seq: u64,
    ) -> u64 {
        seq % self.reset_every as u64
    }

    /// The configured cadence.
    pub fn reset_every(&self) -> usize {
        self.reset_every
    }
}

/// A worker's isolated scratch namespace.
///
/// Isolation is layered: a **run-unique label** ([`WriteScratch::label`], `BenchScratch_<run_token>`
/// in hex) keeps the whole run apart from real data and other benchmark processes, while a disjoint
/// per-worker **key band** ([`WriteScratch::window_key`], `worker · reset_every + window_pos`) keeps
/// concurrent workers apart within the run. Both together mean a reset scoped to this worker's band
/// (or a cleanup scoped to the run label) can never delete another worker's or another run's rows.
///
/// Keys are **run-independent** (they don't fold in `run_token`), so the workload is comparable
/// across runs; only the label carries the per-run nonce.
#[derive(Debug, Clone)]
pub struct WriteScratch {
    /// The per-run nonce that makes the scratch label unique (usually the run's `run_token`).
    /// Private so the invariants proved in [`WriteScratch::new`] can't be broken after construction;
    /// read via [`WriteScratch::run_token`].
    run_token: u64,
    /// This worker's index in the level (`0..concurrency`). Private (see [`WriteScratch::new`]'s i32
    /// key-band bound); read via [`WriteScratch::worker_id`].
    worker_id: usize,
    /// The reset cadence, which also defines the width of this worker's key band. Composed (rather
    /// than storing a bare `reset_every`) so the positive-cadence invariant lives only in
    /// [`ResetSchedule::new`].
    schedule: ResetSchedule,
}

/// The canonical (run-independent) scratch label used when fingerprinting a write workload, so the
/// per-run `run_token` in the real label doesn't change the `corpus_hash`.
pub const CANONICAL_SCRATCH_LABEL: &str = "BenchScratch_RUN";

impl WriteScratch {
    /// Build a worker's scratch, validating that its key band fits in an `i32` (FalkorDB query
    /// parameters are `i32`). The highest key this worker can emit is
    /// `max_key = worker_id · reset_every + (reset_every - 1) = (worker_id + 1) · reset_every - 1`,
    /// which must not exceed [`i32::MAX`]; otherwise a large sweep × cadence would silently overflow.
    pub fn new(
        run_token: u64,
        worker_id: usize,
        reset_every: usize,
    ) -> BenchmarkResult<Self> {
        // The reset cadence carries the positive-cadence invariant (rejects 0) and doubles as this
        // worker's key-band width.
        let schedule = ResetSchedule::new(reset_every)?;
        // The highest key is `(worker_id + 1) * reset_every - 1`, computed with the multiplication
        // guarded against usize overflow. `reset_every >= 1` here, so `upper >= 1` and the `- 1`
        // can't underflow.
        let upper = worker_id
            .checked_add(1)
            .and_then(|w| w.checked_mul(reset_every))
            .ok_or_else(|| OtherError("scratch key band overflows usize".to_string()))?;
        let max_key = upper - 1;
        if max_key > i32::MAX as usize {
            return Err(OtherError(format!(
                "scratch key band overflows i32: worker {}'s highest key {} exceeds {} — reduce \
                 reset_every or the worker count",
                worker_id,
                max_key,
                i32::MAX
            )));
        }
        Ok(Self {
            run_token,
            worker_id,
            schedule,
        })
    }

    /// The run-unique scratch label baked into this worker's query bodies (shared across all workers
    /// of a run, so the plan cache stays warm; unique per run, so it can't hit real data).
    pub fn label(&self) -> String {
        format!("BenchScratch_{:x}", self.run_token)
    }

    /// A within-window-unique, cross-worker-disjoint key/identity for invocation `seq`:
    /// `worker_id · reset_every + (seq % reset_every)`. Two workers never share a key, and within a
    /// reset window every `seq` yields a distinct key (so `merge_miss` always misses and
    /// `create_edge` identities never repeat); after a reset the band is reused without duplicates.
    pub fn window_key(
        &self,
        seq: u64,
    ) -> i32 {
        // Bounds were validated in `new`, so these fit i32.
        let base = self.worker_id * self.schedule.reset_every();
        let pos = self.schedule.window_pos(seq) as usize;
        (base + pos) as i32
    }

    /// The configured reset cadence (the width of this worker's key band).
    pub fn reset_every(&self) -> usize {
        self.schedule.reset_every()
    }

    /// This worker's [`ResetSchedule`] (the reset cadence over the global invocation sequence).
    pub fn schedule(&self) -> ResetSchedule {
        self.schedule
    }

    /// This worker's inclusive key band `[lo, hi]` (`lo = worker_id · reset_every`,
    /// `hi = lo + reset_every - 1`) — the id range a reset scopes its delete to, so it only ever
    /// clears this worker's rows. Both ends fit `i32` (validated in [`WriteScratch::new`]).
    pub fn key_band(&self) -> (i32, i32) {
        let lo = self.worker_id * self.schedule.reset_every();
        let hi = lo + self.schedule.reset_every() - 1;
        (lo as i32, hi as i32)
    }

    /// This worker's index in the level (`0..concurrency`).
    pub fn worker_id(&self) -> usize {
        self.worker_id
    }

    /// The per-run nonce baked into [`WriteScratch::label`].
    pub fn run_token(&self) -> u64 {
        self.run_token
    }
}

/// The lifecycle of a write operation: what it mutates (for per-sample verification), a stable tag
/// for the workload hash, a default reset cadence, and the untimed setup/reset/cleanup hooks plus
/// the timed per-invocation query builder. Fn pointers keep [`crate::synthetic::catalog::
/// OperationSpec`] `Copy`; every hook returns already-built [`Query`]s scoped to a worker's
/// [`WriteScratch`].
#[derive(Clone, Copy)]
pub struct WritePlan {
    /// What each invocation must mutate, checked against the response counters via
    /// [`verify_mutation`].
    pub expected: ExpectedOutcome,
    /// A stable identifier for this operation's query shape, folded into the workload hash so a
    /// change to the write bodies makes old and new runs incomparable. Bump the suffix when the
    /// cypher changes (e.g. `"create_node.v1"`).
    pub plan_tag: &'static str,
    /// Reset cadence (ops per sawtooth window) when the config doesn't override `reset_every`.
    pub default_reset_every: usize,
    /// Untimed statements run once per worker before the measurement window (e.g. clear this
    /// worker's key band, or pre-create a pool).
    pub setup: fn(&WriteScratch) -> BenchmarkResult<Vec<Query>>,
    /// Untimed statements run every `reset_every` ops to undo drift (scoped to this worker's band).
    pub reset: fn(&WriteScratch) -> BenchmarkResult<Vec<Query>>,
    /// Untimed statements run once after the level to drop the run's scratch (scoped to the run
    /// label, so it clears every worker at once).
    pub cleanup: fn(&WriteScratch) -> BenchmarkResult<Vec<Query>>,
    /// The timed query for measured invocation `seq` (identity from `scratch.window_key(seq)`).
    pub render: fn(&WriteScratch, u64) -> BenchmarkResult<Query>,
}

/// The mutation counters FalkorDB reports for a query, used to verify a write actually did what the
/// operation intends (rather than silently matching nothing). Covers the seven counters the write
/// plans verify (Phase 7 §6.2) — a deliberate subset of the server's statistics (e.g. `labels_added`
/// and index counters are not tracked): absent counters read as 0 (FalkorDB omits untouched
/// statistics).
///
/// Serialized verbatim into a bundle's §6.3 oracle records (`oracle/<op>.jsonl`), so the serde
/// contract is strict: every counter is required (no defaults) and unknown fields are rejected —
/// a hand-edited or truncated record fails to parse rather than reading as zeros.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MutationStats {
    pub nodes_created: i64,
    pub nodes_deleted: i64,
    pub relationships_created: i64,
    pub relationships_deleted: i64,
    pub properties_set: i64,
    pub properties_removed: i64,
    pub labels_removed: i64,
}

/// One mutation counter's per-invocation expectation: pinned to an exact value, or explicitly
/// unconstrained (for counters that legitimately vary, like a create's inline `properties_set`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CounterExpectation {
    /// The counter must equal exactly this value.
    Exactly(i64),
    /// The counter is legitimately variable — not checked.
    Any,
}

impl CounterExpectation {
    /// Whether `reported` satisfies this expectation.
    fn accepts(
        self,
        reported: i64,
    ) -> bool {
        match self {
            CounterExpectation::Exactly(want) => reported == want,
            CounterExpectation::Any => true,
        }
    }
}

/// What a write operation must effect on **each** invocation — one [`CounterExpectation`] per
/// [`MutationStats`] counter, checked by [`verify_mutation`] so a no-op (a delete with no target, a
/// merge that hit instead of missed) is a hard error rather than a fast, misleading sample.
///
/// This is the generalized per-invocation outcome model of Phase 7 §6.2 (replacing five rigid
/// unit variants that could not express `DETACH DELETE`'s `relationships_deleted` or `REMOVE`'s
/// `properties_removed`/`labels_removed`): the catalog's fixed-shape writes use the named
/// constructors below, and the §6.3 online oracle pins a recorded per-command outcome with
/// [`ExpectedOutcome::exactly`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedOutcome {
    pub nodes_created: CounterExpectation,
    pub nodes_deleted: CounterExpectation,
    pub relationships_created: CounterExpectation,
    pub relationships_deleted: CounterExpectation,
    pub properties_set: CounterExpectation,
    pub properties_removed: CounterExpectation,
    pub labels_removed: CounterExpectation,
}

impl ExpectedOutcome {
    /// Every counter pinned to 0 — the base the named constructors override, and exactly what a
    /// pure match (`merge_hit`) must report.
    const ZERO: Self = Self {
        nodes_created: CounterExpectation::Exactly(0),
        nodes_deleted: CounterExpectation::Exactly(0),
        relationships_created: CounterExpectation::Exactly(0),
        relationships_deleted: CounterExpectation::Exactly(0),
        properties_set: CounterExpectation::Exactly(0),
        properties_removed: CounterExpectation::Exactly(0),
        labels_removed: CounterExpectation::Exactly(0),
    };

    /// Pin every counter to the given stats — the §6.3 oracle mode, where record captured the
    /// command's *actual* outcome and replay must reproduce it exactly.
    pub fn exactly(stats: MutationStats) -> Self {
        Self {
            nodes_created: CounterExpectation::Exactly(stats.nodes_created),
            nodes_deleted: CounterExpectation::Exactly(stats.nodes_deleted),
            relationships_created: CounterExpectation::Exactly(stats.relationships_created),
            relationships_deleted: CounterExpectation::Exactly(stats.relationships_deleted),
            properties_set: CounterExpectation::Exactly(stats.properties_set),
            properties_removed: CounterExpectation::Exactly(stats.properties_removed),
            labels_removed: CounterExpectation::Exactly(stats.labels_removed),
        }
    }

    /// Exactly one node created (`create_node`, `merge_miss`). `properties_set` is unconstrained:
    /// the shape's inline property (`{id: $id}`) legitimately counts toward it.
    pub fn node_created() -> Self {
        Self {
            nodes_created: CounterExpectation::Exactly(1),
            properties_set: CounterExpectation::Any,
            ..Self::ZERO
        }
    }

    /// Exactly one node deleted (`delete_node`), nothing else — the scratch target is edgeless, so
    /// even `relationships_deleted` must stay 0.
    pub fn node_deleted() -> Self {
        Self {
            nodes_deleted: CounterExpectation::Exactly(1),
            ..Self::ZERO
        }
    }

    /// Exactly one relationship created (`create_edge`). `properties_set` is unconstrained for the
    /// same inline-property reason as [`ExpectedOutcome::node_created`].
    pub fn relationship_created() -> Self {
        Self {
            relationships_created: CounterExpectation::Exactly(1),
            properties_set: CounterExpectation::Any,
            ..Self::ZERO
        }
    }

    /// Exactly one property set (`set_property`), nothing else.
    pub fn property_set() -> Self {
        Self {
            properties_set: CounterExpectation::Exactly(1),
            ..Self::ZERO
        }
    }

    /// A merge that matched an existing node (`merge_hit`) — every counter must be 0.
    pub fn node_matched() -> Self {
        Self::ZERO
    }
}

/// Verify a sample's [`MutationStats`] satisfy the operation's [`ExpectedOutcome`], returning a
/// clear error naming every mismatching counter so an operation that silently benchmarks the wrong
/// thing fails loudly.
pub fn verify_mutation(
    expected: ExpectedOutcome,
    stats: &MutationStats,
) -> BenchmarkResult<()> {
    // (counter name, expectation, reported) — one row per MutationStats counter, so a new counter
    // can't be silently skipped here (the exhaustive destructuring below fails to compile).
    let MutationStats {
        nodes_created,
        nodes_deleted,
        relationships_created,
        relationships_deleted,
        properties_set,
        properties_removed,
        labels_removed,
    } = *stats;
    let checks = [
        ("nodes_created", expected.nodes_created, nodes_created),
        ("nodes_deleted", expected.nodes_deleted, nodes_deleted),
        ("relationships_created", expected.relationships_created, relationships_created),
        ("relationships_deleted", expected.relationships_deleted, relationships_deleted),
        ("properties_set", expected.properties_set, properties_set),
        ("properties_removed", expected.properties_removed, properties_removed),
        ("labels_removed", expected.labels_removed, labels_removed),
    ];
    let mismatches: Vec<String> = checks
        .iter()
        .filter(|(_, want, got)| !want.accepts(*got))
        .map(|(name, want, got)| match want {
            CounterExpectation::Exactly(want) => {
                format!("{name}: expected exactly {want}, server reported {got}")
            }
            CounterExpectation::Any => unreachable!("Any accepts every value"),
        })
        .collect();
    if mismatches.is_empty() {
        Ok(())
    } else {
        Err(OtherError(format!(
            "write outcome mismatch ({}) — the operation is not doing what it should (e.g. a \
             delete matched nothing, a merge hit instead of missed, or a write also mutated \
             something it shouldn't have)",
            mismatches.join("; ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_schedule_rejects_zero() {
        assert!(ResetSchedule::new(0).is_err());
        assert!(ResetSchedule::new(1).is_ok());
    }

    #[test]
    fn reset_fires_on_global_op_count() {
        let s = ResetSchedule::new(4).unwrap();
        // Never at 0; exactly at each window boundary.
        assert!(!s.should_reset(0));
        let boundaries: Vec<u64> = (1..=12).filter(|&seq| s.should_reset(seq)).collect();
        assert_eq!(boundaries, vec![4, 8, 12]);
        // Cadence is independent of any warm-up/measurement split — it's the raw global seq.
        assert_eq!((0..=12).filter(|&seq| s.should_reset(seq)).count(), 3);
    }

    #[test]
    fn window_pos_cycles_within_the_window() {
        let s = ResetSchedule::new(3).unwrap();
        let positions: Vec<u64> = (0..7).map(|seq| s.window_pos(seq)).collect();
        assert_eq!(positions, vec![0, 1, 2, 0, 1, 2, 0]);
    }

    #[test]
    fn scratch_label_is_run_unique_but_keys_are_run_independent() {
        let a = WriteScratch::new(0xABCD, 0, 10).unwrap();
        let b = WriteScratch::new(0x1234, 0, 10).unwrap();
        // Different runs ⇒ different labels…
        assert_eq!(a.label(), "BenchScratch_abcd");
        assert_ne!(a.label(), b.label());
        // …but identical keys (the workload is comparable across runs).
        for seq in 0..25 {
            assert_eq!(a.window_key(seq), b.window_key(seq));
        }
    }

    #[test]
    fn keys_are_disjoint_across_workers_and_deterministic() {
        let reset_every = 10;
        let workers: Vec<WriteScratch> = (0..4)
            .map(|w| WriteScratch::new(7, w, reset_every).unwrap())
            .collect();
        // Collect each worker's full window of keys.
        let mut all = std::collections::HashSet::new();
        for w in &workers {
            let window: Vec<i32> = (0..reset_every as u64)
                .map(|seq| w.window_key(seq))
                .collect();
            // Within a window every key is distinct (⇒ merge_miss misses, edge ids are unique).
            let window_set: std::collections::HashSet<i32> = window.iter().copied().collect();
            assert_eq!(window_set.len(), reset_every, "keys repeat within a window");
            // Determinism: same inputs ⇒ same keys.
            let again: Vec<i32> = (0..reset_every as u64)
                .map(|seq| w.window_key(seq))
                .collect();
            assert_eq!(window, again);
            // No cross-worker collision.
            for k in window {
                assert!(all.insert(k), "worker key bands overlap at {k}");
            }
        }
    }

    #[test]
    fn keys_reuse_the_band_after_a_reset_window() {
        let w = WriteScratch::new(1, 2, 5).unwrap();
        // seq and seq+reset_every land on the same key (the band is reused post-reset).
        for seq in 0..5 {
            assert_eq!(w.window_key(seq), w.window_key(seq + 5));
        }
    }

    #[test]
    fn scratch_exposes_its_composed_reset_schedule() {
        let w = WriteScratch::new(0, 1, 8).unwrap();
        assert_eq!(w.reset_every(), 8);
        assert_eq!(w.schedule().reset_every(), 8);
        assert!(w.schedule().should_reset(8));
        assert!(!w.schedule().should_reset(0));
        // A zero cadence is rejected once, in ResetSchedule::new (no duplicated check).
        assert!(WriteScratch::new(0, 0, 0).is_err());
    }

    #[test]
    fn scratch_rejects_key_band_that_overflows_i32() {
        // A worker index × cadence that would exceed i32::MAX is rejected up front.
        let huge = (i32::MAX as usize / 1000) + 1;
        assert!(WriteScratch::new(0, huge, 1000).is_err());
        assert!(WriteScratch::new(0, 0, 1000).is_ok());

        // Exact boundary: the highest key is `(worker_id + 1) * reset_every - 1`. With
        // `reset_every == 1` and `worker_id == i32::MAX`, the highest key is exactly `i32::MAX` —
        // a valid i32, so it must be ACCEPTED (guards against an off-by-one that rejects it)…
        let at_max = WriteScratch::new(0, i32::MAX as usize, 1).unwrap();
        assert_eq!(at_max.window_key(0), i32::MAX);
        // …while one worker higher pushes the highest key to `i32::MAX + 1`, which is rejected.
        assert!(WriteScratch::new(0, i32::MAX as usize + 1, 1).is_err());
    }

    #[test]
    fn verify_mutation_accepts_the_expected_effect() {
        assert!(verify_mutation(
            ExpectedOutcome::node_created(),
            &MutationStats {
                nodes_created: 1,
                ..Default::default()
            }
        )
        .is_ok());
        assert!(verify_mutation(
            ExpectedOutcome::node_deleted(),
            &MutationStats {
                nodes_deleted: 1,
                ..Default::default()
            }
        )
        .is_ok());
        assert!(verify_mutation(
            ExpectedOutcome::relationship_created(),
            &MutationStats {
                relationships_created: 1,
                ..Default::default()
            }
        )
        .is_ok());
        assert!(verify_mutation(
            ExpectedOutcome::property_set(),
            &MutationStats {
                properties_set: 1,
                ..Default::default()
            }
        )
        .is_ok());
        // merge_hit: a match, so nothing created.
        assert!(verify_mutation(ExpectedOutcome::node_matched(), &MutationStats::default()).is_ok());
        // A create's inline property (`{id: $id}`) legitimately bumps properties_set, so it is not
        // constrained for the create/edge variants.
        assert!(verify_mutation(
            ExpectedOutcome::node_created(),
            &MutationStats {
                nodes_created: 1,
                properties_set: 1,
                ..Default::default()
            }
        )
        .is_ok());
        assert!(verify_mutation(
            ExpectedOutcome::relationship_created(),
            &MutationStats {
                relationships_created: 1,
                properties_set: 1,
                ..Default::default()
            }
        )
        .is_ok());
    }

    #[test]
    fn verify_mutation_rejects_a_silent_no_op() {
        // A delete that matched nothing.
        assert!(verify_mutation(ExpectedOutcome::node_deleted(), &MutationStats::default()).is_err());
        // A merge that HIT when it should have MISSED (created 0, expected 1).
        assert!(verify_mutation(ExpectedOutcome::node_created(), &MutationStats::default()).is_err());
        // A merge that MISSED when it should have HIT (created 1, expected 0).
        assert!(verify_mutation(
            ExpectedOutcome::node_matched(),
            &MutationStats {
                nodes_created: 1,
                ..Default::default()
            }
        )
        .is_err());
    }

    #[test]
    fn verify_mutation_rejects_a_noisy_op() {
        // A create that ALSO deleted a node — a conflicting structural mutation, not just a
        // wrong-count no-op — must be rejected even though it created one node.
        assert!(verify_mutation(
            ExpectedOutcome::node_created(),
            &MutationStats {
                nodes_created: 1,
                nodes_deleted: 1,
                ..Default::default()
            }
        )
        .is_err());
        // A create that also created a relationship.
        assert!(verify_mutation(
            ExpectedOutcome::node_created(),
            &MutationStats {
                nodes_created: 1,
                relationships_created: 1,
                ..Default::default()
            }
        )
        .is_err());
        // A delete that also created a node.
        assert!(verify_mutation(
            ExpectedOutcome::node_deleted(),
            &MutationStats {
                nodes_deleted: 1,
                nodes_created: 1,
                ..Default::default()
            }
        )
        .is_err());
        // A set_property that also created a node.
        assert!(verify_mutation(
            ExpectedOutcome::property_set(),
            &MutationStats {
                properties_set: 1,
                nodes_created: 1,
                ..Default::default()
            }
        )
        .is_err());
        // A merge_hit that unexpectedly set a property.
        assert!(verify_mutation(
            ExpectedOutcome::node_matched(),
            &MutationStats {
                properties_set: 1,
                ..Default::default()
            }
        )
        .is_err());
        // A delete that also silently dropped edges — the new counters are pinned to 0 for the
        // catalog shapes (an edgeless scratch target must not report relationship deletions).
        assert!(verify_mutation(
            ExpectedOutcome::node_deleted(),
            &MutationStats {
                nodes_deleted: 1,
                relationships_deleted: 2,
                ..Default::default()
            }
        )
        .is_err());
    }

    #[test]
    fn exactly_pins_every_counter() {
        // The §6.3 oracle mode: an outcome built from recorded stats accepts exactly those stats…
        let recorded = MutationStats {
            nodes_created: 1,
            nodes_deleted: 2,
            relationships_created: 3,
            relationships_deleted: 4,
            properties_set: 5,
            properties_removed: 6,
            labels_removed: 7,
        };
        let expected = ExpectedOutcome::exactly(recorded);
        assert!(verify_mutation(expected, &recorded).is_ok());
        // …and rejects a deviation in ANY single counter (each counter is independently pinned).
        for i in 0..7 {
            let mut off = recorded;
            match i {
                0 => off.nodes_created += 1,
                1 => off.nodes_deleted += 1,
                2 => off.relationships_created += 1,
                3 => off.relationships_deleted += 1,
                4 => off.properties_set += 1,
                5 => off.properties_removed += 1,
                _ => off.labels_removed += 1,
            }
            assert!(verify_mutation(expected, &off).is_err(), "counter {i} not pinned");
        }
    }

    #[test]
    fn generalized_outcomes_express_detach_delete_and_remove() {
        // The outcomes the 5 rigid variants could not represent (design §2/§10.4), pinned to the
        // live-verified counter behavior: DETACH DELETE reports the deleted node AND its degree…
        let detach_delete = ExpectedOutcome::exactly(MutationStats {
            nodes_deleted: 1,
            relationships_deleted: 3,
            ..Default::default()
        });
        assert!(verify_mutation(
            detach_delete,
            &MutationStats {
                nodes_deleted: 1,
                relationships_deleted: 3,
                ..Default::default()
            }
        )
        .is_ok());
        // …a wrong degree is a mismatch, not a pass…
        assert!(verify_mutation(
            detach_delete,
            &MutationStats {
                nodes_deleted: 1,
                relationships_deleted: 2,
                ..Default::default()
            }
        )
        .is_err());
        // …and REMOVE prop/label reports the removal counters.
        let remove = ExpectedOutcome::exactly(MutationStats {
            properties_removed: 1,
            labels_removed: 1,
            ..Default::default()
        });
        assert!(verify_mutation(
            remove,
            &MutationStats {
                properties_removed: 1,
                labels_removed: 1,
                ..Default::default()
            }
        )
        .is_ok());
        // A REMOVE that silently no-opped (unprepared state) is a hard error.
        assert!(verify_mutation(remove, &MutationStats::default()).is_err());
    }

    #[test]
    fn verify_mutation_names_every_mismatching_counter() {
        let err = verify_mutation(
            ExpectedOutcome::node_deleted(),
            &MutationStats {
                nodes_created: 1,
                properties_removed: 2,
                ..Default::default()
            },
        )
        .unwrap_err();
        let msg = format!("{err}");
        // All three deviations named, with expected vs reported values.
        assert!(msg.contains("nodes_created: expected exactly 0, server reported 1"), "{msg}");
        assert!(msg.contains("nodes_deleted: expected exactly 1, server reported 0"), "{msg}");
        assert!(msg.contains("properties_removed: expected exactly 0, server reported 2"), "{msg}");
        // Unconstrained counters never appear: node_created() leaves properties_set free.
        let ok = verify_mutation(
            ExpectedOutcome::node_created(),
            &MutationStats {
                nodes_created: 1,
                properties_set: 40,
                ..Default::default()
            },
        );
        assert!(ok.is_ok());
    }
}
