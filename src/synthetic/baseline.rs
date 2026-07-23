//! Version-comparison baselines: the guard that `synthetic-compare` runs before invoking Criterion,
//! refusing to compare two runs whose **workload** differs.
//!
//! FalkorDB's **version** is the *subject* of a version comparison (a baseline captured on v4.2.1
//! vs a candidate on v4.3.0), so a version change is *recorded and displayed*, never rejected.
//! The **workload** — identified by [`workload_hash`](crate::synthetic::report::DatasetInfo) — is the
//! hard gate: a different (or absent) hash means the two runs measured different things and the
//! latency comparison would be meaningless. Keeping this logic in the library (rather than the
//! Criterion bench harness) makes it unit-testable.

use crate::synthetic::report::{Report, ServerInfo};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The workload + environment identity of a run (extracted from its [`Report`]) that a
/// version-comparison must agree on — or knowingly differ on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineKey {
    /// Workload fingerprint (dataset knobs + ops in order + query bodies + sampled pools). `None`
    /// for an external graph that couldn't be fingerprinted — such a run can't be safely compared.
    pub workload_hash: Option<String>,
    /// FalkorDB graph-module version (recorded for display; expected to differ across versions).
    pub module_graph_ver: Option<u64>,
    /// Operator-supplied server image identity, when provided.
    pub server_image: Option<String>,
    /// Per-op result-value digests (present for `synthetic run --recording` runs). Compared op-by-op:
    /// two versions must agree, or a wrong/empty-but-faster result could look like a win.
    #[serde(default)]
    pub result_digests: BTreeMap<String, String>,
}

impl BaselineKey {
    /// Extract the comparison key from a run's report.
    pub fn from_report(report: &Report) -> Self {
        BaselineKey {
            workload_hash: report.meta.dataset.as_ref().map(|d| d.workload_hash.clone()),
            module_graph_ver: report.meta.server.module_graph_ver,
            server_image: report.meta.server.server_image.clone(),
            result_digests: report
                .operations
                .iter()
                .filter_map(|(name, op)| op.result_digest.clone().map(|d| (name.clone(), d)))
                .collect(),
        }
    }
}

/// The result of guarding a candidate run against a saved baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardOutcome {
    /// Safe to compare. `warnings` are advisory (e.g. identical or placeholder versions).
    Proceed { warnings: Vec<String> },
    /// Must **not** compare (workload mismatch or unfingerprintable workload).
    Abort { reason: String },
}

