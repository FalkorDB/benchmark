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

    // ...as does the :User(age) index (fixture parity with the A/B baseline — design §3.4), so age
    // is indexed alongside id.
    let age_indexed = scalar_i64(
        &mut g,
        "CALL db.indexes() YIELD label, properties, status \
         WHERE label = 'User' AND status = 'OPERATIONAL' AND 'age' IN properties RETURN count(*)",
    )
    .await;
    assert!(age_indexed >= 1, "expected an OPERATIONAL :User(age) index");

    // Every :Friend edge carries a deterministic bench_capacity in [1, 20] (no NULLs), so
    // capacity-filtered shapes exercise a real predicate rather than always matching zero rows.
    let missing_capacity = scalar_i64(
        &mut g,
        "MATCH (:User)-[r:Friend]->(:User) WHERE r.bench_capacity IS NULL RETURN count(r)",
    )
    .await;
    assert_eq!(missing_capacity, 0, "every :Friend edge must have bench_capacity");
    let out_of_range = scalar_i64(
        &mut g,
        "MATCH (:User)-[r:Friend]->(:User) WHERE r.bench_capacity < 1 OR r.bench_capacity > 20 \
         RETURN count(r)",
    )
    .await;
    assert_eq!(out_of_range, 0, "bench_capacity must stay within [1, 20]");

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
    use benchmark::synthetic::writes::{verify_mutation, ExpectedOutcome, WriteScratch};

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
        verify_mutation(ExpectedOutcome::node_created(), &s.mutations).expect("window-1 must miss");
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
        verify_mutation(ExpectedOutcome::node_created(), &s.mutations).expect("window-2 must miss");
    }
    assert_eq!(
        scalar_i64(&mut g, &count_cypher).await,
        reset_every as i64,
        "the reused band holds exactly one node per key — no duplicate accumulation"
    );

    drop_graph(graph).await;
}

