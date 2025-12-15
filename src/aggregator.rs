use benchmark::error::BenchmarkError::OtherError;
use benchmark::error::BenchmarkResult;
use benchmark::scenario::{Name, Size, Spec, Vendor};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Clone)]
struct RunResultsMeta {
    vendor: String,
    dataset: String,
    queries_file: String,
    queries_count: usize,
    parallel: usize,
    mps: usize,
    simulate_ms: Option<usize>,
    endpoint: Option<String>,
    started_at_epoch_secs: u64,
    finished_at_epoch_secs: u64,
    elapsed_ms: u128,
}

#[derive(Debug, Serialize)]
struct UiLatency {
    p50: String,
    p95: String,
    p99: String,
}

#[derive(Debug, Serialize)]
struct UiResult {
    #[serde(rename = "deadline-offset")]
    deadline_offset: String,
    #[serde(rename = "actual-messages-per-second")]
    actual_messages_per_second: f64,
    latency: UiLatency,
    #[serde(rename = "cpu-usage")]
    cpu_usage: f64,
    #[serde(rename = "ram-usage")]
    ram_usage: String,
    errors: u64,
    #[serde(rename = "successful-requests")]
    successful_requests: u64,
}

#[derive(Debug, Serialize)]
struct UiRun {
    vendor: String,
    #[serde(rename = "read-write-ratio")]
    read_write_ratio: f64,
    clients: u64,
    platform: String,
    #[serde(rename = "target-messages-per-second")]
    target_messages_per_second: u64,
    edges: u64,
    relationships: u64,
    result: UiResult,
}