/// Guard a `candidate` run against a saved `baseline` before comparing their latencies.
///
/// The **workload** (`workload_hash`) is a hard gate — a different or absent hash means the runs
/// measured different things, so we abort. The FalkorDB **version** is only advisory: comparing
/// across versions is the whole point, so a version change is recorded (as a `Proceed` with no
/// warning); identical or placeholder versions produce advisory warnings.
pub fn guard(
    baseline: &BaselineKey,
    candidate: &BaselineKey,
) -> GuardOutcome {
    match (&baseline.workload_hash, &candidate.workload_hash) {
        (Some(a), Some(b)) if a == b => {}
        (Some(a), Some(b)) => {
            return GuardOutcome::Abort {
                reason: format!(
                    "workload_hash mismatch — the workload changed since the baseline was saved \
                     (baseline {a}, candidate {b}); re-save the baseline for the current workload"
                ),
            };
        }
        _ => {
            return GuardOutcome::Abort {
                reason: "missing workload_hash — a comparable baseline needs a generated dataset \
                         (`--generate`) so the workload can be fingerprinted"
                    .to_string(),
            };
        }
    }

    // Result-correctness gate: for every op the baseline recorded a result digest for, the
    // candidate must record the *same* digest — otherwise a version returning wrong or empty
    // results faster could masquerade as an improvement. A candidate that is missing a digest the
    // baseline has is also a mismatch (fail closed, matching the docs' "every op" guarantee).
    // Digests are present for `synthetic run --recording` runs; a `synthetic run` baseline has none, so the
    // loop is a no-op there (and such runs already differ on `workload_hash` above).
    for (op, base_dig) in &baseline.result_digests {
        match candidate.result_digests.get(op) {
            Some(cand_dig) if cand_dig == base_dig => {}
            Some(cand_dig) => {
                return GuardOutcome::Abort {
                    reason: format!(
                        "result mismatch for op '{op}' — baseline and candidate returned different \
                         result cardinalities (baseline {base_dig}, candidate {cand_dig}), so their \
                         latencies are not comparable"
                    ),
                };
            }
            None => {
                return GuardOutcome::Abort {
                    reason: format!(
                        "candidate is missing a result digest for op '{op}' that the baseline \
                         recorded — the runs aren't comparable (re-run the candidate with \
                         `synthetic run --recording`)"
                    ),
                };
            }
        }
    }

    let mut warnings = Vec::new();
    // Only warn "same version" when both versions are actually *known* and equal — two unknown
    // (`None`) versions are not a known match, so don't claim there's no delta to measure.
    if baseline.module_graph_ver.is_some() && baseline.module_graph_ver == candidate.module_graph_ver
    {
        warnings.push(format!(
            "baseline and candidate ran the same FalkorDB module version ({}) — there is no \
             version delta to measure",
            ver_str(candidate.module_graph_ver)
        ));
    }
    if baseline.module_graph_ver == Some(ServerInfo::PLACEHOLDER_VER)
        || candidate.module_graph_ver == Some(ServerInfo::PLACEHOLDER_VER)
    {
        warnings.push(
            "a FalkorDB module version is the dev placeholder — use tagged release images for a \
             meaningful version comparison"
                .to_string(),
        );
    }
    if let (Some(a), Some(b)) = (&baseline.server_image, &candidate.server_image) {
        if a != b {
            warnings.push(format!("server image changed: {a} → {b}"));
        }
    }
    GuardOutcome::Proceed { warnings }
}

/// Human-readable FalkorDB module version (`"4.20.1"`), or `"unknown"` when absent.
fn ver_str(v: Option<u64>) -> String {
    v.map(crate::synthetic::provenance::decode_module_version)
        .unwrap_or_else(|| "unknown".to_string())
}

/// The result of the non-fatal comparability check used by `report --regression`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegressionGuard {
    /// The runs measured the same workload/configuration and can be compared. `diverged_ops` are
    /// operations whose result digests differ (the caller renders those N/A, not a latency
    /// verdict); `warnings` are advisory (version/image).
    Comparable {
        diverged_ops: BTreeSet<String>,
        warnings: Vec<String>,
    },
    /// The runs are not comparable at all (different workload or run configuration); the whole
    /// report should be rendered as "not comparable".
    NotComparable { reason: String },
}