/// Phase 7 §6.2 — pin the full mutation-counter set end-to-end against the server: DETACH DELETE
/// reports `relationships_deleted` = the victim's degree, REMOVE reports `properties_removed` +
/// `labels_removed`, and repeated (no-op) runs of **both** mutations report all-zero (absent
/// counters read as 0) — exactly the outcomes the generalized [`ExpectedOutcome`] model pins via
/// `exactly`.
#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn full_mutation_counters_are_read_from_the_server() {
    use benchmark::synthetic::writes::{verify_mutation, ExpectedOutcome, MutationStats};

    let graph = "syn_it_full_counters";
    drop_graph(graph).await;
    let mut g = open_graph(&endpoint(), graph).await.expect("open graph");
    let deadline = Duration::from_secs(5);

    // A hub (id 1) with 3 edges, and a labeled + flagged node (id 2) to strip.
    for cypher in [
        "CREATE (a:User {id:1}), (b:User {id:2}), (c:User {id:3}), (d:User {id:4}), \
         (a)-[:Friend]->(b), (a)-[:Friend]->(c), (d)-[:Friend]->(a)",
        "MATCH (u:User {id:2}) SET u:Temp, u.flag = 1",
    ] {
        run_and_drain(&mut g, QueryType::Write, cypher, 5_000, deadline)
            .await
            .expect("setup");
    }

    // DETACH DELETE the hub: 1 node + its full degree of edges, nothing else.
    let s = run_and_drain(
        &mut g,
        QueryType::Write,
        "MATCH (u:User {id:1}) DETACH DELETE u",
        5_000,
        deadline,
    )
    .await
    .expect("detach delete");
    let detach = MutationStats {
        nodes_deleted: 1,
        relationships_deleted: 3,
        ..Default::default()
    };
    assert_eq!(s.mutations, detach, "DETACH DELETE counters");
    verify_mutation(ExpectedOutcome::exactly(detach), &s.mutations).expect("detach outcome");

    // REMOVE a property + a label: only the removal counters move.
    let s = run_and_drain(
        &mut g,
        QueryType::Write,
        "MATCH (u:User {id:2}) REMOVE u.flag, u:Temp",
        5_000,
        deadline,
    )
    .await
    .expect("remove");
    let removed = MutationStats {
        properties_removed: 1,
        labels_removed: 1,
        ..Default::default()
    };
    assert_eq!(s.mutations, removed, "REMOVE counters");
    verify_mutation(ExpectedOutcome::exactly(removed), &s.mutations).expect("remove outcome");

    // Repeating both mutations is a silent no-op (E1c): the DETACH DELETE target is gone and the
    // property + label are already removed — the server omits every counter, all must read 0.
    let s = run_and_drain(
        &mut g,
        QueryType::Write,
        "MATCH (u:User {id:1}) DETACH DELETE u",
        5_000,
        deadline,
    )
    .await
    .expect("no-op detach delete");
    assert_eq!(s.mutations, MutationStats::default(), "no-op DETACH DELETE reports all-zero");
    assert!(
        verify_mutation(ExpectedOutcome::exactly(detach), &s.mutations).is_err(),
        "a silent no-op must not satisfy the deletion outcome"
    );

    let s = run_and_drain(
        &mut g,
        QueryType::Write,
        "MATCH (u:User {id:2}) REMOVE u.flag, u:Temp",
        5_000,
        deadline,
    )
    .await
    .expect("no-op remove");
    assert_eq!(s.mutations, MutationStats::default(), "no-op REMOVE reports all-zero");
    // …which the generalized model surfaces as a hard error when an outcome was expected.
    assert!(
        verify_mutation(ExpectedOutcome::exactly(removed), &s.mutations).is_err(),
        "a silent no-op must not satisfy a removal outcome"
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

/// Phase 3 B2 / Phase 4 / Phase 5: record the queries_repository READ shapes (`--repo-reads full`)
/// OFFLINE, then replay --load → --no-load. The workload identity (workload_hash) is byte-identical
/// across replays, every result-gated shape yields matching digests, and the result-N/A shapes (the
/// LIMIT-without-ORDER `entity_path_introspection` and the three fulltext/vector top-k reads) are
/// recorded + timed but carry no digest (`None`) in both replays.
///
/// Full includes the FixtureDependent fulltext/vector reads, so the bundle is recorded with
/// [`recording::record_rendered_with_fixture`] — the fulltext/vector fixture (index DDL + seed data)
/// is baked into the recorded graph ONCE and replayed verbatim, so both replays load the identical
/// fixture and the three smoke queries run against real indexes.
///
/// Uses a multi-thread runtime: the baseline reads return nodes/relations/paths, and the FalkorDB
/// client decodes those via `block_in_place` (as the production `#[tokio::main]` runtime does).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running FalkorDB server"]
async fn record_repo_reads_then_replay_roundtrips_byte_identically() {
    use benchmark::synthetic::{shapes, Tier};
    use std::collections::BTreeSet;

    let graph = "syn_it_repo_reads";
    drop_graph(graph).await;
    let dir = temp_bundle_dir("syn-it-repo-reads");
    let spec = DatasetSpec {
        seed: 9,
        nodes: 500,
        edges: 1500,
    };
    // Render every repo read shape's corpus ONCE from the fixed seed, then record verbatim. Full
    // selects the FixtureDependent reads, so the bundle bakes in the fulltext/vector fixture.
    let recorded = shapes::record_repo_reads(Tier::Full, spec.nodes as i32, spec.edges as i32, spec.seed)
        .expect("render repo reads");
    assert_eq!(
        recorded.len(),
        50,
        "Full records every repo read shape (46 baseline + 1 ExtendedCore + 3 FixtureDependent)"
    );
    assert!(
        shapes::repo_reads_need_fixture(Tier::Full),
        "Full needs the fulltext/vector fixture"
    );
    recording::record_rendered_with_fixture(&spec, graph, &recorded, spec.seed, 256, &dir)
        .expect("record");

    // Replay #1 loads the recorded graph (incl. the fixture); #2 reuses it (no-load), count-verifying
    // first.
    let ref_out = dir.join("ref.json").to_string_lossy().into_owned();
    let cand_out = dir.join("cand.json").to_string_lossy().into_owned();
    let a = replay::run(&replay_config(&dir, graph, &ref_out, true))
        .await
        .expect("replay --load");
    let b = replay::run(&replay_config(&dir, graph, &cand_out, false))
        .await
        .expect("replay --no-load");

    // Same workload identity across replays (record-once → replay-verbatim).
    let ha = a.meta.dataset.as_ref().expect("dataset a").workload_hash.clone();
    let hb = b.meta.dataset.as_ref().expect("dataset b").workload_hash.clone();
    assert_eq!(ha, hb, "workload_hash must match across replays");
    assert_eq!(a.operations.len(), 50);

    // The result-N/A shapes: the LIMIT-without-ORDER read plus the three fulltext/vector top-k reads.
    let result_na: BTreeSet<&str> = [
        "entity_path_introspection",
        "vector_query_nodes_smoke",
        "fulltext_query_nodes_smoke",
        "fulltext_query_relationships_smoke",
    ]
    .into_iter()
    .collect();

    // Every result-gated shape has a digest that matches across the two replays; each result-N/A
    // shape is timed but carries no digest in both replays.
    for (name, op_a) in &a.operations {
        let op_b = b.operations.get(name).expect("op present in both replays");
        if result_na.contains(name.as_str()) {
            assert!(op_a.result_digest.is_none(), "{name} must be result-N/A");
            assert!(op_b.result_digest.is_none(), "{name} must be result-N/A");
        } else {
            let da = op_a.result_digest.as_ref().unwrap_or_else(|| panic!("digest a for {name}"));
            let db = op_b.result_digest.as_ref().unwrap_or_else(|| panic!("digest b for {name}"));
            assert_eq!(da, db, "result digest for {name} must match across replays");
        }
    }
    // The Phase 4 ExtendedCore shape round-trips a byte-stable, gated digest (temporal + spatial +
    // float distance canonicalize deterministically).
    let ts = a.operations.get("temporal_spatial_roundtrip").expect("temporal_spatial_roundtrip present");
    assert!(ts.result_digest.is_some(), "temporal_spatial_roundtrip must be result-gated (digest present)");
    // The Phase 5 FixtureDependent shapes are recorded, timed, and present in both replays (their
    // fulltext/vector fixture loaded and the smoke queries ran against real indexes).
    for name in [
        "vector_query_nodes_smoke",
        "fulltext_query_nodes_smoke",
        "fulltext_query_relationships_smoke",
    ] {
        let op = a.operations.get(name).unwrap_or_else(|| panic!("{name} missing from report"));
        assert!(op.result_digest.is_none(), "{name} is result-N/A (top-k)");
    }

    // The guard proceeds (same workload + matching gated digests).
    match guard(&BaselineKey::from_report(&a), &BaselineKey::from_report(&b)) {
        GuardOutcome::Proceed { .. } => {}
        GuardOutcome::Abort { reason } => panic!("guard aborted unexpectedly: {reason}"),
    }

    std::fs::remove_dir_all(&dir).ok();
    drop_graph(graph).await;
}

/// Per-op budget + result-N/A reference-capture skip at replay (design §3.4).
///
/// Records a bundle whose ops carry different [`RecordedBudget`]s and replays it once under a
/// global `concurrency [1, 2]` / `cache both` config:
/// - `budgeted_op` (samples 1, warmup 0, concurrency `[1]`, cached-only) must measure exactly one
///   C=1 cached level — its recorded budget overrides the run's global sweep and cache selection;
/// - `global_op` (no budget) must measure the full global sweep under both cache modes;
/// - `na_probe_op` (result-N/A, same tight budget) carries a **deliberately broken third
///   command**: replay must still succeed because a result-N/A op skips the full reference
///   capture (only its first command is probed) and its 1-sample C=1 cached measured loop (one
///   untimed prime + one sample) never cycles past `corpus[1]`. Before the skip, the reference
///   pass captured every command and this bundle failed hard.
///
/// Multi-thread runtime: FalkorDB entity decoding uses `block_in_place`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running FalkorDB server"]
async fn replay_honors_per_op_budgets_and_result_na_skips_reference_capture() {
    use benchmark::synthetic::catalog::RecordedBudget;
    use benchmark::synthetic::recording::RecordedOp;
    use benchmark::synthetic::OpKey;

    let graph = "syn_it_budget";
    drop_graph(graph).await;
    let dir = temp_bundle_dir("syn-it-budget");
    let spec = DatasetSpec {
        seed: 9,
        nodes: 200,
        edges: 400,
    };
    let tight = RecordedBudget {
        samples: Some(1),
        warmup: Some(0),
        concurrency: Some(vec![1]),
        cache: Some(benchmark::synthetic::CacheSelection::Cached),
        ..RecordedBudget::default()
    };
    let ops = vec![
        RecordedOp {
            key: OpKey::dynamic("budgeted_op", QueryType::Read),
            result_gated: true,
            budget: tight.clone(),
            capability: None,
            commands: vec!["MATCH (u:User {id: 1}) RETURN u.id".to_string()],
        },
        RecordedOp {
            key: OpKey::dynamic("global_op", QueryType::Read),
            result_gated: true,
            budget: RecordedBudget::default(),
            capability: None,
            commands: vec![
                "MATCH (u:User {id: 2}) RETURN u.id".to_string(),
                "MATCH (u:User {id: 3}) RETURN u.id".to_string(),
            ],
        },
        RecordedOp {
            key: OpKey::dynamic("na_probe_op", QueryType::Read),
            result_gated: false,
            budget: tight.clone(),
            capability: None,
            commands: vec![
                "MATCH (u:User {id: 4}) RETURN u.id".to_string(),
                "MATCH (u:User {id: 5}) RETURN u.id".to_string(),
                // Invalid on purpose: only reachable by a full reference capture.
                "MATCH (u:User RETURN syntax_error".to_string(),
            ],
        },
    ];
    recording::record_rendered(&spec, graph, &ops, spec.seed, 256, &dir).expect("record");

    let out = dir.join("r.json").to_string_lossy().into_owned();
    let mut config = replay_config(&dir, graph, &out, true);
    config.samples = 3;
    config.warmup = 1;
    config.concurrency = vec![1, 2];
    config.cache = benchmark::synthetic::CacheSelection::Both;
    let report = replay::run(&config).await.expect("replay with per-op budgets");

    // budgeted_op: exactly one C=1 level, cached-only, result-gated.
    let budgeted = &report.operations["budgeted_op"];
    let levels: Vec<usize> = budgeted.levels.iter().map(|l| l.concurrency).collect();
    assert_eq!(levels, vec![1], "budgeted_op must measure only its budgeted sweep");
    assert!(budgeted.levels[0].cached.is_some(), "budgeted_op measures cached");
    assert!(budgeted.levels[0].uncached.is_none(), "budgeted_op skips uncached (budget)");
    assert!(budgeted.result_digest.is_some(), "budgeted_op stays result-gated");
    // Its effective (resolved) measurement policy is persisted so the diff/baseline guards can
    // refuse a comparison against a run that measured it under different conditions.
    let expected_policy = benchmark::synthetic::report::OpPolicy {
        samples: 1,
        warmup: 0,
        concurrency: vec![1],
        cache: benchmark::synthetic::CacheSelection::Cached,
        server_timeout_ms: 5_000, // inherited from the run's global knobs
        client_deadline_ms: 6_000,
    };
    assert_eq!(
        budgeted.policy.as_ref(),
        Some(&expected_policy),
        "budgeted_op persists its resolved per-op policy"
    );

    // global_op: the full global sweep, both cache modes, result-gated.
    let global = &report.operations["global_op"];
    let levels: Vec<usize> = global.levels.iter().map(|l| l.concurrency).collect();
    assert_eq!(levels, vec![1, 2], "global_op inherits the global sweep");
    for level in &global.levels {
        assert!(level.cached.is_some(), "global_op measures cached");
        assert!(level.uncached.is_some(), "global_op measures uncached");
    }
    assert!(global.result_digest.is_some(), "global_op stays result-gated");
    assert!(global.policy.is_none(), "an inherit-everything op persists no per-op policy");

    // na_probe_op: replay succeeded despite the broken third command (capture skipped), no digest.
    let na = &report.operations["na_probe_op"];
    assert!(na.result_digest.is_none(), "na_probe_op is result-N/A");
    let levels: Vec<usize> = na.levels.iter().map(|l| l.concurrency).collect();
    assert_eq!(levels, vec![1], "na_probe_op must measure only its budgeted sweep");
    assert_eq!(
        na.policy.as_ref(),
        Some(&expected_policy),
        "the result-N/A op carries the same tight budget, so the same resolved policy"
    );

    // The report's meta echoes the run's global knobs (budgets are per-op policy).
    assert_eq!(report.meta.concurrency, vec![1, 2]);
    assert_eq!(report.meta.samples, 3);

    std::fs::remove_dir_all(&dir).ok();
    drop_graph(graph).await;
}

/// Capability probe-before-capture (design Phase 6 §3.5): an op whose recorded `capability`
/// procedure is absent from the engine's `dbms.procedures()` registry is **skipped** — never
/// executed (its deliberately invalid command would fail the result-gated reference capture),
/// reported with `skipped: Some(reason)` and no levels/digest/policy — while the capability-free
/// op in the same bundle measures normally.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running FalkorDB server"]
async fn replay_skips_ops_whose_capability_procedure_is_missing() {
    use benchmark::synthetic::catalog::RecordedBudget;
    use benchmark::synthetic::recording::RecordedOp;
    use benchmark::synthetic::OpKey;

    let graph = "syn_it_capskip";
    drop_graph(graph).await;
    let dir = temp_bundle_dir("syn-it-capskip");
    let spec = DatasetSpec {
        seed: 9,
        nodes: 200,
        edges: 400,
    };
    let ops = vec![
        RecordedOp {
            key: OpKey::dynamic("measured_op", QueryType::Read),
            result_gated: true,
            budget: RecordedBudget::default(),
            capability: None,
            commands: vec!["MATCH (u:User {id: 1}) RETURN u.id".to_string()],
        },
        RecordedOp {
            key: OpKey::dynamic("phantom_op", QueryType::Read),
            result_gated: true,
            budget: RecordedBudget::default(),
            capability: Some("algo.noSuchProcedureBench".to_string()),
            // Invalid on purpose: result-gated ops get a full reference capture, so replay can
            // only succeed by never executing this op at all.
            commands: vec!["CALL algo.noSuchProcedureBench( RETURN oops".to_string()],
        },
    ];
    recording::record_rendered(&spec, graph, &ops, spec.seed, 256, &dir).expect("record");

    let out = dir.join("r.json").to_string_lossy().into_owned();
    let mut config = replay_config(&dir, graph, &out, true);
    config.samples = 3;
    config.warmup = 1;
    let report = replay::run(&config)
        .await
        .expect("replay must skip the phantom op");

    let phantom = &report.operations["phantom_op"];
    let reason = phantom
        .skipped
        .as_deref()
        .expect("phantom_op must be marked skipped");
    assert!(
        reason.contains("algo.noSuchProcedureBench"),
        "skip reason names the missing procedure: {reason}"
    );
    assert!(
        phantom.levels.is_empty(),
        "a skipped op has no measured levels"
    );
    assert!(
        phantom.result_digest.is_none(),
        "a skipped op has no digest"
    );
    assert!(
        phantom.policy.is_none(),
        "a skipped op has no resolved policy"
    );

    let measured = &report.operations["measured_op"];
    assert!(
        measured.skipped.is_none(),
        "the capability-free op measures normally"
    );
    assert!(!measured.levels.is_empty());
    assert!(measured.result_digest.is_some());

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
        repo_reads: None,
        repo_algorithms: false,
        repo_writes: false,
        seed: Some(11),
        nodes: Some(400),
        edges: Some(1200),
        oracle: None,
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

/// Phase 6 §7.3 acceptance: `record --repo-algorithms` (offline, via the CLI arm) then a live
/// replay measures all 4 whole-graph algorithm shapes end-to-end on the generated **simple** graph
/// (synthbench/v5 — `algo.maxFlow` rejects parallel edges, so this passing proves the guarantee).
/// Each op must obey its recorded per-op budget (C=1, cached-only) and persist its resolved
/// effective policy; the §6 determinism table gates `max_flow`/`msf` digests (byte-stability
/// re-verified here across two independent replays — §7.5) while `pagerank`/`harmonic` stay
/// result-N/A.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running FalkorDB server"]
async fn record_and_replay_algorithm_shapes_end_to_end() {
    use benchmark::cli::SyntheticCommands;
    use benchmark::synthetic::run_command;

    let graph = "syn_it_algos";
    drop_graph(graph).await;
    let dir = temp_bundle_dir("syn-it-algos");
    let out_dir = dir.to_string_lossy().into_owned();

    run_command(SyntheticCommands::Record {
        config: None,
        graph: Some(graph.to_string()),
        ops: vec![],
        all_reads: false,
        tier: None,
        repo_reads: None,
        repo_algorithms: true,
        repo_writes: false,
        seed: Some(7),
        nodes: Some(300),
        edges: Some(900),
        oracle: None,
        out_dir: out_dir.clone(),
    })
    .await
    .expect("record --repo-algorithms via run_command");

    // The bundle's manifest annotates every algorithm op with its required procedure (§3.5).
    let bundle = recording::load(&dir).expect("load the recorded bundle");
    let caps: Vec<Option<&str>> = bundle
        .manifest
        .ops
        .iter()
        .map(|op| op.capability.as_deref())
        .collect();
    assert_eq!(
        caps,
        vec![
            Some("algo.pageRank"),
            Some("algo.maxFlow"),
            Some("algo.MSF"),
            Some("algo.HarmonicCentrality"),
        ],
        "recorded manifest must carry the per-procedure capabilities"
    );

    let out = dir.join("algos.json").to_string_lossy().into_owned();
    let mut config = replay_config(&dir, graph, &out, true);
    // Global knobs the per-op budgets must override (sweep + cache) or inherit (nothing: the
    // algorithm budget pins samples/warmup/cache/sweep/timeouts).
    config.samples = 3;
    config.warmup = 1;
    config.concurrency = vec![1, 2];
    config.cache = benchmark::synthetic::CacheSelection::Both;
    let report = replay::run(&config).await.expect("replay the 4 algorithm shapes");

    for name in [
        "algo_pagerank_summary",
        "algo_max_flow_single_pair",
        "algo_msf_summary",
        "algo_harmonic_summary",
    ] {
        let op = report
            .operations
            .get(name)
            .unwrap_or_else(|| panic!("{name} missing from the replay report"));
        assert!(op.skipped.is_none(), "{name} must pass the capability probe on this server");
        let levels: Vec<usize> = op.levels.iter().map(|l| l.concurrency).collect();
        assert_eq!(levels, vec![1], "{name} must measure only its budgeted C=1 sweep");
        assert!(op.levels[0].cached.is_some(), "{name} measures cached");
        assert!(op.levels[0].uncached.is_none(), "{name} skips uncached (budget)");
        let policy = op.policy.as_ref().unwrap_or_else(|| panic!("{name} must persist policy"));
        assert_eq!(policy.samples, 25, "{name} budgeted samples");
        assert_eq!(policy.concurrency, vec![1], "{name} budgeted sweep");
        assert_eq!(policy.server_timeout_ms, 60_000, "{name} budgeted server timeout");
    }
    // Digest gating per the §6 determinism table: the deterministic pair is gated, the float
    // shapes are N/A.
    for name in ["algo_max_flow_single_pair", "algo_msf_summary"] {
        let op = report
            .operations
            .get(name)
            .unwrap_or_else(|| panic!("{name} missing from the replay report"));
        assert!(op.result_digest.is_some(), "{name} is digest-gated (design §6/§7.5)");
    }
    for name in ["algo_pagerank_summary", "algo_harmonic_summary"] {
        let op = report
            .operations
            .get(name)
            .unwrap_or_else(|| panic!("{name} missing from the replay report"));
        assert!(op.result_digest.is_none(), "{name} stays result-N/A (design §6)");
    }

    // §7.5 byte-stability, verified continuously: an independent second replay of the same bundle
    // must reproduce the gated digests byte-identically (this is what keeps max_flow/msf gated).
    // The recorded per-op algorithm budget pins samples/warmup — the helper's globals are inert.
    let out2 = dir.join("algos2.json").to_string_lossy().into_owned();
    let config2 = replay_config(&dir, graph, &out2, true);
    let report2 = replay::run(&config2).await.expect("second independent replay");
    for name in ["algo_max_flow_single_pair", "algo_msf_summary"] {
        let second = report2
            .operations
            .get(name)
            .unwrap_or_else(|| panic!("{name} must be measured by the second replay"));
        assert!(
            second.skipped.is_none(),
            "{name} must pass the capability probe on the second replay"
        );
        let d2 = second
            .result_digest
            .as_ref()
            .unwrap_or_else(|| panic!("{name} must stay digest-gated on the second replay"));
        let first = report
            .operations
            .get(name)
            .unwrap_or_else(|| panic!("{name} missing from the first replay report"));
        assert_eq!(
            first.result_digest.as_ref(),
            Some(d2),
            "{name} digest must be byte-stable across independent replays"
        );
    }

    std::fs::remove_dir_all(&dir).ok();
    drop_graph(graph).await;
}

/// Phase 7 §6.1 acceptance: `record --repo-writes` (offline, via the CLI arm) then a live replay
/// measures all 10 write shapes end-to-end via `GRAPH.QUERY` (empirics E1: `RO_QUERY` rejects
/// writes) at the budgeted C=1, under BOTH cache modes with a base reset before every measured
/// cell (§3.3 latency tier), asserting nothing about results (`result_digest: None`) while
/// persisting each op's effective policy. The replay's error-safe final restore (§3.5) must leave
/// the endpoint's graph content-identical to the recorded base — re-verified here by raw counts
/// and by a second full replay of the same bundle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running FalkorDB server"]
async fn record_and_replay_write_shapes_end_to_end() {
    use benchmark::cli::SyntheticCommands;
    use benchmark::synthetic::run_command;

    let graph = "syn_it_repo_writes";
    drop_graph(graph).await;
    let dir = temp_bundle_dir("syn-it-writes");
    let out_dir = dir.to_string_lossy().into_owned();

    run_command(SyntheticCommands::Record {
        config: None,
        graph: Some(graph.to_string()),
        ops: vec![],
        all_reads: false,
        tier: None,
        repo_reads: None,
        repo_algorithms: false,
        repo_writes: true,
        seed: Some(7),
        nodes: Some(300),
        edges: Some(900),
        oracle: None,
        out_dir: out_dir.clone(),
    })
    .await
    .expect("record --repo-writes via run_command");

    let bundle = recording::load(&dir).expect("load the recorded write bundle");
    assert_eq!(bundle.manifest.format_version, 2, "write bundles are recording format v2");

    let out = dir.join("writes.json").to_string_lossy().into_owned();
    let mut config = replay_config(&dir, graph, &out, true);
    // Global knobs the per-op write budget must override (samples/warmup/sweep) or inherit
    // (cache: WRITE_BUDGET leaves it None, so the global Both applies — two cells per op).
    config.samples = 3;
    config.warmup = 1;
    config.concurrency = vec![1, 2];
    config.cache = benchmark::synthetic::CacheSelection::Both;
    let report = replay::run(&config).await.expect("replay the 10 write shapes");

    let expected: Vec<String> = bundle.manifest.ops.iter().map(|e| e.name.clone()).collect();
    assert_eq!(expected.len(), 10, "the write family is exactly 10 shapes");
    assert_eq!(
        report.operations.len(),
        10,
        "every write shape must be measured: {:?}",
        report.operations.keys().collect::<Vec<_>>()
    );
    for name in &expected {
        let op = report
            .operations
            .get(name)
            .unwrap_or_else(|| panic!("{name} missing from the replay report"));
        assert!(op.skipped.is_none(), "{name} is plain Cypher — never capability-skipped");
        let level = only_level(op);
        assert_eq!(level.concurrency, 1, "{name} must measure only the budgeted C=1");
        assert!(level.cached.is_some(), "{name} measures the cached cell");
        assert!(level.uncached.is_some(), "{name} measures the uncached cell (inherited Both)");
        assert!(
            level.compilation_ms_median.is_some(),
            "{name} derives compilation cost from the merged cells"
        );
        assert!(
            op.result_digest.is_none(),
            "{name} is latency-tier: nothing asserted about results (§4.1)"
        );
        let policy = op.policy.as_ref().unwrap_or_else(|| panic!("{name} must persist policy"));
        assert_eq!(policy.samples, 100, "{name} budgeted samples");
        assert_eq!(policy.warmup, 10, "{name} budgeted warmup");
        assert_eq!(policy.concurrency, vec![1], "{name} budgeted sweep");
    }

    // §3.5 error-safe final restore: the replay's own content verification passed (or run()
    // would have failed); double-check from a fresh connection that the graph is EXACTLY the
    // 300-node / 900-edge recorded base — no residue from ~2200 measured mutations.
    let mut g = open_graph(&endpoint(), graph).await.expect("open restored graph");
    assert_eq!(scalar_i64(&mut g, "MATCH (n) RETURN count(n)").await, 300);
    assert_eq!(scalar_i64(&mut g, "MATCH ()-[r]->() RETURN count(r)").await, 900);
    drop(g);

    // A second full replay of the same bundle proves the restored graph state is reusable
    // end-to-end (fresh load, fresh resets, same invariants).
    let out2 = dir.join("writes2.json").to_string_lossy().into_owned();
    let mut config2 = replay_config(&dir, graph, &out2, true);
    config2.samples = 3;
    config2.warmup = 1;
    let report2 = replay::run(&config2).await.expect("second write replay");
    assert_eq!(report2.operations.len(), 10);

    std::fs::remove_dir_all(&dir).ok();
    drop_graph(graph).await;
}

/// Phase 7 §3.5 — a write replay that fails **during the reference pass** (a broken recorded
/// command caught by the write fail-fast probe) must still run the error-safe final restore: an
/// earlier op's probe has already mutated the graph by then, so skipping the restore would leave
/// the endpoint polluted for the next recording/replay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running FalkorDB server"]
async fn failed_write_probe_still_restores_the_recorded_base() {
    use benchmark::synthetic::catalog::RecordedBudget;
    use benchmark::synthetic::recording::RecordedOp;
    use benchmark::synthetic::OpKey;

    let graph = "syn_it_write_probe_fail";
    drop_graph(graph).await;
    let dir = temp_bundle_dir("syn-it-probe-fail");
    let spec = DatasetSpec {
        seed: 7,
        nodes: 50,
        edges: 100,
    };
    // Op order matters: the first op's probe mutates the graph, then the second op's broken
    // command fails its probe mid-reference-pass.
    let ops = vec![
        RecordedOp {
            key: OpKey::dynamic("w_marker", QueryType::Write),
            result_gated: false,
            budget: RecordedBudget {
                concurrency: Some(vec![1]),
                ..RecordedBudget::default()
            },
            capability: None,
            commands: vec!["CREATE (:ProbeMarker)".to_string()],
        },
        RecordedOp {
            key: OpKey::dynamic("w_broken", QueryType::Write),
            result_gated: false,
            budget: RecordedBudget {
                concurrency: Some(vec![1]),
                ..RecordedBudget::default()
            },
            capability: None,
            commands: vec!["CREAT (:Broken)".to_string()],
        },
    ];
    recording::record_rendered(&spec, graph, &ops, spec.seed, 256, &dir).expect("record");

    let out = dir.join("r.json").to_string_lossy().into_owned();
    let config = replay_config(&dir, graph, &out, true);
    let err = replay::run(&config)
        .await
        .expect_err("a broken write command must fail the replay at its probe");
    assert!(
        format!("{err:?}").contains("probing write 'w_broken'"),
        "error should name the failing probe, got: {err:?}"
    );

    // The final restore ran despite the reference-pass failure: the marker probe's mutation is
    // gone and the graph is exactly the recorded base again.
    let mut g = open_graph(&endpoint(), graph).await.expect("open restored graph");
    assert_eq!(
        scalar_i64(&mut g, "MATCH (m:ProbeMarker) RETURN count(m)").await,
        0,
        "probe mutation must be erased by the final restore"
    );
    assert_eq!(
        scalar_i64(&mut g, "MATCH (n) RETURN count(n)").await,
        50,
        "restored graph must be the recorded base"
    );
    drop(g);

    std::fs::remove_dir_all(&dir).ok();
    drop_graph(graph).await;
}

/// Phase 7 §6.2 — [`replay::restore_base`] is a *per-invocation* restore primitive: calling it
/// back-to-back after arbitrary pollution must return the endpoint to the recorded base every
/// single time — the property the §6.3 correctness tier will lean on between oracle invocations.
/// "Returns the base" is asserted on **content digests** ([`replay::capture_graph_content`]), not
/// just counts: the count-preserving rounds mutate node/edge *properties* (invisible to counts,
/// asserted so) and only the digest can catch them; the count-changing round proves the reload
/// path. The pristine shape is captured after the first restore (no fixture assumptions).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running FalkorDB server"]
async fn restore_base_returns_the_recorded_base_per_invocation() {
    let graph = "syn_it_restore_base";
    drop_graph(graph).await;
    let dir = temp_bundle_dir("syn-it-restore");
    let spec = DatasetSpec {
        seed: 7,
        nodes: 120,
        edges: 360,
    };
    recording::record(&spec, graph, &[OpName::MatchByIndex], spec.seed, 16, &dir)
        .expect("record a small bundle");
    let bundle = recording::load(&dir).expect("load the bundle");
    let out = dir.join("unused.json").to_string_lossy().into_owned();
    let config = replay_config(&dir, graph, &out, true);

    replay::restore_base(&config, &bundle, graph, &spec)
        .await
        .expect("initial restore");
    let mut g = open_graph(&endpoint(), graph).await.expect("open graph");
    let pristine_nodes = scalar_i64(&mut g, "MATCH (n) RETURN count(n)").await;
    let pristine_edges = scalar_i64(&mut g, "MATCH ()-[r]->() RETURN count(r)").await;
    let pristine = replay::capture_graph_content(&mut g, &config)
        .await
        .expect("capture the pristine content digest");

    let deadline = Duration::from_secs(5);
    for round in 0..3_i64 {
        if round % 2 == 0 {
            // Count-preserving pollution: corrupt a node property and an edge property. Node and
            // edge counts stay pristine — only the content digest can detect this class.
            for cypher in [
                "MATCH (u:User) WITH u ORDER BY u.id LIMIT 1 SET u.polluted = true",
                "MATCH ()-[r:Friend]->() WITH r LIMIT 1 SET r.polluted = true",
            ] {
                run_and_drain(&mut g, QueryType::Write, cypher, 5_000, deadline)
                    .await
                    .expect("property pollution");
            }
            assert_eq!(
                scalar_i64(&mut g, "MATCH (n) RETURN count(n)").await,
                pristine_nodes,
                "round {round}: property pollution must preserve the node count"
            );
            assert_eq!(
                scalar_i64(&mut g, "MATCH ()-[r]->() RETURN count(r)").await,
                pristine_edges,
                "round {round}: property pollution must preserve the edge count"
            );
        } else {
            // Count-changing pollution: add a marker node and detach-delete a connected user.
            let marker = format!("CREATE (:RestoreMarker {{round: {round}}})");
            for cypher in [
                marker.as_str(),
                "MATCH (u:User)-[]->() WITH u LIMIT 1 DETACH DELETE u",
            ] {
                run_and_drain(&mut g, QueryType::Write, cypher, 5_000, deadline)
                    .await
                    .expect("structural pollution");
            }
        }
        let polluted = replay::capture_graph_content(&mut g, &config)
            .await
            .expect("capture the polluted content digest");
        assert_ne!(
            polluted, pristine,
            "round {round}: pollution must be visible to the content digest"
        );
        drop(g);

        replay::restore_base(&config, &bundle, graph, &spec)
            .await
            .unwrap_or_else(|e| panic!("round {round}: restore_base failed: {e}"));

        g = open_graph(&endpoint(), graph).await.expect("reopen graph");
        assert_eq!(
            scalar_i64(&mut g, "MATCH (m:RestoreMarker) RETURN count(m)").await,
            0,
            "round {round}: the marker must be erased"
        );
        assert_eq!(
            scalar_i64(&mut g, "MATCH (n) RETURN count(n)").await,
            pristine_nodes,
            "round {round}: node count must match the recorded base"
        );
        assert_eq!(
            scalar_i64(&mut g, "MATCH ()-[r]->() RETURN count(r)").await,
            pristine_edges,
            "round {round}: edge count must match the recorded base"
        );
        let restored = replay::capture_graph_content(&mut g, &config)
            .await
            .expect("capture the restored content digest");
        assert_eq!(
            restored, pristine,
            "round {round}: restored content must digest-match the recorded base"
        );
    }
    drop(g);

    std::fs::remove_dir_all(&dir).ok();
    drop_graph(graph).await;
}

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

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn max_flow_runs_on_the_generated_simple_graph() {
    // Design synthetic-cover-algorithms-phase6 §3.1: the pre-v5 generator emitted 8 parallel
    // (src,dst) `:Friend` pairs in the seed=7 1000/5000 CI oracle fixture, and FalkorDB's
    // `algo.maxFlow` rejects multigraphs ("relationship type must not contain multi-edges
    // (tensors)"). The generator now guarantees a simple graph; prove it live by loading the exact
    // oracle fixture through the production loader and running the repository's real maxFlow shape.
    use benchmark::queries_repository::{
        AlgorithmQuerySelection, Flavour, QueryCoverageProfile, UsersQueriesRepository,
    };
    use futures::StreamExt;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    let graph = "syn_it_maxflow_simple";
    drop_graph(graph).await;
    let cfg = Config {
        graph: graph.to_string(),
        dataset: Some(DatasetSpec { seed: 7, nodes: 1000, edges: 5000 }),
        ops: vec![OpName::ReturnConst],
        samples: 1,
        warmup: 0,
        cache: CacheSelection::Cached,
        ..base_config(graph)
    };
    run(&cfg).await.expect("generate + load the oracle dataset");

    let mut g = open_graph(&endpoint(), graph).await.expect("open loaded graph");
    // No parallel edges made it into the store. The relationship variable MUST be bound
    // (`[r:Friend]`, `count(r)`): an unnamed `[:Friend]` pattern collapses tensor multi-edges
    // and returns 0 even on a known multigraph (verified live on falkordb/falkordb:edge).
    let mut dup = g
        .ro_query(
            "MATCH (a:User)-[r:Friend]->(b:User) WITH a, b, count(r) AS c WHERE c > 1 \
             RETURN count(*)",
        )
        .execute()
        .await
        .expect("duplicate-pair count query");
    let dups: i64 = dup
        .data
        .next()
        .await
        .expect("one count row")
        .expect("count row decodes")
        .try_get_at(0)
        .expect("count is an integer");
    assert_eq!(dups, 0, "the loaded graph contains {dups} parallel (src,dst) :Friend pairs");

    // The repository's real maxFlow read runs without the tensors error and yields a positive
    // flow (the ring backbone connects every seeded (source, target) pair; bench_capacity >= 1).
    let repo = UsersQueriesRepository::new(
        1000,
        5000,
        Flavour::FalkorDB,
        AlgorithmQuerySelection::default(),
        QueryCoverageProfile::Baseline,
    );
    let mut rng = StdRng::seed_from_u64(7);
    let prepared = repo
        .render_read_with_rng("algo_max_flow_single_pair", &mut rng)
        .expect("render the maxFlow shape");
    let mut res = g
        .ro_query(&prepared.cypher)
        .execute()
        .await
        .expect("algo.maxFlow must not hit the multigraph (tensors) error");
    let flow: f64 = res
        .data
        .next()
        .await
        .expect("maxFlow yields one row")
        .expect("maxFlow row decodes")
        .try_get_at(0)
        .expect("max_flow is a float");
    assert!(flow > 0.0, "ring backbone connects every pair, got max_flow = {flow}");

    drop_graph(graph).await;
}

/// Phase 7 §6.3 — the full oracle flow end to end: `record --repo-writes --oracle` captures each
/// eligible write's per-command outcomes online (double-pass determinism proven at record time),
/// upgrades the bundle to format v3 with the outcomes hash-bound, and `replay::run` re-verifies
/// every recorded outcome from a pristine base before measuring latency.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running FalkorDB server"]
async fn record_with_oracle_captures_verifies_and_replays_end_to_end() {
    use benchmark::cli::SyntheticCommands;
    use benchmark::synthetic::run_command;
    use benchmark::synthetic::shapes::{write_shapes, OracleEligibility};

    let graph = "syn_it_oracle";
    drop_graph(graph).await;
    let dir = temp_bundle_dir("syn-it-oracle");
    let out_dir = dir.to_string_lossy().into_owned();

    run_command(SyntheticCommands::Record {
        config: None,
        graph: Some(graph.to_string()),
        ops: vec![],
        all_reads: false,
        tier: None,
        repo_reads: None,
        repo_algorithms: false,
        repo_writes: true,
        seed: Some(7),
        nodes: Some(300),
        edges: Some(900),
        oracle: Some(endpoint()),
        out_dir: out_dir.clone(),
    })
    .await
    .expect("record --repo-writes --oracle via run_command");

    let bundle = recording::load(&dir).expect("load the oracle bundle");
    assert_eq!(bundle.manifest.format_version, 3, "oracle bundles are recording format v3");
    let mut eligible: Vec<&str> = write_shapes()
        .iter()
        .filter(|s| s.oracle == OracleEligibility::Eligible)
        .map(|s| s.name)
        .collect();
    eligible.sort_unstable(); // bundle.oracle is a BTreeMap — compare in key order
    assert_eq!(
        bundle.oracle.keys().map(String::as_str).collect::<Vec<_>>(),
        eligible,
        "exactly the §6.3 deterministic subset is oracle-captured"
    );
    for entry in &bundle.manifest.ops {
        if eligible.contains(&entry.name.as_str()) {
            assert_eq!(
                entry.oracle,
                Some(entry.count),
                "{}: the oracle covers the complete command corpus",
                entry.name
            );
            assert_eq!(bundle.oracle[&entry.name].len(), entry.count, "{}", entry.name);
        } else {
            assert_eq!(entry.oracle, None, "{} is excluded from the oracle", entry.name);
        }
    }
    // The first outcome of the plain CREATE shape is knowable a priori — pin it as a smoke check
    // that the oracle recorded real counters (everything else is engine-reported).
    let svw = &bundle.oracle["single_vertex_write"][0];
    assert_eq!(svw.nodes_created, 1, "CREATE makes one node: {svw:?}");

    // Replay: the oracle verify pass runs before measurement and the whole run stays green.
    let out = dir.join("oracle.json").to_string_lossy().into_owned();
    let mut config = replay_config(&dir, graph, &out, true);
    config.samples = 2;
    config.warmup = 0;
    config.cache = benchmark::synthetic::CacheSelection::Cached;
    let report = replay::run(&config).await.expect("replay a v3 oracle bundle");
    assert_eq!(report.operations.len(), 10, "all 10 write shapes still measured");
    // The report attests the verified oracle coverage (op → outcome count), so a v3→v2
    // downgrade is visible to report consumers, not just to the replay log.
    let attested = report.meta.oracle_verified.as_ref().expect("v3 replay attests its oracle");
    assert_eq!(
        attested.keys().map(String::as_str).collect::<Vec<_>>(),
        eligible,
        "the attestation names exactly the verified ops"
    );
    for entry in &bundle.manifest.ops {
        if let Some(n) = entry.oracle {
            assert_eq!(attested[&entry.name], n, "{}", entry.name);
        }
    }

    // §3.5: the endpoint's graph is left exactly at the recorded base.
    let mut g = open_graph(&endpoint(), graph).await.expect("open restored graph");
    assert_eq!(scalar_i64(&mut g, "MATCH (n) RETURN count(n)").await, 300);
    assert_eq!(scalar_i64(&mut g, "MATCH ()-[r]->() RETURN count(r)").await, 900);

    std::fs::remove_dir_all(&dir).ok();
    drop_graph(graph).await;
}

/// Phase 7 §6.3 — a recorded outcome the engine no longer reproduces must HARD-FAIL the replay
/// (naming the op, seq and command), and the §3.5 error-safe final restore must still leave the
/// recorded base behind. A diverged write means the engine is doing different work — measuring
/// its latency anyway would silently poison the A/B trend.
///
/// The bundle is a hand-rendered single-op recording carrying the eligible `single_vertex_write`
/// name: the §6.3 exact-set rule makes it a valid v3 bundle (one recorded eligible op, full
/// corpus), and crafting the outcome offline keeps the divergence deterministic — the plain
/// CREATE reports `nodes_created=1`, so expecting 7 always diverges at seq 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running FalkorDB server"]
async fn replay_hard_fails_on_a_diverged_oracle_outcome() {
    use benchmark::synthetic::recording::RecordedOp;
    use benchmark::synthetic::writes::MutationStats;
    use benchmark::synthetic::OpKey;
    use std::collections::BTreeMap;

    let graph = "syn_it_oracle_diverged";
    drop_graph(graph).await;
    let dir = temp_bundle_dir("syn-it-oracle-div");
    let spec = DatasetSpec {
        seed: 7,
        nodes: 60,
        edges: 180,
    };
    let op = RecordedOp {
        key: OpKey::dynamic("single_vertex_write", QueryType::Write),
        result_gated: false,
        budget: Default::default(),
        capability: None,
        commands: vec!["CREATE (:User {id: 999983})".to_string()],
    };
    recording::record_rendered(&spec, graph, &[op], 7, 1_000, &dir).expect("record v2");

    // Attach a hash-valid but WRONG oracle: the plain CREATE reports nodes_created=1, so
    // expecting 7 is a guaranteed, deterministic divergence.
    let mut wrong = BTreeMap::new();
    wrong.insert(
        "single_vertex_write".to_string(),
        vec![MutationStats {
            nodes_created: 7,
            ..MutationStats::default()
        }],
    );
    recording::attach_oracle(&dir, &wrong).expect("attach the crafted oracle");

    let out = dir.join("diverged.json").to_string_lossy().into_owned();
    let mut config = replay_config(&dir, graph, &out, true);
    config.samples = 2;
    config.warmup = 0;
    config.cache = benchmark::synthetic::CacheSelection::Cached;
    let err = replay::run(&config).await.expect_err("a diverged oracle outcome must fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("oracle mismatch for op 'single_vertex_write' seq 0"),
        "must name the op and seq: {msg}"
    );
    assert!(msg.contains("nodes_created"), "must name the diverged counter: {msg}");
    assert!(msg.contains("expected exactly 7"), "must show the recorded expectation: {msg}");

    // The hard failure still ran the §3.5 final restore.
    let mut g = open_graph(&endpoint(), graph).await.expect("open restored graph");
    assert_eq!(scalar_i64(&mut g, "MATCH (n) RETURN count(n)").await, 60);
    assert_eq!(scalar_i64(&mut g, "MATCH ()-[r]->() RETURN count(r)").await, 180);

    std::fs::remove_dir_all(&dir).ok();
    drop_graph(graph).await;
}

/// Phase 7 §6.3 — engine failures inside the oracle paths carry the op/seq context: a command the
/// engine rejects fails the record-time capture as `oracle capture: op … seq …` (base restored),
/// and the same command in a crafted v3 bundle fails the replay verify pass as
/// `oracle verify: op … seq …` — both name where the failure happened, not just what.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running FalkorDB server"]
async fn oracle_paths_name_the_op_and_seq_on_engine_failures() {
    use benchmark::synthetic::oracle;
    use benchmark::synthetic::recording::RecordedOp;
    use benchmark::synthetic::writes::MutationStats;
    use benchmark::synthetic::OpKey;
    use std::collections::BTreeMap;

    let graph = "syn_it_oracle_enginefail";
    drop_graph(graph).await;
    let spec = DatasetSpec {
        seed: 7,
        nodes: 60,
        edges: 180,
    };
    let broken_op = || RecordedOp {
        key: OpKey::dynamic("single_vertex_write", QueryType::Write),
        result_gated: false,
        budget: Default::default(),
        capability: None,
        commands: vec!["THIS IS NOT CYPHER".to_string()],
    };

    // Record-time: capture must fail with the op/seq context and leave the base restored.
    let dir = temp_bundle_dir("syn-it-oracle-capfail");
    recording::record_rendered(&spec, graph, &[broken_op()], 7, 1_000, &dir).expect("record v2");
    let err = oracle::capture(&endpoint(), &dir, 5_000, 6_000)
        .await
        .expect_err("capturing an engine-rejected command must fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("oracle capture: op 'single_vertex_write' seq 0"),
        "capture errors carry op/seq context: {msg}"
    );
    let mut g = open_graph(&endpoint(), graph).await.expect("open restored graph");
    assert_eq!(scalar_i64(&mut g, "MATCH (n) RETURN count(n)").await, 60);
    drop(g);
    std::fs::remove_dir_all(&dir).ok();

    // Replay-time: a crafted v3 bundle whose SECOND command the engine rejects fails the verify
    // pass with the mirrored context — the first command is valid (and its outcome correct), so
    // the fail-fast probe and verify seq 0 both pass, isolating the engine error at seq 1.
    let dir = temp_bundle_dir("syn-it-oracle-verifyfail");
    let two_cmd_op = RecordedOp {
        key: OpKey::dynamic("single_vertex_write", QueryType::Write),
        result_gated: false,
        budget: Default::default(),
        capability: None,
        commands: vec!["CREATE ()".to_string(), "THIS IS NOT CYPHER".to_string()],
    };
    recording::record_rendered(&spec, graph, &[two_cmd_op], 7, 1_000, &dir).expect("record v2");
    let mut crafted = BTreeMap::new();
    crafted.insert(
        "single_vertex_write".to_string(),
        vec![
            MutationStats {
                nodes_created: 1,
                ..MutationStats::default()
            },
            MutationStats::default(),
        ],
    );
    recording::attach_oracle(&dir, &crafted).expect("attach a well-formed oracle");
    let out = dir.join("enginefail.json").to_string_lossy().into_owned();
    let mut config = replay_config(&dir, graph, &out, true);
    config.samples = 2;
    config.warmup = 0;
    config.cache = benchmark::synthetic::CacheSelection::Cached;
    let err = replay::run(&config).await.expect_err("verify pass must surface the engine error");
    let msg = format!("{err}");
    assert!(
        msg.contains("oracle verify: op 'single_vertex_write' seq 1"),
        "verify errors carry op/seq context: {msg}"
    );

    std::fs::remove_dir_all(&dir).ok();
    drop_graph(graph).await;
}

/// Phase 7 §6.3 — capture determinism end to end: two independent record+capture flows over the
/// same seed produce byte-identical v3 bundles (same `workload_hash`, engine outcomes included),
/// and a completed capture leaves the endpoint's graph content-identical to a fresh restore of
/// the recorded base (§3.5 at record time).
///
/// The bundle is a hand-rendered two-op recording carrying eligible shape names (the exact-set
/// rule pins v3 oracles to eligible names): capture semantics — double pass, full corpus,
/// content-verified final restore — are identical to the full repo-writes flow (covered by the
/// end-to-end test above) while the small corpus keeps the double capture fast.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running FalkorDB server"]
async fn oracle_capture_is_deterministic_and_leaves_the_base_restored() {
    use benchmark::synthetic::oracle;
    use benchmark::synthetic::recording::RecordedOp;
    use benchmark::synthetic::replay::capture_graph_content;
    use benchmark::synthetic::OpKey;

    let graph = "syn_it_oracle_det";
    drop_graph(graph).await;

    let record_one = |prefix: &str| {
        let dir = temp_bundle_dir(prefix);
        let spec = DatasetSpec {
            seed: 7,
            nodes: 60,
            edges: 180,
        };
        let op = |name: &str, commands: Vec<String>| RecordedOp {
            key: OpKey::dynamic(name, QueryType::Write),
            result_gated: false,
            budget: Default::default(),
            capability: None,
            commands,
        };
        let ops = [
            op(
                "single_vertex_write",
                (0..3).map(|i| format!("CREATE (:User {{id: {}}})", 999_900 + i)).collect(),
            ),
            op(
                "single_vertex_update",
                (0..2).map(|i| format!("MATCH (u:User {{id: {i}}}) SET u.probe = {i}")).collect(),
            ),
        ];
        recording::record_rendered(&spec, graph, &ops, 7, 1_000, &dir).expect("record v2");
        dir
    };

    let dir_a = record_one("syn-it-oracle-det-a");
    let manifest_a = oracle::capture(&endpoint(), &dir_a, 5_000, 6_000)
        .await
        .expect("capture oracle for bundle A");
    for entry in &manifest_a.ops {
        assert_eq!(
            entry.oracle,
            Some(entry.count),
            "{}: capture covers the complete corpus",
            entry.name
        );
    }

    // Post-capture, BEFORE any further restore: the graph must already be the pristine base.
    let bundle_a = recording::load(&dir_a).expect("reload bundle A");
    let cfg = replay_config(&dir_a, graph, "unused.json", true);
    let mut g = open_graph(&endpoint(), graph).await.expect("open post-capture graph");
    let post_capture = capture_graph_content(&mut g, &cfg).await.expect("digest post-capture");
    replay::restore_base(&cfg, &bundle_a, graph, &bundle_a.spec())
        .await
        .expect("explicit fresh restore");
    let pristine = capture_graph_content(&mut g, &cfg).await.expect("digest fresh restore");
    assert_eq!(
        post_capture, pristine,
        "capture must leave the base content-identical to a fresh restore"
    );
    drop(g);

    let dir_b = record_one("syn-it-oracle-det-b");
    let manifest_b = oracle::capture(&endpoint(), &dir_b, 5_000, 6_000)
        .await
        .expect("capture oracle for bundle B");
    assert_eq!(
        manifest_a.workload_hash, manifest_b.workload_hash,
        "same seed + same engine ⇒ identical v3 bundles, oracle outcomes included"
    );

    std::fs::remove_dir_all(&dir_a).ok();
    std::fs::remove_dir_all(&dir_b).ok();
    drop_graph(graph).await;
}

/// Phase 7 §6.3 — re-recording over an existing v3 bundle must work, with and without `--oracle`
/// (the duck repro: a stale `oracle/` directory used to survive the plain re-record and brick
/// every subsequent load, and the `--oracle` retry loaded the stale bundle before the repair
/// could run). Exercises the real CLI `record` path end to end, twice over the same directory.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a running FalkorDB server"]
async fn re_recording_over_a_v3_bundle_succeeds_with_and_without_oracle() {
    use benchmark::cli::SyntheticCommands;
    use benchmark::synthetic::run_command;

    let graph = "syn_it_oracle_rerecord";
    drop_graph(graph).await;
    let dir = temp_bundle_dir("syn-it-oracle-rerec");
    let out_dir = dir.to_string_lossy().into_owned();
    let record = |oracle: bool| SyntheticCommands::Record {
        config: None,
        graph: Some(graph.to_string()),
        ops: vec![],
        all_reads: false,
        tier: None,
        repo_reads: None,
        repo_algorithms: false,
        repo_writes: true,
        seed: Some(7),
        nodes: Some(60),
        edges: Some(180),
        oracle: oracle.then(endpoint),
        out_dir: out_dir.clone(),
    };

    run_command(record(true)).await.expect("initial record --oracle");
    let v3_hash = recording::load(&dir).expect("v3 loads").manifest.workload_hash;

    // Re-record WITH --oracle over the v3 bundle: the capture-side self-heal must let the retry
    // through (capture loads the bundle BEFORE attach's own heal), reproducing the identical v3.
    run_command(record(true)).await.expect("re-record --oracle over v3");
    let again = recording::load(&dir).expect("the re-captured v3 bundle loads");
    assert_eq!(again.manifest.format_version, 3);
    assert_eq!(again.manifest.workload_hash, v3_hash, "same seed + engine ⇒ same v3 bundle");

    // Re-record WITHOUT --oracle over the v3 bundle: the stale oracle/ must be cleared and the
    // resulting v2 bundle must load cleanly.
    run_command(record(false)).await.expect("re-record without --oracle over v3");
    assert!(!dir.join("oracle").exists(), "stale oracle/ cleared by the plain re-record");
    let v2 = recording::load(&dir).expect("the re-recorded v2 bundle loads");
    assert_eq!(v2.manifest.format_version, 2);

    std::fs::remove_dir_all(&dir).ok();
    drop_graph(graph).await;
}