#[derive(Debug, Serialize)]
struct UiSummary {
    runs: Vec<UiRun>,
    // NOTE: The UI code currently uses a misspelled key: "unrealstic".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unrealstic: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    platforms: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Copy)]
enum HistogramKind {
    Success,
    Error,
}

#[derive(Debug)]
struct HistogramData {
    // cumulative counts per le bucket
    buckets: Vec<(f64, f64)>,
    count: f64,
}

pub fn aggregate_results(
    results_dir: &str,
    out_dir: &str,
) -> BenchmarkResult<()> {
    let results_dir = PathBuf::from(results_dir);
    if !results_dir.exists() {
        return Err(OtherError(format!(
            "results-dir does not exist: {}",
            results_dir.display()
        )));
    }

    let out_dir = PathBuf::from(out_dir);
    fs::create_dir_all(&out_dir).map_err(|e| {
        OtherError(format!(
            "Failed creating out-dir {}: {}",
            out_dir.display(),
            e
        ))
    })?;

    // Required baseline vendor
    let falkor = load_vendor(&results_dir, Vendor::Falkor)?;

    // neo4j vs falkor
    if let Ok(neo4j) = load_vendor(&results_dir, Vendor::Neo4j) {
        let summary = make_summary(&[falkor.clone(), neo4j])?;
        let out_path = out_dir.join("neo4j_vs_falkordb.json");
        write_summary(&out_path, &summary)?;
    }

    // memgraph vs falkor
    if let Ok(memgraph) = load_vendor(&results_dir, Vendor::Memgraph) {
        let summary = make_summary(&[falkor, memgraph])?;
        let out_path = out_dir.join("memgraph_vs_falkordb.json");
        write_summary(&out_path, &summary)?;
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct VendorArtifacts {
    vendor: Vendor,
    meta: RunResultsMeta,
    metrics_text: String,
}

fn load_vendor(
    results_dir: &Path,
    vendor: Vendor,
) -> BenchmarkResult<VendorArtifacts> {
    let vendor_dir = results_dir.join(vendor.to_string());
    let meta_path = vendor_dir.join("meta.json");
    let metrics_path = vendor_dir.join("metrics.prom");

    if !meta_path.exists() {
        return Err(OtherError(format!(
            "Missing meta.json for vendor {} at {}",
            vendor,
            meta_path.display()
        )));
    }
    if !metrics_path.exists() {
        return Err(OtherError(format!(
            "Missing metrics.prom for vendor {} at {}",
            vendor,
            metrics_path.display()
        )));
    }

    let meta_raw = fs::read_to_string(&meta_path)
        .map_err(|e| OtherError(format!("Failed reading {}: {}", meta_path.display(), e)))?;
    let meta: RunResultsMeta = serde_json::from_str(&meta_raw)
        .map_err(|e| OtherError(format!("Failed parsing {}: {}", meta_path.display(), e)))?;

    let metrics_text = fs::read_to_string(&metrics_path)
        .map_err(|e| OtherError(format!("Failed reading {}: {}", metrics_path.display(), e)))?;

    Ok(VendorArtifacts {
        vendor,
        meta,
        metrics_text,
    })
}

fn write_summary(
    path: &Path,
    summary: &UiSummary,
) -> BenchmarkResult<()> {
    let json = serde_json::to_string_pretty(summary)
        .map_err(|e| OtherError(format!("Failed serializing summary: {}", e)))?;
    fs::write(path, json)
        .map_err(|e| OtherError(format!("Failed writing {}: {}", path.display(), e)))?;
    Ok(())
}

fn make_summary(vendors: &[VendorArtifacts]) -> BenchmarkResult<UiSummary> {
    let mut runs = Vec::new();

    for v in vendors {
        runs.push(build_ui_run(v)?);
    }

    Ok(UiSummary {
        runs,
        unrealstic: vec![],
        platforms: vec![],
    })
}

fn parse_size(s: &str) -> BenchmarkResult<Size> {
    match s.to_lowercase().as_str() {
        "small" => Ok(Size::Small),
        "medium" => Ok(Size::Medium),
        "large" => Ok(Size::Large),
        other => Err(OtherError(format!("Unknown dataset size: {}", other))),
    }
}

fn detected_platform() -> String {
    match std::env::consts::ARCH {
        "aarch64" | "arm64" => "arm".to_string(),
        "x86_64" => "intel".to_string(),
        _ => std::env::consts::ARCH.to_string(),
    }
}

fn build_ui_run(v: &VendorArtifacts) -> BenchmarkResult<UiRun> {
    let dataset = parse_size(&v.meta.dataset)?;
    let spec = Spec::new(Name::Users, dataset, v.vendor);

    let metrics = MetricsIndex::from_prometheus_text(&v.metrics_text)?;

    let success_hist = metrics.histogram(v.vendor, HistogramKind::Success)?;
    let error_hist = metrics.histogram(v.vendor, HistogramKind::Error)?;

    let p50 = histogram_quantile_seconds(&success_hist, 0.50);
    let p95 = histogram_quantile_seconds(&success_hist, 0.95);
    let p99 = histogram_quantile_seconds(&success_hist, 0.99);

    let elapsed_secs = (v.meta.elapsed_ms as f64) / 1000.0;
    let actual_mps = if elapsed_secs > 0.0 {
        (success_hist.count / elapsed_secs).max(0.0)
    } else {
        0.0
    };

    let (cpu_usage, ram_usage) = metrics.vendor_cpu_mem(v.vendor);

    Ok(UiRun {
        vendor: vendor_id(v.vendor),
        read_write_ratio: 0.0,
        clients: v.meta.parallel as u64,
        platform: detected_platform(),
        target_messages_per_second: v.meta.mps as u64,
        edges: spec.vertices,
        relationships: spec.edges,
        result: UiResult {
            deadline_offset: "0ms".to_string(),
            actual_messages_per_second: actual_mps,
            latency: UiLatency {
                p50: format_ms(p50 * 1000.0),
                p95: format_ms(p95 * 1000.0),
                p99: format_ms(p99 * 1000.0),
            },
            cpu_usage,
            ram_usage,
            errors: error_hist.count.round().max(0.0) as u64,
            successful_requests: success_hist.count.round().max(0.0) as u64,
        },
    })
}

fn vendor_id(vendor: Vendor) -> String {
    match vendor {
        Vendor::Falkor => "falkordb".to_string(),
        Vendor::Neo4j => "neo4j".to_string(),
        Vendor::Memgraph => "memgraph".to_string(),
    }
}

fn format_ms(ms: f64) -> String {
    if !ms.is_finite() || ms <= 0.0 {
        return "0ms".to_string();
    }

    if ms >= 1000.0 {
        let s = ms / 1000.0;
        // Keep it readable; UI can parse both "ms" and "s".
        return format!("{:.2}s", s);
    }

    format!("{}ms", ms.round() as u64)
}

fn histogram_quantile_seconds(
    hist: &HistogramData,
    q: f64,
) -> f64 {
    if hist.count <= 0.0 {
        return 0.0;
    }

    let target = hist.count * q;
    for (le, c) in &hist.buckets {
        if *c >= target {
            return *le;
        }
    }

    // Fallback to last bucket boundary
    hist.buckets.last().map(|(le, _)| *le).unwrap_or(0.0)
}

#[derive(Debug, Default)]
struct MetricsIndex {
    // name -> (labels_key -> value)
    samples: BTreeMap<String, Vec<(BTreeMap<String, String>, f64)>>,
}

impl MetricsIndex {
    fn from_prometheus_text(text: &str) -> BenchmarkResult<Self> {
        let mut idx = MetricsIndex::default();

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Format is either:
            //   name value
            //   name{a="b",c="d"} value
            let (lhs, rhs) = match line.rsplit_once(' ') {
                Some(v) => v,
                None => continue,
            };

            let value: f64 = match rhs.parse() {
                Ok(v) => v,
                Err(_) => continue,
            };

            let (name, labels) = parse_metric_lhs(lhs);
            idx.samples.entry(name).or_default().push((labels, value));
        }

        Ok(idx)
    }

    fn histogram(
        &self,
        vendor: Vendor,
        kind: HistogramKind,
    ) -> BenchmarkResult<HistogramData> {
        let prefix = match vendor {
            Vendor::Falkor => "falkordb",
            Vendor::Neo4j => "neo4j",
            Vendor::Memgraph => "memgraph",
        };

        let base = match kind {
            HistogramKind::Success => format!("{}_response_time_success_histogram", prefix),
            HistogramKind::Error => format!("{}_response_time_error_histogram", prefix),
        };

        let count_name = format!("{}_count", base);
        let bucket_name = format!("{}_bucket", base);

        let count = self.get_single_value(&count_name).unwrap_or(0.0);

        let mut buckets = Vec::new();
        if let Some(samples) = self.samples.get(&bucket_name) {
            for (labels, value) in samples {
                if let Some(le) = labels.get("le") {
                    if le == "+Inf" {
                        // Skip; quantile fallback will use last finite bucket
                        continue;
                    }
                    if let Ok(boundary) = le.parse::<f64>() {
                        buckets.push((boundary, *value));
                    }
                }
            }
        }

        buckets.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        Ok(HistogramData { buckets, count })
    }

    fn vendor_cpu_mem(
        &self,
        vendor: Vendor,
    ) -> (f64, String) {
        let (cpu_name, mem_name) = match vendor {
            Vendor::Falkor => ("falkor_cpu_usage", "falkor_memory_usage"),
            Vendor::Neo4j => ("neo4j_cpu_usage", "neo4j_memory_usage"),
            Vendor::Memgraph => ("memgraph_cpu_usage", "memgraph_memory_usage"),
        };

        let cpu = self.get_single_value(cpu_name).unwrap_or(0.0);
        let mem_kib = self.get_single_value(mem_name).unwrap_or(0.0);

        (cpu, format_mem_from_kib(mem_kib))
    }

    fn get_single_value(
        &self,
        name: &str,
    ) -> Option<f64> {
        let samples = self.samples.get(name)?;
        // prefer no-label sample if present
        for (labels, value) in samples {
            if labels.is_empty() {
                return Some(*value);
            }
        }
        samples.first().map(|(_, v)| *v)
    }
}

fn parse_metric_lhs(lhs: &str) -> (String, BTreeMap<String, String>) {
    if let Some((name, labels_str)) = lhs.split_once('{') {
        let labels_str = labels_str.strip_suffix('}').unwrap_or(labels_str);
        let labels = parse_labels(labels_str);
        return (name.to_string(), labels);
    }

    (lhs.to_string(), BTreeMap::new())
}

fn parse_labels(s: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    // naive split on ,
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((k, v)) = part.split_once('=') {
            let v = v.trim().trim_matches('"');
            out.insert(k.trim().to_string(), v.to_string());
        }
    }
    out
}

fn format_mem_from_kib(kib: f64) -> String {
    if !kib.is_finite() || kib <= 0.0 {
        return "0MB".to_string();
    }

    let mib = kib / 1024.0;
    if mib >= 1024.0 {
        let gib = mib / 1024.0;
        return format!("{:.2}GB", gib);
    }

    format!("{:.1}MB", mib)
}
