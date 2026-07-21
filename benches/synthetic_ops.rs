//! Criterion C=1 single-flight latency baselines for the synthetic **read** operations, for
//! tracking latency regressions across FalkorDB versions (see `just synthetic-baseline` /
//! `synthetic-compare`, which gate this on a `corpus_hash` guard first).
//!
//! Requires a live FalkorDB holding a **generated** dataset — the recipes run `benchmark synthetic
//! run --generate` first. The endpoint, graph, dataset dimensions (`seed`/`nodes`/`edges`) and
//! `operations` all come from `synthetic-bench.toml` (defaulting to `falkor://127.0.0.1:6379` /
//! `falkor`), resolved **exactly like `benchmark synthetic run`** so both halves of a
//! baseline/compare recipe always target the same server and graph. The corpus is reproduced
//! **byte-for-byte** from the same seed the `corpus_hash` fingerprints (`DatasetSpec::handle()` is
//! what a generated run uses too). Each op is measured single-flight (one query in flight) — the
//! honest C=1 latency Criterion's outlier handling + HTML plots are built for.
//!
//! Write ops are out of scope here: their per-invocation setup/reset lifecycle (Part 5) doesn't fit
//! Criterion's iteration model.

use benchmark::queries_repository::QueryType;
use benchmark::synthetic::catalog::spec;
use benchmark::synthetic::config::FileConfig;
use benchmark::synthetic::dataset::DatasetSpec;
use benchmark::synthetic::op_runner::run_and_drain;
use benchmark::synthetic::{open_graph, OpName};
use criterion::{criterion_group, criterion_main, Criterion};
use rand::rngs::StdRng;
use rand::SeedableRng;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;

const SERVER_TIMEOUT_MS: i64 = 5_000;
const CLIENT_DEADLINE: Duration = Duration::from_secs(5);

/// The FalkorDB endpoint — resolved like `benchmark synthetic run`: the config's `endpoint`, else
/// the local default. (Deliberately does NOT read `FALKORDB_HOST`/`PORT`, so the bench and the
/// `synthetic run --generate` step of the same recipe always target the same server.)
fn resolve_endpoint(cfg: &FileConfig) -> String {
    cfg.endpoint
        .clone()
        .unwrap_or_else(|| "falkor://127.0.0.1:6379".to_string())
}

/// The graph key to measure against — the config's `graph`, else the default (matching
/// `benchmark synthetic run`, so it's the same graph the recipe generated into).
fn resolve_graph(cfg: &FileConfig) -> String {
    cfg.graph
        .clone()
        .unwrap_or_else(|| benchmark::synthetic::DEFAULT_GRAPH.to_string())
}

/// The read ops to baseline: the config's `operations` (or every read op), keeping reads only.
fn read_ops(cfg: &FileConfig) -> Vec<OpName> {
    let ops = cfg.operations.clone().unwrap_or_else(OpName::all_reads);
    ops.into_iter()
        .filter(|op| op.kind() == QueryType::Read)
        .collect()
}

fn bench_synthetic_ops(c: &mut Criterion) {
    let cfg = FileConfig::load(None)
        .expect("read synthetic-bench.toml")
        .unwrap_or_default();
    let endpoint = resolve_endpoint(&cfg);
    let graph = resolve_graph(&cfg);
    let seed = cfg.seed.unwrap_or(0);
    let (nodes, edges) = match (cfg.nodes, cfg.edges) {
        (Some(n), Some(e)) => (n, e),
        _ => panic!(
            "synthetic baselines need a generated dataset — set `nodes` and `edges` in \
             synthetic-bench.toml and generate the graph with `benchmark synthetic run --generate` \
             (the baseline/compare recipes do this for you)"
        ),
    };
    let dataset = DatasetSpec { seed, nodes, edges };
    dataset.validate().expect("valid dataset spec");
    // The same deterministic handle a generated run builds, so the corpus matches the corpus_hash.
    let handle = dataset.handle();

    let ops = read_ops(&cfg);
    assert!(
        !ops.is_empty(),
        "no read operations selected for the baseline"
    );

    let rt = Runtime::new().expect("tokio runtime");
    let mut group = c.benchmark_group("synthetic");

    for op in ops {
        let s = spec(op);
        let mut rng = StdRng::seed_from_u64(seed ^ op.salt());
        let corpus = s
            .build_corpus(&mut rng, &handle, 0, 1)
            .unwrap_or_else(|e| panic!("corpus for {}: {e}", op.as_str()));
        // Pre-render the cached-mode cypher for each corpus entry so query building isn't timed.
        let cyphers: Arc<Vec<String>> = Arc::new(corpus.iter().map(|q| q.to_cypher()).collect());
        let endpoint = endpoint.clone();
        let graph = graph.clone();

        group.bench_function(format!("{}/total_ms", op.as_str()), |b| {
            b.to_async(&rt).iter_custom(|iters| {
                let cyphers = Arc::clone(&cyphers);
                let endpoint = endpoint.clone();
                let graph = graph.clone();
                async move {
                    // One dedicated connection per measurement batch (its open cost is amortized
                    // over `iters` and lands outside the timer below), driven single-flight — the
                    // honest C=1 latency.
                    let mut conn = open_graph(&endpoint, &graph)
                        .await
                        .expect("open FalkorDB connection");
                    // Prime the plan cache so the first measured iteration isn't a compile.
                    run_and_drain(
                        &mut conn,
                        QueryType::Read,
                        &cyphers[0],
                        SERVER_TIMEOUT_MS,
                        CLIENT_DEADLINE,
                    )
                    .await
                    .expect("prime query");

                    let start = Instant::now();
                    for i in 0..iters {
                        let cypher = &cyphers[(i as usize) % cyphers.len()];
                        // Propagate (panic) on any error so a timeout/failure can't masquerade as a
                        // dramatic speed-up in the measurement.
                        run_and_drain(
                            &mut conn,
                            QueryType::Read,
                            cypher,
                            SERVER_TIMEOUT_MS,
                            CLIENT_DEADLINE,
                        )
                        .await
                        .expect("benchmarked query must succeed");
                    }
                    start.elapsed()
                }
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_synthetic_ops);
criterion_main!(benches);
