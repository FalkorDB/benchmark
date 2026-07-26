//! Version-comparison baselines: the guard that `synthetic-compare` runs before invoking Criterion,
//! refusing to compare two runs whose **workload** differs.
//!
//! FalkorDB's **version** is the *subject* of a version comparison (a baseline captured on v4.2.1
//! vs a candidate on v4.3.0), so a version change is *recorded and displayed*, never rejected.
//! The **workload** — identified by [`workload_hash`](crate::synthetic::report::DatasetInfo) — is the
//! hard gate: a different (or absent) hash means the two runs measured different things and the
//! latency comparison would be meaningless. Keeping this logic in the library (rather than the
//! Criterion bench harness) makes it unit-testable.

use crate::synthetic::report::{OpPolicy, Report, ServerInfo};
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
    /// Per-op **effective measurement policy** for ops whose budget overrode the global knobs
    /// (design §3.4). Budgets are outside the `workload_hash`, so this map is compared op-by-op:
    /// two runs that measured the same workload under different per-op sampling/cache/sweep/
    /// timeout conditions must not have their latencies compared. Empty for pre-budget reports
    /// and for runs where every op inherited the global knobs.
    #[serde(default)]
    pub op_policies: BTreeMap<String, OpPolicy>,
    /// Ops the run **skipped** (capability probe — design Phase 6 §3.5): recorded but never
    /// executed because the engine lacks their required procedure. An op skipped on either side
    /// is exempt from the per-op policy and result-digest gates (it carries neither), and reads
    /// as neither a pass nor a divergence. Empty for pre-Phase-6 reports.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub skipped_ops: BTreeSet<String>,
    /// §6.3 oracle attestation (`meta.oracle_verified`): op → number of recorded write outcomes
    /// the replay **re-verified** before measuring. `None` for a latency-tier-only run. Compared
    /// across sides so a run that dropped the correctness tier can't silently pair with one that
    /// kept it. Absent from pre-§6.3 baselines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oracle_verified: Option<BTreeMap<String, usize>>,
    /// The **oracle-eligible** write ops (the §6.3 deterministic subset) this run measured. Lets
    /// the guards flag a pair that measured eligible writes with *no* oracle on either side —
    /// legitimate for a v2 latency-tier bundle, but exactly what a two-sided v3→v2 downgrade
    /// looks like, so it warrants a prominent warning. Empty for read runs and older baselines.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub eligible_write_ops: BTreeSet<String>,
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
            op_policies: report
                .operations
                .iter()
                .filter_map(|(name, op)| op.policy.clone().map(|p| (name.clone(), p)))
                .collect(),
            skipped_ops: report
                .operations
                .iter()
                .filter(|(_, op)| op.skipped.is_some())
                .map(|(name, _)| name.clone())
                .collect(),
            oracle_verified: report.meta.oracle_verified.clone(),
            eligible_write_ops: eligible_write_ops(report),
        }
    }
}

/// The oracle-eligible write ops (§6.3 deterministic subset) present in a report's op set.
fn eligible_write_ops(report: &Report) -> BTreeSet<String> {
    let eligible = crate::synthetic::shapes::oracle_eligible_names();
    report
        .operations
        .keys()
        .filter(|name| eligible.contains(name.as_str()))
        .cloned()
        .collect()
}

/// Compact human summary of an oracle attestation map: `"7 op(s), 1792 outcome(s)"`.
fn attestation_summary(m: &BTreeMap<String, usize>) -> String {
    format!("{} op(s), {} outcome(s)", m.len(), m.values().sum::<usize>())
}

