//! Robust latency statistics for the synthetic benchmark.
//!
//! Computes the summary each measured metric reports (min/mean/median/p90/p99/std-dev) after
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
    pub p99: f64,
    pub max: f64,
    /// Population standard deviation of the retained samples.
    pub stddev: f64,
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

/// Remove severe (`> 3·IQR` beyond the quartiles) outliers, returning the retained values (sorted)
/// and the number removed. With fewer than 4 samples the IQR is not meaningful, so nothing is
/// removed.
fn remove_severe_outliers(mut values: Vec<f64>) -> (Vec<f64>, usize) {
    values.retain(|v| v.is_finite());
    values.sort_by(|a, b| a.partial_cmp(b).expect("finite values sort"));
    if values.len() < 4 {
        return (values, 0);
    }
    let q1 = percentile_sorted(&values, 25.0);
    let q3 = percentile_sorted(&values, 75.0);
    let iqr = q3 - q1;
    let lower = q1 - SEVERE_IQR_MULTIPLIER * iqr;
    let upper = q3 + SEVERE_IQR_MULTIPLIER * iqr;
    let before = values.len();
    let kept: Vec<f64> = values
        .into_iter()
        .filter(|&v| v >= lower && v <= upper)
        .collect();
    let removed = before - kept.len();
    (kept, removed)
}

/// Compute a [`Summary`] over `samples`, removing severe outliers first.
///
/// Returns `None` when no finite samples remain (nothing meaningful to summarize).
pub fn summarize(samples: &[f64]) -> Option<Summary> {
    let (kept, removed) = remove_severe_outliers(samples.to_vec());
    if kept.is_empty() {
        return None;
    }
    let n = kept.len();
    let sum: f64 = kept.iter().sum();
    let mean = sum / n as f64;
    let variance = kept.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64;
    Some(Summary {
        n,
        removed,
        min: kept[0],
        mean,
        median: percentile_sorted(&kept, 50.0),
        p90: percentile_sorted(&kept, 90.0),
        p99: percentile_sorted(&kept, 99.0),
        max: kept[n - 1],
        stddev: variance.sqrt(),
    })
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
}
