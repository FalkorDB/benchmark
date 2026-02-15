use crate::{
    FALKOR_TELEMETRY_EXEC_US, FALKOR_TELEMETRY_REPORT_US, FALKOR_TELEMETRY_WAIT_US,
};
use redis::aio::MultiplexedConnection;
use redis::Value;
use std::collections::HashMap;
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::info;

/// Telemetry fields we care about (durations in seconds).
#[derive(Debug)]
struct TelemetryEntry {
    query: String,
    total: f64,
    wait: f64,
    exec: f64,
    report: f64,
    write: bool,
}

/// Normalise a Cypher query so it can be matched across minor formatting
/// differences. This keeps the actual text (including parameters), but
/// collapses whitespace.
fn normalize_query(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn value_to_string(v: &Value) -> Option<String> {
    match v {
        Value::BulkString(bytes) => String::from_utf8(bytes.clone()).ok(),
        Value::SimpleString(s) => Some(s.clone()),
        Value::Int(i) => Some(i.to_string()),
        Value::Double(f) => Some(f.to_string()),
        _ => None,
    }
}

/// Parse a single Redis stream record (array of key/value bulk strings).
fn parse_telemetry_entry(fields_val: &Value) -> Option<TelemetryEntry> {
    let Value::Array(flat) = fields_val else {
        return None;
    };
    if flat.len() % 2 != 0 {
        return None;
    }

    let mut m: HashMap<String, String> = HashMap::new();
    let mut iter = flat.iter();
    while let (Some(kv), Some(vv)) = (iter.next(), iter.next()) {
        if let (Some(k), Some(v)) = (value_to_string(kv), value_to_string(vv)) {
            m.insert(k, v);
        }
    }

    let query = m.get("Query")?.clone();
    let total = m
        .get("Total duration")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    let wait = m
        .get("Wait duration")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    let exec = m
        .get("Execution duration")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    let report = m
        .get("Report duration")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    let write = m.get("Write").map(|v| v == "1").unwrap_or(false);

    Some(TelemetryEntry {
        query,
        total,
        wait,
        exec,
        report,
        write,
    })
}

async fn xread_block(
    conn: &mut MultiplexedConnection,
    stream_key: &str,
    last_id: &str,
) -> redis::RedisResult<Value> {
    redis::cmd("XREAD")
        .arg("BLOCK")
        .arg(1000_i64)
        .arg("STREAMS")
        .arg(stream_key)
        .arg(last_id)
        .query_async(conn)
        .await
}

/// Start a background task that reads FalkorDB telemetry and exports
/// per-query-type average wait/exec/report durations to Prometheus.
///
/// `redis_url` is e.g. "redis://127.0.0.1:6379".
/// `query_map` maps a normalised Cypher query string to the benchmark
/// query name (q_name). This should be built from all PreparedQuery
/// instances used in the run so that both read and write queries are
/// covered.
pub fn spawn_falkor_telemetry_collector(
    redis_url: String,
    query_map: HashMap<String, String>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let client = match redis::Client::open(redis_url.as_str()) {
            Ok(c) => c,
            Err(e) => {
                info!("Failed to create redis client for telemetry: {:?}", e);
                return;
            }
        };

        let mut conn = match client.get_multiplexed_async_connection().await {
            Ok(c) => c,
            Err(e) => {
                info!("Failed to connect to redis for telemetry: {:?}", e);
                return;
            }
        };

        // Running sums in microseconds, per query name.
        #[derive(Default)]
        struct Agg {
            count: u64,
            wait_us: f64,
            exec_us: f64,
            report_us: f64,
        }

        let mut agg: HashMap<String, Agg> = HashMap::new();
        let mut last_id = String::from("$");
        let stream_key = String::from("telemetry{falkor}");
        let flush_interval = Duration::from_secs(5);
        let mut last_flush = tokio::time::Instant::now();

        loop {
            let res = xread_block(&mut conn, &stream_key, &last_id).await;
            match res {
                Ok(Value::Array(streams)) if !streams.is_empty() => {
                    for stream in streams {
                        // Each stream is: [ key, [ [id, [field, value, ...]], ... ] ]
                        let Value::Array(stream_parts) = stream else { continue };
                        if stream_parts.len() != 2 {
                            continue;
                        }
                        let entries_val = &stream_parts[1];
                        let Value::Array(entries) = entries_val else { continue };

                        for entry in entries {
                            let Value::Array(entry_parts) = entry else { continue };
                            if entry_parts.len() != 2 {
                                continue;
                            }
                            let id_val = &entry_parts[0];
                            let fields_val = &entry_parts[1];

                            if let Some(id_str) = value_to_string(id_val) {
                                last_id = id_str;
                            }

                            if let Some(entry) = parse_telemetry_entry(fields_val) {
                                let norm = normalize_query(&entry.query);
                                let q_name = query_map
                                    .get(&norm)
                                    .cloned()
                                    .unwrap_or_else(|| norm.clone());

                                let a = agg.entry(q_name).or_default();
                                a.count += 1;
                                a.wait_us += entry.wait * 1_000_000.0;
                                a.exec_us += entry.exec * 1_000_000.0;
                                a.report_us += entry.report * 1_000_000.0;
                            }
                        }
                    }
                }
                Ok(_) => {
                    // No entries; fall through to flush check.
                }
                Err(e) => {
                    info!("Error reading Falkor telemetry stream: {:?}", e);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }

            // Periodically flush averages into Prometheus.
            if last_flush.elapsed() >= flush_interval {
                for (q_name, a) in &agg {
                    if a.count == 0 {
                        continue;
                    }
                    let c = a.count as f64;
                    let avg_wait = (a.wait_us / c).round().max(0.0) as i64;
                    let avg_exec = (a.exec_us / c).round().max(0.0) as i64;
                    let avg_report = (a.report_us / c).round().max(0.0) as i64;

                    FALKOR_TELEMETRY_WAIT_US
                        .with_label_values(&[q_name.as_str()])
                        .set(avg_wait);
                    FALKOR_TELEMETRY_EXEC_US
                        .with_label_values(&[q_name.as_str()])
                        .set(avg_exec);
                    FALKOR_TELEMETRY_REPORT_US
                        .with_label_values(&[q_name.as_str()])
                        .set(avg_report);
                }
                last_flush = tokio::time::Instant::now();
            }
        }
    })
}
