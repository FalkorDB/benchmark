//! Record-side §6.3 **outcome-oracle capture**: run each oracle-eligible write command against the
//! recorded **pristine base** on a live engine, capture the [`MutationStats`] it effects, prove the
//! outcomes are deterministic with a second independent pass, and fold them into the bundle
//! ([`recording::attach_oracle`], format v3).
//!
//! This is the one deliberate departure from the offline read recorder (design §3.2 / risk §7.2):
//! write counters are state/value/order-dependent — MERGE create-vs-match, SET-same-value counting
//! 0, an upsert *removing* a property — so no static model can predict them. The oracle instead
//! records what each command **actually did** from the pristine base, and replay re-verifies that
//! exact outcome per invocation (per-invocation restore, C=1). Capture never times anything: it is
//! a correctness pass, run once at record time.
//!
//! Restore discipline mirrors replay's §3.5: every command runs from a freshly restored base
//! ([`replay::restore_base`]), and a final restore runs on success **and** failure — a dual
//! failure surfaces both errors, never just the first.

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::queries_repository::QueryType;
use crate::synthetic::op_runner::run_and_drain;
use crate::synthetic::recording::{self, Bundle};
use crate::synthetic::replay::{restore_base, ReplayConfig};
use crate::synthetic::shapes::{write_shapes, OracleEligibility};
use crate::synthetic::writes::MutationStats;
use crate::synthetic::{open_graph, CacheSelection};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;
use tracing::info;

/// Default per-op oracle sample count (`--oracle-samples`): how many leading commands of each
/// eligible op are captured and later re-verified. 8 balances coverage of the corpus's parameter
/// variety against capture/verify cost (each sample is a full base restore + one command, twice at
/// record time and once per replay).
pub const DEFAULT_ORACLE_SAMPLES: usize = 8;

/// Capture the §6.3 outcome oracle for the (already-recorded) write bundle in `dir` against the
/// live engine at `endpoint`, and fold it into the bundle (upgrading it to format v3).
///
/// For every **oracle-eligible** write op (the deterministic subset —
/// [`ShapeSpec::oracle`](crate::synthetic::shapes::ShapeSpec::oracle)), the first
/// `samples_per_op` commands (capped at the corpus size) each run **once from a freshly restored
/// pristine base**, recording the [`MutationStats`] the engine reports. A second full pass then
/// repeats the capture; any difference is a hard error naming the op/seq — determinism is proven
/// at record time, not assumed (design §3.2: the recorded outcome is only an oracle if it is
/// reproducible). Ineligible write ops stay latency-only.
///
/// The bundle's graph on `endpoint` is left restored to the pristine base on success **and**
/// failure (§3.5 discipline); a dual failure reports both errors.
pub async fn capture(
    endpoint: &str,
    dir: &Path,
    samples_per_op: usize,
    server_timeout_ms: i64,
    client_deadline_ms: u64,
) -> BenchmarkResult<recording::Manifest> {
    if samples_per_op == 0 {
        return Err(OtherError("--oracle-samples must be greater than 0".to_string()));
    }
    let bundle = recording::load(dir)?;
    if bundle.manifest.format_version >= recording::RECORDING_FORMAT_VERSION_ORACLE {
        return Err(OtherError(format!(
            "{} already carries an outcome oracle (format_version {}) — re-record instead of \
             re-capturing",
            dir.display(),
            bundle.manifest.format_version
        )));
    }
    // The §6.3 eligibility table (single source of truth: the annotated write-shape table).
    let eligible: BTreeMap<&str, ()> = write_shapes()
        .iter()
        .filter(|s| s.oracle == OracleEligibility::Eligible)
        .map(|s| (s.name, ()))
        .collect();
    let targets: Vec<(String, Vec<String>)> = bundle
        .commands
        .iter()
        .filter(|(op, _)| op.kind() == QueryType::Write && eligible.contains_key(op.name()))
        .map(|(op, cyphers)| (op.name().to_string(), cyphers.clone()))
        .collect();
    if targets.is_empty() {
        return Err(OtherError(
            "--oracle: the bundle records no oracle-eligible write op (the §6.3 deterministic \
             subset) — nothing to capture"
                .to_string(),
        ));
    }

    // A minimal replay-shaped config: `restore_base` needs only the endpoint + timeouts.
    let config = ReplayConfig {
        recording_dir: dir.to_path_buf(),
        endpoint: endpoint.to_string(),
        graph: None,
        load: true,
        samples: 1,
        warmup: 0,
        concurrency: vec![1],
        cache: CacheSelection::Cached,
        server_timeout_ms,
        client_deadline_ms,
        out: String::new(),
        server_image: None,
        label: None,
    };
    let graph_name = bundle.manifest.graph.clone();
    let dataset_spec = bundle.spec();

    let captured: BenchmarkResult<BTreeMap<String, Vec<MutationStats>>> = async {
        let first =
            capture_pass(&config, &bundle, &graph_name, &targets, samples_per_op).await?;
        let second =
            capture_pass(&config, &bundle, &graph_name, &targets, samples_per_op).await?;
        for (name, first_outcomes) in &first {
            let second_outcomes = &second[name];
            for (seq, (a, b)) in first_outcomes.iter().zip(second_outcomes).enumerate() {
                if a != b {
                    return Err(OtherError(format!(
                        "oracle capture is not deterministic for op '{}' seq {}: pass 1 reported \
                         {:?}, pass 2 reported {:?} — the op cannot be oracle-verified on this \
                         engine (both passes ran from the same restored pristine base)",
                        name, seq, a, b
                    )));
                }
            }
        }
        Ok(first)
    }
    .await;
    // §3.5 at record time: leave the endpoint's graph restored to the pristine base whether the
    // capture succeeded or not, and surface BOTH errors on a dual failure.
    let restored = restore_base(&config, &bundle, &graph_name, &dataset_spec).await;
    let captured = reconcile_capture_and_restore(captured, restored, &graph_name)?;

    let ops = captured.len();
    let outcomes: usize = captured.values().map(Vec::len).sum();
    let manifest = recording::attach_oracle(dir, &captured)?;
    info!(
        "oracle captured: {} outcome(s) across {} op(s), determinism proven by a second pass",
        outcomes, ops
    );
    Ok(manifest)
}

