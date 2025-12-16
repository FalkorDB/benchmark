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
struct UiLatencyHistogram {
    // Bucket upper bounds (in milliseconds) and cumulative counts.
    #[serde(rename = "buckets-ms")]
    buckets_ms: Vec<u64>,
    #[serde(rename = "cumulative-counts")]
    cumulative_counts: Vec<u64>,
    count: u64,
}

#[derive(Debug, Serialize)]
struct UiOpsBreakdown {
    #[serde(rename = "by-query")]
    by_query: BTreeMap<String, u64>,
    #[serde(rename = "by-spawn")]
    by_spawn: BTreeMap<String, u64>,
}

#[derive(Debug, Serialize)]
struct UiSpawnStats {
    min: u64,
    max: u64,
    #[serde(rename = "p50")]
    p50: u64,
    #[serde(rename = "p95")]
    p95: u64,
    #[serde(rename = "max-min-ratio")]
    max_min_ratio: f64,
    // Coefficient of variation (stddev / mean) for per-spawn totals.
    cv: f64,
}

#[derive(Debug, Serialize)]
struct UiResult {
    #[serde(rename = "deadline-offset")]
    deadline_offset: String,
    #[serde(rename = "actual-messages-per-second")]
    actual_messages_per_second: f64,
    latency: UiLatency,
    #[serde(rename = "avg-latency-ms")]
    avg_latency_ms: f64,
    #[serde(rename = "latency-histogram")]
    latency_histogram: UiLatencyHistogram,
    #[serde(rename = "elapsed-ms")]
    elapsed_ms: u64,
    #[serde(rename = "cpu-usage")]
    cpu_usage: f64,
    #[serde(rename = "ram-usage")]
    ram_usage: String,
    // Memgraph-only today: base dataset memory estimate from formula
    // StorageRAMUsage = NumberOfVertices×212B + NumberOfEdges×162B
    #[serde(rename = "base-dataset-bytes", skip_serializing_if = "Option::is_none")]
    base_dataset_bytes: Option<u64>,
    errors: u64,
    #[serde(rename = "successful-requests")]
    successful_requests: u64,
    #[serde(rename = "operations")]
    operations: UiOpsBreakdown,
    #[serde(rename = "spawn-stats")]
    spawn_stats: UiSpawnStats,
    // "single"-workload style latency percentiles (P10..P99) per query type.
    #[serde(
        rename = "histogram_for_type",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    histogram_for_type: BTreeMap<String, Vec<f64>>,
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
    sum: f64,
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

    // Prefer in-process computed percentiles (microseconds gauges) when present.
    let (p50_s, p95_s, p99_s) =
        if let Some((p50_us, p95_us, p99_us)) = metrics.latency_percentiles_us(v.vendor) {
            (
                (p50_us / 1_000_000.0),
                (p95_us / 1_000_000.0),
                (p99_us / 1_000_000.0),
            )
        } else {
            (
                histogram_quantile_seconds(&success_hist, 0.50),
                histogram_quantile_seconds(&success_hist, 0.95),
                histogram_quantile_seconds(&success_hist, 0.99),
            )
        };

    let avg_latency_ms = if success_hist.count > 0.0 {
        (success_hist.sum / success_hist.count) * 1000.0
    } else {
        0.0
    };

    let elapsed_secs = (v.meta.elapsed_ms as f64) / 1000.0;
    let actual_mps = if elapsed_secs > 0.0 {
        (success_hist.count / elapsed_secs).max(0.0)
    } else {
        0.0
    };

    let latency_histogram = UiLatencyHistogram {
        buckets_ms: success_hist
            .buckets
            .iter()
            .map(|(le_s, _)| (*le_s * 1000.0).round().max(0.0) as u64)
            .collect(),
        cumulative_counts: success_hist
            .buckets
            .iter()
            .map(|(_, c)| c.round().max(0.0) as u64)
            .collect(),
        count: success_hist.count.round().max(0.0) as u64,
    };

    let (cpu_usage, ram_usage) = metrics.vendor_cpu_mem(v.vendor);

    let base_dataset_bytes = match v.vendor {
        Vendor::Memgraph => {
            let from_metric = metrics
                .get_single_value("memgraph_storage_base_dataset_bytes")
                .map(|v| v.round().max(0.0) as u64)
                .filter(|v| *v > 0);

            // Back-compat for older runs: compute from dataset constants.
            // StorageRAMUsage = NumberOfVertices×212B + NumberOfEdges×162B
            let computed = {
                let bytes: i128 = (spec.vertices as i128) * 212 + (spec.edges as i128) * 162;
                if bytes > 0 {
                    Some(bytes.min(u64::MAX as i128) as u64)
                } else {
                    None
                }
            };

            from_metric.or(computed)
        }
        Vendor::Neo4j => {
            let store = metrics
                .get_single_value("neo4j_store_size_bytes")
                .map(|v| v.round().max(0.0) as u64)
                .filter(|v| *v > 0);

            let estimate = metrics
                .get_single_value("neo4j_base_dataset_estimate_bytes")
                .map(|v| v.round().max(0.0) as u64)
                .filter(|v| *v > 0);

            store.or(estimate)
        }
        _ => None,
    };

