//! Regression thresholds for `synthetic report --regression`.
//!
//! A cell (operation × concurrency × cache-mode) is flagged **🔴 regressed** only when the
//! candidate's p50 (the total-latency median) is slower than the baseline by **more than**
//! `budget_pct` **and** the absolute p50 increase exceeds `floor_ms` (a noise guard for
//! sub-millisecond ops). Faster — or slower within either bound — is **🟢**. A missing/zero
//! baseline is **N/A**.
//!
//! The budget is resolved per `(op, concurrency)`; the **per-concurrency** override applies to
//! `budget_pct` only, with precedence `op.<name>.concurrency.<C>` > `op.<name>` > `[default]`,
//! while `floor_ms`/`metric` resolve at `op.<name>` > `[default]`. The config ships a built-in
//! default (10 %, `floor_ms = 0.5`, `metric = "p50"`) and is overridable from a TOML file that
//! lives in the consuming repo (e.g. `falkordb-rs-next-gen`).

use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::synthetic::OpName;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Built-in default slowdown budget (percent) before a cell is flagged.
pub const DEFAULT_BUDGET_PCT: f64 = 10.0;
/// Built-in default absolute p50 floor (ms): slowdowns below this are treated as noise.
pub const DEFAULT_FLOOR_MS: f64 = 0.5;

/// The verdict metric. Only [`Metric::P50`] is implemented today; the others are reserved and
/// **rejected at load time** so a config can't silently select an unimplemented rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Metric {
    /// Total-latency median (`total_ms` p50) — the implemented verdict metric.
    #[default]
    P50,
    /// Reserved for a later iteration.
    Throughput,
    /// Reserved for a later iteration.
    Both,
}

/// The per-cell verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Faster, or slower within budget/floor.
    Ok,
    /// Slower than the budget (and beyond the noise floor).
    Regressed,
    /// No usable baseline (missing/zero/non-finite) — no verdict.
    NotApplicable,
}

impl Verdict {
    /// The emoji shown in the report's verdict column.
    pub fn emoji(self) -> &'static str {
        match self {
            Verdict::Ok => "🟢",
            Verdict::Regressed => "🔴",
            Verdict::NotApplicable => "N/A",
        }
    }
}

/// The budget resolved for one `(op, concurrency)` cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedBudget {
    pub metric: Metric,
    pub budget_pct: f64,
    pub floor_ms: f64,
}

impl ResolvedBudget {
    /// Verdict for a cell given the baseline and candidate p50 (ms). A non-finite or non-positive
    /// baseline **or candidate** yields [`Verdict::NotApplicable`] (latencies are strictly positive,
    /// so a `0`/negative/NaN reading is a missing/invalid metric, not a real speedup).
    pub fn verdict(&self, baseline_p50: f64, candidate_p50: f64) -> Verdict {
        if !baseline_p50.is_finite()
            || baseline_p50 <= 0.0
            || !candidate_p50.is_finite()
            || candidate_p50 <= 0.0
        {
            return Verdict::NotApplicable;
        }
        let slower = candidate_p50 > baseline_p50;
        let over_budget = candidate_p50 > baseline_p50 * (1.0 + self.budget_pct / 100.0);
        let over_floor = (candidate_p50 - baseline_p50) > self.floor_ms;
        if slower && over_budget && over_floor {
            Verdict::Regressed
        } else {
            Verdict::Ok
        }
    }
}

// ---- On-disk (raw) TOML shape ------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawDefault {
    metric: Option<Metric>,
    budget_pct: Option<f64>,
    floor_ms: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOp {
    metric: Option<Metric>,
    budget_pct: Option<f64>,
    floor_ms: Option<f64>,
    /// Per-concurrency `budget_pct` overrides, keyed by the concurrency level `C` (TOML keys are
    /// strings, parsed to `usize` on load).
    #[serde(default)]
    concurrency: BTreeMap<String, f64>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    default: RawDefault,
    #[serde(default)]
    op: BTreeMap<String, RawOp>,
}

// ---- Validated config --------------------------------------------------------------------------

/// A validated per-operation override.
#[derive(Debug, Clone, Default)]
struct OpThresholds {
    metric: Option<Metric>,
    budget_pct: Option<f64>,
    floor_ms: Option<f64>,
    /// Per-concurrency `budget_pct` overrides.
    per_concurrency_budget_pct: BTreeMap<usize, f64>,
}

