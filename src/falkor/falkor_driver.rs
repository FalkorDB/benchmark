use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::falkor::falkor_process::FalkorProcess;
use crate::queries_repository::{PreparedQuery, QueryType};
use crate::scenario::Size;
use crate::scheduler::Msg;
use crate::utils::{
    delete_file, falkor_shared_lib_path, file_exists, get_command_pid, redis_save, redis_shutdown,
    wait_for_redis_ready,
};
use crate::{
    FALKOR_MSG_DEADLINE_OFFSET_GAUGE, OPERATION_COUNTER, OPERATION_ERROR_COUNTER, REDIS_DATA_DIR,
};
use falkordb::FalkorValue::I64;
use falkordb::{AsyncGraph, FalkorClientBuilder, FalkorResult, LazyResultSet, QueryResult};
use std::env;
use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;
use tokio::fs;
use tokio::time::error::Elapsed;
use tracing::{error, info};

const REDIS_DUMP_FILE: &str = "./redis-data/dump.rdb";

#[allow(dead_code)]
pub struct Started(FalkorProcess);
pub struct Stopped;

pub struct Falkor<U> {
    path: String,
    #[allow(dead_code)]
    state: U,
}

impl Default for Falkor<Stopped> {
    fn default() -> Self {
        Self::new()
    }
}

impl Falkor<Stopped> {
    fn new() -> Falkor<Stopped> {
        let default = falkor_shared_lib_path().unwrap();
        let path = env::var("FALKOR_PATH").unwrap_or(default);
        info!("falkor shared lib path: {}", path);
        Falkor {
            path,
            state: Stopped,
        }
    }
    pub async fn start(self) -> BenchmarkResult<Falkor<Started>> {
        let falkor_process: FalkorProcess = FalkorProcess::new().await?;
        Self::wait_for_ready().await?;
        Ok(Falkor {
            path: self.path.clone(),
            state: Started(falkor_process),
        })
    }
    pub async fn clean_db(&self) -> BenchmarkResult<()> {
        info!("deleting: {}", REDIS_DUMP_FILE);
        delete_file(REDIS_DUMP_FILE).await?;
        Ok(())
    }

