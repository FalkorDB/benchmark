//! Robust latency statistics for the synthetic benchmark.
//!
//! Computes the summary each measured metric reports (min/mean/median/p90/p95/p99/std-dev) after
//! removing *severe* outliers with Tukey fences (values outside `[Q1 - 3·IQR, Q3 + 3·IQR]`), the
//! same idea Criterion.rs uses to keep a few pathological samples from dominating an estimate.

use serde::{Deserialize, Serialize};

/// Summary statistics for one metric (e.g. `server_ms` or `total_ms`) over a set of samples,
/// after severe-outlier removal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// Number of samples retained after severe-outlier removal.
    pub n: usize,
    /// Number of severe outliers removed.
    pub removed: usize,
    pub min: f64,
    pub mean: f64,
    pub median: f64,
    pub p90: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
    /// Population standard deviation of the retained samples.
    pub stddev: f64,
}

impl Summary {
    /// **Sample** standard deviation of the retained samples — Bessel-corrected (n−1 denominator):
    /// `s = stddev · √(n/(n−1))`, exact because the stored [`Self::stddev`] is the population σ
    /// over the same `n` values. The n−1 denominator is deliberate: the retained samples are a
    /// *sample* of the operation's latency distribution, and the population formula would
    /// understate its dispersion (most at small `n`). `None` when `n < 2` (dispersion of a single
    /// sample is undefined) or the stored σ is negative (impossible from this tool — only a
    /// corrupted/foreign report) or the result is non-finite — never a nonsensical value that
    /// could poison a report.
    pub fn sample_stddev(&self) -> Option<f64> {
        if self.n < 2 || self.stddev < 0.0 {
            return None;
        }
        let s = self.stddev * (self.n as f64 / (self.n as f64 - 1.0)).sqrt();
        s.is_finite().then_some(s)
    }

    /// Coefficient of variation, in percent: `100 · sample_stddev / mean` — the dispersion of the
    /// retained samples relative to their own scale, so cells of very different magnitudes are
    /// comparable at a glance. `None` whenever [`Self::sample_stddev`] is undefined or the mean is
    /// non-finite or non-positive (a ratio to a ≤ 0 denominator is meaningless for latencies).
    pub fn cv_pct(&self) -> Option<f64> {
        let s = self.sample_stddev()?;
        if !(self.mean.is_finite() && self.mean > 0.0) {
            return None;
        }
        let cv = s / self.mean * 100.0;
        cv.is_finite().then_some(cv)
    }
}

/// The Tukey multiplier that classifies a *severe* outlier (`3·IQR`); mild would be `1.5`.
const SEVERE_IQR_MULTIPLIER: f64 = 3.0;

/// Linear-interpolation percentile (`p` in `[0, 100]`) over an already-sorted slice.
///
/// Uses the same "fraction of the way between order statistics" definition as NumPy's default
/// (`linear`) method. `sorted` must be non-empty and sorted ascending.
fn percentile_sorted(
    sorted: &[f64],
    p: f64,
) -> f64 {
    debug_assert!(!sorted.is_empty());
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = (p / 100.0) * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        return sorted[lo];
    }
    let frac = rank - lo as f64;
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
}

/// Compute the severe-outlier fence `[Q1 - 3·IQR, Q3 + 3·IQR]` for a set of samples.
///
/// Returns `None` when there are fewer than 4 finite samples (IQR is not meaningful), meaning
/// "no fence — keep everything".
pub fn severe_fence(samples: &[f64]) -> Option<(f64, f64)> {
    let mut values: Vec<f64> = samples.iter().copied().filter(|v| v.is_finite()).collect();
    values.sort_by(|a, b| a.partial_cmp(b).expect("finite values sort"));
    if values.len() < 4 {
        return None;
    }
    let q1 = percentile_sorted(&values, 25.0);
    let q3 = percentile_sorted(&values, 75.0);
    let iqr = q3 - q1;
    // A zero-width IQR (Q1 == Q3) gives a degenerate fence that would classify every value not
    // exactly at the quartiles as a severe outlier — pathological for quantized/low-resolution
    // distributions (common for very fast ops). Treat it as "no meaningful fence".
    if iqr <= 0.0 {
        return None;
    }
    Some((
        q1 - SEVERE_IQR_MULTIPLIER * iqr,
        q3 + SEVERE_IQR_MULTIPLIER * iqr,
    ))
}