/// One full capture pass: for each target op, restore the pristine base, run command `seq` once,
/// and record the [`MutationStats`] the engine reports — for `seq` in `0..min(k, corpus)`.
async fn capture_pass(
    config: &ReplayConfig,
    bundle: &Bundle,
    graph_name: &str,
    targets: &[(String, Vec<String>)],
    samples_per_op: usize,
) -> BenchmarkResult<BTreeMap<String, Vec<MutationStats>>> {
    let client_deadline = Duration::from_millis(config.client_deadline_ms);
    let mut out = BTreeMap::new();
    for (name, cyphers) in targets {
        let n = samples_per_op.min(cyphers.len());
        let mut outcomes = Vec::with_capacity(n);
        for (seq, cypher) in cyphers.iter().take(n).enumerate() {
            restore_base(config, bundle, graph_name, &bundle.spec()).await?;
            let mut graph = open_graph(&config.endpoint, graph_name).await?;
            let sample = run_and_drain(
                &mut graph,
                QueryType::Write,
                cypher,
                config.server_timeout_ms,
                client_deadline,
            )
            .await
            .map_err(|e| {
                OtherError(format!("oracle capture: op '{}' seq {}: {}", name, seq, e))
            })?;
            outcomes.push(sample.mutations);
        }
        out.insert(name.clone(), outcomes);
    }
    Ok(out)
}

