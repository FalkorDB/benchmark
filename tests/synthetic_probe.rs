//! Integration tests for the synthetic per-operation probe.
//!
//! These require a reachable FalkorDB (set `FALKORDB_HOST`/`FALKORDB_PORT` or default to
//! `127.0.0.1:6379`). They are `#[ignore]`d so a plain `cargo test` stays hermetic; run them with
//! a live server via `just synthetic-it`, and the coverage job runs them with `--include-ignored`
//! against a FalkorDB service. Each test uses its own graph key so the ignored tests can run
//! concurrently without clobbering each other.

use benchmark::queries_repository::QueryType;
use benchmark::synthetic::dataset::DatasetSpec;
use benchmark::synthetic::op_runner::run_and_drain;
use benchmark::synthetic::report::{LevelReport, OperationReport};
use benchmark::synthetic::{
    list_ops, open_graph, run, run_and_report, CacheSelection, Config, OpName,
};
use std::time::Duration;

fn endpoint() -> String {
    let host = std::env::var("FALKORDB_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("FALKORDB_PORT").unwrap_or_else(|_| "6379".to_string());
    format!("falkor://{host}:{port}")
}

fn base_config(graph: &str) -> Config {
    Config {
        endpoint: endpoint(),
        graph: graph.to_string(),
        ops: vec![OpName::ReturnConst],
        samples: 300,
        warmup: 50,
        concurrency: vec![1],
        reset_every: 1000,
        seed: 1,
        server_timeout_ms: 5_000,
        client_deadline_ms: 6_000,
        cache: CacheSelection::Both,
        out: "synthetic-report.json".to_string(),
        server_image: None,
        label: None,
        dataset: None,
    }
}

/// Assert an operation was measured at exactly one concurrency level (the single-level default the
/// non-sweep integration tests use) and return that [`LevelReport`].
fn only_level(op: &OperationReport) -> &LevelReport {
    assert_eq!(
        op.levels.len(),
        1,
        "expected exactly one concurrency level, got {}",
        op.levels.len()
    );
    &op.levels[0]
}

/// Drop `graph` if it exists (ignore "missing key" errors).
async fn drop_graph(graph: &str) {
    if let Ok(mut g) = open_graph(&endpoint(), graph).await {
        let _ = g.delete().await;
    }
}

/// Seed a tiny `:User {id, age}` graph wired with `:Friend` edges (a `+1` ring plus longer skip
/// edges) so the read primitives (index lookup, expansion, aggregation, shortest path) have data to
/// touch.
async fn seed_user_graph(
    graph: &str,
    users: i64,
) {
    drop_graph(graph).await;
    let mut g = open_graph(&endpoint(), graph)
        .await
        .expect("open seed graph");
    // Any query instantiates the (freshly dropped) graph key; index first so lookups use it.
    g.query("CREATE INDEX FOR (u:User) ON (u.id)")
        .execute()
        .await
        .expect("create id index");
    g.query(&format!(
        "UNWIND range(1, {users}) AS i CREATE (:User {{id: i, age: 18 + i % 50}})"
    ))
    .execute()
    .await
    .expect("create users");
    if users > 1 {
        // Ring edges i -> (i mod N) + 1 (a +1 step), plus skip edges i -> ((i + 6) mod N) + 1
        // (a +7 step for these 1-based ids) to give expansions and shortest paths more structure.
        g.query(&format!(
            "MATCH (u:User) WITH u MATCH (v:User {{id: (u.id % {users}) + 1}}) CREATE (u)-[:Friend]->(v)"
        ))
        .execute()
        .await
        .expect("ring edges");
        g.query(&format!(
            "MATCH (u:User) WITH u MATCH (v:User {{id: ((u.id + 6) % {users}) + 1}}) CREATE (u)-[:Friend]->(v)"
        ))
        .execute()
        .await
        .expect("skip edges");
    }
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn probe_produces_valid_report() {
    // `return_const` needs no dataset, so a fresh empty graph is fine.
    let config = base_config("syn_it_return_const");
    let samples = config.samples;
    drop_graph(&config.graph).await;

    let report = run(&config).await.expect("probe run should succeed");

    let op = report
        .operations
        .get("return_const")
        .expect("report should contain the measured op");
    let lvl = only_level(op);
    let cached_lm = lvl.cached.as_ref().expect("cached metrics present");
    let uncached_lm = lvl.uncached.as_ref().expect("uncached metrics present");
    let cached = &cached_lm.metrics;
    let uncached = &uncached_lm.metrics;

    // Every sample is accounted for (retained + severe-outliers removed) in each mode.
    assert_eq!(cached.server_ms.n + cached.server_ms.removed, samples);
    assert_eq!(uncached.server_ms.n + uncached.server_ms.removed, samples);

    // The single-connection level still records an achieved throughput.
    assert!(
        cached_lm.throughput_ops_per_sec > 0.0,
        "throughput should be positive, got {}",
        cached_lm.throughput_ops_per_sec
    );

    // Positive server + total time, and total >= server within each mode.
    assert!(cached.server_ms.median > 0.0);
    assert!(cached.total_ms.median >= cached.server_ms.median);
    assert!(uncached.total_ms.median >= uncached.server_ms.median);

    // The uncached mode forces plan-cache misses: most invocations report cached_execution=false.
    assert!(
        uncached.cached_false_rate > 0.5,
        "uncached mode should mostly miss the plan cache (got {})",
        uncached.cached_false_rate
    );
    assert!(lvl.compilation_ms_median.is_some());

    // Provenance + run metadata were captured.
    assert!(report.meta.server.redis_version.is_some());
    assert!(report.meta.server.cache_size.is_some());
    assert_eq!(report.meta.graph, "syn_it_return_const");
    assert_eq!(
        report.meta.corpus_size,
        benchmark::synthetic::catalog::CORPUS_SIZE
    );
    drop_graph(&config.graph).await;
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn read_catalog_runs_against_seeded_graph() {
    let graph = "syn_it_reads";
    seed_user_graph(graph, 200).await;

    let report = run(&Config {
        graph: graph.to_string(),
        ops: OpName::all_reads(),
        samples: 60,
        warmup: 10,
        cache: CacheSelection::Both,
        ..base_config(graph)
    })
    .await
    .expect("read catalog run should succeed");

    // Every read op was measured and produced samples with a sane (finite, non-negative) server
    // time and total >= server. Smoke-testing the whole catalog catches invalid Cypher/plans.
    for op in OpName::all_reads() {
        let r = report
            .operations
            .get(op.as_str())
            .unwrap_or_else(|| panic!("report missing op {}", op.as_str()));
        let lvl = only_level(r);
        let cached = &lvl
            .cached
            .as_ref()
            .unwrap_or_else(|| panic!("op {} missing cached metrics", op.as_str()))
            .metrics;
        assert!(cached.server_ms.n > 0, "op {} has no samples", op.as_str());
        assert!(
            cached.server_ms.median >= 0.0 && cached.server_ms.median.is_finite(),
            "op {} server median not sane: {}",
            op.as_str(),
            cached.server_ms.median
        );
        assert!(
            cached.total_ms.median >= cached.server_ms.median,
            "op {} total < server",
            op.as_str()
        );
        assert!(
            lvl.compilation_ms_median.is_some(),
            "op {} lacks compilation",
            op.as_str()
        );
    }
    drop_graph(graph).await;
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn same_seed_yields_identical_report_shape() {
    // Determinism end-to-end: two runs with the same seed measure identical corpora, so the report
    // structure (ops + cache modes) matches. (Latencies differ; we assert on the corpus metadata.)
    let graph = "syn_it_seeded";
    seed_user_graph(graph, 120).await;
    let cfg = Config {
        graph: graph.to_string(),
        ops: vec![OpName::MatchByIndex, OpName::Expand1Hop],
        samples: 40,
        warmup: 10,
        seed: 4242,
        cache: CacheSelection::Cached,
        ..base_config(graph)
    };
    let a = run(&cfg).await.expect("run a");
    let b = run(&cfg).await.expect("run b");
    assert_eq!(a.meta.seed, b.meta.seed);
    assert_eq!(
        a.operations.keys().collect::<Vec<_>>(),
        b.operations.keys().collect::<Vec<_>>()
    );
    drop_graph(graph).await;
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn run_and_report_writes_json_file() {
    let dir = std::env::temp_dir();
    let out = dir
        .join(format!("synthetic-report-it-{}.json", std::process::id()))
        .to_string_lossy()
        .into_owned();
    let graph = "syn_it_json";
    drop_graph(graph).await;
    let config = Config {
        graph: graph.to_string(),
        samples: 120,
        warmup: 20,
        out: out.clone(),
        ..base_config(graph)
    };

    run_and_report(&config)
        .await
        .expect("run_and_report should succeed");

    let written = std::fs::read_to_string(&out).expect("report file should exist");
    assert!(written.contains("return_const"));
    assert!(written.contains("\"levels\""));
    assert!(written.contains("\"throughput_ops_per_sec\""));
    assert!(written.contains("\"cached\""));
    assert!(written.contains("\"uncached\""));
    assert!(written.contains("\"corpus_size\""));
    let _ = std::fs::remove_file(&out);
    drop_graph(graph).await;
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn cached_only_and_uncached_only_modes() {
    let graph = "syn_it_modes";
    drop_graph(graph).await;
    // Cached-only: no uncached block.
    let cached_report = run(&Config {
        graph: graph.to_string(),
        samples: 100,
        warmup: 20,
        cache: CacheSelection::Cached,
        ..base_config(graph)
    })
    .await
    .expect("cached-only run should succeed");
    let cop = cached_report.operations.get("return_const").unwrap();
    let clvl = only_level(cop);
    assert!(clvl.cached.is_some());
    assert!(clvl.uncached.is_none());
    assert!(clvl.compilation_ms_median.is_none());

    // Uncached-only: no cached block, and it misses the plan cache.
    let uncached_report = run(&Config {
        graph: graph.to_string(),
        samples: 100,
        warmup: 20,
        cache: CacheSelection::Uncached,
        ..base_config(graph)
    })
    .await
    .expect("uncached-only run should succeed");
    let uop = uncached_report.operations.get("return_const").unwrap();
    let ulvl = only_level(uop);
    assert!(ulvl.cached.is_none());
    let uncached = &ulvl.uncached.as_ref().unwrap().metrics;
    assert!(uncached.cached_false_rate > 0.5);
    drop_graph(graph).await;
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn missing_seed_data_errors_clearly() {
    // A graph with no :User nodes: an op that needs seed ids fails with a clear message.
    let graph = "syn_it_empty";
    seed_user_graph(graph, 0).await; // creates the index but no users
    let err = run(&Config {
        graph: graph.to_string(),
        ops: vec![OpName::MatchByIndex],
        samples: 20,
        warmup: 2,
        ..base_config(graph)
    })
    .await
    .expect_err("match_by_index should fail without seed ids");
    assert!(
        format!("{err:?}").contains("seed"),
        "error should mention missing seed data: {err:?}"
    );
    drop_graph(graph).await;
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn warmup_zero_still_primes_cached_plan() {
    // With warmup=0 the cached mode primes the plan once before measuring, so it never pays
    // first-touch compilation and still reports all-cache-hit executions.
    let graph = "syn_it_warm0";
    drop_graph(graph).await;
    let report = run(&Config {
        graph: graph.to_string(),
        samples: 40,
        warmup: 0,
        cache: CacheSelection::Cached,
        ..base_config(graph)
    })
    .await
    .expect("warmup=0 cached run should succeed");
    let op = report.operations.get("return_const").unwrap();
    let cached = &only_level(op).cached.as_ref().unwrap().metrics;
    assert_eq!(cached.server_ms.n + cached.server_ms.removed, 40);
    // The pre-measurement prime means every measured sample is a cache hit.
    assert_eq!(
        cached.cached_false_rate, 0.0,
        "cached-mode run with a prime should report all cache hits"
    );
    drop_graph(graph).await;
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn bad_endpoint_errors_out() {
    // Nothing is listening on this port → the run should error rather than hang or panic.
    let config = Config {
        endpoint: "falkor://127.0.0.1:6390".to_string(),
        samples: 10,
        warmup: 2,
        client_deadline_ms: 1_000,
        ..base_config("syn_it_bad")
    };
    assert!(run(&config).await.is_err());
}

#[test]
fn list_ops_is_non_empty() {
    // Pure helper — no server needed; keeps the smoke path covered even without `--ignored`.
    let listing = list_ops();
    assert!(listing.contains("return_const"));
    assert!(listing.contains("match_by_index"));
    assert!(listing.contains("shortest_path"));
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn op_runner_reads_writes_and_reports_errors() {
    let mut graph = open_graph(&endpoint(), "synthetic_op_runner_it")
        .await
        .expect("open graph");

    // A write goes through the `GRAPH.QUERY` path, instantiates the graph, and drains its row.
    // Return a scalar (not the node itself) so row decoding doesn't trigger schema round-trips.
    let write = run_and_drain(
        &mut graph,
        QueryType::Write,
        "CREATE (n:T {v: 1}) RETURN n.v",
        5_000,
        Duration::from_secs(5),
    )
    .await
    .expect("write op should succeed");
    assert_eq!(write.rows, 1);

    // A read that returns a row: drains it and reports a finite, non-negative server time.
    let read = run_and_drain(
        &mut graph,
        QueryType::Read,
        "RETURN 1 AS x",
        5_000,
        Duration::from_secs(5),
    )
    .await
    .expect("read op should succeed");
    assert_eq!(read.rows, 1);
    assert!(read.server_ms.is_finite() && read.server_ms >= 0.0);
    assert!(read.total_ms >= read.server_ms);

    // A syntactically invalid query surfaces as an error rather than a panic.
    assert!(run_and_drain(
        &mut graph,
        QueryType::Read,
        "THIS IS NOT CYPHER",
        5_000,
        Duration::from_secs(5)
    )
    .await
    .is_err());

    // A tiny client deadline against a query that does real server-side work reliably trips the
    // whole-operation timeout guard. (A trivial query like `RETURN 1` can finish within tokio's
    // ~1ms timer resolution on a fast localhost server, so use a query that takes many ms.)
    assert!(run_and_drain(
        &mut graph,
        QueryType::Read,
        "UNWIND range(1, 5000000) AS x RETURN count(x)",
        5_000,
        Duration::from_millis(1)
    )
    .await
    .is_err());

    // Tidy up the scratch graph.
    let _ = graph.delete().await;
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn probe_instantiates_a_missing_graph() {
    // Use a dedicated graph and delete it first so `run()` exercises the empty-key instantiation
    // path deterministically without racing other tests on a shared key.
    let graph = "syn_it_instantiate";
    drop_graph(graph).await;
    let report = run(&Config {
        graph: graph.to_string(),
        samples: 60,
        warmup: 10,
        cache: CacheSelection::Cached,
        ..base_config(graph)
    })
    .await
    .expect("probe should instantiate the missing graph and succeed");
    assert!(report.operations.contains_key("return_const"));
    drop_graph(graph).await;
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn generated_dataset_has_exact_counts_index_and_hash() {
    // Generate a small reproducible dataset, then assert node/edge counts, that the :User(id) index
    // exists and is used, and that the report carries a dataset block with a corpus_hash.
    let graph = "syn_it_gen";
    let report = run(&Config {
        graph: graph.to_string(),
        ops: vec![OpName::MatchByIndex, OpName::ShortestPath],
        samples: 40,
        warmup: 10,
        seed: 123,
        cache: CacheSelection::Cached,
        dataset: Some(DatasetSpec {
            seed: 123,
            nodes: 400,
            edges: 2000,
        }),
        ..base_config(graph)
    })
    .await
    .expect("generation run should succeed");

    // The report records the generated dataset + a corpus_hash.
    let ds = report.meta.dataset.as_ref().expect("dataset info present");
    assert_eq!((ds.seed, ds.nodes, ds.edges), (123, 400, 2000));
    assert!(ds.workload_hash.starts_with("sha256:"));

    // Exact counts in the graph.
    let mut g = open_graph(&endpoint(), graph).await.expect("open graph");
    let node_count = scalar_i64(&mut g, "MATCH (n:User) RETURN count(n)").await;
    let edge_count = scalar_i64(&mut g, "MATCH (:User)-[e:Friend]->(:User) RETURN count(e)").await;
    assert_eq!(node_count, 400);
    assert_eq!(edge_count, 2000);

    // The :User(id) index exists and is OPERATIONAL...
    let operational = scalar_i64(
        &mut g,
        "CALL db.indexes() YIELD label, status WHERE label = 'User' AND status = 'OPERATIONAL' RETURN count(*)",
    )
    .await;
    assert!(operational >= 1, "expected an OPERATIONAL :User index");

    // ...and the point-lookup op uses it (Node By Index Scan in the plan).
    let plan = explain(&mut g, "MATCH (n:User {id: 7}) RETURN n.id").await;
    assert!(
        plan.iter().any(|line| line.contains("Index Scan")),
        "match_by_index should use the index, got plan:\n{}",
        plan.join("\n")
    );

    // shortest_path produced measured samples (the connected-pair pool guarantees a bounded path).
    let op = report.operations.get("shortest_path").unwrap();
    assert!(only_level(op).cached.as_ref().unwrap().metrics.server_ms.n > 0);
    drop_graph(graph).await;
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn generation_is_reproducible_across_runs() {
    // Same seed + knobs ⇒ identical corpus_hash, even though the graph is regenerated from scratch.
    let graph = "syn_it_gen_repro";
    let cfg = Config {
        graph: graph.to_string(),
        ops: vec![OpName::MatchByIndex, OpName::AggregateCount],
        samples: 30,
        warmup: 5,
        seed: 77,
        cache: CacheSelection::Cached,
        dataset: Some(DatasetSpec {
            seed: 77,
            nodes: 300,
            edges: 1500,
        }),
        ..base_config(graph)
    };
    let a = run(&cfg).await.expect("run a");
    let b = run(&cfg).await.expect("run b");
    assert_eq!(
        a.meta.dataset.unwrap().workload_hash,
        b.meta.dataset.unwrap().workload_hash
    );
    drop_graph(graph).await;
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn concurrency_sweep_produces_per_level_throughput_and_percentiles() {
    // Sweep one op over [1, 4, 8]; every level must report achieved throughput and a full set of
    // percentiles, and throughput must rise with concurrency somewhere (monotonic-ish up) — the
    // whole point of the latency-vs-throughput curve.
    let graph = "syn_it_sweep";
    seed_user_graph(graph, 200).await;

    let report = run(&Config {
        graph: graph.to_string(),
        ops: vec![OpName::MatchByIndex],
        samples: 120,
        warmup: 20,
        concurrency: vec![1, 4, 8],
        cache: CacheSelection::Cached,
        ..base_config(graph)
    })
    .await
    .expect("concurrency sweep should succeed");

    let op = report.operations.get("match_by_index").expect("op present");
    assert_eq!(
        op.levels.iter().map(|l| l.concurrency).collect::<Vec<_>>(),
        vec![1, 4, 8],
        "levels are the swept concurrencies, sorted ascending"
    );
    assert_eq!(report.meta.concurrency, vec![1, 4, 8]);

    let mut throughputs = Vec::new();
    for lvl in &op.levels {
        let m = lvl
            .cached
            .as_ref()
            .unwrap_or_else(|| panic!("level C={} missing cached metrics", lvl.concurrency));
        assert!(
            m.throughput_ops_per_sec > 0.0,
            "level C={} has non-positive throughput {}",
            lvl.concurrency,
            m.throughput_ops_per_sec
        );
        // Every level carries a full percentile set, correctly ordered.
        let s = &m.metrics.server_ms;
        assert!(s.n > 0, "level C={} has no samples", lvl.concurrency);
        assert!(
            s.median <= s.p90 && s.p90 <= s.p95 && s.p95 <= s.p99 && s.p99.is_finite(),
            "level C={} percentiles must be ordered p50<=p90<=p95<=p99 (got {:?})",
            lvl.concurrency,
            (s.median, s.p90, s.p95, s.p99)
        );
        throughputs.push(m.throughput_ops_per_sec);
    }

    // Closed-loop achieved throughput should climb with concurrency at least somewhere before it
    // saturates (a loose, non-flaky check for "monotonic-ish up").
    assert!(
        throughputs[1..].iter().any(|&t| t > throughputs[0]),
        "throughput should rise above the C=1 baseline as concurrency grows: {throughputs:?}"
    );
    drop_graph(graph).await;
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn write_ops_run_isolated_sweep_and_clean_up() {
    // create_node + merge_miss at C=8 over several sawtooth windows. A green run is itself the
    // isolation proof: every sample verifies `nodes_created == 1`, so if two of the 8 workers ever
    // shared a key, a key repeated within a window, or a reset failed to clear its band, a MERGE
    // would hit instead of miss (nodes_created == 0) and the run would error. We also assert the
    // seeded real data is untouched and the run's scratch is fully cleaned up afterward.
    let graph = "syn_it_writes";
    let seeded_users: i64 = 50;
    seed_user_graph(graph, seeded_users).await;

    let report = run(&Config {
        graph: graph.to_string(),
        ops: vec![OpName::CreateNode, OpName::MergeMiss],
        samples: 200, // > reset_every ⇒ multiple resets per worker
        warmup: 20,
        concurrency: vec![8],
        reset_every: 50,
        cache: CacheSelection::Cached,
        ..base_config(graph)
    })
    .await
    .expect("write sweep should succeed (isolation keeps every MERGE a miss)");

    for name in ["create_node", "merge_miss"] {
        let op = report
            .operations
            .get(name)
            .unwrap_or_else(|| panic!("{name} missing from report"));
        let lvl = only_level(op);
        assert_eq!(lvl.concurrency, 8);
        let m = lvl
            .cached
            .as_ref()
            .unwrap_or_else(|| panic!("{name} missing cached metrics"));
        assert!(
            m.throughput_ops_per_sec > 0.0,
            "{name} must report positive throughput"
        );
        let s = &m.metrics.server_ms;
        assert!(s.n > 0, "{name} must have samples");
        assert!(
            s.median <= s.p90 && s.p90 <= s.p95 && s.p95 <= s.p99 && s.p99.is_finite(),
            "{name} percentiles must be ordered p50<=p90<=p95<=p99 (got {:?})",
            (s.median, s.p90, s.p95, s.p99)
        );
    }

    // Isolation from real data + cleanup: the seeded :User nodes are untouched, and no scratch node
    // of any label leaks past the run's post-level cleanup (total node count == seeded users).
    let mut g = open_graph(&endpoint(), graph).await.expect("reopen graph");
    assert_eq!(
        scalar_i64(&mut g, "MATCH (u:User) RETURN count(u)").await,
        seeded_users,
        "seeded :User data must be untouched by the write sweep"
    );
    assert_eq!(
        scalar_i64(&mut g, "MATCH (n) RETURN count(n)").await,
        seeded_users,
        "no scratch nodes may remain after the run's post-level cleanup"
    );
    drop_graph(graph).await;
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn write_scratch_reset_reuses_its_band_without_duplicates() {
    // Pin the isolation model against the server directly: within a window every MERGE misses
    // (unique keys), a band-scoped reset clears exactly this worker's rows, and the next window
    // reuses the very same keys — still all misses, with no duplicate accumulation.
    use benchmark::synthetic::writes::{verify_mutation, ExpectedMutation, WriteScratch};

    let graph = "syn_it_write_reset";
    drop_graph(graph).await;
    let mut g = open_graph(&endpoint(), graph).await.expect("open graph");

    let reset_every = 5usize;
    let scratch = WriteScratch::new(0xBEEF, 0, reset_every).expect("scratch");
    let label = scratch.label();
    let count_cypher = format!("MATCH (n:{label}) RETURN count(n)");

    // Window 1: `reset_every` distinct MERGEs, each a miss (creates exactly one node).
    for seq in 0..reset_every as u64 {
        let cypher = format!(
            "MERGE (n:{label} {{id: {}}}) RETURN n.id",
            scratch.window_key(seq)
        );
        let s = run_and_drain(&mut g, QueryType::Write, &cypher, 5_000, Duration::from_secs(5))
            .await
            .expect("window-1 merge");
        verify_mutation(ExpectedMutation::NodeCreated, &s.mutations).expect("window-1 must miss");
    }
    assert_eq!(
        scalar_i64(&mut g, &count_cypher).await,
        reset_every as i64,
        "one node per key after the first window"
    );

    // Reset: delete exactly this worker's key band (scoped by label + id range).
    let (lo, hi) = scratch.key_band();
    run_and_drain(
        &mut g,
        QueryType::Write,
        &format!("MATCH (n:{label}) WHERE n.id >= {lo} AND n.id <= {hi} DELETE n"),
        5_000,
        Duration::from_secs(5),
    )
    .await
    .expect("reset delete");
    assert_eq!(
        scalar_i64(&mut g, &count_cypher).await,
        0,
        "the reset clears the whole band"
    );

    // Window 2: `window_key` cycles back over the same keys, and every MERGE misses again.
    for seq in reset_every as u64..2 * reset_every as u64 {
        let cypher = format!(
            "MERGE (n:{label} {{id: {}}}) RETURN n.id",
            scratch.window_key(seq)
        );
        let s = run_and_drain(&mut g, QueryType::Write, &cypher, 5_000, Duration::from_secs(5))
            .await
            .expect("window-2 merge");
        verify_mutation(ExpectedMutation::NodeCreated, &s.mutations).expect("window-2 must miss");
    }
    assert_eq!(
        scalar_i64(&mut g, &count_cypher).await,
        reset_every as i64,
        "the reused band holds exactly one node per key — no duplicate accumulation"
    );

    drop_graph(graph).await;
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn write_ops_5c_run_isolated_and_clean_up() {
    // The four Part-5c write ops at C=8 over several reset windows. A green run is the isolation +
    // correctness proof: every sample verifies its exact mutation (edge created / property set /
    // node deleted / merge hit), so a cross-worker collision, a missed reset, a wrong target, or a
    // broken refill would fail verification. Also assert the seeded data is untouched and the run's
    // scratch (nodes *and* edges) is fully cleaned up.
    let graph = "syn_it_writes_5c";
    let seeded_users: i64 = 40;
    seed_user_graph(graph, seeded_users).await;

    let report = run(&Config {
        graph: graph.to_string(),
        ops: vec![
            OpName::CreateEdge,
            OpName::SetProperty,
            OpName::DeleteNode,
            OpName::MergeHit,
        ],
        samples: 150, // > reset_every ⇒ multiple sawtooth resets per worker
        warmup: 20,
        concurrency: vec![8],
        reset_every: 40,
        cache: CacheSelection::Cached,
        ..base_config(graph)
    })
    .await
    .expect("5c write sweep should succeed under isolation");

    for name in ["create_edge", "set_property", "delete_node", "merge_hit"] {
        let op = report
            .operations
            .get(name)
            .unwrap_or_else(|| panic!("{name} missing from report"));
        let lvl = only_level(op);
        assert_eq!(lvl.concurrency, 8);
        let m = lvl
            .cached
            .as_ref()
            .unwrap_or_else(|| panic!("{name} missing cached metrics"));
        assert!(
            m.throughput_ops_per_sec > 0.0,
            "{name} must report positive throughput"
        );
        assert!(m.metrics.server_ms.n > 0, "{name} must have samples");
    }

    // Isolation from real data + full cleanup: the seeded :User nodes are untouched, no scratch node
    // of any label leaks, and no scratch :BenchEdge relationship survives the DETACH DELETE cleanup.
    let mut g = open_graph(&endpoint(), graph).await.expect("reopen graph");
    assert_eq!(
        scalar_i64(&mut g, "MATCH (u:User) RETURN count(u)").await,
        seeded_users,
        "seeded :User data must be untouched"
    );
    assert_eq!(
        scalar_i64(&mut g, "MATCH (n) RETURN count(n)").await,
        seeded_users,
        "no scratch nodes may remain after cleanup"
    );
    assert_eq!(
        scalar_i64(&mut g, "MATCH ()-[r:BenchEdge]->() RETURN count(r)").await,
        0,
        "no scratch edges may remain after cleanup"
    );
    drop_graph(graph).await;
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn create_edge_builds_band_internal_edges_and_reset_drops_them() {
    // Counters alone can't prove edge topology, so pin it directly against the server: fill a
    // worker's band, run one window of create_edge, and assert exactly R band-internal edges exist
    // (each op created one edge and no node), then a band reset drops every edge and node.
    use benchmark::synthetic::writes::WriteScratch;

    let graph = "syn_it_create_edge";
    drop_graph(graph).await;
    let mut g = open_graph(&endpoint(), graph).await.expect("open graph");

    let reset_every = 5usize;
    let scratch = WriteScratch::new(0xED9E, 0, reset_every).expect("scratch");
    let label = scratch.label();
    let (lo, hi) = scratch.key_band();

    // Setup: fill the band with R clean nodes and confirm one distinct node per key.
    run_and_drain(
        &mut g,
        QueryType::Write,
        &format!("UNWIND range({lo}, {hi}) AS i CREATE (:{label} {{id: i}})"),
        5_000,
        Duration::from_secs(5),
    )
    .await
    .expect("fill band");
    assert_eq!(
        scalar_i64(&mut g, &format!("MATCH (n:{label}) RETURN count(n)")).await,
        reset_every as i64,
        "fill creates one node per band key"
    );
    assert_eq!(
        scalar_i64(&mut g, &format!("MATCH (n:{label}) RETURN count(DISTINCT n.id)")).await,
        reset_every as i64,
        "band keys are distinct (no duplicate merge-hit targets)"
    );

    // One window of create_edge: src → (src+1, wrapping the top back to the bottom).
    for seq in 0..reset_every as u64 {
        let src = scratch.window_key(seq);
        let dst = if src == hi { lo } else { src + 1 };
        let s = run_and_drain(
            &mut g,
            QueryType::Write,
            &format!("MATCH (a:{label} {{id: {src}}}), (b:{label} {{id: {dst}}}) CREATE (a)-[:BenchEdge]->(b)"),
            5_000,
            Duration::from_secs(5),
        )
        .await
        .expect("create edge");
        assert_eq!(s.mutations.relationships_created, 1, "one edge per invocation");
        assert_eq!(s.mutations.nodes_created, 0, "endpoints pre-exist");
    }

    // R distinct edges, every endpoint inside this worker's band (no cross-band leakage).
    assert_eq!(
        scalar_i64(
            &mut g,
            &format!("MATCH (:{label})-[r:BenchEdge]->(:{label}) RETURN count(r)")
        )
        .await,
        reset_every as i64,
        "one band-internal edge per window invocation"
    );
    assert_eq!(
        scalar_i64(
            &mut g,
            &format!(
                "MATCH (a:{label})-[:BenchEdge]->(b:{label}) \
                 WHERE a.id < {lo} OR a.id > {hi} OR b.id < {lo} OR b.id > {hi} RETURN count(*)"
            )
        )
        .await,
        0,
        "no edge escapes the worker's band"
    );

    // A band reset (DETACH DELETE) drops the accumulated edges and the nodes together.
    run_and_drain(
        &mut g,
        QueryType::Write,
        &format!("MATCH (n:{label}) WHERE n.id >= {lo} AND n.id <= {hi} DETACH DELETE n"),
        5_000,
        Duration::from_secs(5),
    )
    .await
    .expect("reset detach-delete");
    assert_eq!(
        scalar_i64(&mut g, "MATCH ()-[r:BenchEdge]->() RETURN count(r)").await,
        0,
        "the reset drops every accumulated edge"
    );
    assert_eq!(
        scalar_i64(&mut g, &format!("MATCH (n:{label}) RETURN count(n)")).await,
        0,
        "the reset clears the band nodes"
    );
    drop_graph(graph).await;
}

/// Read a single-row `RETURN count(...)`/scalar i64.
async fn scalar_i64(
    graph: &mut falkordb::AsyncGraph,
    cypher: &str,
) -> i64 {
    use futures::StreamExt;
    let mut result = graph
        .ro_query(cypher)
        .execute()
        .await
        .expect("scalar query");
    match result.data.next().await {
        Some(Ok(row)) => row.try_get_at::<i64>(0).expect("i64 scalar"),
        other => panic!("unexpected scalar response: {other:?}"),
    }
}

/// Return the `GRAPH.EXPLAIN` plan lines for `cypher`.
async fn explain(
    graph: &mut falkordb::AsyncGraph,
    cypher: &str,
) -> Vec<String> {
    let plan = graph.explain(cypher).execute().await.expect("explain");
    plan.plan().to_vec()
}

// ---------------------------------------------------------------------------
// Record / replay (record-once, replay-identically across versions).
// ---------------------------------------------------------------------------

use benchmark::synthetic::baseline::{guard, BaselineKey, GuardOutcome};
use benchmark::synthetic::recording::{self, temp_bundle_dir};
use benchmark::synthetic::replay::{self, ReplayConfig};

fn replay_config(dir: &std::path::Path, graph: &str, out: &str, load: bool) -> ReplayConfig {
    ReplayConfig {
        recording_dir: dir.to_path_buf(),
        endpoint: endpoint(),
        graph: Some(graph.to_string()),
        load,
        samples: 200,
        warmup: 30,
        concurrency: vec![1],
        cache: benchmark::synthetic::CacheSelection::Cached,
        server_timeout_ms: 5_000,
        client_deadline_ms: 6_000,
        out: out.to_string(),
        server_image: None,
        label: None,
    }
}

/// record (offline) → replay --load → replay --no-load produces byte-identical workload identity
/// (workload_hash + per-op result digests), and the guard proceeds — the whole cross-version basis.
#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn record_then_replay_roundtrips_and_guard_proceeds() {
    let graph = "syn_it_replay";
    drop_graph(graph).await;
    let dir = temp_bundle_dir("syn-it-rec");
    let spec = DatasetSpec {
        seed: 9,
        nodes: 500,
        edges: 1500,
    };
    let ops = vec![OpName::MatchByIndex, OpName::Expand1Hop, OpName::AggregateCount];
    recording::record(&spec, graph, &ops, spec.seed, 256, &dir).expect("record");

    // Replay #1 loads the recorded graph; #2 reuses it (no-load), count-verifying first.
    let ref_out = dir.join("ref.json").to_string_lossy().into_owned();
    let cand_out = dir.join("cand.json").to_string_lossy().into_owned();
    let a = replay::run(&replay_config(&dir, graph, &ref_out, true))
        .await
        .expect("replay --load");
    let b = replay::run(&replay_config(&dir, graph, &cand_out, false))
        .await
        .expect("replay --no-load");

    // Same workload identity: the workload_hash (stamped as corpus_hash) matches.
    let ha = a.meta.dataset.as_ref().expect("dataset a").workload_hash.clone();
    let hb = b.meta.dataset.as_ref().expect("dataset b").workload_hash.clone();
    assert_eq!(ha, hb, "workload_hash must match across replays");

    // Every op has a result digest, and they match across the two replays.
    for op in ["match_by_index", "expand_1_hop", "aggregate_count"] {
        let da = a.operations[op].result_digest.as_ref().expect("digest a");
        let db = b.operations[op].result_digest.as_ref().expect("digest b");
        assert_eq!(da, db, "result digest for {op} must match");
        // A single C=1 cached level was measured.
        let lvl = only_level(&a.operations[op]);
        assert_eq!(lvl.concurrency, 1);
        assert!(lvl.cached.is_some());
    }

    // The guard proceeds (same workload + matching result digests).
    match guard(&BaselineKey::from_report(&a), &BaselineKey::from_report(&b)) {
        GuardOutcome::Proceed { .. } => {}
        GuardOutcome::Abort { reason } => panic!("guard aborted unexpectedly: {reason}"),
    }

    std::fs::remove_dir_all(&dir).ok();
    drop_graph(graph).await;
}

/// replay --no-load against a graph that doesn't hold the recorded dataset fails closed (the
/// count-verify rejects it) rather than silently measuring the wrong graph.
#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn replay_no_load_fails_closed_on_wrong_graph() {
    let graph = "syn_it_replay_missing";
    drop_graph(graph).await; // ensure it's empty / absent
    let dir = temp_bundle_dir("syn-it-rec-missing");
    let spec = DatasetSpec {
        seed: 3,
        nodes: 300,
        edges: 900,
    };
    recording::record(&spec, graph, &[OpName::MatchByIndex], spec.seed, 256, &dir).expect("record");

    let out = dir.join("r.json").to_string_lossy().into_owned();
    let err = replay::run(&replay_config(&dir, graph, &out, false))
        .await
        .expect_err("replay --no-load on an unloaded graph must fail");
    assert!(
        format!("{err}").contains("load the recording first"),
        "expected a count-verify failure, got: {err}"
    );

    std::fs::remove_dir_all(&dir).ok();
    drop_graph(graph).await;
}

/// Drive the CLI arms end-to-end: `run_command(Record)` (offline) then `run_command(Replay)`
/// (load + measure + write report), covering config resolution + report writing.
#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn record_and_replay_via_run_command() {
    use benchmark::cli::SyntheticCommands;
    use benchmark::synthetic::run_command;

    let graph = "syn_it_cli_replay";
    drop_graph(graph).await;
    let dir = temp_bundle_dir("syn-it-cli");
    let out_dir = dir.to_string_lossy().into_owned();

    run_command(SyntheticCommands::Record {
        config: None,
        graph: Some(graph.to_string()),
        ops: vec![
            benchmark::cli::OpSelector::One(OpName::MatchByIndex),
            benchmark::cli::OpSelector::One(OpName::AggregateCount),
        ],
        all_reads: false,
        tier: None,
        seed: Some(11),
        nodes: Some(400),
        edges: Some(1200),
        out_dir: out_dir.clone(),
    })
    .await
    .expect("record via run_command");
    assert!(dir.join("manifest.json").exists());

    let report_out = dir.join("cli.json").to_string_lossy().into_owned();
    run_command(SyntheticCommands::Run {
        config: None,
        endpoint: Some(endpoint()),
        graph: None,
        ops: vec![],
        all_reads: false,
        tier: None,
        samples: Some(150),
        warmup: Some(20),
        concurrency: vec![1, 4],
        reset_every: None,
        seed: None,
        cache: Some(benchmark::synthetic::CacheSelection::Both),
        server_timeout_ms: None,
        client_deadline_ms: None,
        out: Some(report_out.clone()),
        server_image: None,
        label: None,
        generate: false,
        nodes: None,
        edges: None,
        recording: Some(out_dir),
        no_load: false,
    })
    .await
    .expect("run --recording via run_command");

    let written = std::fs::read_to_string(&report_out).expect("report exists");
    assert!(written.contains("match_by_index"));
    assert!(written.contains("result_digest"));
    // The Markdown sibling is written too.
    assert!(std::path::Path::new(&report_out.replace(".json", ".md")).exists());

    std::fs::remove_dir_all(&dir).ok();
    drop_graph(graph).await;
}

/// A recorded workload replayed at concurrency > 1 (both cache modes) must return identical results
/// (the untimed concurrent verify passes) and produce a per-level, per-mode report.
#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn replay_concurrency_sweep_verifies_results_and_reports_levels() {
    let graph = "syn_it_replay_conc";
    drop_graph(graph).await;
    let dir = temp_bundle_dir("syn-it-conc");
    let spec = DatasetSpec {
        seed: 5,
        nodes: 600,
        edges: 1800,
    };
    let ops = vec![
        OpName::MatchByIndex,
        OpName::Expand1Hop,
        OpName::AggregateCount,
        OpName::ExpandHops5,
        OpName::AggregateGroup,
    ];
    recording::record(&spec, graph, &ops, spec.seed, 256, &dir).expect("record");

    let cfg = ReplayConfig {
        recording_dir: dir.clone(),
        endpoint: endpoint(),
        graph: Some(graph.to_string()),
        load: true,
        samples: 150,
        warmup: 30,
        concurrency: vec![1, 4],
        cache: benchmark::synthetic::CacheSelection::Both,
        server_timeout_ms: 5_000,
        client_deadline_ms: 6_000,
        out: dir.join("conc.json").to_string_lossy().into_owned(),
        server_image: None,
        label: None,
    };
    // If any op returned different results at C=4 vs the single-flight reference, run() errors here.
    // The two LIMIT ops (expand_hops_5, aggregate_group) are totally ordered, so their value digests
    // are deterministic too.
    let report = replay::run(&cfg).await.expect("replay concurrency sweep");

    assert_eq!(report.meta.concurrency, vec![1, 4]);
    for op in [
        "match_by_index",
        "expand_1_hop",
        "aggregate_count",
        "expand_hops_5",
        "aggregate_group",
    ] {
        let opr = &report.operations[op];
        assert_eq!(opr.levels.len(), 2, "op {op} should have two concurrency levels");
        assert!(opr.result_digest.is_some(), "op {op} needs a result digest");
        // Both cache modes were measured at each level.
        for lvl in &opr.levels {
            assert!(lvl.cached.is_some(), "op {op} C={} missing cached", lvl.concurrency);
            assert!(lvl.uncached.is_some(), "op {op} C={} missing uncached", lvl.concurrency);
        }
    }

    std::fs::remove_dir_all(&dir).ok();
    drop_graph(graph).await;
}
