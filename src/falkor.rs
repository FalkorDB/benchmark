use crate::error::BenchmarkError::{FailedToSpawnProcessError, OtherError};
use crate::error::BenchmarkResult;
use crate::utils::{
    create_directory_if_not_exists, delete_file, falkor_shared_lib_path, get_command_pid,
    kill_process, wait_for_redis_ready,
};
use falkordb::{AsyncGraph, FalkorClientBuilder, LazyResultSet, QueryResult};
use futures::StreamExt;
use histogram::Histogram;
use neo4rs::{query, Row};
use std::pin::Pin;
use std::process::{Child, Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};
use std::{env, io};
use tokio::fs;
use tokio::time::timeout;
use tracing::{error, info, trace};

#[derive(Clone)]
pub struct Connected(AsyncGraph);
#[derive(Clone)]
pub struct Disconnected;

#[derive(Clone)]
pub(crate) struct Falkor<U> {
    path: String,
    graph: U,
}

impl Falkor<Disconnected> {
    pub fn new() -> Falkor<Disconnected> {
        let default = falkor_shared_lib_path().unwrap();
        let path = env::var("FALKOR_PATH").unwrap_or_else(|_| default);
        info!("Falkor shared lib path: {}", path);
        Falkor {
            path,
            graph: Disconnected,
        }
    }
    pub async fn connect(&self) -> BenchmarkResult<Falkor<Connected>> {
        let connection_info = "falkor://127.0.0.1:6379".try_into()?;
        let client = FalkorClientBuilder::new_async()
            .with_connection_info(connection_info)
            .build()
            .await?;
        Ok(Falkor {
            path: self.path.clone(),
            graph: Connected(client.select_graph("falkor")),
        })
    }
}
impl Falkor<Connected> {
    pub async fn disconnect(&self) -> BenchmarkResult<Falkor<Disconnected>> {
        Ok(Falkor {
            path: self.path.clone(),
            graph: Disconnected,
        })
    }
    pub(crate) async fn execute_query(
        &mut self,
        q: &str,
    ) -> BenchmarkResult<QueryResult<LazyResultSet>> {
        trace!("Executing query: {}", q);
        let graph = &mut self.graph.0;
        let falkor_result = graph.query(q).with_timeout(5000).execute().await;
        match falkor_result {
            Ok(query_result) => Ok(query_result),
            Err(e) => {
                error!("Error executing query: {}, error: {}", q, e);
                Err(OtherError(e.to_string()))
            }
        }
    }
    pub(crate) async fn execute_query_stream<S>(
        &mut self,
        mut stream: S,
        histogram: &mut Histogram,
    ) -> BenchmarkResult<()>
    where
        S: StreamExt<Item = Result<String, io::Error>> + Unpin,
    {
        while let Some(line_or_error) = stream.next().await {
            match line_or_error {
                Ok(line) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() || trimmed == ";" {
                        continue;
                    }
                    let start = Instant::now();
                    let mut results = self.execute_query(line.as_str()).await?;
                    while let Some(nodes) = results.data.next() {
                        trace!("Row: {:?}", nodes);
                    }

                    let duration = start.elapsed();
                    histogram.increment(duration.as_micros() as u64)?;
                }
                Err(e) => error!("Error reading line: {}", e),
            }
        }
        Ok(())
    }
}

impl<U> Falkor<U> {
    pub async fn start(&self) -> BenchmarkResult<Child> {
        self.stop().await;
        create_directory_if_not_exists("./redis-data").await?;
        let command = "redis-server";
        let args = ["--dir", "./redis-data", "--loadmodule", self.path.as_str()];
        info!("Starting Falkor process: {} {}", command, args.join(" "));

        let child = Command::new(command)
            .args(args)
            // .stdout(Stdio::null())
            .spawn()
            .map_err(|e| {
                FailedToSpawnProcessError(
                    e,
                    format!(
                        "Failed to spawn falkor process, cmd: {} {}",
                        command,
                        args.join(" ")
                    )
                    .to_string(),
                )
            })?;

        let pid: u32 = child.id();
        self.wait_for_ready().await?;

        info!("Falkor is running: {}", pid);
        Ok(child)
    }

    async fn wait_for_ready(&self) -> BenchmarkResult<()> {
        info!("wait_for_ready");
        wait_for_redis_ready(10, Duration::from_millis(500)).await
    }

    pub(crate) async fn clean_db(&self) -> bool {
        self.stop().await;
        delete_file("./redis-data/dump.rdb").await
    }

    pub(crate) async fn get_redis_pid(&self) -> BenchmarkResult<Option<u32>> {
        get_command_pid("redis-server").await
    }
    pub(crate) async fn stop(&self) -> bool {
        if let Ok(Some(pid)) = self.get_redis_pid().await {
            info!("Killing Falkor: {}", pid);
            kill_process(pid).await.is_ok()
        } else {
            false
        }
    }
}
