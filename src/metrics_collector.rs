use crate::error::BenchmarkResult;
use crate::queries_repository::QueryType;
use crate::utils::format_number;
use histogram::Histogram;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use sysinfo::System;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MetricsCollector {
    pub vendor: String,
    pub node_count: u64,
    pub relation_count: u64,
    pub query_count: u64,
    pub histogram_for_type: HashMap<String, Histogram>,
    pub worst_call_for_type: HashMap<String, (String, String, Duration)>,
    pub total_calls_for_type: HashMap<String, u64>,
    pub machine_metadata: MachineMetadata,
    pub total_operations_duration: Duration,
}

#[derive(Debug, Serialize, Clone)]
pub struct Percentile {
    pub vendor: String,
    pub node_count: u64,
    pub relation_count: u64,
    pub query_count: u64,
    pub histogram_for_type: HashMap<String, Vec<f32>>,
    pub worst_call_for_type: HashMap<String, (String, String, Duration)>,
    pub total_calls_for_type: HashMap<String, u64>,
    pub machine_metadata: MachineMetadata,
    pub total_operations_duration: Duration,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MachineMetadata {
    pub os: String,
    pub arch: String,
    pub cpu_count: usize,
    pub cores_count: usize,
    pub total_memory_kb: u64,
    pub free_memory_kb: u64,
    pub hostname: String,
}
impl MachineMetadata {
    pub fn new() -> MachineMetadata {
        let mut sys = System::new_all();
        sys.refresh_all();
        let os = std::env::consts::OS.into();
        let arch = std::env::consts::ARCH.into();
        let cpu_count = sys.cpus().len();
        let cores_count = sys.physical_core_count().unwrap_or(0);
        let total_memory_kb = sys.total_memory();
        let free_memory_kb = sys.used_memory();
        let hostname = System::host_name().unwrap_or_else(|| "Unknown".to_string());

        MachineMetadata {
            os,
            arch,
            cpu_count,
            cores_count,
            total_memory_kb,
            free_memory_kb,
            hostname,
        }
    }
}

impl MetricsCollector {
    pub async fn from_file(path: impl AsRef<Path>) -> BenchmarkResult<Self> {
        let mut file = File::open(path).await?;
        let mut contents = String::new();
        file.read_to_string(&mut contents).await?;
        Ok(serde_json::from_str::<MetricsCollector>(&contents)?)
    }

    pub fn new(
        node_count: u64,
        relation_count: u64,
        query_count: u64,
        vendor: String,
        machine_metadata: MachineMetadata,
    ) -> BenchmarkResult<Self> {
        Ok(Self {
            vendor,
            node_count,
            relation_count,
            query_count,
            histogram_for_type: HashMap::new(),
            worst_call_for_type: HashMap::new(),
            total_calls_for_type: HashMap::new(),
            machine_metadata,
            total_operations_duration: Duration::default(),
        })
    }

    fn record_operation(
        &mut self,
        duration: Duration,
        operation: &str,
        query: &str,
        statistics: &str,
    ) -> BenchmarkResult<()> {
        // Update worst call for operation type
        self.worst_call_for_type
            .entry(operation.to_string())
            .and_modify(|(worst_query, worst_statistics, worst_duration)| {
                if duration > *worst_duration {
                    *worst_duration = duration;
                    *worst_query = query.to_string();
                    *worst_statistics = statistics.to_string();
                }
            })
            .or_insert((query.to_string(), statistics.to_string(), duration));
        // Update histogram for specific operation type
        self.histogram_for_type
            .entry(operation.to_string())
            .or_insert_with(|| Histogram::new(7, 64).unwrap())
            .increment(duration.as_micros() as u64)?;

        // Update total calls for operation type
        *self
            .total_calls_for_type
            .entry(operation.to_string())
            .or_insert(0) += 1;
        Ok(())
    }
    pub fn record(
        &mut self,
        duration: Duration,
        operation: &str,
        operation_type: QueryType,
        query: &str,
        statistics: &str,
    ) -> BenchmarkResult<()> {
        self.total_operations_duration += duration;
        self.record_operation(duration, "all", query, statistics)?;
        if operation_type == QueryType::Read {
            self.record_operation(duration, "read", query, statistics)?;
        } else {
            self.record_operation(duration, "write", query, statistics)?;
        }
        self.record_operation(duration, operation, query, statistics)
    }

    pub async fn save(
        &self,
        path: impl AsRef<Path>,
    ) -> BenchmarkResult<()> {
        let json = serde_json::to_string_pretty(self)?;
        let mut file = File::create(path).await?;
        file.write_all(json.as_bytes()).await?;
        Ok(())
    }

