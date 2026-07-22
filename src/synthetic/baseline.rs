//! Version-comparison baselines: the guard that `synthetic-compare` runs before invoking Criterion,
//! refusing to compare two runs whose **workload** differs.
//!
//! FalkorDB's **version** is the *subject* of a version comparison (a baseline captured on v4.2.1
//! vs a candidate on v4.3.0), so a version change is *recorded and displayed*, never rejected.
//! The **workload** — identified by [`corpus_hash`](crate::synthetic::report::DatasetInfo) — is the
//! hard gate: a different (or absent) hash means the two runs measured different things and the
//! latency comparison would be meaningless. Keeping this logic in the library (rather than the
//! Criterion bench harness) makes it unit-testable.

use crate::synthetic::report::{Report, ServerInfo};
use serde::{Deserialize, Serialize};

/// The workload + environment identity of a run (extracted from its [`Report`]) that a
/// version-comparison must agree on — or knowingly differ on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineKey {
    /// Workload fingerprint (dataset knobs + ops in order + query bodies + sampled pools). `None`
    /// for an external graph that couldn't be fingerprinted — such a run can't be safely compared.
    pub corpus_hash: Option<String>,
    /// FalkorDB graph-module version (recorded for display; expected to differ across versions).
    pub module_graph_ver: Option<u64>,
    /// Operator-supplied server image identity, when provided.
    pub server_image: Option<String>,
}

impl BaselineKey {
    /// Extract the comparison key from a run's report.
    pub fn from_report(report: &Report) -> Self {
        BaselineKey {
            corpus_hash: report.meta.dataset.as_ref().map(|d| d.corpus_hash.clone()),
            module_graph_ver: report.meta.server.module_graph_ver,
            server_image: report.meta.server.server_image.clone(),
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
/// The **workload** (`corpus_hash`) is a hard gate — a different or absent hash means the runs
/// measured different things, so we abort. The FalkorDB **version** is only advisory: comparing
/// across versions is the whole point, so a version change is recorded (as a `Proceed` with no
/// warning); identical or placeholder versions produce advisory warnings.
pub fn guard(
    baseline: &BaselineKey,
    candidate: &BaselineKey,
) -> GuardOutcome {
    match (&baseline.corpus_hash, &candidate.corpus_hash) {
        (Some(a), Some(b)) if a == b => {}
        (Some(a), Some(b)) => {
            return GuardOutcome::Abort {
                reason: format!(
                    "corpus_hash mismatch — the workload changed since the baseline was saved \
                     (baseline {a}, candidate {b}); re-save the baseline for the current workload"
                ),
            };
        }
        _ => {
            return GuardOutcome::Abort {
                reason: "missing corpus_hash — a comparable baseline needs a generated dataset \
                         (`--generate`) so the workload can be fingerprinted"
                    .to_string(),
            };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn key(corpus: Option<&str>, ver: Option<u64>) -> BaselineKey {
        BaselineKey {
            corpus_hash: corpus.map(|s| s.to_string()),
            module_graph_ver: ver,
            server_image: None,
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
    fn aborts_on_corpus_hash_mismatch() {
        let base = key(Some("sha256:abc"), Some(42001));
        let cand = key(Some("sha256:def"), Some(42001));
        match guard(&base, &cand) {
            GuardOutcome::Abort { reason } => assert!(reason.contains("corpus_hash mismatch")),
            other => panic!("expected Abort, got {other:?}"),
        }
    }

    #[test]
    fn aborts_when_a_corpus_hash_is_absent() {
        // An external graph (no generated dataset) has no corpus_hash ⇒ unsafe to compare.
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
            corpus_hash: Some("sha256:abc".to_string()),
            module_graph_ver: Some(42001),
            server_image: Some("falkordb@sha256:aaa".to_string()),
        };
        let cand = BaselineKey {
            corpus_hash: Some("sha256:abc".to_string()),
            module_graph_ver: Some(42002),
            server_image: Some("falkordb@sha256:bbb".to_string()),
        };
        match guard(&base, &cand) {
            GuardOutcome::Proceed { warnings } => {
                assert!(warnings.iter().any(|w| w.contains("server image changed")));
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }
}
