use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::process_monitor::ProcessMonitor;
use crate::utils::{
    create_directory_if_not_exists, delete_file, falkor_shared_lib_path, get_falkor_log_path,
    ping_redis, redis_shutdown,
};
use crate::{
    prometheus_metrics, CPU_USAGE_GAUGE, FALKOR_CPU_USAGE_GAUGE, FALKOR_MEM_USAGE_GAUGE,
    FALKOR_NODES_GAUGE, FALKOR_RELATIONSHIPS_GAUGE, FALKOR_RESTART_COUNTER,
    FALKOR_RUNNING_REQUESTS_GAUGE, FALKOR_WAITING_REQUESTS_GAUGE, MEM_USAGE_GAUGE, REDIS_DATA_DIR,
};
use falkordb::FalkorValue::I64;
use falkordb::{AsyncGraph, FalkorClientBuilder, FalkorConnectionInfo};
use prometheus::core::{AtomicU64, GenericCounter};
use std::env;
use std::sync::{Arc, Mutex};
use sysinfo::{Pid, System};
use tokio::task::JoinHandle;
use tracing::{error, info};

#[derive(Default)]
pub struct FalkorProcess {
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    process_handle: Option<JoinHandle<()>>,
    prom_shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    prom_process_handle: Option<JoinHandle<()>>,
    ping_server_shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    ping_server_handle: Option<JoinHandle<()>>,
    dropped: bool,
}

impl FalkorProcess {
    pub fn external() -> Self {
        // Create a FalkorProcess that doesn't manage any actual process
        // This is used for external endpoints
        Self {
            shutdown_tx: None,
            process_handle: None,
            prom_shutdown_tx: None,
            prom_process_handle: None,
            ping_server_shutdown_tx: None,
            ping_server_handle: None,
            dropped: true, // Mark as dropped so Drop doesn't try to terminate
        }
    }
    
    pub async fn new() -> BenchmarkResult<Self> {
        redis_shutdown().await?; // if redis run on this machine, use redis-cli to shut it down

        create_directory_if_not_exists(REDIS_DATA_DIR).await?;
        let falkor_log_path = get_falkor_log_path()?;
        delete_file(falkor_log_path.as_str()).await?;

        let default_so_path = falkor_shared_lib_path()?;
        let default_so_path = env::var("FALKOR_PATH").unwrap_or_else(|_| default_so_path.clone());
        let falkor_log_path = get_falkor_log_path()?;
        let command = "redis-server".to_string();

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
            "400",
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
        let falkor_process_monitor = tokio::spawn(async move {
            let _ = process_monitor.run(counter).await;
        });
        let process_handle = Some(falkor_process_monitor);

        let (prom_process_handle, prom_shutdown_tx) =
            prometheus_metrics::run_metrics_reporter(report_metrics);

        let (ping_server_handle, ping_server_shutdown_tx) = ping_server();

        Ok(Self {
            shutdown_tx: Some(shutdown_tx),
            process_handle,
            prom_shutdown_tx: Some(prom_shutdown_tx),
            prom_process_handle: Some(prom_process_handle),
            ping_server_shutdown_tx: Some(ping_server_shutdown_tx),
            ping_server_handle: Some(ping_server_handle),
            dropped: false,
        })
    }
    async fn terminate(&mut self) {
        if let Some(ping_server_shutdown_tx) = self.ping_server_shutdown_tx.take() {
            drop(ping_server_shutdown_tx);
        }
        if let Some(ping_server_handle) = self.ping_server_handle.take() {
            let _ = ping_server_handle.await;
        }
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
                        "Error dropping FalkorProcess: {:?}, cleanup task finish with error",
                        e
                    );
                }
            }
        }
    }
}

fn ping_server() -> (JoinHandle<()>, tokio::sync::oneshot::Sender<()>) {
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
                    if let Err(e) = ping_redis().await {
                        error!("Error pinging server: {:?}", e);
                    }
                }
                _ = &mut shutdown_rx => {
                    info!("Shutting down ping_server");
                    return;
                }
            }
        }
    });

    (handle, shutdown_tx)
}

async fn report_metrics(system: Arc<Mutex<System>>) -> BenchmarkResult<()> {
    let client = redis::Client::open("redis://127.0.0.1:6379/")?;
    let mut con = client.get_multiplexed_async_connection().await?;

    let command = redis::cmd("GRAPH.INFO");
    let redis_value = con.send_packed_command(&command).await?;
    let (running_queries, waiting_queries) = redis_to_query_info(redis_value)?;

    FALKOR_RUNNING_REQUESTS_GAUGE.set(running_queries as i64);
    FALKOR_WAITING_REQUESTS_GAUGE.set(waiting_queries as i64);

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

    fill_memory_and_cpu_metrics(system).await?;

    Ok(())
}

async fn fill_memory_and_cpu_metrics(sys: Arc<Mutex<System>>) -> BenchmarkResult<()> {
    let mut system = sys.lock().unwrap();
    // Refresh CPU usage
    system.refresh_all();
    let logical_cpus = system.cpus().len();
    let cpu_usage = system.global_cpu_usage() as i64 / logical_cpus as i64;
    CPU_USAGE_GAUGE.set(cpu_usage);

    // Refresh memory usage
    let mem_used = system.used_memory();
    MEM_USAGE_GAUGE.set(mem_used as i64);

    // Find the specific process
    if let Some(pid) = get_falkor_server_pid() {
        if let Some(process) = system.process(Pid::from(pid as usize)) {
            let cpu_usage = process.cpu_usage() as i64 / logical_cpus as i64;
            FALKOR_CPU_USAGE_GAUGE.set(cpu_usage);
            let mem_used = process.memory() as i64;
            FALKOR_MEM_USAGE_GAUGE.set(mem_used);
        }
    }

    Ok(())
}

fn get_falkor_server_pid() -> Option<u32> {
    let system = System::new_all();
    let res = system.processes().iter().find(|(_, process)| {
        if let Some(name_str) = process.name().to_str() {
            name_str == "redis-server"
        } else {
            false
        }
    });
    res.map(|(pid, _)| pid.as_u32())
}

// return a tuple the of (running_queries, waiting_queries)
// first element of the tuple is a vector of the running queries
// second element of the tuple is a vector of waiting
// use redis_vec_as_query_info to parse each query info
fn redis_to_query_info(value: redis::Value) -> BenchmarkResult<(usize, usize)> {
    // Convert the value into a vector of redis::Value
    let queries = redis_value_as_vec(value)?;
    if queries.len() < 4 {
        return Err(OtherError(format!(
            "Insufficient data in Redis response {:?}",
            queries
        )));
    }
    let running_vec = redis_value_as_vec(queries[1].clone())?;

    let waiting_vec = redis_value_as_vec(queries[3].clone())?;

    // Return the collected running and waiting queries
    Ok((running_vec.len(), waiting_vec.len()))
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
            [I64(i64_value)] => Ok(*i64_value),
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
