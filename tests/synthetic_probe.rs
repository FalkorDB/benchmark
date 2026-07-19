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
        out: "synthetic-report.json".to_string(),
        server_image: None,
    };

    let report = run(&config).await.expect("probe run should succeed");

    let op = report
        .operations
        .get("return_const")
        .expect("report should contain the measured op");

    // Every sample is accounted for (retained + severe-outliers removed).
    assert_eq!(
        op.server_ms.n + op.server_ms.removed,
        samples,
        "server_ms samples should sum to the requested count"
    );
    assert_eq!(op.total_ms.n + op.total_ms.removed, samples);

    // The server reported a positive internal execution time, and the round-trip is at least as
    // long as the server time (the residual can't be negative in aggregate).
    assert!(op.server_ms.median > 0.0, "server_ms should be positive");
    assert!(op.total_ms.median > 0.0, "total_ms should be positive");
    assert!(
        op.total_ms.median >= op.server_ms.median,
        "total time should be >= server time"
    );

    // Provenance was captured from the live server.
    assert!(
        report.meta.server.redis_version.is_some(),
        "redis_version should be read from INFO server"
    );
}
