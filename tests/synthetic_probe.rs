//! Integration test for the synthetic single-operation probe.
//!
//! Requires a reachable FalkorDB (set `FALKORDB_HOST`/`FALKORDB_PORT` or default to
//! `127.0.0.1:6379`). It is `#[ignore]`d so `cargo test` stays hermetic; run it with a live
//! server via `just synthetic-it` (or `cargo test --test synthetic_probe -- --ignored`).

use benchmark::synthetic::{run, Config, OpName};

fn endpoint() -> String {
    let host = std::env::var("FALKORDB_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("FALKORDB_PORT").unwrap_or_else(|_| "6379".to_string());
    format!("falkor://{host}:{port}")
}

#[tokio::test]
#[ignore = "requires a running FalkorDB server"]
async fn probe_produces_valid_report() {
    let samples = 300usize;
    let config = Config {
        endpoint: endpoint(),
        op: OpName::ReturnConst,
        samples,
        warmup: 50,
        server_timeout_ms: 5_000,
        client_deadline_ms: 6_000,
        cache: benchmark::synthetic::CacheSelection::Both,
        out: "synthetic-report.json".to_string(),
        server_image: None,
    };

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

    // Provenance was captured from the live server.
    assert!(
        report.meta.server.redis_version.is_some(),
        "redis_version should be read from INFO server"
    );
}
