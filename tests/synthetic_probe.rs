//! Integration tests for the synthetic single-operation probe.
//!
//! These require a reachable FalkorDB (set `FALKORDB_HOST`/`FALKORDB_PORT` or default to
//! `127.0.0.1:6379`). They are `#[ignore]`d so a plain `cargo test` stays hermetic; run them with
//! a live server via `just synthetic-it`, and the coverage job runs them with `--include-ignored`
//! against a FalkorDB service.

use benchmark::queries_repository::QueryType;
use benchmark::synthetic::op_runner::run_and_drain;
use benchmark::synthetic::{list_ops, open_graph, run, run_and_report, CacheSelection, Config, OpName};
use std::time::Duration;

fn endpoint() -> String {
    let host = std::env::var("FALKORDB_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("FALKORDB_PORT").unwrap_or_else(|_| "6379".to_string());
    format!("falkor://{host}:{port}")
}

fn base_config() -> Config {
    Config {
        endpoint: endpoint(),
        op: OpName::ReturnConst,
        samples: 300,
        warmup: 50,
        server_timeout_ms: 5_000,
        client_deadline_ms: 6_000,
        cache: CacheSelection::Both,
        out: "synthetic-report.json".to_string(),
        server_image: None,
    }
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn probe_produces_valid_report() {
    let config = base_config();
    let samples = config.samples;

    let report = run(&config).await.expect("probe run should succeed");

    let op = report
        .operations
        .get("return_const")
        .expect("report should contain the measured op");

    // Both cache modes were measured.
    let cached = op.cached.as_ref().expect("cached metrics present");
    let uncached = op.uncached.as_ref().expect("uncached metrics present");

    // Every sample is accounted for (retained + severe-outliers removed) in each mode.
    assert_eq!(cached.server_ms.n + cached.server_ms.removed, samples);
    assert_eq!(uncached.server_ms.n + uncached.server_ms.removed, samples);

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

    // A compilation cost was derived from both modes.
    assert!(op.compilation_ms_median.is_some());

    // Provenance was captured from the live server.
    assert!(
        report.meta.server.redis_version.is_some(),
        "redis_version should be read from INFO server"
    );
    assert!(
        report.meta.server.cache_size.is_some(),
        "CACHE_SIZE should be read via GRAPH.CONFIG"
    );
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn run_and_report_writes_json_file() {
    let dir = std::env::temp_dir();
    let out = dir
        .join(format!("synthetic-report-it-{}.json", std::process::id()))
        .to_string_lossy()
        .into_owned();
    let config = Config {
        samples: 120,
        warmup: 20,
        out: out.clone(),
        ..base_config()
    };

    run_and_report(&config)
        .await
        .expect("run_and_report should succeed");

    let written = std::fs::read_to_string(&out).expect("report file should exist");
    assert!(written.contains("return_const"));
    assert!(written.contains("\"cached\""));
    assert!(written.contains("\"uncached\""));
    let _ = std::fs::remove_file(&out);
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn cached_only_and_uncached_only_modes() {
    // Cached-only: no uncached block.
    let cached_report = run(&Config {
        samples: 100,
        warmup: 20,
        cache: CacheSelection::Cached,
        ..base_config()
    })
    .await
    .expect("cached-only run should succeed");
    let cop = cached_report.operations.get("return_const").unwrap();
    assert!(cop.cached.is_some());
    assert!(cop.uncached.is_none());
    assert!(cop.compilation_ms_median.is_none());

    // Uncached-only: no cached block, and it misses the plan cache.
    let uncached_report = run(&Config {
        samples: 100,
        warmup: 20,
        cache: CacheSelection::Uncached,
        ..base_config()
    })
    .await
    .expect("uncached-only run should succeed");
    let uop = uncached_report.operations.get("return_const").unwrap();
    assert!(uop.cached.is_none());
    let uncached = uop.uncached.as_ref().unwrap();
    assert!(uncached.cached_false_rate > 0.5);
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
        ..base_config()
    };
    assert!(run(&config).await.is_err());
}

#[test]
fn list_ops_is_non_empty() {
    // Pure helper — no server needed; keeps the smoke path covered even without `--ignored`.
    assert!(list_ops().contains("return_const"));
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
    let read = run_and_drain(&mut graph, QueryType::Read, "RETURN 1 AS x", 5_000, Duration::from_secs(5))
        .await
        .expect("read op should succeed");
    assert_eq!(read.rows, 1);
    assert!(read.server_ms.is_finite() && read.server_ms >= 0.0);
    assert!(read.total_ms >= read.server_ms);

    // A syntactically invalid query surfaces as an error rather than a panic.
    assert!(
        run_and_drain(&mut graph, QueryType::Read, "THIS IS NOT CYPHER", 5_000, Duration::from_secs(5))
            .await
            .is_err()
    );

    // An impossibly small client deadline trips the whole-operation timeout guard.
    assert!(
        run_and_drain(&mut graph, QueryType::Read, "RETURN 1", 5_000, Duration::from_nanos(1))
            .await
            .is_err()
    );

    // Tidy up the scratch graph.
    let _ = graph.delete().await;
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn probe_instantiates_a_missing_graph() {
    // Delete the probe's graph first so `run()` exercises the empty-key instantiation path.
    if let Ok(mut g) = open_graph(&endpoint(), "falkor").await {
        let _ = g.delete().await;
    }
    let report = run(&Config {
        samples: 60,
        warmup: 10,
        cache: CacheSelection::Cached,
        ..base_config()
    })
    .await
    .expect("probe should instantiate the missing graph and succeed");
    assert!(report.operations.contains_key("return_const"));
}
