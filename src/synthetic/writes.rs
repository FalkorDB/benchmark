//! Write-operation lifecycle primitives: per-worker scratch isolation, a reset cadence, and
//! per-sample mutation verification. (Part 5 of the synthetic benchmark — see
//! [`synthetic-benchmark.md`](../../synthetic-benchmark.md).)
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
//!    mutation counters against the operation's [`ExpectedMutation`] on every sample.
//!
//! These primitives are deliberately pure (no I/O), so the tricky invariants — reset cadence, key
//! disjointness, mutation checks — are unit-tested in isolation before being wired into the engine.

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;

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
    reset_every: usize,
}

/// The canonical (run-independent) scratch label used when fingerprinting a write workload, so the
/// per-run `run_token` in the real label doesn't change the `corpus_hash`.
pub const CANONICAL_SCRATCH_LABEL: &str = "BenchScratch_RUN";

impl WriteScratch {
    /// Build a worker's scratch, validating that its key band fits in an `i32` (FalkorDB query
    /// parameters are `i32`). The highest key this worker can emit is
    /// `worker_id · reset_every + (reset_every - 1)`, so `(worker_id + 1) · reset_every` must not
    /// exceed [`i32::MAX`]; otherwise a large sweep × cadence would silently overflow.
    pub fn new(
        run_token: u64,
        worker_id: usize,
        reset_every: usize,
    ) -> BenchmarkResult<Self> {
        if reset_every == 0 {
            return Err(OtherError("reset_every must be >= 1".to_string()));
        }
        // (worker_id + 1) * reset_every, guarded against usize *and* i32 overflow.
        let upper = worker_id
            .checked_add(1)
            .and_then(|w| w.checked_mul(reset_every))
            .ok_or_else(|| OtherError("scratch key band overflows usize".to_string()))?;
        if upper > i32::MAX as usize {
            return Err(OtherError(format!(
                "scratch key band overflows i32: worker {} × reset_every {} exceeds {} — lower \
                 --reset-every or the concurrency",
                worker_id,
                reset_every,
                i32::MAX
            )));
        }
        Ok(Self {
            run_token,
            worker_id,
            reset_every,
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
        let base = self.worker_id * self.reset_every;
        let pos = (seq % self.reset_every as u64) as usize;
        (base + pos) as i32
    }

    /// The configured reset cadence (the width of this worker's key band).
    pub fn reset_every(&self) -> usize {
        self.reset_every
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

/// The mutation counters FalkorDB reports for a query, used to verify a write actually did what the
/// operation intends (rather than silently matching nothing).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MutationStats {
    pub nodes_created: i64,
    pub nodes_deleted: i64,
    pub relationships_created: i64,
    pub properties_set: i64,
}

/// What a write operation must effect on **each** invocation, checked against the response's
/// [`MutationStats`] so a no-op (a delete with no target, a merge that hit instead of missed) is a
/// hard error rather than a fast, misleading sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedMutation {
    /// Exactly one node created (`create_node`, `merge_miss`).
    NodeCreated,
    /// Exactly one node deleted (`delete_node`).
    NodeDeleted,
    /// Exactly one relationship created (`create_edge`).
    RelationshipCreated,
    /// Exactly one property set (`set_property`).
    PropertySet,
    /// A merge that matched an existing node — **no** node created (`merge_hit`).
    NodeMatched,
}

/// Verify a sample's [`MutationStats`] match the operation's [`ExpectedMutation`], returning a clear
/// error naming the mismatch so an operation that silently benchmarks the wrong thing fails loudly.
///
/// Each variant asserts its **primary** counter *and* that no **conflicting structural** mutation
/// happened (a `create_node` that also deleted a node or created a relationship is rejected, not
/// just one that created zero nodes). `properties_set` is deliberately left unconstrained for the
/// create/edge variants because inline properties (`{id: $id}`, `{eid: $eid}`) legitimately count
/// toward it, and FalkorDB's exact accounting for inline-vs-`SET` properties is pinned per operation
/// against a live server in the Part 5b integration tests; the non-create variants, which set no
/// inline properties, do assert `properties_set == 0`.
pub fn verify_mutation(
    expected: ExpectedMutation,
    stats: &MutationStats,
) -> BenchmarkResult<()> {
    let ok = match expected {
        ExpectedMutation::NodeCreated => {
            stats.nodes_created == 1
                && stats.nodes_deleted == 0
                && stats.relationships_created == 0
        }
        ExpectedMutation::NodeDeleted => {
            stats.nodes_deleted == 1
                && stats.nodes_created == 0
                && stats.relationships_created == 0
                && stats.properties_set == 0
        }
        ExpectedMutation::RelationshipCreated => {
            stats.relationships_created == 1
                && stats.nodes_created == 0
                && stats.nodes_deleted == 0
        }
        ExpectedMutation::PropertySet => {
            stats.properties_set == 1
                && stats.nodes_created == 0
                && stats.nodes_deleted == 0
                && stats.relationships_created == 0
        }
        ExpectedMutation::NodeMatched => {
            stats.nodes_created == 0
                && stats.nodes_deleted == 0
                && stats.relationships_created == 0
                && stats.properties_set == 0
        }
    };
    if ok {
        Ok(())
    } else {
        Err(OtherError(format!(
            "write operation expected {:?} but the server reported {:?} — the operation is not \
             doing what it should (e.g. a delete matched nothing, a merge hit instead of missed, or \
             a create also mutated something it shouldn't have)",
            expected, stats
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
    fn scratch_rejects_key_band_that_overflows_i32() {
        // A worker index × cadence that would exceed i32::MAX is rejected up front.
        let huge = (i32::MAX as usize / 1000) + 1;
        assert!(WriteScratch::new(0, huge, 1000).is_err());
        assert!(WriteScratch::new(0, 0, 1000).is_ok());
    }

    #[test]
    fn verify_mutation_accepts_the_expected_effect() {
        assert!(verify_mutation(
            ExpectedMutation::NodeCreated,
            &MutationStats {
                nodes_created: 1,
                ..Default::default()
            }
        )
        .is_ok());
        assert!(verify_mutation(
            ExpectedMutation::NodeDeleted,
            &MutationStats {
                nodes_deleted: 1,
                ..Default::default()
            }
        )
        .is_ok());
        assert!(verify_mutation(
            ExpectedMutation::RelationshipCreated,
            &MutationStats {
                relationships_created: 1,
                ..Default::default()
            }
        )
        .is_ok());
        assert!(verify_mutation(
            ExpectedMutation::PropertySet,
            &MutationStats {
                properties_set: 1,
                ..Default::default()
            }
        )
        .is_ok());
        // merge_hit: a match, so nothing created.
        assert!(verify_mutation(ExpectedMutation::NodeMatched, &MutationStats::default()).is_ok());
        // A create's inline property (`{id: $id}`) legitimately bumps properties_set, so it is not
        // constrained for the create/edge variants.
        assert!(verify_mutation(
            ExpectedMutation::NodeCreated,
            &MutationStats {
                nodes_created: 1,
                properties_set: 1,
                ..Default::default()
            }
        )
        .is_ok());
        assert!(verify_mutation(
            ExpectedMutation::RelationshipCreated,
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
        assert!(verify_mutation(ExpectedMutation::NodeDeleted, &MutationStats::default()).is_err());
        // A merge that HIT when it should have MISSED (created 0, expected 1).
        assert!(verify_mutation(ExpectedMutation::NodeCreated, &MutationStats::default()).is_err());
        // A merge that MISSED when it should have HIT (created 1, expected 0).
        assert!(verify_mutation(
            ExpectedMutation::NodeMatched,
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
            ExpectedMutation::NodeCreated,
            &MutationStats {
                nodes_created: 1,
                nodes_deleted: 1,
                ..Default::default()
            }
        )
        .is_err());
        // A create that also created a relationship.
        assert!(verify_mutation(
            ExpectedMutation::NodeCreated,
            &MutationStats {
                nodes_created: 1,
                relationships_created: 1,
                ..Default::default()
            }
        )
        .is_err());
        // A delete that also created a node.
        assert!(verify_mutation(
            ExpectedMutation::NodeDeleted,
            &MutationStats {
                nodes_deleted: 1,
                nodes_created: 1,
                ..Default::default()
            }
        )
        .is_err());
        // A set_property that also created a node.
        assert!(verify_mutation(
            ExpectedMutation::PropertySet,
            &MutationStats {
                properties_set: 1,
                nodes_created: 1,
                ..Default::default()
            }
        )
        .is_err());
        // A merge_hit that unexpectedly set a property.
        assert!(verify_mutation(
            ExpectedMutation::NodeMatched,
            &MutationStats {
                properties_set: 1,
                ..Default::default()
            }
        )
        .is_err());
    }
}
