use crate::error::BenchmarkError::{FailedToSpawnProcessError, OtherError};
use crate::error::BenchmarkResult;
use crate::metrics_collector::MetricsCollector;
use crate::queries_repository::QueryType;
use crate::scenario::Size;
use crate::utils::{
    create_directory_if_not_exists, delete_file, falkor_shared_lib_path, file_exists,
    get_command_pid, kill_process, redis_save, wait_for_redis_ready,
};
use falkordb::FalkorValue::I64;
use falkordb::{AsyncGraph, FalkorClientBuilder, LazyResultSet, QueryResult};
use futures::{Stream, StreamExt};
use histogram::Histogram;
use std::process::{Child, Command};
use std::time::{Duration, Instant};
use std::{env, io};
use tokio::fs;
use tracing::{error, info, trace};

const REDIS_DUMP_FILE: &str = "./redis-data/dump.rdb";
const REDIS_DATA_DIR: &str = "./redis-data";

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
        info!("falkor shared lib path: {}", path);
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
    pub(crate) async fn graph_size(&self) -> BenchmarkResult<(u64, u64)> {
        let mut graph = self.graph.0.clone();
        let mut falkor_result = graph
            .query("MATCH (n) RETURN count(n) as count")
            .with_timeout(5000)
            .execute()
            .await?;
        let node_count = self.extract_u64_value(&mut falkor_result)?;
        let mut falkor_result = graph
            .query("MATCH ()-->() RETURN count(*) AS relationshipCount")
            .with_timeout(5000)
            .execute()
            .await?;
        let relation_count = self.extract_u64_value(&mut falkor_result)?;
        Ok((node_count, relation_count))
    }

    fn extract_u64_value(
        &self,
        falkor_result: &mut QueryResult<LazyResultSet>,
    ) -> BenchmarkResult<u64> {
        match falkor_result.data.next().as_deref() {
            Some([I64(value)]) => Ok(*value as u64),
            _ => Err(OtherError(
                "Value not found or not of expected type".to_string(),
            )),
        }
    }

    pub(crate) async fn execute_query<T: AsRef<str>>(
        &mut self,
        q: T,
    ) -> BenchmarkResult<QueryResult<LazyResultSet>> {
        let q = q.as_ref();
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
    pub(crate) async fn execute_query_iterator(
        &mut self,
        iter: Box<dyn Iterator<Item = (String, QueryType, String)> + '_>,
        metric_collector: &mut MetricsCollector,
    ) -> BenchmarkResult<()> {
        for (name, query_type, query) in iter {
            let start = Instant::now();
            let mut results = self.execute_query(query.as_str()).await?;
            // let mut size = 0;
            // let results: Vec<FalkorValue> = results.data.flatten().collect();
            // info!("Results: {:?}", results);
            let stats = results.stats.join(", ");
            while let Some(nodes) = results.data.next() {
                trace!("Row: {:?}", nodes);
            }
            let duration = start.elapsed();
            metric_collector.record(
                duration,
                name.as_str(),
                query_type,
                query.as_str(),
                stats.as_str(),
            )?;
        }
        Ok(())
    }

    pub(crate) async fn execute_query_stream<S>(
        &mut self,
        mut stream: S,
        histogram: &mut Histogram,
    ) -> BenchmarkResult<()>
    where
        S: Stream<Item = Result<String, io::Error>> + Unpin,
    {
        while let Some(line_or_error) = stream.next().await {
            match line_or_error {
                Ok(line) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() || trimmed == ";" {
                        continue;
                    }
                    trace!("Executing query: {}", line);
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
        self.stop(false).await?;
        create_directory_if_not_exists(REDIS_DATA_DIR).await?;
        let command = "redis-server";
        let args = [
            "--dir",
            REDIS_DATA_DIR,
            "--loadmodule",
            self.path.as_str(),
            "CACHE_SIZE",
            "40",
        ];
        info!("starting falkor: {} {}", command, args.join(" "));

        let child = Command::new(command)
            .args(args)
            // .stdout(Stdio::null())
            .spawn()
            .map_err(|e| {
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

        let pid: u32 = child.id();
        self.wait_for_ready().await?;

        info!("falkor is running: {}", pid);
        Ok(child)
    }

    async fn wait_for_ready(&self) -> BenchmarkResult<()> {
        wait_for_redis_ready(10, Duration::from_millis(500)).await
    }

    pub(crate) async fn clean_db(&self) -> BenchmarkResult<()> {
        self.stop(false).await?;
        info!("deleting: {}", REDIS_DUMP_FILE);
        delete_file(REDIS_DUMP_FILE).await?;
        Ok(())
    }

    pub(crate) async fn get_redis_pid(&self) -> BenchmarkResult<Option<u32>> {
        get_command_pid("redis-server").await
    }

    pub(crate) async fn stop(
        &self,
        flash: bool,
    ) -> BenchmarkResult<()> {
        if let Ok(Some(pid)) = self.get_redis_pid().await {
            info!("stopping falkor: {}", pid);
            if flash {
                info!("asking redis to save all data to disk");
                redis_save().await?;
                self.wait_for_ready().await?;
            }
            kill_process(pid).await?;
            info!("falkor stopped: {}", pid);
        }
        Ok(())
    }

    pub(crate) async fn save_db(
        &self,
        size: Size,
    ) -> BenchmarkResult<()> {
        if let Ok(Some(pid)) = self.get_redis_pid().await {
            Err(OtherError(format!(
                "Can't save the dump file: {}, while falkor is running",
                pid
            )))
        } else {
            let target = format!(
                "{}/{}_dump.rdb",
                REDIS_DATA_DIR,
                size.to_string().to_lowercase()
            );
            info!(
                "saving redis dump file {} to {}",
                REDIS_DUMP_FILE,
                target.as_str()
            );
            fs::copy(REDIS_DUMP_FILE, target.as_str()).await?;
            Ok(())
        }
    }

    pub(crate) async fn restore_db(
        &self,
        size: Size,
    ) -> BenchmarkResult<()> {
        let source = format!(
            "{}/{}_dump.rdb",
            REDIS_DATA_DIR,
            size.to_string().to_lowercase()
        );
        if let Ok(Some(pid)) = self.get_redis_pid().await {
            return Err(OtherError(format!(
                "Can't restore the dump file: {}, while falkor is running {}",
                source, pid
            )));
        }
        info!("copy {} to {}", source, REDIS_DUMP_FILE);
        if file_exists(source.as_str()).await {
            fs::copy(source.as_str(), REDIS_DUMP_FILE).await?;
        }
        Ok(())
    }

    pub(crate) async fn dump_exists_or_error(
        &self,
        size: Size,
    ) -> BenchmarkResult<()> {
        let path = format!(
            "{}/{}_dump.rdb",
            REDIS_DATA_DIR,
            size.to_string().to_lowercase()
        );
        if !file_exists(path.as_str()).await {
            Err(OtherError(format!(
                "Dump file not found: {}",
                path.as_str()
            )))
        } else {
            Ok(())
        }
    }
}