    pub async fn save_db(
        &self,
        size: Size,
    ) -> BenchmarkResult<()> {
        if self.get_redis_pid().await.is_ok() {
            redis_shutdown().await?;
        }

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
impl Falkor<Started> {
    pub async fn stop(self) -> BenchmarkResult<Falkor<Stopped>> {
        redis_save().await?;
        Self::wait_for_ready().await?;
        Ok(Falkor {
            path: self.path.clone(),
            state: Stopped,
        })
    }
    pub async fn graph_size(&self) -> BenchmarkResult<(u64, u64)> {
        let mut graph = self.client().await?.graph;
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
}

impl<U> Falkor<U> {
    pub async fn client(&self) -> BenchmarkResult<FalkorBenchmarkClient> {
        let connection_info = "falkor://127.0.0.1:6379".try_into()?;
        let client = FalkorClientBuilder::new_async()
            .with_connection_info(connection_info)
            .with_num_connections(nonzero::nonzero!(1u8))
            .build()
            .await?;
        Ok(FalkorBenchmarkClient {
            graph: client.select_graph("falkor"),
        })
    }

    async fn wait_for_ready() -> BenchmarkResult<()> {
        wait_for_redis_ready(10, Duration::from_millis(500)).await
    }

    pub async fn get_redis_pid(&self) -> BenchmarkResult<u32> {
        get_command_pid("redis-server").await
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
        if self.get_redis_pid().await.is_ok() {
            redis_shutdown().await?;
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
    pub async fn execute_queries(
        &mut self,
        spawn_id: usize,
        queries: Arc<Box<dyn Iterator<Item = PreparedQuery> + Send + Sync>>,
    ) {
        let spawn_id = spawn_id.to_string();
        match Arc::try_unwrap(queries) {
            Ok(queries) => {
                for PreparedQuery { q_name, cypher, .. } in queries {
                    let res = self
                        ._execute_query(spawn_id.as_str(), q_name.as_str(), cypher.as_str())
                        .await;
                    if let Err(e) = res {
                        error!("Error executing query: {}, the error is: {:?}", cypher, e);
                    }
                }
            }
            Err(arc) => {
                error!(
                    "Failed to unwrap queries iterator, Remaining references count: {}",
                    Arc::strong_count(&arc)
                );
            }
        }
    }

    pub async fn execute_prepared_query<S: AsRef<str>>(
        &mut self,
        worker_id: S,
        msg: &Msg<PreparedQuery>,
        simulate: &Option<usize>,
    ) -> BenchmarkResult<()> {
        let Msg {
            payload:
                PreparedQuery {
                    q_name,
                    cypher,
                    q_type,
                    ..
                },
            ..
        } = msg;

        let worker_id = worker_id.as_ref();
        let query = cypher.as_str();
        let falkor_result = match q_type {
            QueryType::Read => self.graph.ro_query(query).execute(),
            QueryType::Write => self.graph.query(query).execute(),
        };

        let timeout = Duration::from_secs(60);
        let offset = msg.compute_offset_ms();

        FALKOR_MSG_DEADLINE_OFFSET_GAUGE.set(offset);
        if offset > 0 {
            // sleep offset millis
            tokio::time::sleep(Duration::from_millis(offset as u64)).await;
        }

        if let Some(delay) = simulate {
            if *delay > 0 {
                let delay: u64 = *delay as u64;
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            return Ok(());
        }

        let falkor_result = tokio::time::timeout(timeout, falkor_result).await;
        OPERATION_COUNTER
            .with_label_values(&["falkor", worker_id, "", q_name, "", ""])
            .inc();
        Self::read_reply(worker_id, q_name, query, falkor_result)
    }

    // #[instrument(skip(self), fields(query = %query, query_name = %query_name))]
    pub async fn _execute_query<'a>(
        &'a mut self,
        spawn_id: &'a str,
        query_name: &'a str,
        query: &'a str,
    ) -> BenchmarkResult<()> {
        // "vendor", "type", "name", "dataset", "dataset_size"
        OPERATION_COUNTER
            .with_label_values(&["falkor", spawn_id, "", query_name, "", ""])
            .inc();

        let falkor_result = self.graph.query(query).with_timeout(5000).execute();
        let timeout = Duration::from_secs(5);
        let falkor_result = tokio::time::timeout(timeout, falkor_result).await;
        Self::read_reply(spawn_id, query_name, query, falkor_result)
    }

    fn read_reply<'a>(
        spawn_id: &'a str,
        query_name: &'a str,
        query: &'a str,
        reply: Result<FalkorResult<QueryResult<LazyResultSet<'a>>>, Elapsed>,
    ) -> BenchmarkResult<()> {
        match reply {
            Ok(falkor_result) => match falkor_result {
                Ok(query_result) => {
                    for row in query_result.data {
                        black_box(row);
                    }
                    Ok(())
                }
                Err(e) => {
                    let error_type = std::any::type_name_of_val(&e);
                    error!("Error executing query: {}, the error is: {:?}", query, e);
                    Err(OtherError(format!(
                        "Error (type {}) executing query: {}, the error is: {:?}",
                        error_type, query, e
                    )))
                }
            },

            Err(e) => {
                OPERATION_ERROR_COUNTER
                    .with_label_values(&["falkor", spawn_id, "", query_name, "", ""])
                    .inc();
                let error_type = std::any::type_name_of_val(&e);
                error!("Error executing query: {}, the error is: {:?}", query, e);
                Err(OtherError(format!(
                    "Error (type {}) executing query: {}, the error is: {:?}",
                    error_type, query, e
                )))
            }
        }
    }
}