/// Combine the capture result with the final-restore result (§3.5): a restore failure must never
/// be shadowed by the capture error alone — a dual failure reports both.
fn reconcile_capture_and_restore(
    captured: BenchmarkResult<BTreeMap<String, Vec<MutationStats>>>,
    restored: BenchmarkResult<()>,
    graph_name: &str,
) -> BenchmarkResult<BTreeMap<String, Vec<MutationStats>>> {
    match (captured, restored) {
        (Ok(v), Ok(())) => Ok(v),
        (Ok(_), Err(restore)) => Err(restore),
        (Err(original), Ok(())) => Err(original),
        (Err(original), Err(restore)) => Err(OtherError(format!(
            "oracle capture failed AND its final restore failed — graph '{}' may be left polluted \
             (capture error: {}; restore error: {})",
            graph_name, original, restore
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::dataset::DatasetSpec;
    use crate::synthetic::recording::{record_rendered, temp_bundle_dir, RecordedOp};
    use crate::synthetic::OpKey;

    fn record_write_bundle(
        prefix: &str,
        op_name: &str,
    ) -> std::path::PathBuf {
        let dir = temp_bundle_dir(prefix);
        let spec = DatasetSpec {
            seed: 1,
            nodes: 10,
            edges: 20,
        };
        let ops = vec![RecordedOp {
            key: OpKey::dynamic(op_name, crate::queries_repository::QueryType::Write),
            result_gated: false,
            budget: Default::default(),
            capability: None,
            commands: vec!["CYPHER x=1 CREATE (n:User {id:$x})".to_string()],
        }];
        record_rendered(&spec, "g", &ops, 1, 8, &dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn capture_rejects_zero_samples_offline() {
        let err = capture("falkor://127.0.0.1:1", Path::new("/nonexistent"), 0, 5_000, 6_000)
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("--oracle-samples"), "got: {err}");
    }

    #[tokio::test]
    async fn capture_rejects_a_bundle_with_no_eligible_op_offline() {
        // A write bundle whose only op is not in the §6.3 deterministic subset has nothing to
        // capture — fail offline, before any connection (the endpoint is unroutable).
        let dir = record_write_bundle("synthorc-inel", "w_custom");
        let err = capture("falkor://127.0.0.1:1", &dir, 2, 5_000, 6_000).await.unwrap_err();
        assert!(format!("{err}").contains("no oracle-eligible write op"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_failed_capture_leaves_the_bundle_valid_and_pre_oracle() {
        // An eligible op but an unreachable endpoint: the capture fails at connect time, AFTER
        // the offline validation — and must leave the recorded v2 bundle exactly as it was
        // (attach only runs on a successful, determinism-proven capture).
        let dir = record_write_bundle("synthorc-conn", "single_vertex_write");
        let before = recording::load(&dir).unwrap().manifest;
        let err = capture("falkor://127.0.0.1:1", &dir, 1, 1_000, 1_500).await.unwrap_err();
        // Both the capture and the §3.5 final restore hit the same unreachable endpoint — the
        // combined error must surface the pollution risk rather than hide the restore failure.
        assert!(format!("{err}").contains("restore failed"), "got: {err}");
        let after = recording::load(&dir).unwrap().manifest;
        assert_eq!(after, before, "a failed capture must not touch the bundle");
        assert_eq!(after.format_version, recording::RECORDING_FORMAT_VERSION_WRITES);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn capture_rejects_a_bundle_that_already_carries_an_oracle_offline() {
        let dir = record_write_bundle("synthorc-v3", "single_vertex_write");
        let mut oracle = BTreeMap::new();
        oracle.insert("single_vertex_write".to_string(), vec![MutationStats::default()]);
        recording::attach_oracle(&dir, &oracle).unwrap();
        let err = capture("falkor://127.0.0.1:1", &dir, 1, 5_000, 6_000).await.unwrap_err();
        assert!(format!("{err}").contains("already carries"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reconcile_capture_and_restore_combines_a_dual_failure() {
        // §3.5 at record time: when the capture AND the final restore both fail, the caller must
        // see both — the restore failure means the endpoint's graph may be left polluted, which
        // the capture error alone would hide.
        let err = reconcile_capture_and_restore(
            Err(OtherError("capture exploded".to_string())),
            Err(OtherError("restore diverged".to_string())),
            "g9",
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("may be left polluted"), "got: {msg}");
        assert!(msg.contains("g9"), "must name the graph: {msg}");
        assert!(msg.contains("capture exploded"), "must carry the capture error: {msg}");
        assert!(msg.contains("restore diverged"), "must carry the restore error: {msg}");
    }

    #[test]
    fn reconcile_capture_and_restore_passes_through_single_outcomes() {
        let ok: BenchmarkResult<BTreeMap<String, Vec<MutationStats>>> = Ok(BTreeMap::new());
        assert!(reconcile_capture_and_restore(ok, Ok(()), "g").is_ok());
        let restore_only = reconcile_capture_and_restore(
            Ok(BTreeMap::new()),
            Err(OtherError("restore failed".to_string())),
            "g",
        )
        .unwrap_err();
        assert!(format!("{restore_only}").contains("restore failed"));
        let capture_only = reconcile_capture_and_restore(
            Err(OtherError("capture failed".to_string())),
            Ok(()),
            "g",
        )
        .unwrap_err();
        assert!(format!("{capture_only}").contains("capture failed"));
    }
}
