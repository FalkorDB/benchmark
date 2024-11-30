use crate::error::BenchmarkError::{FailedToSpawnProcessError, OtherError};
use crate::error::BenchmarkResult;
use crate::utils::{create_directory_if_not_exists, falkor_shared_lib_path};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::env;
use std::sync::Arc;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{error, info};

const REDIS_DATA_DIR: &str = "./redis-data";
pub struct FalkorProcess {
    state: Arc<Mutex<State>>,
}

struct State {
    child: Child,
    should_monitor: bool,
    handles: Vec<JoinHandle<()>>,
}

impl FalkorProcess {
    pub async fn new() -> BenchmarkResult<Self> {
        let child = Self::spawn_process().await?;
        let should_monitor = true;
        let state = State {
            child,
            should_monitor,
            handles: Vec::new(),
        };
        let state_arc = Arc::new(Mutex::new(state));
        let state_arc_clone = Arc::clone(&state_arc);
        let handle = Self::monitor_process(state_arc_clone);
        {
            let mut state = state_arc.lock().await;
            state.handles.push(handle);
        }
        let state_arc_clone = Arc::clone(&state_arc);
        let handle = Self::monitor_server(state_arc_clone);
        {
            let mut state = state_arc.lock().await;
            state.handles.push(handle);
        }
        Ok(FalkorProcess { state: state_arc })
    }

    async fn spawn_process() -> BenchmarkResult<Child> {
        create_directory_if_not_exists(REDIS_DATA_DIR).await?;
        let default_so_path = falkor_shared_lib_path()?;
        let default_so_path = env::var("FALKOR_PATH").unwrap_or_else(|_| default_so_path.clone());

        let command = "redis-server";
        let args = [
            "--dir",
            REDIS_DATA_DIR,
            "--loadmodule",
            default_so_path.as_str(),
            "CACHE_SIZE",
            "40",
        ];

        info!("starting falkor: {} {}", command, args.join(" "));

        let child = Command::new(command).args(&args).spawn().map_err(|e| {
            FailedToSpawnProcessError(
                e,
                format!(
                    "failed to spawn falkor process, cmd: {} {}",
                    command,
                    args.join(" ")
                )
                .to_string(),
            )
        })?;
        if let Some(pid) = child.id() {
            info!("falkor process started with PID: {}", pid);
        } else {
            error!("Failed to get PID of falkor process");
        }
        Ok(child)
    }
    fn monitor_process(arc_state: Arc<Mutex<State>>) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let mut state = arc_state.lock().await;

                // Check if monitoring is disabled
                if !state.should_monitor {
                    info!("Monitoring stopped; exiting loop.");
                    break;
                }
                let child_lock = &mut state.child;
                match child_lock.try_wait() {
                    Ok(Some(status)) => {
                        info!("falkor process exited with status: {:?}", status);
                        match Self::spawn_process().await {
                            Ok(new_child) => {
                                *child_lock = new_child;
                                info!("Restarted falkor process with PID: {:?}", child_lock.id());
                            }
                            Err(e) => {
                                error!("Failed to restart falkor process: {:?}", e);
                                break;
                            }
                        }
                    }
                    Ok(None) => {
                        info!("falkor process is running with PID: {:?}", child_lock.id());
                    }
                    Err(e) => {
                        error!("Failed to check falkor process status: {:?}", e);
                        break;
                    }
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
        })
    }

    fn monitor_server(arc_state: Arc<Mutex<State>>) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let state = arc_state.lock().await;
                if !state.should_monitor {
                    info!("Monitoring server stopped; exiting loop.");
                    break;
                }
                match Self::monitor_server_once().await {
                    Ok(_) => {}
                    Err(e) => {
                        error!("Failed to monitor server: {:?}", e);
                        // break;
                    }
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
        })
    }

    async fn monitor_server_once() -> BenchmarkResult<()> {
        let client = redis::Client::open("redis://127.0.0.1:6379/")?;
        let mut con = client.get_multiplexed_async_connection().await?;
        // let graph_info = redis::cmd("GRAPH.INFO").query_async(&mut con).await?;

        let command = redis::cmd("GRAPH.INFO");
        let redis_value = con.send_packed_command(&command).await?;
        let (running_queries, waiting_queries) = redis_to_query_info(redis_value)?;
        info!(
            "Running Queries ({}):  {:?}",
            running_queries.len(),
            running_queries
        );
        info!(
            "Waiting Queries ({}): {:?}",
            waiting_queries.len(),
            waiting_queries
        );
        if running_queries.len() > 3 {
            error!("{} running queries found", running_queries.len());
        }
        if !waiting_queries.is_empty() {
            error!("Waiting queries found: {:?}", waiting_queries);
        }
        Ok(())
    }

    pub async fn terminate(&mut self) -> BenchmarkResult<()> {
        info!("Terminating FalkorProcess");
        // Lock the state mutex to access the child process
        let mut state_lock = self.state.lock().await;

        // Set should_monitor to false in order to prevent restarting
        state_lock.should_monitor = false;

        if let Some(pid) = state_lock.child.id() {
            let pid = Pid::from_raw(pid as i32);

            // Send SIGTERM signal to request graceful termination
            match kill(pid, Signal::SIGTERM) {
                Ok(_) => {
                    info!("Sent SIGTERM signal to falkor process with PID: {}", pid);

                    // Optionally wait for the process to exit
                    if let Err(e) = tokio::time::timeout(
                        std::time::Duration::from_secs(60),
                        state_lock.child.wait(),
                    )
                    .await
                    {
                        error!(
                            "Timed out waiting for falkor process with PID: {} to exit: {:?}",
                            pid, e
                        );
                    } else {
                        info!("Falkor process with PID: {} exited successfully", pid);
                    }
                }
                Err(e) => error!(
                    "Failed to send SIGTERM signal to falkor process: {:?}, with PID: {}",
                    e, pid
                ),
            }
        } else {
            error!("Failed to get PID of falkor process");
        }

        // waiting for each handle in state_lock.handles
        // for handle in state_lock.handles.drain(..) {
        //     if let Err(e) = handle.await {
        //         error!("Failed to wait for handle: {:?}", e);
        //     }
        // }
        Ok(())
    }
}

impl Drop for FalkorProcess {
    fn drop(&mut self) {
        info!("Dropping FalkorProcess");
        let rt = tokio::runtime::Runtime::new().unwrap();
        match rt.block_on(async { self.terminate().await }) {
            Ok(_) => {
                info!("FalkorProcess terminated successfully");
            }
            Err(_) => {
                error!("Failed to terminate FalkorProcess");
            }
        }
    }
}

/*
1) "# Running queries"
2) 1)  1) "Received at"
       2) (integer) 1732952858
       3) "Graph name"
       4) "falkor"
       5) "Query"
       6) "CYPHER id = 5610 MATCH (s:User {id: $id})-->()-->()-->(n:User) RETURN DISTINCT n.id"
       7) "Execution duration"
       8) "1.1392910000000003"
       9) "Replicated command"
      10) (integer) 0
3) "# Waiting queries"
4) (empty array)
 */

#[derive(Debug)]
#[allow(dead_code)]
struct QueryInfo {
    received_at: i64,
    graph_name: String,
    query: String,
    execution_duration: f64,
    replicated_command: i64,
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
