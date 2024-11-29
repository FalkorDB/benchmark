use crate::error::BenchmarkError::{FailedToSpawnProcessError, OtherError};
use crate::error::BenchmarkResult;
use crate::metrics_collector::MetricsCollector;
use crate::queries_repository::QueryType;
use crate::scenario::Size;
use crate::utils::{
    create_directory_if_not_exists, delete_file, falkor_shared_lib_path, file_exists,
    get_command_pid, kill_process, redis_save, wait_for_redis_ready,
};
use crate::{OPERATION_COUNTER, OPERATION_ERROR_COUNTER};
use falkordb::FalkorValue::I64;
use falkordb::{AsyncGraph, FalkorClientBuilder, FalkorResult, LazyResultSet, QueryResult};
use futures::{Stream, StreamExt};
use histogram::Histogram;
use std::process::{Child, Command};
use std::time::{Duration, Instant};
use std::{env, io};
use tokio::fs;
use tokio::time::sleep;
use tracing::{error, field, info, instrument, trace};

const REDIS_DUMP_FILE: &str = "./redis-data/dump.rdb";
const REDIS_DATA_DIR: &str = "./redis-data";

#[derive(Clone)]
pub struct Connected(AsyncGraph);
#[derive(Clone)]
pub struct Disconnected;

#[derive(Clone)]
pub struct Falkor<U> {
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
    pub async fn graph_size(&self) -> BenchmarkResult<(u64, u64)> {
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

    #[instrument(skip(self, q), fields(query = %q.as_ref()))]
    pub async fn execute_query<T: AsRef<str>>(
        &mut self,
        _query_type: QueryType,
        _query_template: String,
        q: T,
    ) -> BenchmarkResult<QueryResult<LazyResultSet>> {
        let q = q.as_ref().to_owned();
        info!("Executing query: {}", q);
        let graph = &mut self.graph.0;
        let falkor_result = Self::call_server(q.clone(), graph).await;
        Self::read_reply(falkor_result)
    }

    #[instrument(skip(graph))]
    async fn call_server(
        q: String,
        graph: &mut AsyncGraph,
    ) -> FalkorResult<QueryResult<LazyResultSet>> {
        graph.query(q).with_timeout(5000).execute().await
    }

    #[instrument(skip(falkor_result))]
    fn read_reply(
        falkor_result: FalkorResult<QueryResult<LazyResultSet>>
    ) -> BenchmarkResult<QueryResult<LazyResultSet>> {
        match falkor_result {
            Ok(query_result) => Ok(query_result),
            Err(e) => {
                error!("Error {} while executing query", e);
                Err(OtherError(e.to_string()))
            }
        }
    }

    pub async fn execute_query_un_trace<T: AsRef<str>>(
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
    pub async fn execute_query_iterator(
        &mut self,
        iter: Box<dyn Iterator<Item = (String, QueryType, String)> + '_>,
        metric_collector: &mut MetricsCollector,
    ) -> BenchmarkResult<()> {
        let mut count = 0u64;
        for (query_template, query_type, query) in iter {
            let start = Instant::now();
            let mut results = self
                .execute_query(query_type.clone(), query_template.clone(), query.as_str())
                .await?;
            let mut rows = 0;
            while let Some(nodes) = results.data.next() {
                trace!("Row: {:?}", nodes);
                rows += 1;
            }
            let duration = start.elapsed();
            count += 1;
            if count % 10000 == 0 {
                info!("{} queries executed", count);
            }
            let stats = format!("{}, {} rows returned", results.stats.join(", "), rows);
            metric_collector.record(
                duration,
                query_template.as_str(),
                query_type,
                query.as_str(),
                stats.as_str(),
            )?;
        }
        Ok(())
    }

    pub async fn execute_query_stream<S>(
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
                    let mut count = 0u64;
                    let start = Instant::now();
                    let mut results = self.execute_query_un_trace(line.as_str()).await?;
                    while let Some(nodes) = results.data.next() {
                        trace!("Row: {:?}", nodes);
                    }

                    let duration = start.elapsed();
                    count += 1;
                    if count % 10000 == 0 {
                        info!("{} queries executed", count);
                    }
                    histogram.increment(duration.as_micros() as u64)?;
                }
                Err(e) => error!("Error reading line: {}", e),
            }
        }
        Ok(())
    }
}