/// Validated regression thresholds: a `[default]` plus per-operation overrides.
#[derive(Debug, Clone)]
pub struct Thresholds {
    default: ResolvedBudget,
    ops: BTreeMap<OpName, OpThresholds>,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Thresholds {
    /// The built-in defaults (10 %, `floor_ms = 0.5`, `metric = p50`, no per-op overrides).
    pub fn builtin() -> Self {
        Thresholds {
            default: ResolvedBudget {
                metric: Metric::P50,
                budget_pct: DEFAULT_BUDGET_PCT,
                floor_ms: DEFAULT_FLOOR_MS,
            },
            ops: BTreeMap::new(),
        }
    }

    /// Load + validate thresholds from a TOML file, layering it over the built-in defaults.
    pub fn from_file(path: &str) -> BenchmarkResult<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| OtherError(format!("could not read thresholds '{path}': {e}")))?;
        Self::from_toml_str(&text)
            .map_err(|e| OtherError(format!("invalid thresholds '{path}': {e}")))
    }

    /// Parse + validate thresholds from a TOML string (used by [`Self::from_file`] and tests).
    /// Returns a bare message on error (the file path is added by the caller).
    pub fn from_toml_str(text: &str) -> Result<Self, String> {
        let raw: RawConfig = toml::from_str(text).map_err(|e| e.to_string())?;

        let default = ResolvedBudget {
            metric: check_metric(raw.default.metric.unwrap_or_default())?,
            budget_pct: check_budget(raw.default.budget_pct.unwrap_or(DEFAULT_BUDGET_PCT), "[default].budget_pct")?,
            floor_ms: check_floor(raw.default.floor_ms.unwrap_or(DEFAULT_FLOOR_MS), "[default].floor_ms")?,
        };

        let mut ops = BTreeMap::new();
        for (name, raw_op) in raw.op {
            let op = OpName::from_tag(&name).ok_or_else(|| {
                format!("unknown operation '{name}' in [op.{name}] — see `synthetic list-ops`")
            })?;
            if let Some(m) = raw_op.metric {
                check_metric(m)?;
            }
            let budget_pct = raw_op
                .budget_pct
                .map(|v| check_budget(v, &format!("[op.{name}].budget_pct")))
                .transpose()?;
            let floor_ms = raw_op
                .floor_ms
                .map(|v| check_floor(v, &format!("[op.{name}].floor_ms")))
                .transpose()?;
            let mut per_concurrency_budget_pct = BTreeMap::new();
            for (c_str, pct) in raw_op.concurrency {
                let c: usize = c_str.parse().map_err(|_| {
                    format!("[op.{name}].concurrency has a non-integer key '{c_str}'")
                })?;
                if c == 0 {
                    return Err(format!(
                        "[op.{name}].concurrency has an invalid level 0 (must be ≥ 1)"
                    ));
                }
                let pct = check_budget(pct, &format!("[op.{name}].concurrency.{c}"))?;
                per_concurrency_budget_pct.insert(c, pct);
            }
            ops.insert(
                op,
                OpThresholds {
                    metric: raw_op.metric,
                    budget_pct,
                    floor_ms,
                    per_concurrency_budget_pct,
                },
            );
        }

        Ok(Thresholds { default, ops })
    }

    /// Resolve the budget for one `(op, concurrency)` cell. Precedence per field:
    /// `op.<op>.concurrency.<C>` (budget only) > `op.<op>` > `[default]`.
    pub fn resolve(&self, op: OpName, concurrency: usize) -> ResolvedBudget {
        let over = self.ops.get(&op);
        let budget_pct = over
            .and_then(|o| o.per_concurrency_budget_pct.get(&concurrency).copied())
            .or_else(|| over.and_then(|o| o.budget_pct))
            .unwrap_or(self.default.budget_pct);
        let floor_ms = over
            .and_then(|o| o.floor_ms)
            .unwrap_or(self.default.floor_ms);
        let metric = over
            .and_then(|o| o.metric)
            .unwrap_or(self.default.metric);
        ResolvedBudget {
            metric,
            budget_pct,
            floor_ms,
        }
    }
}

fn check_metric(m: Metric) -> Result<Metric, String> {
    match m {
        Metric::P50 => Ok(m),
        Metric::Throughput | Metric::Both => Err(
            "metric must be \"p50\" — \"throughput\"/\"both\" are reserved for a later iteration"
                .to_string(),
        ),
    }
}

fn check_budget(v: f64, what: &str) -> Result<f64, String> {
    if !v.is_finite() || v < 0.0 {
        return Err(format!("{what} must be a finite, non-negative percent (got {v})"));
    }
    Ok(v)
}