    let operations = metrics.operations_breakdown(v.vendor);
    let spawn_stats = compute_spawn_stats(&operations.by_spawn);

    let histogram_for_type = metrics.query_latency_histogram_ms(v.vendor);
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
                p50: format_ms(p50_s * 1000.0),
                p95: format_ms(p95_s * 1000.0),
                p99: format_ms(p99_s * 1000.0),
            },
            avg_latency_ms,
            latency_histogram,
            elapsed_ms: v.meta.elapsed_ms as u64,
            cpu_usage,
            ram_usage,
            base_dataset_bytes,
            errors: error_hist.count.round().max(0.0) as u64,
            successful_requests: success_hist.count.round().max(0.0) as u64,
            operations,
            spawn_stats,
            histogram_for_type,
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
        return format!("{:.3}s", s);
    }

    if ms >= 10.0 {
        return format!("{:.2}ms", ms);
    }

    format!("{:.3}ms", ms)
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

    fn latency_percentiles_us(
        &self,
        vendor: Vendor,
    ) -> Option<(f64, f64, f64)> {
        let (p50, p95, p99) = match vendor {
            Vendor::Falkor => (
                "falkordb_latency_p50_us",
                "falkordb_latency_p95_us",
                "falkordb_latency_p99_us",
            ),
            Vendor::Neo4j => (
                "neo4j_latency_p50_us",
                "neo4j_latency_p95_us",
                "neo4j_latency_p99_us",
            ),
            Vendor::Memgraph => (
                "memgraph_latency_p50_us",
                "memgraph_latency_p95_us",
                "memgraph_latency_p99_us",
            ),
        };

        let p50v = self.get_single_value(p50)?;
        let p95v = self.get_single_value(p95)?;
        let p99v = self.get_single_value(p99)?;

        // If any is zero, treat as missing.
        if p50v <= 0.0 || p95v <= 0.0 || p99v <= 0.0 {
            return None;
        }

        Some((p50v, p95v, p99v))
    }

    fn query_latency_histogram_ms(
        &self,
        vendor: Vendor,
    ) -> BTreeMap<String, Vec<f64>> {
        let metric = match vendor {
            Vendor::Falkor => "falkordb_query_latency_pct_us",
            Vendor::Neo4j => "neo4j_query_latency_pct_us",
            Vendor::Memgraph => "memgraph_query_latency_pct_us",
        };

        let Some(samples) = self.samples.get(metric) else {
            return BTreeMap::new();
        };

        // The UI expects this exact order.
        let wanted_pcts: [(&str, f64); 11] = [
            ("10", 10.0),
            ("20", 20.0),
            ("30", 30.0),
            ("40", 40.0),
            ("50", 50.0),
            ("60", 60.0),
            ("70", 70.0),
            ("80", 80.0),
            ("90", 90.0),
            ("95", 95.0),
            ("99", 99.0),
        ];

        // query -> pct -> us
        let mut tmp: BTreeMap<String, BTreeMap<String, f64>> = BTreeMap::new();
        for (labels, value) in samples {
            let Some(query) = labels.get("query").cloned() else {
                continue;
            };
            let Some(pct) = labels.get("pct").cloned() else {
                continue;
            };
            tmp.entry(query).or_default().insert(pct, *value);
        }

        let mut out = BTreeMap::new();
        for (query, by_pct) in tmp {
            let mut arr = Vec::with_capacity(wanted_pcts.len());
            for (label, _p) in wanted_pcts {
                let us = by_pct.get(label).copied().unwrap_or(0.0);
                arr.push(us / 1000.0); // ms
            }
            // Only keep queries with at least one non-zero percentile.
            if arr.iter().any(|v| *v > 0.0) {
                out.insert(query, arr);
            }
        }

        out
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
        let sum_name = format!("{}_sum", base);

        let count = self.get_single_value(&count_name).unwrap_or(0.0);
        let sum = self.get_single_value(&sum_name).unwrap_or(0.0);

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

        Ok(HistogramData {
            buckets,
            count,
            sum,
        })
    }

    fn operations_breakdown(
        &self,
        vendor: Vendor,
    ) -> UiOpsBreakdown {
        let mut by_query: BTreeMap<String, u64> = BTreeMap::new();
        let mut by_spawn: BTreeMap<String, u64> = BTreeMap::new();

        let want_vendor = vendor.to_string();
        if let Some(samples) = self.samples.get("operations_total") {
            for (labels, value) in samples {
                if labels
                    .get("vendor")
                    .map(|v| v != &want_vendor)
                    .unwrap_or(false)
                {
                    continue;
                }

                let name = labels
                    .get("name")
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());
                let spawn_id = labels
                    .get("spawn_id")
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());

                let v = value.round().max(0.0) as u64;
                *by_query.entry(name).or_insert(0) += v;
                *by_spawn.entry(spawn_id).or_insert(0) += v;
            }
        }

        UiOpsBreakdown { by_query, by_spawn }
    }

    fn vendor_cpu_mem(
        &self,
        vendor: Vendor,
    ) -> (f64, String) {
        let cpu = match vendor {
            Vendor::Falkor => self.get_single_value("falkor_cpu_usage").unwrap_or(0.0),
            Vendor::Neo4j => self.get_single_value("neo4j_cpu_usage").unwrap_or(0.0),
            Vendor::Memgraph => self.get_single_value("memgraph_cpu_usage").unwrap_or(0.0),
        };

        // Prefer query-interface memory metrics when present.
        let mem_str = match vendor {
            Vendor::Falkor => {
                // `GRAPH.MEMORY USAGE` reports MB.
                if let Some(mb) = self.get_single_value("falkordb_graph_memory_usage_mb") {
                    if mb > 0.0 {
                        format_mem_from_mb(mb)
                    } else {
                        // Fallback: process RSS (sysinfo, KiB)
                        let mem_kib = self.get_single_value("falkor_memory_usage").unwrap_or(0.0);
                        format_mem_from_kib(mem_kib)
                    }
                } else {
                    let mem_kib = self.get_single_value("falkor_memory_usage").unwrap_or(0.0);
                    format_mem_from_kib(mem_kib)
                }
            }
            Vendor::Memgraph => {
                // `SHOW STORAGE INFO` reports bytes.
                let bytes = self
                    .get_single_value("memgraph_storage_memory_tracked_bytes")
                    .or_else(|| self.get_single_value("memgraph_storage_memory_res_bytes"))
                    .or_else(|| self.get_single_value("memgraph_storage_peak_memory_res_bytes"))
                    .unwrap_or(0.0);

                if bytes > 0.0 {
                    format_mem_from_bytes(bytes)
                } else {
                    // Fallback: process RSS (sysinfo, KiB)
                    let mem_kib = self
                        .get_single_value("memgraph_memory_usage")
                        .unwrap_or(0.0);
                    format_mem_from_kib(mem_kib)
                }
            }
            Vendor::Neo4j => {
                // No query-interface metric wired yet; use process RSS (sysinfo, KiB)
                let mem_kib = self.get_single_value("neo4j_memory_usage").unwrap_or(0.0);
                format_mem_from_kib(mem_kib)
            }
        };

        (cpu, mem_str)
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
    format_mem_from_mib(mib)
}