impl<U> Falkor<U> {
    pub async fn client(&self) -> BenchmarkResult<FalkorBenchmarkClient> {
        let connection_info = "falkor://127.0.0.1:6379".try_into()?;
        let client = FalkorClientBuilder::new_async()
            .with_connection_info(connection_info)
            .build()
            .await?;
        Ok(FalkorBenchmarkClient {
            graph: client.select_graph("falkor"),
        })
    }

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

    pub async fn clean_db(&self) -> BenchmarkResult<()> {
        self.stop(false).await?;
        info!("deleting: {}", REDIS_DUMP_FILE);
        delete_file(REDIS_DUMP_FILE).await?;
        Ok(())
    }

    pub async fn get_redis_pid(&self) -> BenchmarkResult<u32> {
        get_command_pid("redis-server").await
    }

    pub async fn stop(
        &self,
        flash: bool,
    ) -> BenchmarkResult<()> {
        if let Ok(pid) = self.get_redis_pid().await {
            if flash {
                info!("asking redis to save all data to disk");
                redis_save().await?;
                self.wait_for_ready().await?;
            }
            info!("stopping falkor: {}", pid);
            kill_process(pid).await?;
            info!("falkor stopped: {}", pid);
        }
        while let Ok(pid) = self.get_redis_pid().await {
            info!("waiting for falkor (pid) to stop: {}", pid);
            sleep(Duration::from_millis(200)).await;
        }
        Ok(())
    }

    pub async fn save_db(
        &self,
        size: Size,
    ) -> BenchmarkResult<()> {
        if let Ok(pid) = self.get_redis_pid().await {
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

    pub async fn restore_db(
        &self,
        size: Size,
    ) -> BenchmarkResult<()> {
        let source = format!(
            "{}/{}_dump.rdb",
            REDIS_DATA_DIR,
            size.to_string().to_lowercase()
        );
        if let Ok(pid) = self.get_redis_pid().await {
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

    pub async fn dump_exists_or_error(
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

#[derive(Clone)]
pub struct FalkorBenchmarkClient {
    graph: AsyncGraph,
}

impl FalkorBenchmarkClient {
    #[instrument(skip(self, queries))]
    pub async fn execute_queries(
        &mut self,
        spawn_id: usize,
        queries: Vec<(String, QueryType, String)>,
    ) {
        let spawn_id = spawn_id.to_string();
        for (query_name, _query_type, query) in queries.into_iter() {
            let _res = self
                .execute_query(spawn_id.as_str(), query_name.as_str(), query.as_str())
                .await;
            // info!("executed: query_name={}, query:{} ", query_name, query);
        }
    }

    #[instrument(skip(self), fields(query = %query, query_name = %query_name))]
    pub async fn execute_query<'a>(
        &'a mut self,
        spawn_id: &'a str,
        query_name: &'a str,
        query: &'a str,
    ) {
        // "vendor", "type", "name", "dataset", "dataset_size"
        OPERATION_COUNTER
            .with_label_values(&["falkor", spawn_id, "", query_name, "", ""])
            .inc();
        let falkor_result = self.graph.query(query).with_timeout(5000).execute().await;
        Self::read_reply(spawn_id, query_name, falkor_result)
    }

    #[instrument(skip(reply), fields(result = field::Empty, error_type = field::Empty))]
    fn read_reply(
        spawn_id: &str,
        query_name: &str,
        reply: FalkorResult<QueryResult<LazyResultSet>>,
    ) {
        match reply {
            Ok(_) => {
                tracing::Span::current().record("result", &"success");
                // info!("Query executed successfully");
            }
            Err(e) => {
                OPERATION_ERROR_COUNTER
                    .with_label_values(&["falkor", spawn_id, "", query_name, "", ""])
                    .inc();
                // tracing::Span::current().record("result", &"failure");
                let error_type = std::any::type_name_of_val(&e);
                tracing::Span::current().record("result", &"failure");
                tracing::Span::current().record("error_type", &error_type);
                error!("Error executing query: {:?}", e);
            }
        }
    }
}