/// Comparability guard for the **non-fatal** `report --regression` mode.
///
/// Unlike [`guard`], a per-op result-digest mismatch does **not** abort the whole comparison — it's
/// reported per-op via `diverged_ops` (the caller shows those ops N/A). Only a *comparability*
/// mismatch in the behaviour-affecting inputs (workload_hash, samples/warmup, concurrency sweep)
/// makes the whole pair [`RegressionGuard::NotComparable`].
pub fn regression_guard(
    baseline: &Report,
    candidate: &Report,
) -> RegressionGuard {
    // 1. Comparability manifest: the inputs that must match for a latency comparison to be valid.
    let bh = baseline.meta.dataset.as_ref().map(|d| d.workload_hash.as_str());
    let ch = candidate.meta.dataset.as_ref().map(|d| d.workload_hash.as_str());
    match (bh, ch) {
        (Some(a), Some(b)) if a == b => {}
        (Some(_), Some(_)) => {
            return RegressionGuard::NotComparable {
                reason: "workload_hash differs — the two runs measured different workloads"
                    .to_string(),
            }
        }
        _ => {
            return RegressionGuard::NotComparable {
                reason: "missing workload_hash — both runs must have a fingerprinted workload \
                         (a `--recording` or `--generate` run); an externally-loaded graph can't \
                         be compared"
                    .to_string(),
            }
        }
    }
    if baseline.meta.samples != candidate.meta.samples
        || baseline.meta.warmup != candidate.meta.warmup
    {
        return RegressionGuard::NotComparable {
            reason: format!(
                "sampling differs — baseline {}/{} vs candidate {}/{} (samples/warmup)",
                baseline.meta.samples,
                baseline.meta.warmup,
                candidate.meta.samples,
                candidate.meta.warmup
            ),
        };
    }
    let bc = sorted_levels(&baseline.meta.concurrency);
    let cc = sorted_levels(&candidate.meta.concurrency);
    if bc != cc {
        return RegressionGuard::NotComparable {
            reason: format!("concurrency sweep differs — baseline {bc:?} vs candidate {cc:?}"),
        };
    }
    // Server settings that affect sustained throughput (recorded when readable). Only a *known*
    // difference is disqualifying — an unread setting (None) can't be compared, so we don't block.
    if let (Some(bq), Some(cq)) = (
        baseline.meta.server.max_queued_queries,
        candidate.meta.server.max_queued_queries,
    ) {
        if bq != cq {
            return RegressionGuard::NotComparable {
                reason: format!("MAX_QUEUED_QUERIES differs — baseline {bq} vs candidate {cq}"),
            };
        }
    }

    // 2. Per-op result divergence — reported, never fatal. Over the *union* of ops: two present
    //    digests that differ, or an asymmetric one-side-only digest, is diverged (we can't verify
    //    correctness). Two absent digests carry no correctness info (e.g. a non-recording run) and
    //    are left comparable, matching the strict guard.
    let mut diverged_ops = BTreeSet::new();
    let all_ops: BTreeSet<&String> = baseline
        .operations
        .keys()
        .chain(candidate.operations.keys())
        .collect();
    for op in all_ops {
        let bd = baseline.operations.get(op).and_then(|o| o.result_digest.as_ref());
        let cd = candidate.operations.get(op).and_then(|o| o.result_digest.as_ref());
        let diverged = match (bd, cd) {
            (Some(a), Some(b)) => a != b,
            (None, None) => false,
            _ => true, // asymmetric: only one side recorded a digest
        };
        if diverged {
            diverged_ops.insert(op.clone());
        }
    }

    RegressionGuard::Comparable {
        diverged_ops,
        warnings: advisory_warnings(baseline, candidate),
    }
}

/// Sorted, deduped concurrency levels for the comparability comparison.
fn sorted_levels(levels: &[usize]) -> Vec<usize> {
    let mut v: Vec<usize> = levels.to_vec();
    v.sort_unstable();
    v.dedup();
    v
}