fn format_mem_from_bytes(bytes: f64) -> String {
    if !bytes.is_finite() || bytes <= 0.0 {
        return "0MB".to_string();
    }

    let mib = bytes / (1024.0 * 1024.0);
    format_mem_from_mib(mib)
}

fn format_mem_from_mb(mb: f64) -> String {
    if !mb.is_finite() || mb <= 0.0 {
        return "0MB".to_string();
    }

    format_mem_from_mib(mb)
}

fn format_mem_from_mib(mib: f64) -> String {
    if !mib.is_finite() || mib <= 0.0 {
        return "0MB".to_string();
    }

    if mib >= 1024.0 {
        let gib = mib / 1024.0;
        return format!("{:.2}GB", gib);
    }

    format!("{:.1}MB", mib)
}

fn compute_spawn_stats(by_spawn: &BTreeMap<String, u64>) -> UiSpawnStats {
    if by_spawn.is_empty() {
        return UiSpawnStats {
            min: 0,
            max: 0,
            p50: 0,
            p95: 0,
            max_min_ratio: 0.0,
            cv: 0.0,
        };
    }

    let mut values: Vec<u64> = by_spawn.values().copied().collect();
    values.sort_unstable();

    let min = *values.first().unwrap_or(&0);
    let max = *values.last().unwrap_or(&0);

    let p50 = quantile_u64(&values, 0.50);
    let p95 = quantile_u64(&values, 0.95);

    let max_min_ratio = if min > 0 {
        (max as f64) / (min as f64)
    } else {
        0.0
    };

    let mean = (values.iter().sum::<u64>() as f64) / (values.len() as f64);
    let var = if values.len() > 1 {
        values
            .iter()
            .map(|v| {
                let d = (*v as f64) - mean;
                d * d
            })
            .sum::<f64>()
            / (values.len() as f64)
    } else {
        0.0
    };
    let stddev = var.sqrt();
    let cv = if mean > 0.0 { stddev / mean } else { 0.0 };

    UiSpawnStats {
        min,
        max,
        p50,
        p95,
        max_min_ratio,
        cv,
    }
}

fn quantile_u64(
    sorted: &[u64],
    q: f64,
) -> u64 {
    if sorted.is_empty() {
        return 0;
    }

    if q <= 0.0 {
        return sorted[0];
    }
    if q >= 1.0 {
        return *sorted.last().unwrap();
    }

    let idx = ((sorted.len() as f64 - 1.0) * q).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}
