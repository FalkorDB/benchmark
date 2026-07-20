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
        seed: 1,
        server_timeout_ms: 5_000,
        client_deadline_ms: 6_000,
        cache: CacheSelection::Both,
        out: "synthetic-report.json".to_string(),
        server_image: None,
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
    assert!(ds.corpus_hash.starts_with("sha256:"));

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
        a.meta.dataset.unwrap().corpus_hash,
        b.meta.dataset.unwrap().corpus_hash
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
