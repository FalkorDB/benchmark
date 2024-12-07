use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::process_monitor::ProcessMonitor;
use crate::utils::{
    create_directory_if_not_exists, delete_file, falkor_shared_lib_path, get_falkor_log_path,
    redis_shutdown,
};
use crate::{
    FALKOR_NODES_GAUGE, FALKOR_RELATIONSHIPS_GAUGE, FALKOR_RESTART_COUNTER,
    FALKOR_RUNNING_REQUESTS_GAUGE, FALKOR_WAITING_REQUESTS_GAUGE, REDIS_DATA_DIR,
};
use falkordb::FalkorValue::I64;
use falkordb::{AsyncGraph, FalkorClientBuilder, FalkorConnectionInfo};
use prometheus::core::{AtomicU64, GenericCounter};
use std::env;
use tokio::task::JoinHandle;
use tracing::{error, info};

#[derive(Debug)]
#[allow(dead_code)]
struct QueryInfo {
    received_at: i64,
    graph_name: String,
    query: String,
    execution_duration: f64,
    replicated_command: i64,
}

#[derive(Default)]
pub struct FalkorProcess {
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    process_handle: Option<JoinHandle<()>>,
    prom_shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    prom_process_handle: Option<JoinHandle<()>>,
    dropped: bool,
}

impl FalkorProcess {
    pub async fn new(san: bool) -> BenchmarkResult<Self> {
        redis_shutdown().await?; // if redis run on this machine use redis-cli to shut it down

        create_directory_if_not_exists(REDIS_DATA_DIR).await?;
        let falkor_log_path = get_falkor_log_path()?;
        delete_file(falkor_log_path.as_str()).await?;

        let default_so_path = falkor_shared_lib_path()?;
        let default_so_path = env::var("FALKOR_PATH").unwrap_or_else(|_| default_so_path.clone());
        let falkor_log_path = get_falkor_log_path()?;
        let command = if san {
            "redis-server-asan-7.2"
        } else {
            "redis-server"
        }
        .to_string();

        let args: Vec<String> = vec![
            "--dir",
            REDIS_DATA_DIR,
            "--logfile",
            falkor_log_path.as_str(),
            "--protected-mode",
            "no",
            "--loadmodule",
            default_so_path.as_str(),
            "CACHE_SIZE",
            "40",
            "MAX_QUEUED_QUERIES",
            "100",
        ]
        .into_iter()
        .map(|s| s.to_string())
        .collect();

        let (mut process_monitor, shutdown_tx) = ProcessMonitor::new(
            command,
            args,
            Default::default(),
            std::time::Duration::from_secs(5),
        );
        let counter: GenericCounter<AtomicU64> = FALKOR_RESTART_COUNTER.clone();
        let process_handle = Some(tokio::spawn(async move {
            let _ = process_monitor.run(counter).await;
        }));

        let (prom_process_handle, prom_shutdown_tx) = prometheus_metrics_reporter();

        Ok(Self {
            shutdown_tx: Some(shutdown_tx),
            process_handle,
            prom_shutdown_tx: Some(prom_shutdown_tx),
            prom_process_handle: Some(prom_process_handle),
            dropped: false,
        })
    }
    async fn terminate(&mut self) {
        if let Some(prom_shutdown_tx) = self.prom_shutdown_tx.take() {
            drop(prom_shutdown_tx);
        }
        if let Some(prom_process_handle) = self.prom_process_handle.take() {
            let _ = prom_process_handle.await;
        }
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            drop(shutdown_tx);
        }
        if let Some(process_handle) = self.process_handle.take() {
            let _ = process_handle.await;
        }
        info!("Falkor process terminated correctly");
    }
}
impl Drop for FalkorProcess {
    fn drop(&mut self) {
        info!("Dropping FalkorProcess started");
        if !self.dropped {
            let mut this = FalkorProcess::default();
            std::mem::swap(&mut this, self);
            this.dropped = true;
            let task = tokio::spawn(async move { this.terminate().await });
            match futures::executor::block_on(task) {
                Ok(_) => {
                    info!("Dropping FalkorProcess ended");
                }
                Err(e) => {
                    info!(
                        "Error dropping FalkorProcess: {:?}, cleanup task finish with errro",
                        e
                    );
                }
            }
        }
    }
}

fn prometheus_metrics_reporter() -> (JoinHandle<()>, tokio::sync::oneshot::Sender<()>) {
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        match report_metrics().await {
            Ok(_) => {}
            Err(e) => {
                info!("Error reporting metrics: {:?}", e);
            }
        }
        loop {
            tokio::select! {

                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
                   match report_metrics().await{
                          Ok(_) => {}
                          Err(e) => {
                            info!("Error reporting metrics: {:?}", e);
                          }
                    }
                }
                _ = &mut shutdown_rx => {
                    info!("Shutting down prometheus_metrics_reporter");
                    return;
                }
            }
        }
    });
    (handle, shutdown_tx)
}

