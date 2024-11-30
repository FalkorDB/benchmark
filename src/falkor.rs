use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::falkor_process::FalkorProcess;
use crate::queries_repository::QueryType;
use crate::scenario::Size;
use crate::utils::{
    delete_file, falkor_shared_lib_path, file_exists, get_command_pid, redis_save,
    wait_for_redis_ready,
};
use crate::{OPERATION_COUNTER, OPERATION_ERROR_COUNTER};
use falkordb::FalkorValue::I64;
use falkordb::{AsyncGraph, FalkorClientBuilder, FalkorResult, LazyResultSet, QueryResult};
use std::env;
use std::time::Duration;
use tokio::fs;
use tracing::{error, info};

const REDIS_DUMP_FILE: &str = "./redis-data/dump.rdb";
const REDIS_DATA_DIR: &str = "./redis-data";

#[allow(dead_code)]
pub struct Started(FalkorProcess);
pub struct Stopped;

pub struct Falkor<U> {
    path: String,
    #[allow(dead_code)]
    state: U,
}

impl Falkor<Stopped> {
    pub fn new() -> Falkor<Stopped> {
        let default = falkor_shared_lib_path().unwrap();
        let path = env::var("FALKOR_PATH").unwrap_or_else(|_| default);
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
}
impl Falkor<Started> {
    pub async fn stop(mut self) -> BenchmarkResult<Falkor<Stopped>> {
        redis_save().await?;
        Self::wait_for_ready().await?;
        self.state.0.terminate().await?;
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
    // #[instrument(skip(self, queries))]
    pub async fn execute_queries(
        &mut self,
        spawn_id: usize,
        queries: Vec<(String, QueryType, String)>,
    ) {
        let spawn_id = spawn_id.to_string();
        for (index, (query_name, _query_type, query)) in queries.into_iter().enumerate() {
            let _res = self
                .execute_query(spawn_id.as_str(), query_name.as_str(), query.as_str())
                .await;
            // info!(
            //     "executed: query_name={}, query:{}, res {:?}",
            //     query_name, query, _res
            // );
            if let Err(e) = _res {
                error!(
                    "Error executing query: {}, the error is: {:?}, index is: {}",
                    query, e, index
                );
            }
        }
    }

    // #[instrument(skip(self), fields(query = %query, query_name = %query_name))]
    pub async fn execute_query<'a>(
        &'a mut self,
        spawn_id: &'a str,
        query_name: &'a str,
        query: &'a str,
    ) -> BenchmarkResult<()> {
        // "vendor", "type", "name", "dataset", "dataset_size"
        OPERATION_COUNTER
            .with_label_values(&["falkor", spawn_id, "", query_name, "", ""])
            .inc();
        let falkor_result = self.graph.query(query).with_timeout(5000).execute().await;
        Self::read_reply(spawn_id, query_name, query, falkor_result)
    }

    // #[instrument(skip(reply), fields(result = field::Empty, error_type = field::Empty))]
    fn read_reply<'a>(
        spawn_id: &'a str,
        query_name: &'a str,
        query: &'a str,
        reply: FalkorResult<QueryResult<LazyResultSet<'a>>>,
    ) -> BenchmarkResult<()> {
        match reply {
            Ok(_) => {
                tracing::Span::current().record("result", &"success");
                Ok(())
            }
            Err(e) => {
                OPERATION_ERROR_COUNTER
                    .with_label_values(&["falkor", spawn_id, "", query_name, "", ""])
                    .inc();
                let error_type = std::any::type_name_of_val(&e);
                // tracing::Span::current().record("result", &"failure");
                // tracing::Span::current().record("error_type", &error_type);
                error!("Error executing query: {}, the error is: {:?}", query, e);
                Err(OtherError(format!(
                    "Error (type {}) executing query: {}, the error is: {:?}",
                    error_type, query, e
                )))
            }
        }
    }
}