/// Summarize an already-filtered set of `kept` values, recording `removed` (the number of samples
/// excluded before this call). Returns `None` when `kept` has no finite values.
pub fn summarize_kept(
    kept: &[f64],
    removed: usize,
) -> Option<Summary> {
    let mut vals: Vec<f64> = kept.iter().copied().filter(|v| v.is_finite()).collect();
    if vals.is_empty() {
        return None;
    }
    vals.sort_by(|a, b| a.partial_cmp(b).expect("finite values sort"));
    let n = vals.len();
    let sum: f64 = vals.iter().sum();
    let mean = sum / n as f64;
    let variance = vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64;
    Some(Summary {
        n,
        removed,
        min: vals[0],
        mean,
        median: percentile_sorted(&vals, 50.0),
        p90: percentile_sorted(&vals, 90.0),
        p95: percentile_sorted(&vals, 95.0),
        p99: percentile_sorted(&vals, 99.0),
        max: vals[n - 1],
        stddev: variance.sqrt(),
    })
}

/// Compute a [`Summary`] over `samples`, removing severe outliers first.
///
/// Returns `None` when no finite samples remain (nothing meaningful to summarize).
pub fn summarize(samples: &[f64]) -> Option<Summary> {
    let finite: Vec<f64> = samples.iter().copied().filter(|v| v.is_finite()).collect();
    match severe_fence(&finite) {
        Some((lo, hi)) => {
            let kept: Vec<f64> = finite
                .iter()
                .copied()
                .filter(|&v| v >= lo && v <= hi)
                .collect();
            let removed = finite.len() - kept.len();
            summarize_kept(&kept, removed)
        }
        None => summarize_kept(&finite, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(
        a: f64,
        b: f64,
    ) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn percentile_matches_known_values() {
        let v: Vec<f64> = (1..=10).map(|i| i as f64).collect(); // 1..=10
        assert!(approx(percentile_sorted(&v, 0.0), 1.0));
        assert!(approx(percentile_sorted(&v, 100.0), 10.0));
        // NumPy linear: p50 of 1..=10 is 5.5.
        assert!(approx(percentile_sorted(&v, 50.0), 5.5));
        // p90 interpolates between the 9th and 10th values: 9 + 0.1*(10-9) = 9.1.
        assert!(approx(percentile_sorted(&v, 90.0), 9.1));
        // p95 interpolates 9 + 0.55*(10-9) = 9.55.
        assert!(approx(percentile_sorted(&v, 95.0), 9.55));
    }

    #[test]
    fn percentile_single_value() {
        assert!(approx(percentile_sorted(&[42.0], 25.0), 42.0));
        assert!(approx(percentile_sorted(&[42.0], 99.0), 42.0));
    }

    #[test]
    fn summarize_basic_metrics() {
        let s = summarize(&[1.0, 2.0, 3.0, 4.0, 5.0]).unwrap();
        assert_eq!(s.n, 5);
        assert_eq!(s.removed, 0);
        assert!(approx(s.min, 1.0));
        assert!(approx(s.max, 5.0));
        assert!(approx(s.mean, 3.0));
        assert!(approx(s.median, 3.0));
    }

    #[test]
    fn removes_severe_outlier_but_keeps_normal_spread() {
        // 20 tightly-clustered points plus one huge spike; the spike must be dropped.
        let mut data: Vec<f64> = (0..20).map(|i| 100.0 + i as f64).collect();
        data.push(100_000.0);
        let s = summarize(&data).unwrap();
        assert_eq!(s.removed, 1, "the severe spike should be removed");
        assert_eq!(s.n, 20);
        assert!(s.max < 1_000.0, "max should reflect the retained cluster");
    }

    #[test]
    fn does_not_remove_mild_variation() {
        // A moderate spread with no point beyond 3*IQR keeps every sample.
        let data = vec![10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 17.0];
        let s = summarize(&data).unwrap();
        assert_eq!(s.removed, 0);
        assert_eq!(s.n, data.len());
    }

    #[test]
    fn tiny_sample_keeps_everything() {
        // Fewer than 4 samples: IQR is not meaningful, so nothing is removed.
        let s = summarize(&[1.0, 1000.0, 2.0]).unwrap();
        assert_eq!(s.removed, 0);
        assert_eq!(s.n, 3);
    }

    #[test]
    fn zero_width_iqr_keeps_everything() {
        // A quantized distribution where Q1 == Q3 (IQR == 0) must not classify the off-quartile
        // values as severe outliers and discard most of the sample.
        let mut data = vec![0.01; 90];
        data.extend(std::iter::repeat_n(0.02, 10));
        assert!(severe_fence(&data).is_none());
        let s = summarize(&data).unwrap();
        assert_eq!(s.removed, 0);
        assert_eq!(s.n, 100);
    }

    #[test]
    fn empty_and_nonfinite_samples() {
        assert!(summarize(&[]).is_none());
        assert!(summarize(&[f64::NAN, f64::INFINITY]).is_none());
        // Finite values survive alongside non-finite ones.
        let s = summarize(&[f64::NAN, 1.0, 2.0]).unwrap();
        assert_eq!(s.n, 2);
    }

    #[test]
    fn stddev_of_constant_is_zero() {
        let s = summarize(&[5.0, 5.0, 5.0, 5.0, 5.0]).unwrap();
        assert!(approx(s.stddev, 0.0));
    }

    #[test]
    fn sample_stddev_applies_bessel_correction() {
        // For [2, 4, 4, 4, 5, 5, 7, 9]: population σ = 2, sample s = √(32/7) ≈ 2.13809.
        let s = summarize(&[2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0]).unwrap();
        assert!(approx(s.stddev, 2.0));
        assert!(approx(s.sample_stddev().unwrap(), (32.0f64 / 7.0).sqrt()));
    }

    #[test]
    fn sample_stddev_undefined_below_two_samples() {
        // n = 1: the n−1 denominator would divide by zero — must be None, not a panic/NaN.
        let s = summarize(&[42.0]).unwrap();
        assert_eq!(s.n, 1);
        assert_eq!(s.sample_stddev(), None);
        assert_eq!(s.cv_pct(), None);
        // A synthetic n = 0 summary (never produced by `summarize`, but reachable through serde).
        let zero = Summary { n: 0, ..s };
        assert_eq!(zero.sample_stddev(), None);
        assert_eq!(zero.cv_pct(), None);
        // A negative stored σ (corrupted/foreign report): invalid, not a negative "deviation".
        let corrupt = Summary { n: 4, stddev: -1.0, ..s };
        assert_eq!(corrupt.sample_stddev(), None);
        assert_eq!(corrupt.cv_pct(), None);
    }

    #[test]
    fn cv_pct_is_relative_sample_dispersion() {
        // mean = 5, sample s = √(32/7) → CV = 100·s/5 ≈ 42.76%.
        let s = summarize(&[2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0]).unwrap();
        assert!(approx(s.cv_pct().unwrap(), (32.0f64 / 7.0).sqrt() / 5.0 * 100.0));
        // A constant series has zero dispersion → CV 0%.
        let flat = summarize(&[5.0, 5.0, 5.0, 5.0]).unwrap();
        assert!(approx(flat.cv_pct().unwrap(), 0.0));
    }

    #[test]
    fn cv_pct_undefined_for_nonpositive_or_nonfinite_mean() {
        let base = summarize(&[1.0, 2.0, 3.0]).unwrap();
        // Zeroed/negative means (an invalid server-time summary) must not divide.
        assert_eq!(Summary { mean: 0.0, ..base.clone() }.cv_pct(), None);
        assert_eq!(Summary { mean: -1.0, ..base.clone() }.cv_pct(), None);
        assert_eq!(Summary { mean: f64::NAN, ..base.clone() }.cv_pct(), None);
        // A non-finite stored σ (hand-crafted/corrupt) degrades to None, never NaN.
        assert_eq!(Summary { stddev: f64::NAN, ..base }.sample_stddev(), None);
    }
}
