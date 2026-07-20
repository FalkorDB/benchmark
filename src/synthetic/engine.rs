//! The closed-loop concurrency engine that turns a set of workers into one measurement *level*.
//!
//! A *level* runs `C` worker tasks against a connection pool of size `C` (one connection per
//! worker). Each worker fires an operation, awaits it to completion (row draining included), then
//! fires the next from its pre-generated sequence — a **closed loop**: there is always exactly one
//! outstanding request per worker, so at most `C` are in flight. Because a new request is issued
//! only after the previous one *completes*, the throughput it reports is **achieved** (what the
//! server actually served), not **offered** (a target arrival rate) — so it cannot suffer
//! coordinated omission, but it also can't push past the server's own service rate.
//!
//! ## Windowing
//! Every worker runs `warmup` discarded invocations, then all workers cross a [`Barrier`] together
//! so the measurement window starts with all `C` active. Each worker records when its first
//! measured invocation *started* and its last one *completed*; the level's window is
//! `max(last_completed) − min(first_started)` and achieved throughput is `completed / window`.
//! Using the observed span (rather than a single leader clock) keeps the throughput honest even
//! though workers are released a few microseconds apart.
//!
//! The engine is generic over an [`OpInvoker`] so the real graph worker and a deterministic fake
//! (used by the unit tests to assert exact in-flight concurrency and throughput math) share one
//! code path.

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::synthetic::op_runner::OpSample;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Barrier;
use tokio::task::JoinSet;
// `tokio::time::Instant` (not `std::time::Instant`) so the measurement window advances with the
// runtime clock — real time in production, and the virtual clock under a `start_paused` test, which
// makes the throughput math deterministic.
use tokio::time::Instant;

/// One unit of work the engine can drive: invocation `seq` of an operation on a worker's own
/// connection. `seq` is the worker-local monotonic counter (warm-up invocations are `0..warmup`,
/// measured ones continue from `warmup`), letting the implementor vary parameters and force a
/// plan-cache miss deterministically.
///
/// The returned future must be `Send` (the engine drives each worker on its own `tokio` task) and
/// borrows `&mut self`, so a worker owns its connection exclusively for the whole level.
pub trait OpInvoker: Send + 'static {
    fn invoke(
        &mut self,
        seq: u64,
    ) -> impl Future<Output = BenchmarkResult<OpSample>> + Send;
}

/// The raw result of one measurement level: every retained-or-not paired sample pooled across
/// workers, the number of completed measured invocations, and the wall-clock measurement window.
pub struct LevelRun {
    /// Paired `(server_ms, total_ms)` samples pooled across all workers (warm-up excluded).
    pub samples: Vec<OpSample>,
    /// Count of completed measured invocations (`= samples.len()`), the numerator of throughput.
    pub completed: usize,
    /// Observed measurement window: `max(last_completed) − min(first_started)` across workers.
    pub window: Duration,
}

impl LevelRun {
    /// Achieved throughput in operations per second (`completed / window`), or `0.0` for a
    /// degenerate (zero-length) window.
    pub fn throughput_ops_per_sec(&self) -> f64 {
        let secs = self.window.as_secs_f64();
        if secs > 0.0 {
            self.completed as f64 / secs
        } else {
            0.0
        }
    }
}

/// Per-worker output collected before pooling: its measured samples and the span it was active.
struct WorkerOutput {
    samples: Vec<OpSample>,
    first_started: Instant,
    last_completed: Instant,
}