/// Advisory (non-blocking) version/image notes shared by the regression report.
fn advisory_warnings(
    baseline: &Report,
    candidate: &Report,
) -> Vec<String> {
    let mut warnings = Vec::new();
    let bv = baseline.meta.server.module_graph_ver;
    let cv = candidate.meta.server.module_graph_ver;
    if bv.is_some() && bv == cv {
        warnings.push(format!(
            "baseline and candidate ran the same FalkorDB module version ({}) — there is no \
             version delta to measure",
            ver_str(cv)
        ));
    }
    if bv == Some(ServerInfo::PLACEHOLDER_VER) || cv == Some(ServerInfo::PLACEHOLDER_VER) {
        warnings.push(
            "a FalkorDB module version is the dev placeholder — use tagged release images for a \
             meaningful version comparison"
                .to_string(),
        );
    }
    if let (Some(a), Some(b)) = (
        &baseline.meta.server.server_image,
        &candidate.meta.server.server_image,
    ) {
        if a != b {
            warnings.push(format!("server image changed: {a} → {b}"));
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(corpus: Option<&str>, ver: Option<u64>) -> BaselineKey {
        BaselineKey {
            workload_hash: corpus.map(|s| s.to_string()),
            module_graph_ver: ver,
            server_image: None,
            result_digests: BTreeMap::new(),
        }
    }

    fn key_with_digests(
        corpus: Option<&str>,
        digests: &[(&str, &str)],
    ) -> BaselineKey {
        BaselineKey {
            workload_hash: corpus.map(|s| s.to_string()),
            module_graph_ver: Some(42001),
            server_image: None,
            result_digests: digests
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn aborts_on_result_digest_mismatch() {
        // Same workload, but an op returned a different result cardinality across versions.
        let base = key_with_digests(Some("sha256:abc"), &[("expand_1_hop", "sha256:aaa")]);
        let cand = key_with_digests(Some("sha256:abc"), &[("expand_1_hop", "sha256:bbb")]);
        match guard(&base, &cand) {
            GuardOutcome::Abort { reason } => {
                assert!(reason.contains("result mismatch for op 'expand_1_hop'"), "got: {reason}");
            }
            other => panic!("expected Abort, got {other:?}"),
        }
    }

    #[test]
    fn proceeds_when_result_digests_match() {
        let base = key_with_digests(Some("sha256:abc"), &[("expand_1_hop", "sha256:aaa")]);
        let cand = key_with_digests(Some("sha256:abc"), &[("expand_1_hop", "sha256:aaa")]);
        assert!(matches!(guard(&base, &cand), GuardOutcome::Proceed { .. }));
    }

    #[test]
    fn aborts_when_candidate_missing_a_baseline_digest() {
        // The baseline recorded a digest for an op the candidate has none for → fail closed.
        let base = key_with_digests(Some("sha256:abc"), &[("expand_1_hop", "sha256:aaa")]);
        let cand = key_with_digests(Some("sha256:abc"), &[]);
        match guard(&base, &cand) {
            GuardOutcome::Abort { reason } => {
                assert!(reason.contains("missing a result digest for op 'expand_1_hop'"), "got: {reason}");
            }
            other => panic!("expected Abort, got {other:?}"),
        }
    }

    #[test]
    fn proceeds_when_workload_matches_across_versions() {
        let base = key(Some("sha256:abc"), Some(42001));
        let cand = key(Some("sha256:abc"), Some(42002)); // upgraded FalkorDB
        match guard(&base, &cand) {
            GuardOutcome::Proceed { warnings } => {
                // A version change is expected — no same-version warning.
                assert!(!warnings.iter().any(|w| w.contains("same FalkorDB module version")));
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    #[test]
    fn aborts_on_workload_hash_mismatch() {
        let base = key(Some("sha256:abc"), Some(42001));
        let cand = key(Some("sha256:def"), Some(42001));
        match guard(&base, &cand) {
            GuardOutcome::Abort { reason } => assert!(reason.contains("workload_hash mismatch")),
            other => panic!("expected Abort, got {other:?}"),
        }
    }

    #[test]
    fn aborts_when_a_workload_hash_is_absent() {
        // An external graph (no generated dataset) has no workload_hash ⇒ unsafe to compare.
        assert!(matches!(
            guard(&key(None, Some(42001)), &key(Some("sha256:abc"), Some(42001))),
            GuardOutcome::Abort { .. }
        ));
        assert!(matches!(
            guard(&key(Some("sha256:abc"), Some(42001)), &key(None, Some(42001))),
            GuardOutcome::Abort { .. }
        ));
    }

    #[test]
    fn warns_on_identical_version() {
        let k = key(Some("sha256:abc"), Some(42001));
        match guard(&k, &k) {
            GuardOutcome::Proceed { warnings } => {
                assert!(warnings.iter().any(|w| w.contains("same FalkorDB module version")));
            }
            other => panic!("expected Proceed with a warning, got {other:?}"),
        }
    }

    #[test]
    fn unknown_versions_do_not_claim_a_same_version_match() {
        // Both versions unknown (None) is not a *known* match, so we must not warn "no delta".
        let base = key(Some("sha256:abc"), None);
        let cand = key(Some("sha256:abc"), None);
        match guard(&base, &cand) {
            GuardOutcome::Proceed { warnings } => {
                assert!(!warnings
                    .iter()
                    .any(|w| w.contains("same FalkorDB module version")));
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    #[test]
    fn warns_on_placeholder_version() {
        let base = key(Some("sha256:abc"), Some(ServerInfo::PLACEHOLDER_VER));
        let cand = key(Some("sha256:abc"), Some(42002));
        match guard(&base, &cand) {
            GuardOutcome::Proceed { warnings } => {
                assert!(warnings.iter().any(|w| w.contains("dev placeholder")));
            }
            other => panic!("expected Proceed with a placeholder warning, got {other:?}"),
        }
    }

    #[test]
    fn warns_on_server_image_change() {
        let base = BaselineKey {
            workload_hash: Some("sha256:abc".to_string()),
            module_graph_ver: Some(42001),
            server_image: Some("falkordb@sha256:aaa".to_string()),
            result_digests: BTreeMap::new(),
        };
        let cand = BaselineKey {
            workload_hash: Some("sha256:abc".to_string()),
            module_graph_ver: Some(42002),
            server_image: Some("falkordb@sha256:bbb".to_string()),
            result_digests: BTreeMap::new(),
        };
        match guard(&base, &cand) {
            GuardOutcome::Proceed { warnings } => {
                assert!(warnings.iter().any(|w| w.contains("server image changed")));
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod regression_guard_tests {
    use super::*;
    use crate::synthetic::report::{
        DatasetInfo, Meta, OperationReport, Report, ServerInfo, SCHEMA_VERSION,
    };

    #[allow(clippy::too_many_arguments)]
    fn rep(
        hash: &str,
        samples: usize,
        warmup: usize,
        concurrency: Vec<usize>,
        ver: Option<u64>,
        image: Option<&str>,
        ops: &[(&str, Option<&str>)],
    ) -> Report {
        let mut operations = BTreeMap::new();
        for (name, dig) in ops {
            operations.insert(
                name.to_string(),
                OperationReport {
                    levels: vec![],
                    result_digest: dig.map(|s| s.to_string()),
                },
            );
        }
        Report {
            schema_version: SCHEMA_VERSION,
            meta: Meta {
                tool_version: "t".to_string(),
                endpoint: "e".to_string(),
                graph: "g".to_string(),
                samples,
                warmup,
                concurrency,
                seed: 0,
                corpus_size: 0,
                server_timeout_ms: 5000,
                client_deadline_ms: 6000,
                connection: "c".to_string(),
                started_at_epoch_secs: 0,
                server: ServerInfo {
                    module_graph_ver: ver,
                    server_image: image.map(|s| s.to_string()),
                    ..Default::default()
                },
                host: Default::default(),
                dataset: Some(DatasetInfo {
                    seed: 0,
                    nodes: 1,
                    edges: 1,
                    workload_hash: hash.to_string(),
                }),
                label: None,
            },
            operations,
        }
    }

    #[test]
    fn comparable_when_manifest_matches_no_divergence() {
        let a = rep("h", 100, 50, vec![1, 4], Some(42001), Some("main"), &[("match_by_index", Some("d1"))]);
        let b = rep("h", 100, 50, vec![1, 4], Some(42002), Some("pr"), &[("match_by_index", Some("d1"))]);
        match regression_guard(&a, &b) {
            RegressionGuard::Comparable { diverged_ops, warnings } => {
                assert!(diverged_ops.is_empty());
                assert!(warnings.iter().any(|w| w.contains("server image changed")));
            }
            other => panic!("expected Comparable, got {other:?}"),
        }
    }

    #[test]
    fn per_op_divergence_is_reported_not_fatal() {
        let a = rep("h", 100, 50, vec![1], None, None, &[("match_by_index", Some("d1")), ("expand_1_hop", Some("e1"))]);
        let b = rep("h", 100, 50, vec![1], None, None, &[("match_by_index", Some("d1")), ("expand_1_hop", Some("DIFFERENT"))]);
        match regression_guard(&a, &b) {
            RegressionGuard::Comparable { diverged_ops, .. } => {
                assert_eq!(diverged_ops.len(), 1);
                assert!(diverged_ops.contains("expand_1_hop"));
            }
            other => panic!("expected Comparable, got {other:?}"),
        }
    }

    #[test]
    fn candidate_missing_digest_is_diverged() {
        let a = rep("h", 100, 50, vec![1], None, None, &[("match_by_index", Some("d1"))]);
        let b = rep("h", 100, 50, vec![1], None, None, &[("match_by_index", None)]);
        match regression_guard(&a, &b) {
            RegressionGuard::Comparable { diverged_ops, .. } => {
                assert!(diverged_ops.contains("match_by_index"));
            }
            other => panic!("expected Comparable, got {other:?}"),
        }
    }

    #[test]
    fn asymmetric_digest_is_diverged_but_both_absent_is_comparable() {
        // baseline None, candidate Some ⇒ diverged (can't verify correctness).
        let a = rep("h", 100, 50, vec![1], None, None, &[("match_by_index", None)]);
        let b = rep("h", 100, 50, vec![1], None, None, &[("match_by_index", Some("d1"))]);
        match regression_guard(&a, &b) {
            RegressionGuard::Comparable { diverged_ops, .. } => {
                assert!(diverged_ops.contains("match_by_index"));
            }
            other => panic!("expected Comparable, got {other:?}"),
        }
        // Both absent ⇒ no correctness info, not diverged.
        let a2 = rep("h", 100, 50, vec![1], None, None, &[("match_by_index", None)]);
        let b2 = rep("h", 100, 50, vec![1], None, None, &[("match_by_index", None)]);
        match regression_guard(&a2, &b2) {
            RegressionGuard::Comparable { diverged_ops, .. } => assert!(diverged_ops.is_empty()),
            other => panic!("expected Comparable, got {other:?}"),
        }
    }

    #[test]
    fn workload_mismatch_is_not_comparable() {
        let a = rep("h1", 100, 50, vec![1], None, None, &[]);
        let b = rep("h2", 100, 50, vec![1], None, None, &[]);
        assert!(matches!(regression_guard(&a, &b), RegressionGuard::NotComparable { .. }));
    }

    #[test]
    fn sampling_or_sweep_mismatch_is_not_comparable() {
        let base = rep("h", 100, 50, vec![1, 4], None, None, &[]);
        let diff_samples = rep("h", 200, 50, vec![1, 4], None, None, &[]);
        assert!(matches!(regression_guard(&base, &diff_samples), RegressionGuard::NotComparable { .. }));
        let diff_sweep = rep("h", 100, 50, vec![1, 4, 8], None, None, &[]);
        assert!(matches!(regression_guard(&base, &diff_sweep), RegressionGuard::NotComparable { .. }));
    }

    #[test]
    fn differing_max_queued_queries_is_not_comparable_but_unread_is_ok() {
        let mut a = rep("h", 100, 50, vec![1], None, None, &[]);
        let mut b = rep("h", 100, 50, vec![1], None, None, &[]);
        a.meta.server.max_queued_queries = Some(1000);
        b.meta.server.max_queued_queries = Some(25);
        assert!(matches!(regression_guard(&a, &b), RegressionGuard::NotComparable { .. }));
        // One side unread (None) can't be compared, so it does not disqualify.
        b.meta.server.max_queued_queries = None;
        assert!(matches!(regression_guard(&a, &b), RegressionGuard::Comparable { .. }));
    }
}