async fn report_metrics() -> BenchmarkResult<()> {
    info!("-->  Reporting metrics");
    let client = redis::Client::open("redis://127.0.0.1:6379/")?;
    let mut con = client.get_multiplexed_async_connection().await?;
    // let graph_info = redis::cmd("GRAPH.INFO").query_async(&mut con).await?;

    let command = redis::cmd("GRAPH.INFO");
    let redis_value = con.send_packed_command(&command).await?;
    let (running_queries, waiting_queries) = redis_to_query_info(redis_value)?;
    // trace!(
    //     "Running Queries ({}):  {:?}",
    //     running_queries.len(),
    //     running_queries
    // );
    // trace!(
    //     "Waiting Queries ({}): {:?}",
    //     waiting_queries.len(),
    //     waiting_queries
    // );

    let running_queries_len: i64 = running_queries.len() as i64;
    let waiting_queries_len: i64 = waiting_queries.len() as i64;
    FALKOR_RUNNING_REQUESTS_GAUGE.set(running_queries_len);
    FALKOR_WAITING_REQUESTS_GAUGE.set(waiting_queries_len);

    let connection_info: FalkorConnectionInfo = "falkor://127.0.0.1:6379"
        .try_into()
        .expect("Invalid connection info");
    let client = FalkorClientBuilder::new_async()
        .with_connection_info(connection_info)
        .build()
        .await
        .expect("Failed to build client");
    let mut graph = client.select_graph("falkor");
    if let Ok(relationships_number) =
        execute_i64_query(&mut graph, "MATCH ()-[r]->() RETURN count(r)").await
    {
        FALKOR_RELATIONSHIPS_GAUGE.set(relationships_number);
    }
    if let Ok(nodes_number) = execute_i64_query(&mut graph, "MATCH (n) RETURN count(n)").await {
        FALKOR_NODES_GAUGE.set(nodes_number);
    }

    Ok(())
}

// return a tuple of (running_queries, waiting_queries)
// first element of the tuple is a vector of running queries
// second element of the tuple is a vector of waiting
// use redis_vec_as_query_info to parse each query info
fn redis_to_query_info(value: redis::Value) -> BenchmarkResult<(Vec<QueryInfo>, Vec<QueryInfo>)> {
    // Convert the value into a vector of redis::Value
    let queries = redis_value_as_vec(value)?;
    if queries.len() < 4 {
        return Err(OtherError(format!(
            "Insufficient data in Redis response {:?}",
            queries
        )));
    }
    let mut running_queries = Vec::new();
    let mut waiting_queries = Vec::new();

    let running_vec = redis_value_as_vec(queries[1].clone())?;
    for value in running_vec {
        if let Ok(query_info) = redis_vec_as_query_info(value) {
            running_queries.push(query_info);
        }
    }
    let waiting_vec = redis_value_as_vec(queries[3].clone())?;
    for value in waiting_vec {
        if let Ok(query_info) = redis_vec_as_query_info(value) {
            waiting_queries.push(query_info);
        }
    }
    // Return the collected running and waiting queries
    Ok((running_queries, waiting_queries))
}
fn redis_vec_as_query_info(value: redis::Value) -> BenchmarkResult<QueryInfo> {
    let value = redis_value_as_vec(value)?;
    if value.len() < 10 {
        return Err(OtherError(
            "Insufficient data in Redis response".to_string(),
        ));
    }

    let received_at = redis_value_as_int(value[1].clone())?;
    let graph_name = redis_value_as_string(value[3].clone())?;
    let query = redis_value_as_string(value[5].clone())?;
    let execution_duration = redis_value_as_string(value[7].clone())?
        .parse::<f64>()
        .map_err(|e| OtherError(format!("Failed to parse execution_duration: {}", e)))?;
    let replicated_command = redis_value_as_int(value[9].clone())?;

    Ok(QueryInfo {
        received_at,
        graph_name,
        query,
        execution_duration,
        replicated_command,
    })
}
fn redis_value_as_string(value: redis::Value) -> BenchmarkResult<String> {
    match value {
        redis::Value::BulkString(data) => String::from_utf8(data.clone())
            .map_err(|_| OtherError(format!("parsing string failed: {:?}", data))),
        redis::Value::SimpleString(data) => Ok(data),
        redis::Value::VerbatimString { format: _, text } => Ok(text),
        _ => Err(OtherError(format!("parsing string failed: {:?}", value))),
    }
}
fn redis_value_as_int(value: redis::Value) -> BenchmarkResult<i64> {
    match value {
        redis::Value::Int(int_val) => Ok(int_val),
        _ => Err(OtherError(format!("parsing int failed: {:?}", value))),
    }
}
fn redis_value_as_vec(value: redis::Value) -> BenchmarkResult<Vec<redis::Value>> {
    match value {
        redis::Value::Array(bulk_val) => Ok(bulk_val),
        _ => Err(OtherError(format!("parsing array failed: {:?}", value))),
    }
}

async fn execute_i64_query(
    graph: &mut AsyncGraph,
    query: &str,
) -> BenchmarkResult<i64> {
    let mut values = graph.query(query).with_timeout(5000).execute().await?;
    if let Some(value) = values.data.next() {
        match value.as_slice() {
            [I64(i64_value)] => Ok(i64_value.clone()),
            _ => {
                let msg = format!("Unexpected response: {:?} for query {}", value, query);
                error!(msg);
                Err(OtherError(msg))
            }
        }
    } else {
        Err(OtherError(format!("No response for query: {}", query)))
    }
}
