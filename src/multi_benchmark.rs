use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::scenario::Size;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiBenchmarkScenario {
    pub graph_prefix: String,
    pub n_small: usize,
    pub n_medium: usize,
    pub n_large: usize,
    pub graph_window_pct: f64,
    #[serde(default)]
    pub graph_window_size: Option<usize>,
    #[serde(default)]
    pub query_window_pct: Option<f64>,
    #[serde(default)]
    pub query_window_size: Option<usize>,
    #[serde(default = "default_batch_size")]
    pub load_batch_size: usize,
    #[serde(default)]
    pub target_label: Option<String>,
    #[serde(default)]
    pub memory_limit: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GraphDeploymentPlan {
    pub graph_name: String,
    pub size: Size,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollingWindow {
    pub start: usize,
    pub end: usize,
}

fn default_batch_size() -> usize {
    5_000
}

impl MultiBenchmarkScenario {
    pub fn validate(&self) -> BenchmarkResult<()> {
        if self.graph_prefix.trim().is_empty() {
            return Err(OtherError(
                "multi-benchmark scenario requires a non-empty graph_prefix".to_string(),
            ));
        }
        if self.n_small + self.n_medium + self.n_large == 0 {
            return Err(OtherError(
                "multi-benchmark scenario requires at least one graph (n_small/n_medium/n_large)"
                    .to_string(),
            ));
        }
        validate_percent("graph_window_pct", self.graph_window_pct)?;
        if let Some(pct) = self.query_window_pct {
            validate_percent("query_window_pct", pct)?;
        }
        if matches!(self.graph_window_size, Some(0)) {
            return Err(OtherError(
                "graph_window_size must be greater than zero when provided".to_string(),
            ));
        }
        if matches!(self.query_window_size, Some(0)) {
            return Err(OtherError(
                "query_window_size must be greater than zero when provided".to_string(),
            ));
        }
        if self.load_batch_size == 0 {
            return Err(OtherError(
                "load_batch_size must be greater than zero".to_string(),
            ));
        }
        Ok(())
    }

    pub fn expanded_graph_plan(&self) -> Vec<GraphDeploymentPlan> {
        let mut plans = Vec::with_capacity(self.n_small + self.n_medium + self.n_large);
        append_graphs(
            &mut plans,
            self.graph_prefix.as_str(),
            "small",
            self.n_small,
            Size::Small,
        );
        append_graphs(
            &mut plans,
            self.graph_prefix.as_str(),
            "medium",
            self.n_medium,
            Size::Medium,
        );
        append_graphs(
            &mut plans,
            self.graph_prefix.as_str(),
            "large",
            self.n_large,
            Size::Large,
        );
        plans
    }

    pub fn query_window_pct_effective(&self) -> f64 {
        self.query_window_pct.unwrap_or(self.graph_window_pct)
    }
}

pub fn load_scenario_config(path: &str) -> BenchmarkResult<MultiBenchmarkScenario> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        OtherError(format!(
            "failed reading multi scenario config '{}': {}",
            path, e
        ))
    })?;
    let ext = Path::new(path)
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let parsed = if ext == "yaml" || ext == "yml" {
        serde_yaml::from_str::<MultiBenchmarkScenario>(&raw).map_err(|e| {
            OtherError(format!(
                "failed parsing YAML multi scenario config '{}': {}",
                path, e
            ))
        })?
    } else {
        serde_json::from_str::<MultiBenchmarkScenario>(&raw).map_err(|e| {
            OtherError(format!(
                "failed parsing JSON multi scenario config '{}': {}",
                path, e
            ))
        })?
    };

    parsed.validate()?;
    Ok(parsed)
}

pub fn rolling_windows(
    total_items: usize,
    window_pct: f64,
    fixed_window_size: Option<usize>,
) -> BenchmarkResult<Vec<RollingWindow>> {
    if total_items == 0 {
        return Ok(Vec::new());
    }
    validate_percent("window_pct", window_pct)?;
    let window_size = match fixed_window_size {
        Some(size) if size > 0 => size,
        Some(_) => {
            return Err(OtherError(
                "window size must be greater than zero".to_string(),
            ));
        }
        None => (((total_items as f64) * (window_pct / 100.0)).floor() as usize).max(1),
    };

    let mut windows = Vec::new();
    let mut start = 0usize;
    while start < total_items {
        let end = (start + window_size).min(total_items);
        windows.push(RollingWindow { start, end });
        start = end;
    }

    Ok(windows)
}

fn append_graphs(
    out: &mut Vec<GraphDeploymentPlan>,
    prefix: &str,
    bucket: &str,
    count: usize,
    size: Size,
) {
    for idx in 0..count {
        out.push(GraphDeploymentPlan {
            graph_name: format!("{}-{}-{:03}", prefix, bucket, idx),
            size,
        });
    }
}

fn validate_percent(
    name: &str,
    value: f64,
) -> BenchmarkResult<()> {
    if (0.0..=100.0).contains(&value) && value > 0.0 {
        return Ok(());
    }

    Err(OtherError(format!(
        "{} must be > 0 and <= 100, got {}",
        name, value
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_graph_plan() {
        let cfg = MultiBenchmarkScenario {
            graph_prefix: "offload".to_string(),
            n_small: 2,
            n_medium: 1,
            n_large: 1,
            graph_window_pct: 25.0,
            graph_window_size: None,
            query_window_pct: None,
            query_window_size: None,
            load_batch_size: 1_000,
            target_label: None,
            memory_limit: None,
        };

        let plan = cfg.expanded_graph_plan();
        assert_eq!(plan.len(), 4);
        assert_eq!(plan[0].graph_name, "offload-small-000");
        assert_eq!(plan[1].graph_name, "offload-small-001");
        assert_eq!(plan[2].graph_name, "offload-medium-000");
        assert_eq!(plan[3].graph_name, "offload-large-000");
    }

    #[test]
    fn test_rolling_windows_pct() {
        let windows = rolling_windows(10, 30.0, None).expect("windows");
        assert_eq!(
            windows,
            vec![
                RollingWindow { start: 0, end: 3 },
                RollingWindow { start: 3, end: 6 },
                RollingWindow { start: 6, end: 9 },
                RollingWindow { start: 9, end: 10 },
            ]
        );
    }

    #[test]
    fn test_rolling_windows_fixed_size() {
        let windows = rolling_windows(7, 50.0, Some(2)).expect("windows");
        assert_eq!(
            windows,
            vec![
                RollingWindow { start: 0, end: 2 },
                RollingWindow { start: 2, end: 4 },
                RollingWindow { start: 4, end: 6 },
                RollingWindow { start: 6, end: 7 },
            ]
        );
    }

    #[test]
    fn test_validate_rejects_invalid_percentages() {
        let cfg = MultiBenchmarkScenario {
            graph_prefix: "offload".to_string(),
            n_small: 1,
            n_medium: 0,
            n_large: 0,
            graph_window_pct: 0.0,
            graph_window_size: None,
            query_window_pct: Some(120.0),
            query_window_size: None,
            load_batch_size: 1_000,
            target_label: None,
            memory_limit: None,
        };
        assert!(cfg.validate().is_err());
    }
}