fn check_floor(v: f64, what: &str) -> Result<f64, String> {
    if !v.is_finite() || v < 0.0 {
        return Err(format!("{what} must be a finite, non-negative number of ms (got {v})"));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_defaults_are_10pct_half_ms_p50() {
        let t = Thresholds::builtin();
        let b = t.resolve(OpName::MatchByIndex, 32);
        assert_eq!(b.budget_pct, 10.0);
        assert_eq!(b.floor_ms, 0.5);
        assert_eq!(b.metric, Metric::P50);
    }

    #[test]
    fn verdict_faster_or_within_budget_is_ok_beyond_is_regressed() {
        let b = ResolvedBudget { metric: Metric::P50, budget_pct: 10.0, floor_ms: 0.0 };
        // faster
        assert_eq!(b.verdict(2.0, 1.5), Verdict::Ok);
        // exactly at budget (10% => 2.2) is not "more than" => Ok
        assert_eq!(b.verdict(2.0, 2.2), Verdict::Ok);
        // just over budget => Regressed
        assert_eq!(b.verdict(2.0, 2.21), Verdict::Regressed);
        // zero/non-finite baseline => N/A
        assert_eq!(b.verdict(0.0, 1.0), Verdict::NotApplicable);
        assert_eq!(b.verdict(f64::NAN, 1.0), Verdict::NotApplicable);
        // zero/negative/non-finite candidate => N/A (invalid latency, not a speedup)
        assert_eq!(b.verdict(1.0, 0.0), Verdict::NotApplicable);
        assert_eq!(b.verdict(1.0, -1.0), Verdict::NotApplicable);
        assert_eq!(b.verdict(1.0, f64::INFINITY), Verdict::NotApplicable);
    }

    #[test]
    fn floor_suppresses_tiny_absolute_slowdowns() {
        // 50% slower but only +0.15ms absolute; floor 0.5ms suppresses it.
        let b = ResolvedBudget { metric: Metric::P50, budget_pct: 10.0, floor_ms: 0.5 };
        assert_eq!(b.verdict(0.30, 0.45), Verdict::Ok);
        // same relative slowdown but a large absolute delta => Regressed.
        assert_eq!(b.verdict(3.0, 4.5), Verdict::Regressed);
    }

    #[test]
    fn precedence_per_concurrency_over_op_over_default() {
        let cfg = r#"
[default]
budget_pct = 10.0
floor_ms = 0.5

[op.match_by_index]
budget_pct = 20.0
floor_ms = 0.1
concurrency = { 32 = 40.0 }
"#;
        let t = Thresholds::from_toml_str(cfg).unwrap();
        // default op falls back to [default]
        assert_eq!(t.resolve(OpName::ReturnConst, 1).budget_pct, 10.0);
        // per-op override
        let r16 = t.resolve(OpName::MatchByIndex, 16);
        assert_eq!(r16.budget_pct, 20.0);
        assert_eq!(r16.floor_ms, 0.1);
        // per-op×concurrency override wins for C=32 (floor still from the op level)
        let r32 = t.resolve(OpName::MatchByIndex, 32);
        assert_eq!(r32.budget_pct, 40.0);
        assert_eq!(r32.floor_ms, 0.1);
    }

    #[test]
    fn rejects_unknown_op_key() {
        let err = Thresholds::from_toml_str("[op.not_a_real_op]\nbudget_pct = 5.0\n").unwrap_err();
        assert!(err.contains("unknown operation 'not_a_real_op'"), "{err}");
    }

    #[test]
    fn rejects_invalid_budget_and_floor_and_metric() {
        assert!(Thresholds::from_toml_str("[default]\nbudget_pct = -1.0\n")
            .unwrap_err()
            .contains("non-negative percent"));
        assert!(Thresholds::from_toml_str("[default]\nfloor_ms = -0.1\n")
            .unwrap_err()
            .contains("non-negative number of ms"));
        assert!(Thresholds::from_toml_str("[default]\nmetric = \"throughput\"\n")
            .unwrap_err()
            .contains("reserved for a later iteration"));
    }

    #[test]
    fn rejects_non_integer_concurrency_key() {
        let err = Thresholds::from_toml_str(
            "[op.match_by_index]\nconcurrency = { fast = 40.0 }\n",
        )
        .unwrap_err();
        assert!(err.contains("non-integer key 'fast'"), "{err}");
    }

    #[test]
    fn rejects_zero_concurrency_key() {
        let err = Thresholds::from_toml_str(
            "[op.match_by_index]\nconcurrency = { 0 = 40.0 }\n",
        )
        .unwrap_err();
        assert!(err.contains("invalid level 0"), "{err}");
    }

    #[test]
    fn rejects_unknown_top_level_key() {
        // deny_unknown_fields guards typos like `budjet_pct`.
        assert!(Thresholds::from_toml_str("[default]\nbudjet_pct = 10.0\n").is_err());
    }

    #[test]
    fn verdict_emoji_covers_all_variants() {
        assert_eq!(Verdict::Ok.emoji(), "🟢");
        assert_eq!(Verdict::Regressed.emoji(), "🔴");
        assert_eq!(Verdict::NotApplicable.emoji(), "N/A");
    }

    #[test]
    fn default_equals_builtin() {
        let b = Thresholds::default().resolve(OpName::MatchByIndex, 1);
        assert_eq!(b.budget_pct, DEFAULT_BUDGET_PCT);
        assert_eq!(b.floor_ms, DEFAULT_FLOOR_MS);
        assert_eq!(b.metric, Metric::P50);
    }

    #[test]
    fn from_file_reads_validates_and_errors_on_missing() {
        let dir = std::env::temp_dir();
        // Unique per (process, nanos) so parallel tests can't collide on the temp file name.
        let uniq = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = dir.join(format!("thr-{}-{}.toml", std::process::id(), uniq));
        // op-level metric override exercises the op metric validation path.
        std::fs::write(&p, "[op.match_by_index]\nmetric = \"p50\"\nbudget_pct = 7.0\n").unwrap();
        let t = Thresholds::from_file(p.to_str().unwrap()).unwrap();
        assert_eq!(t.resolve(OpName::MatchByIndex, 1).budget_pct, 7.0);
        let _ = std::fs::remove_file(&p);
        assert!(Thresholds::from_file("/no/such/thresholds-xyz.toml").is_err());
    }
}