    pub fn to_percentile(&self) -> Percentile {
        let mut percentile = Percentile {
            vendor: self.vendor.clone(),
            node_count: self.node_count,
            relation_count: self.relation_count,
            query_count: self.query_count,
            histogram_for_type: HashMap::new(),
            worst_call_for_type: self.worst_call_for_type.clone(),
            total_calls_for_type: self.total_calls_for_type.clone(),
            machine_metadata: self.machine_metadata.clone(),
            total_operations_duration: self.total_operations_duration,
        };

        for (operation, histogram) in &self.histogram_for_type {
            let mut percentiles = Vec::new();
            for p in &[
                10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 95.0, 99.0,
            ] {
                // format_duration_ms(&Duration::from_micros(b.end()))
                let percentile = histogram.percentile(*p).unwrap_or(None).map_or(0.0, |b| {
                    format_duration_to_f32(&Duration::from_micros(b.end()))
                });
                percentiles.push(percentile);
            }
            percentile
                .histogram_for_type
                .insert(operation.clone(), percentiles);
        }

        percentile
    }
    // using histogram from https://github.com/pelikan-io/rustcommon/blob/main/histogram/src/standard.rs
    // return a markdown report that consist of
    // - table with the columns: operation, total calls, 50th percentile, 95th percentile, 99th percentile, worst time, worst call
    //   sorted by 99th percentile call in descending order
    //   operations is one of all, read, write, and specific operation types
    pub fn markdown_report(&self) -> String {
        let ordered_operations = order_keys_by_p(&self.histogram_for_type, 99.0);
        let ordered_operations = reorder_rows(ordered_operations, &["all", "read", "write"]);

        let mut report = String::from(format!(
            "vendor: {}\n\nnodes: {}\n\nrelations: {}\n\nqueries: {}\n\n",
            self.vendor,
            format_number(self.node_count),
            format_number(self.relation_count),
            format_number(self.query_count)
        ));
        report.push_str("| Query | Total Calls | 50th Percentile | 95th Percentile | 99th Percentile | Worst Time | Worst Call | Worst Call Statistics |\n");
        report.push_str("|-----------|-------------|-----------------|-----------------|-----------------|------------|------------|------------|\n");

        // Add rows for other operation types
        let rows: HashMap<String, String> = self
            .histogram_for_type
            .iter()
            .map(|(op, histogram)| self.create_row(op, histogram))
            .collect();

        for operation in ordered_operations {
            if let Some(row) = rows.get(operation.as_str()) {
                report.push_str(&format!("| {} | {}|\n", operation, row));
            }
        }

        report
    }

    fn format_percentile(
        &self,
        histogram: &Histogram,
        percentile: f64,
    ) -> String {
        histogram
            .percentile(percentile)
            .unwrap_or(None)
            .map_or("NA".to_string(), |b| {
                format_duration_ms(&Duration::from_micros(b.end()))
            })
    }

    fn create_row(
        &self,
        operation: &str,
        histogram: &Histogram,
    ) -> (String, String) {
        let total_calls = self.total_calls_for_type.get(operation).unwrap_or(&0);
        let (worst_call, worst_statistics, worst_time) = self
            .worst_call_for_type
            .get(operation)
            .map(|(call, statistics, duration)| {
                (
                    call.to_string(),
                    statistics.to_string(),
                    format_duration_ms(duration).to_string(),
                )
            })
            .unwrap_or_else(|| ("NA".to_string(), "NA".to_string(), "NA".to_string()));

        let row = format!(
            "{} | {} | {} | {} | {} | `{}` | `{}`",
            format_number(*total_calls),
            self.format_percentile(histogram, 50.0),
            self.format_percentile(histogram, 95.0),
            self.format_percentile(histogram, 99.0),
            worst_time,
            worst_call,
            worst_statistics
        );

        (operation.to_string(), row)
    }
}

fn order_keys_by_p(
    histogram: &HashMap<String, Histogram>,
    percentile: f64,
) -> Vec<String> {
    let mut key_percentile_pairs: Vec<(String, u64)> = histogram
        .iter()
        .map(|(key, histogram)| {
            let p = histogram
                .percentile(percentile)
                .unwrap_or(None)
                .map_or(0, |b| b.end());
            (key.clone(), p)
        })
        .collect();

    // Sort by p99 in descending order
    key_percentile_pairs.sort_by(|a, b| b.1.cmp(&a.1));

    // Extract only the keys
    key_percentile_pairs
        .into_iter()
        .map(|(key, _)| key)
        .collect()
}
fn reorder_rows(
    mut vec: Vec<String>,
    to_prepend: &[&str],
) -> Vec<String> {
    // Create a vector to store the removed items
    let removed: Vec<String> = to_prepend.iter().map(|&s| s.to_string()).collect();

    // Remove specified strings and collect them
    vec.retain(|item| {
        if to_prepend.contains(&item.as_str()) {
            false
        } else {
            true
        }
    });

    // Prepend the removed items back to the vector
    vec.splice(0..0, removed);

    vec
}

fn format_duration_ms(duration: &Duration) -> String {
    let total_ms = duration.as_secs_f64() * 1000.0;
    format!("{:.3}ms", total_ms)
}

fn format_duration_to_f32(duration: &Duration) -> f32 {
    let total_ms = duration.as_secs_f64() * 1000.0;
    let as_str = format!("{:.1}", total_ms);
    as_str.parse::<f32>().unwrap()
}