/// §6.3 oracle-attestation comparability between two runs.
///
/// `Err(reason)` is a **fatal** mismatch — the runs re-verified different outcome coverage
/// (one-sided or differing attestations), so their correctness tiers are not the same check and
/// the comparison must refuse, exactly like a workload mismatch. Unreachable through the real
/// pipeline (a v3 bundle's oracle is hash-bound, so hash-equal replays attest identically) — this
/// arm defends hand-edited or mixed-provenance reports.
///
/// `Ok(warnings)` may carry the **downgrade-visibility** warning: both sides measured
/// oracle-eligible write ops with no attestation at all. That pair is legitimate (a v2
/// latency-tier bundle) but indistinguishable from a two-sided re-hashed v3→v2 strip, so it is
/// surfaced prominently rather than silently passed (§6.3 — duck's downgrade finding).
fn oracle_attestation_check(
    baseline: Option<&BTreeMap<String, usize>>,
    candidate: Option<&BTreeMap<String, usize>>,
    eligible_ops: &BTreeSet<String>,
) -> Result<Vec<String>, String> {
    match (baseline, candidate) {
        (Some(b), Some(c)) if b == c => Ok(Vec::new()),
        (Some(b), Some(c)) => Err(format!(
            "oracle attestation differs — baseline re-verified {} but candidate re-verified {}: \
             the runs verified different write-outcome coverage, so their correctness tiers are \
             not comparable",
            attestation_summary(b),
            attestation_summary(c)
        )),
        (Some(b), None) => Err(format!(
            "oracle attestation is one-sided — baseline re-verified the write outcome oracle \
             ({}) but candidate carries no attestation (latency tier only): a re-hashed v3→v2 \
             downgrade looks exactly like this. Re-run the candidate against the oracle-bearing \
             (v3) bundle, with `--require-oracle` to refuse the downgrade",
            attestation_summary(b)
        )),
        (None, Some(c)) => Err(format!(
            "oracle attestation is one-sided — candidate re-verified the write outcome oracle \
             ({}) but baseline carries no attestation (latency tier only): a re-hashed v3→v2 \
             downgrade looks exactly like this. Re-run the baseline against the oracle-bearing \
             (v3) bundle, with `--require-oracle` to refuse the downgrade",
            attestation_summary(c)
        )),
        (None, None) if !eligible_ops.is_empty() => Ok(vec![format!(
            "both runs measured oracle-eligible write op(s) ({}) with no outcome oracle — \
             latencies were compared WITHOUT the §6.3 correctness tier. Re-record with --oracle \
             and replay with --require-oracle to enforce it",
            eligible_ops.iter().cloned().collect::<Vec<_>>().join(", ")
        )]),
        (None, None) => Ok(Vec::new()),
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

/// The first per-op **effective-policy** mismatch between two runs, as a human-readable refusal
/// reason (`None` when every op's policy matches). Compared over the union of budgeted ops: a
/// policy present on one side only is a mismatch too — that op was measured under different
/// conditions (e.g. one run's bundle carried a budget the other's didn't). Ops in `skipped` —
/// capability-skipped on either side (design Phase 6 §3.5) — are exempt: a skipped op was never
/// measured, so its structurally-absent policy is not a config mismatch.
fn op_policy_mismatch(
    baseline: &BTreeMap<String, OpPolicy>,
    candidate: &BTreeMap<String, OpPolicy>,
    skipped: &BTreeSet<String>,
) -> Option<String> {
    let all: BTreeSet<&String> = baseline.keys().chain(candidate.keys()).collect();
    for op in all {
        if skipped.contains(op.as_str()) {
            continue;
        }
        let b = baseline.get(op);
        let c = candidate.get(op);
        if b != c {
            let render = |p: Option<&OpPolicy>| {
                p.map_or_else(|| "inherits the global knobs".to_string(), |p| p.to_string())
            };
            return Some(format!(
                "per-op measurement policy differs for '{op}' — baseline: {}; candidate: {}. The \
                 op's effective measurement conditions (its recorded budget and/or the global \
                 knobs it inherits) changed between the runs, so their latencies are not comparable",
                render(b),
                render(c)
            ));
        }
    }
    None
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

    // Per-op measurement-policy gate: budgets are deliberately outside the workload_hash (replay
    // policy, not workload content — design §3.4), so two runs of "the same workload" can still
    // have measured an op under different sampling/cache/sweep/timeout conditions. Such latencies
    // are not comparable — refuse, exactly like a workload mismatch. Ops capability-skipped on
    // either side are exempt (never measured ⇒ nothing to compare — design Phase 6 §3.5).
    let skipped_either: BTreeSet<String> = baseline
        .skipped_ops
        .union(&candidate.skipped_ops)
        .cloned()
        .collect();
    if let Some(reason) = op_policy_mismatch(
        &baseline.op_policies,
        &candidate.op_policies,
        &skipped_either,
    ) {
        return GuardOutcome::Abort { reason };
    }

    // §6.3 oracle-attestation gate: one-sided or differing attestation means the two runs did not
    // run the same correctness tier — refuse, like a workload mismatch. Both-absent over
    // oracle-eligible write ops is legitimate (latency tier) but downgrade-shaped ⇒ warning below.
    let attestation_warnings = match oracle_attestation_check(
        baseline.oracle_verified.as_ref(),
        candidate.oracle_verified.as_ref(),
        &baseline
            .eligible_write_ops
            .union(&candidate.eligible_write_ops)
            .cloned()
            .collect(),
    ) {
        Ok(warnings) => warnings,
        Err(reason) => return GuardOutcome::Abort { reason },
    };

    // Result-correctness gate: for every op the baseline recorded a result digest for, the
    // candidate must record the *same* digest — otherwise a version returning wrong or empty
    // results faster could masquerade as an improvement. A candidate that is missing a digest the
    // baseline has is also a mismatch (fail closed, matching the docs' "every op" guarantee).
    // Digests are present for `synthetic run --recording` runs; a `synthetic run` baseline has none, so the
    // loop is a no-op there (and such runs already differ on `workload_hash` above). An op
    // capability-skipped on either side is exempt: its absent digest means "never ran", not
    // "returned different results".
    for (op, base_dig) in &baseline.result_digests {
        if skipped_either.contains(op) {
            continue;
        }
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

    let mut warnings = attestation_warnings;
    // Only warn "same version" when both versions are actually *known* and equal — two unknown
    // (`None`) versions are not a known match, so don't claim there's no delta to measure. The
    // dev placeholder is excluded too: two edge/RC images both reporting the placeholder are NOT
    // known to be the same build (the separate placeholder warning below covers them).
    if baseline.module_graph_ver.is_some()
        && baseline.module_graph_ver == candidate.module_graph_ver
        && baseline.module_graph_ver != Some(ServerInfo::PLACEHOLDER_VER)
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
    // Global timeouts are measurement config like samples/sweep: every op that inherits them (no
    // per-op policy below) was measured under these values, so a known difference disqualifies the
    // pair the same way. (Cache selection needs no guard here — cells are compared per cache mode,
    // so a mode measured on one side only is simply absent, never mis-paired.)
    if baseline.meta.server_timeout_ms != candidate.meta.server_timeout_ms
        || baseline.meta.client_deadline_ms != candidate.meta.client_deadline_ms
    {
        return RegressionGuard::NotComparable {
            reason: format!(
                "timeouts differ — baseline {}/{} vs candidate {}/{} (server_timeout_ms/client_deadline_ms)",
                baseline.meta.server_timeout_ms,
                baseline.meta.client_deadline_ms,
                candidate.meta.server_timeout_ms,
                candidate.meta.client_deadline_ms
            ),
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
    // Per-op effective measurement policy (design §3.4): budgets are outside the workload_hash,
    // so a matching hash does not prove each op was measured under the same conditions. A per-op
    // policy mismatch is a *config* mismatch — the whole pair is NotComparable, exactly like the
    // global sampling/sweep checks above. Ops capability-skipped on either side (design Phase 6
    // §3.5) are exempt — never measured ⇒ no measurement conditions to mismatch.
    let skipped = |r: &Report| -> Vec<String> {
        r.operations
            .iter()
            .filter(|(_, op)| op.skipped.is_some())
            .map(|(name, _)| name.clone())
            .collect()
    };
    let skipped_either: BTreeSet<String> = skipped(baseline)
        .into_iter()
        .chain(skipped(candidate))
        .collect();
    let policies = |r: &Report| -> BTreeMap<String, OpPolicy> {
        r.operations
            .iter()
            .filter_map(|(name, op)| op.policy.clone().map(|p| (name.clone(), p)))
            .collect()
    };
    if let Some(reason) =
        op_policy_mismatch(&policies(baseline), &policies(candidate), &skipped_either)
    {
        return RegressionGuard::NotComparable { reason };
    }
    // §6.3 oracle-attestation gate — same rule as the strict [`guard`]: one-sided or differing
    // attestation ⇒ the correctness tiers differ ⇒ NotComparable; both-absent over eligible
    // write ops ⇒ the prominent latency-tier-only warning (merged into the advisory set below).
    let attestation_warnings = match oracle_attestation_check(
        baseline.meta.oracle_verified.as_ref(),
        candidate.meta.oracle_verified.as_ref(),
        &eligible_write_ops(baseline)
            .union(&eligible_write_ops(candidate))
            .cloned()
            .collect(),
    ) {
        Ok(warnings) => warnings,
        Err(reason) => return RegressionGuard::NotComparable { reason },
    };

    // 2. Per-op result divergence — reported, never fatal. Over the *union* of ops: two present
    //    digests that differ, or an asymmetric one-side-only digest, is diverged (we can't verify
    //    correctness). Two absent digests carry no correctness info (e.g. a non-recording run) and
    //    are left comparable, matching the strict guard. An op capability-skipped on either side
    //    is never diverged — its absent digest means "never ran", not "returned different results".
    let mut diverged_ops = BTreeSet::new();
    let all_ops: BTreeSet<&String> = baseline
        .operations
        .keys()
        .chain(candidate.operations.keys())
        .collect();
    for op in all_ops {
        if skipped_either.contains(op.as_str()) {
            continue;
        }
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
        warnings: {
            let mut warnings = attestation_warnings;
            warnings.extend(advisory_warnings(baseline, candidate));
            warnings
        },
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
    // Two placeholder versions are NOT a known match (every edge/RC image reports the same
    // placeholder), so suppress the "same version" claim and let the placeholder warning below
    // carry the signal (design §A6 of synthetic-three-way-report.md).
    if bv.is_some() && bv == cv && bv != Some(ServerInfo::PLACEHOLDER_VER) {
        warnings.push(format!(
            "baseline and candidate ran the same FalkorDB module version ({}) — there is no \
             version delta to measure",
            ver_str(cv)
        ));
    }
    // A *differing* module version is noted too — advisory only, never a comparability guard
    // (design §A1). Placeholder sides are excluded: the placeholder warning below carries the
    // signal, and a placeholder-to-real "change" is not a meaningful version delta.
    if let (Some(a), Some(b)) = (bv, cv) {
        if a != b && a != ServerInfo::PLACEHOLDER_VER && b != ServerInfo::PLACEHOLDER_VER {
            warnings.push(format!(
                "FalkorDB module version changed: {} → {}",
                ver_str(bv),
                ver_str(cv)
            ));
        }
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
            op_policies: BTreeMap::new(),
            skipped_ops: BTreeSet::new(),
            oracle_verified: None,
            eligible_write_ops: BTreeSet::new(),
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
            op_policies: BTreeMap::new(),
            skipped_ops: BTreeSet::new(),
            oracle_verified: None,
            eligible_write_ops: BTreeSet::new(),
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

    fn policy(samples: usize) -> OpPolicy {
        OpPolicy {
            samples,
            warmup: 0,
            concurrency: vec![1],
            cache: crate::synthetic::CacheSelection::Cached,
            server_timeout_ms: 5000,
            client_deadline_ms: 6000,
        }
    }

    fn key_with_policies(policies: &[(&str, OpPolicy)]) -> BaselineKey {
        BaselineKey {
            workload_hash: Some("sha256:abc".to_string()),
            module_graph_ver: Some(42001),
            server_image: None,
            result_digests: BTreeMap::new(),
            op_policies: policies
                .iter()
                .map(|(k, p)| (k.to_string(), p.clone()))
                .collect(),
            skipped_ops: BTreeSet::new(),
            oracle_verified: None,
            eligible_write_ops: BTreeSet::new(),
        }
    }

    #[test]
    fn aborts_on_op_policy_mismatch() {
        // Same workload, but the op's recorded budget (samples here) changed between the runs.
        let base = key_with_policies(&[("algo_max_flow", policy(1))]);
        let cand = key_with_policies(&[("algo_max_flow", policy(3))]);
        match guard(&base, &cand) {
            GuardOutcome::Abort { reason } => {
                assert!(
                    reason.contains("measurement policy differs for 'algo_max_flow'"),
                    "got: {reason}"
                );
                assert!(reason.contains("samples=1"), "got: {reason}");
                assert!(reason.contains("samples=3"), "got: {reason}");
            }
            other => panic!("expected Abort, got {other:?}"),
        }
    }

    #[test]
    fn aborts_when_op_policy_is_one_sided() {
        // One run's bundle carried a budget the other's didn't: also not comparable.
        let base = key_with_policies(&[("algo_max_flow", policy(1))]);
        let cand = key_with_policies(&[]);
        match guard(&base, &cand) {
            GuardOutcome::Abort { reason } => {
                assert!(reason.contains("'algo_max_flow'"), "got: {reason}");
                assert!(reason.contains("inherits the global knobs"), "got: {reason}");
            }
            other => panic!("expected Abort, got {other:?}"),
        }
    }

    #[test]
    fn proceeds_when_op_policies_match() {
        let base = key_with_policies(&[("algo_max_flow", policy(1))]);
        let cand = key_with_policies(&[("algo_max_flow", policy(1))]);
        assert!(matches!(guard(&base, &cand), GuardOutcome::Proceed { .. }));
    }

    #[test]
    fn skipped_op_is_exempt_from_the_policy_and_digest_gates() {
        // The baseline measured `algo_max_flow` under a budget and gated its digest; the candidate
        // engine lacks the procedure and skipped the op. Its structurally-absent policy/digest is
        // "never ran", not a config or correctness mismatch — the pair stays comparable
        // (design Phase 6 §3.5). Both skip directions are exempt.
        let mut base = key_with_policies(&[("algo_max_flow", policy(1))]);
        base.result_digests
            .insert("algo_max_flow".to_string(), "sha256:aaa".to_string());
        let mut cand = key_with_policies(&[]);
        cand.skipped_ops.insert("algo_max_flow".to_string());
        assert!(matches!(guard(&base, &cand), GuardOutcome::Proceed { .. }));
        assert!(matches!(guard(&cand, &base), GuardOutcome::Proceed { .. }));
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
    fn equal_placeholder_versions_do_not_claim_a_same_version_match() {
        // Two edge/RC images both report the placeholder — that is NOT a known version match, so
        // only the placeholder warning fires, not the misleading "no delta to measure" one.
        let base = key(Some("sha256:abc"), Some(ServerInfo::PLACEHOLDER_VER));
        let cand = key(Some("sha256:abc"), Some(ServerInfo::PLACEHOLDER_VER));
        match guard(&base, &cand) {
            GuardOutcome::Proceed { warnings } => {
                assert!(!warnings.iter().any(|w| w.contains("same FalkorDB module version")));
                assert!(warnings.iter().any(|w| w.contains("dev placeholder")));
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    #[test]
    fn warns_on_server_image_change() {
        let base = BaselineKey {
            workload_hash: Some("sha256:abc".to_string()),
            module_graph_ver: Some(42001),
            server_image: Some("falkordb@sha256:aaa".to_string()),
            result_digests: BTreeMap::new(),
            op_policies: BTreeMap::new(),
            skipped_ops: BTreeSet::new(),
            oracle_verified: None,
            eligible_write_ops: BTreeSet::new(),
        };
        let cand = BaselineKey {
            workload_hash: Some("sha256:abc".to_string()),
            module_graph_ver: Some(42002),
            server_image: Some("falkordb@sha256:bbb".to_string()),
            result_digests: BTreeMap::new(),
            op_policies: BTreeMap::new(),
            skipped_ops: BTreeSet::new(),
            oracle_verified: None,
            eligible_write_ops: BTreeSet::new(),
        };
        match guard(&base, &cand) {
            GuardOutcome::Proceed { warnings } => {
                assert!(warnings.iter().any(|w| w.contains("server image changed")));
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    fn attested(entries: &[(&str, usize)]) -> BTreeMap<String, usize> {
        entries.iter().map(|(op, n)| (op.to_string(), *n)).collect()
    }

    #[test]
    fn attestation_check_covers_every_arm() {
        let full = attested(&[("single_vertex_write", 256), ("foreach_loop_mutation", 256)]);
        let subset = attested(&[("single_vertex_write", 256)]);
        let eligible: BTreeSet<String> = ["single_vertex_write".to_string()].into();
        // Matching attestations: clean pass.
        assert_eq!(oracle_attestation_check(Some(&full), Some(&full), &eligible), Ok(Vec::new()));
        // Differing attestations: fatal.
        let err = oracle_attestation_check(Some(&full), Some(&subset), &eligible).unwrap_err();
        assert!(err.contains("oracle attestation differs"), "{err}");
        assert!(err.contains("2 op(s), 512 outcome(s)"), "{err}");
        assert!(err.contains("1 op(s), 256 outcome(s)"), "{err}");
        // One-sided (either way): fatal, naming the downgrade and the flag.
        for (b, c, side) in
            [(Some(&full), None, "candidate"), (None, Some(&full), "baseline")]
        {
            let err = oracle_attestation_check(b, c, &eligible).unwrap_err();
            assert!(err.contains("one-sided"), "{err}");
            assert!(err.contains(&format!("Re-run the {side}")), "{err}");
            assert!(err.contains("--require-oracle"), "{err}");
            assert!(err.contains("v3→v2"), "{err}");
        }
        // Both absent over eligible write ops: legitimate but downgrade-shaped ⇒ warning.
        let warnings = oracle_attestation_check(None, None, &eligible).unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("WITHOUT"), "{}", warnings[0]);
        assert!(warnings[0].contains("single_vertex_write"), "{}", warnings[0]);
        assert!(warnings[0].contains("--require-oracle"), "{}", warnings[0]);
        // Both absent, nothing eligible (read runs): silent.
        assert_eq!(oracle_attestation_check(None, None, &BTreeSet::new()), Ok(Vec::new()));
    }

    #[test]
    fn guard_aborts_on_one_sided_or_differing_attestation() {
        let mut base = key(Some("sha256:abc"), Some(42001));
        let mut cand = key(Some("sha256:abc"), Some(42002));
        base.oracle_verified = Some(attested(&[("single_vertex_write", 256)]));
        match guard(&base, &cand) {
            GuardOutcome::Abort { reason } => assert!(reason.contains("one-sided"), "{reason}"),
            other => panic!("expected Abort, got {other:?}"),
        }
        cand.oracle_verified = Some(attested(&[("single_vertex_write", 8)]));
        match guard(&base, &cand) {
            GuardOutcome::Abort { reason } => {
                assert!(reason.contains("attestation differs"), "{reason}");
            }
            other => panic!("expected Abort, got {other:?}"),
        }
    }

    #[test]
    fn guard_warns_on_downgrade_shaped_pair_and_passes_matching_attestation() {
        // Both sides measured an oracle-eligible write op with no attestation: comparable, but
        // the latency-tier-only warning must surface (the two-sided-downgrade blind spot).
        let mut base = key(Some("sha256:abc"), Some(42001));
        let mut cand = key(Some("sha256:abc"), Some(42002));
        base.eligible_write_ops = ["single_vertex_write".to_string()].into();
        cand.eligible_write_ops = base.eligible_write_ops.clone();
        match guard(&base, &cand) {
            GuardOutcome::Proceed { warnings } => {
                assert!(warnings.iter().any(|w| w.contains("no outcome oracle")), "{warnings:?}");
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
        // Matching attestations: no oracle warning at all.
        base.oracle_verified = Some(attested(&[("single_vertex_write", 256)]));
        cand.oracle_verified = base.oracle_verified.clone();
        match guard(&base, &cand) {
            GuardOutcome::Proceed { warnings } => {
                assert!(!warnings.iter().any(|w| w.contains("oracle")), "{warnings:?}");
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    #[test]
    fn baseline_key_serde_defaults_for_pre_oracle_json() {
        // A pre-§6.3 baseline JSON (no oracle fields) must load with the defaults…
        let old = r#"{"workload_hash":"sha256:abc","module_graph_ver":42001,"result_digests":{}}"#;
        let key: BaselineKey = serde_json::from_str(old).unwrap();
        assert_eq!(key.oracle_verified, None);
        assert!(key.eligible_write_ops.is_empty());
        // …and a key without attestation serializes without the new fields (read baselines stay
        // byte-identical to pre-§6.3 output).
        let json = serde_json::to_string(&key).unwrap();
        assert!(!json.contains("oracle_verified"), "{json}");
        assert!(!json.contains("eligible_write_ops"), "{json}");
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
                    policy: None,
                    skipped: None,
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
                oracle_verified: None,
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

    #[test]
    fn advisory_warnings_suppress_same_version_for_placeholder_pairs() {
        // A real matching version still warns "no delta to measure"…
        let a = rep("h", 100, 50, vec![1], Some(42001), None, &[]);
        let b = rep("h", 100, 50, vec![1], Some(42001), None, &[]);
        match regression_guard(&a, &b) {
            RegressionGuard::Comparable { warnings, .. } => {
                assert!(warnings.iter().any(|w| w.contains("same FalkorDB module version")));
            }
            other => panic!("expected Comparable, got {other:?}"),
        }
        // …but two placeholder versions (every edge/RC image) are not a known match: only the
        // placeholder warning fires (design §A6 of synthetic-three-way-report.md).
        let pa = rep("h", 100, 50, vec![1], Some(ServerInfo::PLACEHOLDER_VER), None, &[]);
        let pb = rep("h", 100, 50, vec![1], Some(ServerInfo::PLACEHOLDER_VER), None, &[]);
        match regression_guard(&pa, &pb) {
            RegressionGuard::Comparable { warnings, .. } => {
                assert!(!warnings.iter().any(|w| w.contains("same FalkorDB module version")));
                assert!(warnings.iter().any(|w| w.contains("dev placeholder")));
            }
            other => panic!("expected Comparable, got {other:?}"),
        }
    }

    #[test]
    fn advisory_warnings_note_a_real_module_version_change() {
        // Two differing *real* versions: an advisory "version changed" note — never a guard
        // (design §A1; the comparison stays Comparable).
        let a = rep("h", 100, 50, vec![1], Some(42001), None, &[]);
        let b = rep("h", 100, 50, vec![1], Some(42002), None, &[]);
        match regression_guard(&a, &b) {
            RegressionGuard::Comparable { warnings, .. } => {
                assert!(
                    warnings.iter().any(|w| w.contains("module version changed")),
                    "{warnings:?}"
                );
            }
            other => panic!("expected Comparable, got {other:?}"),
        }
        // A placeholder on either side is not a meaningful delta: only the placeholder warning
        // fires, never a "changed" note against a placeholder.
        let pa = rep("h", 100, 50, vec![1], Some(ServerInfo::PLACEHOLDER_VER), None, &[]);
        let pb = rep("h", 100, 50, vec![1], Some(42002), None, &[]);
        match regression_guard(&pa, &pb) {
            RegressionGuard::Comparable { warnings, .. } => {
                assert!(!warnings.iter().any(|w| w.contains("module version changed")));
                assert!(warnings.iter().any(|w| w.contains("dev placeholder")));
            }
            other => panic!("expected Comparable, got {other:?}"),
        }
    }

    #[test]
    fn differing_global_timeouts_are_not_comparable() {
        // Global timeouts are inherited by every op without a per-op policy, so a known
        // difference is a config mismatch exactly like samples/sweep.
        let a = rep("h", 100, 50, vec![1], None, None, &[]);
        let mut b = rep("h", 100, 50, vec![1], None, None, &[]);
        b.meta.server_timeout_ms = 30_000;
        match regression_guard(&a, &b) {
            RegressionGuard::NotComparable { reason } => {
                assert!(reason.contains("timeouts differ"), "got: {reason}");
            }
            other => panic!("expected NotComparable, got {other:?}"),
        }
        let mut c = rep("h", 100, 50, vec![1], None, None, &[]);
        c.meta.client_deadline_ms = 60_000;
        assert!(matches!(regression_guard(&a, &c), RegressionGuard::NotComparable { .. }));
    }

    fn policy(samples: usize) -> OpPolicy {
        OpPolicy {
            samples,
            warmup: 0,
            concurrency: vec![1],
            cache: crate::synthetic::CacheSelection::Cached,
            server_timeout_ms: 5000,
            client_deadline_ms: 6000,
        }
    }

    #[test]
    fn per_op_policy_mismatch_is_not_comparable() {
        // Same workload_hash and global knobs, but one op was measured under a different
        // recorded budget — the pair must be refused like any other config mismatch.
        let mut a = rep("h", 100, 50, vec![1], None, None, &[("algo_max_flow", None)]);
        let mut b = rep("h", 100, 50, vec![1], None, None, &[("algo_max_flow", None)]);
        a.operations.get_mut("algo_max_flow").unwrap().policy = Some(policy(1));
        b.operations.get_mut("algo_max_flow").unwrap().policy = Some(policy(3));
        match regression_guard(&a, &b) {
            RegressionGuard::NotComparable { reason } => {
                assert!(
                    reason.contains("measurement policy differs for 'algo_max_flow'"),
                    "got: {reason}"
                );
            }
            other => panic!("expected NotComparable, got {other:?}"),
        }
        // One-sided policy (e.g. an old-tool report vs a budget-carrying rerun): also refused —
        // the policy-less run really did measure that op under the global knobs.
        b.operations.get_mut("algo_max_flow").unwrap().policy = None;
        assert!(matches!(regression_guard(&a, &b), RegressionGuard::NotComparable { .. }));
    }

    #[test]
    fn matching_per_op_policies_are_comparable() {
        let mut a = rep("h", 100, 50, vec![1], None, None, &[("algo_max_flow", None)]);
        let mut b = rep("h", 100, 50, vec![1], None, None, &[("algo_max_flow", None)]);
        a.operations.get_mut("algo_max_flow").unwrap().policy = Some(policy(1));
        b.operations.get_mut("algo_max_flow").unwrap().policy = Some(policy(1));
        assert!(matches!(regression_guard(&a, &b), RegressionGuard::Comparable { .. }));
    }

    #[test]
    fn baseline_key_collects_per_op_policies_from_a_report() {
        let mut r =
            rep("h", 100, 50, vec![1], None, None, &[("algo_max_flow", None), ("scan", None)]);
        r.operations.get_mut("algo_max_flow").unwrap().policy = Some(policy(1));
        let key = BaselineKey::from_report(&r);
        // Only the budgeted op lands in the key; inherit-everything ops stay absent.
        assert_eq!(key.op_policies.len(), 1);
        assert_eq!(key.op_policies.get("algo_max_flow"), Some(&policy(1)));
    }

    /// Mark `op` in `rep` as capability-skipped, exactly as replay records it (design Phase 6
    /// §3.5): a skip reason, no levels, no digest, no policy.
    fn skip_op(
        rep: &mut Report,
        op: &str,
    ) {
        let o = rep.operations.get_mut(op).unwrap();
        o.levels = vec![];
        o.result_digest = None;
        o.policy = None;
        o.skipped = Some("engine lacks procedure 'algo.maxFlow' (capability probe)".to_string());
    }

    #[test]
    fn regression_guard_exempts_skipped_ops_from_the_policy_gate() {
        // The baseline measured `algo_max_flow` under a recorded budget; the candidate engine
        // lacks its procedure and skipped it (so it carries no policy). That asymmetry is
        // "never ran", not a config mismatch — the pair stays comparable, in both directions.
        let mut a = rep(
            "h",
            100,
            50,
            vec![1],
            None,
            None,
            &[("algo_max_flow", None), ("scan", Some("d"))],
        );
        let mut b = rep(
            "h",
            100,
            50,
            vec![1],
            None,
            None,
            &[("algo_max_flow", None), ("scan", Some("d"))],
        );
        a.operations.get_mut("algo_max_flow").unwrap().policy = Some(policy(1));
        skip_op(&mut b, "algo_max_flow");
        assert!(matches!(
            regression_guard(&a, &b),
            RegressionGuard::Comparable { .. }
        ));
        assert!(matches!(
            regression_guard(&b, &a),
            RegressionGuard::Comparable { .. }
        ));
    }

    #[test]
    fn regression_guard_never_marks_a_skipped_op_diverged() {
        // The measured side has a digest, the skipped side has none — the usual asymmetric-digest
        // divergence rule must NOT fire for a skip (nothing ran, so nothing differed).
        let a = rep(
            "h",
            100,
            50,
            vec![1],
            None,
            None,
            &[("algo_max_flow", Some("d1")), ("scan", Some("d"))],
        );
        let mut b = rep(
            "h",
            100,
            50,
            vec![1],
            None,
            None,
            &[("algo_max_flow", Some("d1")), ("scan", Some("d"))],
        );
        skip_op(&mut b, "algo_max_flow");
        match regression_guard(&a, &b) {
            RegressionGuard::Comparable { diverged_ops, .. } => {
                assert!(
                    diverged_ops.is_empty(),
                    "skip is never a divergence: {diverged_ops:?}"
                );
            }
            other => panic!("expected Comparable, got {other:?}"),
        }
    }

    #[test]
    fn baseline_key_collects_skipped_ops_from_a_report() {
        let mut r = rep(
            "h",
            100,
            50,
            vec![1],
            None,
            None,
            &[("algo_max_flow", None), ("scan", None)],
        );
        skip_op(&mut r, "algo_max_flow");
        let key = BaselineKey::from_report(&r);
        assert_eq!(key.skipped_ops.len(), 1);
        assert!(key.skipped_ops.contains("algo_max_flow"));
        // …and a fully-measured report yields an empty set (which serialization omits, keeping
        // pre-Phase-6 baseline JSON byte-identical).
        let clean = rep("h", 100, 50, vec![1], None, None, &[("scan", None)]);
        let clean_key = BaselineKey::from_report(&clean);
        assert!(clean_key.skipped_ops.is_empty());
        let json = serde_json::to_string(&clean_key).unwrap();
        assert!(!json.contains("skipped_ops"), "{json}");
    }

    fn oracle_map(entries: &[(&str, usize)]) -> BTreeMap<String, usize> {
        entries.iter().map(|(op, n)| (op.to_string(), *n)).collect()
    }

    #[test]
    fn regression_guard_not_comparable_on_one_sided_or_differing_attestation() {
        // Baseline replayed the v3 bundle (attested); candidate replayed a stripped/downgraded
        // v2 twin — hash-equal, so only the attestation gate can catch it.
        let mut a =
            rep("h", 100, 50, vec![1], None, None, &[("single_vertex_write", Some("d1"))]);
        let mut b =
            rep("h", 100, 50, vec![1], None, None, &[("single_vertex_write", Some("d1"))]);
        a.meta.oracle_verified = Some(oracle_map(&[("single_vertex_write", 256)]));
        match regression_guard(&a, &b) {
            RegressionGuard::NotComparable { reason } => {
                assert!(reason.contains("one-sided"), "{reason}");
                assert!(reason.contains("--require-oracle"), "{reason}");
            }
            other => panic!("expected NotComparable, got {other:?}"),
        }
        // Differing attestations are just as fatal.
        b.meta.oracle_verified = Some(oracle_map(&[("single_vertex_write", 8)]));
        assert!(matches!(regression_guard(&a, &b), RegressionGuard::NotComparable { .. }));
    }

    #[test]
    fn regression_guard_warns_on_unattested_eligible_write_ops_only() {
        // Two latency-tier (v2) write replays: comparable, but prominently flagged.
        let a = rep("h", 100, 50, vec![1], None, None, &[("single_vertex_write", Some("d1"))]);
        let b = rep("h", 100, 50, vec![1], None, None, &[("single_vertex_write", Some("d1"))]);
        match regression_guard(&a, &b) {
            RegressionGuard::Comparable { warnings, .. } => {
                assert!(
                    warnings.iter().any(|w| w.contains("no outcome oracle")),
                    "{warnings:?}"
                );
            }
            other => panic!("expected Comparable, got {other:?}"),
        }
        // A read-only pair carries no oracle-eligible ops ⇒ no oracle warning.
        let ra = rep("h", 100, 50, vec![1], None, None, &[("match_by_index", Some("d1"))]);
        let rb = rep("h", 100, 50, vec![1], None, None, &[("match_by_index", Some("d1"))]);
        match regression_guard(&ra, &rb) {
            RegressionGuard::Comparable { warnings, .. } => {
                assert!(!warnings.iter().any(|w| w.contains("oracle")), "{warnings:?}");
            }
            other => panic!("expected Comparable, got {other:?}"),
        }
    }

    #[test]
    fn regression_guard_passes_matching_attestation_without_warning() {
        let mut a =
            rep("h", 100, 50, vec![1], None, None, &[("single_vertex_write", Some("d1"))]);
        let mut b =
            rep("h", 100, 50, vec![1], None, None, &[("single_vertex_write", Some("d1"))]);
        a.meta.oracle_verified = Some(oracle_map(&[("single_vertex_write", 256)]));
        b.meta.oracle_verified = a.meta.oracle_verified.clone();
        match regression_guard(&a, &b) {
            RegressionGuard::Comparable { diverged_ops, warnings } => {
                assert!(diverged_ops.is_empty());
                assert!(!warnings.iter().any(|w| w.contains("oracle")), "{warnings:?}");
            }
            other => panic!("expected Comparable, got {other:?}"),
        }
    }

    #[test]
    fn baseline_key_collects_oracle_attestation_and_eligible_write_ops() {
        let mut r = rep(
            "h",
            100,
            50,
            vec![1],
            None,
            None,
            &[("single_vertex_write", Some("d1")), ("match_by_index", Some("d2"))],
        );
        r.meta.oracle_verified = Some(oracle_map(&[("single_vertex_write", 256)]));
        let key = BaselineKey::from_report(&r);
        assert_eq!(key.oracle_verified, r.meta.oracle_verified);
        // Only the oracle-eligible write op is collected — reads never carry oracles.
        assert_eq!(
            key.eligible_write_ops,
            ["single_vertex_write".to_string()].into()
        );
    }
}