/// Drive one closed-loop measurement level to completion.
///
/// Spawns one task per worker; each runs `warmup` discarded invocations, waits on a shared
/// [`Barrier`] so all `C` enter the measurement window together, then runs `samples` measured
/// invocations recording paired latencies. The window is the observed `max(last_completed) −
/// min(first_started)` span. On the first worker error (or a task panic) every other worker is
/// aborted and the error is returned — a partial, misleading throughput is never reported.
///
/// Fails fast if `workers` is empty or `samples == 0`.
pub async fn run_closed_loop<W>(
    workers: Vec<W>,
    warmup: usize,
    samples: usize,
) -> BenchmarkResult<LevelRun>
where
    W: OpInvoker,
{
    let concurrency = workers.len();
    if concurrency == 0 {
        return Err(OtherError("concurrency must be >= 1".to_string()));
    }
    if samples == 0 {
        return Err(OtherError("samples must be >= 1".to_string()));
    }

    // All workers cross this barrier after warm-up, so the measurement window opens with every
    // worker active (no ramp where only some workers have started measuring).
    let barrier = Arc::new(Barrier::new(concurrency));
    let mut set: JoinSet<BenchmarkResult<WorkerOutput>> = JoinSet::new();

    for mut worker in workers {
        let barrier = Arc::clone(&barrier);
        set.spawn(async move {
            // Warm-up (discarded): primes the plan cache (cached mode) and the connection.
            for seq in 0..warmup as u64 {
                let _ = worker.invoke(seq).await?;
            }

            barrier.wait().await;

            let mut out: Vec<OpSample> = Vec::with_capacity(samples);
            let mut first_started: Option<Instant> = None;
            let mut last_completed = Instant::now();
            for k in 0..samples as u64 {
                // Continue the sequence past warm-up so every worker key stays unique.
                let seq = warmup as u64 + k;
                let started = Instant::now();
                if first_started.is_none() {
                    first_started = Some(started);
                }
                let sample = worker.invoke(seq).await?;
                last_completed = Instant::now();
                out.push(sample);
            }

            Ok(WorkerOutput {
                // `samples >= 1` is enforced above, so `first_started` is always set.
                first_started: first_started.unwrap_or(last_completed),
                last_completed,
                samples: out,
            })
        });
    }

    // Collect fail-fast: on the first error or panic, abort the rest and surface the error rather
    // than aggregating a misleading throughput from a half-run level.
    let mut outputs: Vec<WorkerOutput> = Vec::with_capacity(concurrency);
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Ok(output)) => outputs.push(output),
            Ok(Err(e)) => {
                set.abort_all();
                while set.join_next().await.is_some() {}
                return Err(e);
            }
            Err(join_err) => {
                set.abort_all();
                while set.join_next().await.is_some() {}
                return Err(OtherError(format!("worker task failed: {}", join_err)));
            }
        }
    }

    let first = outputs.iter().map(|o| o.first_started).min();
    let last = outputs.iter().map(|o| o.last_completed).max();
    let window = match (first, last) {
        (Some(f), Some(l)) => l.saturating_duration_since(f),
        _ => Duration::ZERO,
    };

    let mut samples_all: Vec<OpSample> = Vec::new();
    for output in outputs {
        samples_all.extend(output.samples);
    }
    let completed = samples_all.len();

    Ok(LevelRun {
        samples: samples_all,
        completed,
        window,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A fake worker that guarantees the engine really holds `C` requests in flight at once: on its
    /// first measured invocation it bumps a shared in-flight counter, records the running maximum,
    /// then blocks on a shared `Barrier(C)`. Only when *all* `C` workers have incremented (so the
    /// observed max is exactly `C`) does the barrier release. This makes `max == C` deterministic
    /// regardless of scheduler timing, without any instrumentation in the production engine.
    struct InFlightProbe {
        in_flight: Arc<AtomicUsize>,
        max_in_flight: Arc<AtomicUsize>,
        gate: Arc<Barrier>,
        gated: bool,
    }

    impl OpInvoker for InFlightProbe {
        async fn invoke(
            &mut self,
            _seq: u64,
        ) -> BenchmarkResult<OpSample> {
            let gate_now = !self.gated;
            self.gated = true;
            let cur = self.in_flight.fetch_add(1, Ordering::Relaxed) + 1;
            self.max_in_flight.fetch_max(cur, Ordering::Relaxed);
            if gate_now {
                // Hold every worker's first request in flight until all C have arrived.
                self.gate.wait().await;
            }
            self.in_flight.fetch_sub(1, Ordering::Relaxed);
            Ok(OpSample {
                server_ms: 1.0,
                total_ms: 1.0,
                rows: 0,
                cached: Some(false),
            })
        }
    }

    #[tokio::test]
    async fn engine_holds_exactly_c_in_flight() {
        const C: usize = 6;
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(Barrier::new(C));
        let workers: Vec<InFlightProbe> = (0..C)
            .map(|_| InFlightProbe {
                in_flight: Arc::clone(&in_flight),
                max_in_flight: Arc::clone(&max_in_flight),
                gate: Arc::clone(&gate),
                gated: false,
            })
            .collect();

        let run = run_closed_loop(workers, 0, 4).await.unwrap();
        assert_eq!(run.completed, C * 4);
        assert_eq!(
            max_in_flight.load(Ordering::Relaxed),
            C,
            "closed loop must hold exactly C requests in flight"
        );
        assert_eq!(
            in_flight.load(Ordering::Relaxed),
            0,
            "every request must complete"
        );
    }

    /// A fake fixed-latency worker: each invocation sleeps exactly `latency` then returns a sample
    /// reporting that latency. Under paused `tokio` time the sleeps advance a virtual clock, so the
    /// throughput math is exact and instant.
    struct FixedLatency {
        latency: Duration,
    }

    impl OpInvoker for FixedLatency {
        async fn invoke(
            &mut self,
            _seq: u64,
        ) -> BenchmarkResult<OpSample> {
            let latency = self.latency;
            tokio::time::sleep(latency).await;
            Ok(OpSample {
                server_ms: latency.as_secs_f64() * 1_000.0,
                total_ms: latency.as_secs_f64() * 1_000.0,
                rows: 0,
                cached: Some(false),
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn throughput_is_c_over_latency() {
        const C: usize = 4;
        let latency = Duration::from_millis(10);
        let workers: Vec<FixedLatency> = (0..C).map(|_| FixedLatency { latency }).collect();

        let run = run_closed_loop(workers, 0, 5).await.unwrap();
        assert_eq!(run.completed, C * 5);
        // window ≈ 5 × 10ms = 50ms; throughput ≈ 20 / 0.05 = 400 = C / latency.
        let expected = C as f64 / latency.as_secs_f64();
        let throughput = run.throughput_ops_per_sec();
        assert!(
            (throughput - expected).abs() / expected < 0.05,
            "throughput {throughput} should be ~C/latency {expected}"
        );
    }

    /// A fake whose reported `server_ms` equals its invocation `seq`, so leaked warm-up samples are
    /// detectable (they'd carry `seq < warmup`).
    struct SeqEcho;

    impl OpInvoker for SeqEcho {
        async fn invoke(
            &mut self,
            seq: u64,
        ) -> BenchmarkResult<OpSample> {
            Ok(OpSample {
                server_ms: seq as f64,
                total_ms: seq as f64,
                rows: 0,
                cached: Some(false),
            })
        }
    }

    #[tokio::test]
    async fn warmup_samples_are_excluded_and_pooled() {
        const C: usize = 3;
        let warmup = 5;
        let samples = 4;
        let workers: Vec<SeqEcho> = (0..C).map(|_| SeqEcho).collect();

        let run = run_closed_loop(workers, warmup, samples).await.unwrap();
        // Pooled across all workers, warm-up excluded.
        assert_eq!(run.samples.len(), C * samples);
        assert_eq!(run.completed, C * samples);
        // Every retained sample is a measured one (seq >= warmup); no warm-up leaked in.
        assert!(
            run.samples.iter().all(|s| s.server_ms >= warmup as f64),
            "warm-up invocations must not appear in the measured samples"
        );
    }

    /// A fake that errors on a chosen invocation, to prove fail-fast doesn't deadlock on the
    /// warm-up barrier when a worker returns early.
    struct FailAt {
        fail_seq: u64,
    }

    impl OpInvoker for FailAt {
        async fn invoke(
            &mut self,
            seq: u64,
        ) -> BenchmarkResult<OpSample> {
            if seq == self.fail_seq {
                return Err(OtherError("boom".to_string()));
            }
            Ok(OpSample {
                server_ms: 1.0,
                total_ms: 1.0,
                rows: 0,
                cached: Some(false),
            })
        }
    }

    #[tokio::test]
    async fn error_during_warmup_fails_fast_without_deadlock() {
        // Worker 0 errors in warm-up before reaching the barrier; the others are parked on it.
        // Fail-fast must abort them and return the error rather than hang forever.
        let mut workers: Vec<FailAt> = (0..4).map(|_| FailAt { fail_seq: u64::MAX }).collect();
        workers[0].fail_seq = 0; // fail on the first warm-up invocation
        let result = run_closed_loop(workers, 2, 3).await;
        assert!(result.is_err(), "a worker error must fail the level");
    }

    #[tokio::test]
    async fn empty_workers_is_an_error() {
        let workers: Vec<SeqEcho> = Vec::new();
        assert!(run_closed_loop(workers, 0, 4).await.is_err());
    }

    #[tokio::test]
    async fn zero_samples_is_an_error() {
        let workers: Vec<SeqEcho> = (0..2).map(|_| SeqEcho).collect();
        assert!(run_closed_loop(workers, 1, 0).await.is_err());
    }

    #[test]
    fn throughput_handles_degenerate_window() {
        // A normal window divides completed by seconds…
        let run = LevelRun {
            samples: Vec::new(),
            completed: 100,
            window: Duration::from_secs(2),
        };
        assert_eq!(run.throughput_ops_per_sec(), 50.0);
        // …and a zero-length window yields 0.0 rather than an infinity/NaN.
        let degenerate = LevelRun {
            samples: Vec::new(),
            completed: 100,
            window: Duration::ZERO,
        };
        assert_eq!(degenerate.throughput_ops_per_sec(), 0.0);
    }

    /// A fake whose invocation panics, to exercise the engine's task-panic (`JoinError`) path.
    struct PanicWorker;

    impl OpInvoker for PanicWorker {
        async fn invoke(
            &mut self,
            _seq: u64,
        ) -> BenchmarkResult<OpSample> {
            panic!("worker panic");
        }
    }

    #[tokio::test]
    async fn worker_panic_is_surfaced_as_an_error() {
        let workers: Vec<PanicWorker> = (0..2).map(|_| PanicWorker).collect();
        let result = run_closed_loop(workers, 0, 2).await;
        assert!(result.is_err(), "a panicking worker must fail the level");
    }
}
