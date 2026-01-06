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
    FALKOR_GRAPH_MEMORY_USAGE_MB, FALKOR_MSG_DEADLINE_OFFSET_GAUGE, OPERATION_COUNTER,
    OPERATION_ERROR_COUNTER, REDIS_DATA_DIR,
};
use falkordb::FalkorValue::I64;
use falkordb::{AsyncGraph, FalkorClientBuilder, FalkorResult, LazyResultSet, QueryResult};
use std::env;
use std::hint::black_box;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tokio::fs;
use tokio::time::error::Elapsed;
use tracing::{error, info};

const REDIS_DUMP_FILE: &str = "./redis-data/dump.rdb";

#[allow(dead_code)]
pub struct Started(FalkorProcess);
pub struct Stopped;

pub struct Falkor<U> {
    path: String,
    endpoint: Option<String>,
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
        Self::with_endpoint(None)
    }

    pub fn new_with_endpoint(endpoint: Option<String>) -> Self {
        Self::with_endpoint(endpoint)
    }

    fn with_endpoint(endpoint: Option<String>) -> Falkor<Stopped> {
        let default = falkor_shared_lib_path().unwrap();
        let path = env::var("FALKOR_PATH").unwrap_or(default);
        if let Some(ref ep) = endpoint {
            info!("using external falkor endpoint: {}", ep);
        } else {
            info!("falkor shared lib path: {}", path);
        }
        Falkor {
            path,
            endpoint,
            state: Stopped,
        }
    }
    pub async fn start(self) -> BenchmarkResult<Falkor<Started>> {
        if self.endpoint.is_some() {
            // For external endpoints, we don't manage a process
            // Just verify the connection is available
            info!("using external falkor endpoint, skipping process management");
            Ok(Falkor {
                path: self.path.clone(),
                endpoint: self.endpoint.clone(),
                state: Started(FalkorProcess::external()),
            })
        } else {
            let falkor_process: FalkorProcess = FalkorProcess::new().await?;
            Self::wait_for_ready().await?;
            Ok(Falkor {
                path: self.path.clone(),
                endpoint: self.endpoint.clone(),
                state: Started(falkor_process),
            })
        }
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
        if self.endpoint.is_none() {
            redis_save().await?;
            Self::wait_for_ready().await?;
        }
        Ok(Falkor {
            path: self.path.clone(),
            endpoint: self.endpoint.clone(),
            state: Stopped,
        })
    }

    /// Best-effort collection of graph memory usage via `GRAPH.MEMORY USAGE <graph>`.
    ///
    /// Sets the Prometheus gauge `falkordb_graph_memory_usage_mb` if the command succeeds.
    pub async fn collect_graph_memory_usage_metrics(&self) {
        // Avoid stale values when multiple runs happen in a single process.
        FALKOR_GRAPH_MEMORY_USAGE_MB.set(0);

        let graph_name = "falkor";
        match self.graph_memory_usage_mb(graph_name).await {
            Ok(Some(mb)) => {
                FALKOR_GRAPH_MEMORY_USAGE_MB.set(mb.round().max(0.0) as i64);
            }
            Ok(None) => {
                // Keep the reset-to-0 value.
            }
            Err(e) => {
                tracing::debug!("Failed collecting falkor graph memory: {}", e);
            }
        }
    }

    async fn graph_memory_usage_mb(
        &self,
        graph_name: &str,
    ) -> BenchmarkResult<Option<f64>> {
        let redis_url = falkor_endpoint_to_redis_url(self.endpoint.as_ref());
        let client = redis::Client::open(redis_url.as_str())?;
        let mut con = client.get_multiplexed_async_connection().await?;

        let mut command = redis::cmd("GRAPH.MEMORY");
        command.arg("USAGE").arg(graph_name);
        let redis_value = con.send_packed_command(&command).await?;

        Ok(parse_graph_memory_total_mb(redis_value))
    }

    pub async fn graph_size(&self) -> BenchmarkResult<(u64, u64)> {
        // Use FalkorDB's metadata procedure instead of full graph scans.
        // This is dramatically faster on large graphs and avoids query
        // timeouts that can occur with `MATCH (n) RETURN count(n)` on
        // multi-million-node datasets.
        let mut graph = self.client().await?.graph;
        let mut falkor_result = graph
            .query("CALL db.meta.stats()")
            // Allow up to 30 seconds for metadata retrieval on busy servers.
            .with_timeout(30_000)
            .execute()
            .await?;

        // According to FalkorDB docs, db.meta.stats() yields the columns:
        //   labels, relTypes, relCount, nodeCount, labelCount, relTypeCount, propertyKeyCount
        // in that order.
        match falkor_result.data.next().as_deref() {
            Some([_, _, I64(rel_count), I64(node_count), ..]) => {
                Ok((*node_count as u64, *rel_count as u64))
            }
            other => Err(OtherError(format!(
                "Unexpected response from CALL db.meta.stats(): {:?}",
                other
            ))),
        }
    }

    #[allow(dead_code)]
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

    /// Best-effort check that the Pokec benchmark indexes exist before
    /// running workload queries.
    ///
    /// This looks for indexes on :User(id) and :User(age) via `CALL db.indexes()`.
    /// If they are not visible after several attempts, we fail fast so the user
    /// gets a clear error instead of timeouts during the run.
    pub async fn wait_for_pokec_indexes_ready(&self) -> BenchmarkResult<()> {
        const MAX_ATTEMPTS: u32 = 12;
        const DELAY_SECS: u64 = 5;

        let mut client = self.client().await?;

        for attempt in 1..=MAX_ATTEMPTS {
            match Self::check_pokec_indexes(&mut client).await {
                Ok(true) => {
                    info!(
                        "FalkorDB Pokec indexes (:User.id, :User.age) are ready after {} attempt(s)",
                        attempt
                    );
                    return Ok(());
                }
                Ok(false) => {
                    info!(
                        "FalkorDB Pokec indexes not ready yet (attempt {}/{})",
                        attempt, MAX_ATTEMPTS
                    );
                }
                Err(e) => {
                    // Log and keep retrying; transient errors are expected while
                    // FalkorDB is still coming up.
                    info!(
                        "Error while checking FalkorDB indexes (attempt {}/{}): {}",
                        attempt, MAX_ATTEMPTS, e
                    );
                }
            }

            tokio::time::sleep(Duration::from_secs(DELAY_SECS)).await;
        }

        Err(OtherError(
            "Timed out waiting for FalkorDB Pokec indexes (:User.id, :User.age) to become ready"
                .to_string(),
        ))
    }

    async fn check_pokec_indexes(
        client: &mut FalkorBenchmarkClient,
    ) -> BenchmarkResult<bool> {
        // `CALL db.indexes()` returns metadata about all indexes.
        // We do a best-effort scan of the rows looking for :User(id) and :User(age).
        let mut result = client
            .graph
            .query("CALL db.indexes()")
            .with_timeout(30_000)
            .execute()
            .await?;

        let mut have_user_id = false;
        let mut have_user_age = false;

        while let Some(row) = result.data.next() {
            let row_str = format!("{:?}", row);
            if !have_user_id && row_str.contains("User") && row_str.contains("id") {
                have_user_id = true;
            }
            if !have_user_age && row_str.contains("User") && row_str.contains("age") {
                have_user_age = true;
            }

            if have_user_id && have_user_age {
                break;
            }
        }

        Ok(have_user_id && have_user_age)
    }
}

impl<U> Falkor<U> {
    pub async fn client(&self) -> BenchmarkResult<FalkorBenchmarkClient> {
        let connection_string = self
            .endpoint
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or("falkor://127.0.0.1:6379");
        let connection_info = connection_string.try_into()?;
        let client = FalkorClientBuilder::new_async()
            .with_connection_info(connection_info)
            .with_num_connections(nonzero::nonzero!(8u8))
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

pub fn falkor_endpoint_to_redis_url(endpoint: Option<&String>) -> String {
    let ep = endpoint
        .map(|s| s.as_str())
        .unwrap_or("falkor://127.0.0.1:6379");

    if let Some(rest) = ep.strip_prefix("falkor://") {
        format!("redis://{}", rest)
    } else {
        ep.to_string()
    }
}

fn parse_graph_memory_total_mb(value: redis::Value) -> Option<f64> {
    // Expected to be an array of key/value pairs.
    let redis::Value::Array(items) = value else {
        return None;
    };

    let mut i = 0;
    while i + 1 < items.len() {
        let key = redis_value_to_string(&items[i]);
        let val = &items[i + 1];

        if let Some(k) = key {
            if k == "total_graph_sz_mb" || k == "total_graph_size_mb" || k == "total_graph_mb" {
                return redis_value_to_f64(val);
            }
        }

        i += 2;
    }

    None
}

fn redis_value_to_string(v: &redis::Value) -> Option<String> {
    match v {
        redis::Value::BulkString(bytes) => Some(String::from_utf8_lossy(bytes).to_string()),
        redis::Value::SimpleString(s) => Some(s.clone()),
        redis::Value::Int(i) => Some(i.to_string()),
        _ => None,
    }
}

fn redis_value_to_f64(v: &redis::Value) -> Option<f64> {
    match v {
        redis::Value::Int(i) => Some(*i as f64),
        redis::Value::Double(d) => Some(*d),
        redis::Value::BulkString(bytes) => String::from_utf8_lossy(bytes).parse::<f64>().ok(),
        redis::Value::SimpleString(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

#[derive(Clone)]
pub struct FalkorBenchmarkClient {
    graph: AsyncGraph,
}

impl FalkorBenchmarkClient {
    async fn run_query_no_results(
        &mut self,
        q: &str,
    ) -> BenchmarkResult<()> {
        // Use a longer timeout for import statements.
        let res = self.graph.query(q).with_timeout(60_000).execute().await?;
        // Consume results (for completeness; most CREATE queries return no rows)
        for row in res.data {
            black_box(row);
        }
        Ok(())
    }

    /// Fast-path loader for the Pokec "Users" dataset using UNWIND batches.
    ///
    /// Input is a stream of cypher statements in two phases:
    /// - Node lines: `CREATE (:User { ... });`
    /// - Edge lines: `MATCH (n:User {id: X}), (m:User {id: Y}) CREATE (n)-[e: Friend]->(m);`
    ///
    /// We batch into:
    /// - Nodes: `UNWIND [ {...}, ... ] AS row CREATE (u:User) SET u = row`
    /// - Edges: `UNWIND [ {src:X,dst:Y}, ... ] AS row MATCH ... CREATE (n)-[:Friend]->(m)`
    pub async fn execute_pokec_users_import_unwind<S>(
        &mut self,
        mut stream: S,
        batch_size: usize,
    ) -> BenchmarkResult<usize>
    where
        S: StreamExt<Item = Result<String, io::Error>> + Unpin,
    {
        info!(
            "Processing Pokec Users import via UNWIND batches of {}",
            batch_size
        );

        #[derive(Copy, Clone, PartialEq, Eq)]
        enum Phase {
            Nodes,
            Edges,
        }

        let mut phase = Phase::Nodes;
        let mut node_maps: Vec<String> = Vec::with_capacity(batch_size);
        let mut edge_pairs: Vec<(u64, u64)> = Vec::with_capacity(batch_size);

        let mut total_processed: usize = 0;
        let mut batch_count: usize = 0;
        let start_time = tokio::time::Instant::now();
        let mut last_progress_report = start_time;
        const PROGRESS_INTERVAL_SECS: u64 = 5;

        async fn flush_nodes(
            client: &mut FalkorBenchmarkClient,
            node_maps: &mut Vec<String>,
            batch_count: &mut usize,
        ) -> BenchmarkResult<()> {
            if node_maps.is_empty() {
                return Ok(());
            }
            *batch_count += 1;
            let q = format!(
                "UNWIND [{}] AS row CREATE (u:User) SET u = row",
                node_maps.join(",")
            );
            client.run_query_no_results(&q).await?;
            node_maps.clear();
            Ok(())
        }

        async fn flush_edges(
            client: &mut FalkorBenchmarkClient,
            edge_pairs: &mut Vec<(u64, u64)>,
            batch_count: &mut usize,
        ) -> BenchmarkResult<()> {
            if edge_pairs.is_empty() {
                return Ok(());
            }
            *batch_count += 1;
            let mut maps = String::new();
            for (i, (src, dst)) in edge_pairs.iter().enumerate() {
                if i > 0 {
                    maps.push(',');
                }
                maps.push_str(&format!("{{src:{},dst:{}}}", src, dst));
            }
            let q = format!(
                "UNWIND [{}] AS row MATCH (n:User {{id: row.src}}), (m:User {{id: row.dst}}) CREATE (n)-[:Friend]->(m)",
                maps
            );
            client.run_query_no_results(&q).await?;
            edge_pairs.clear();
            Ok(())
        }

        while let Some(item_result) = stream.next().await {
            let line = match item_result {
                Ok(v) => v,
                Err(e) => {
                    error!("Error reading import line: {:?}", e);
                    continue;
                }
            };

            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed == ";" || trimmed.starts_with("//") {
                continue;
            }

            if phase == Phase::Nodes && trimmed.starts_with("MATCH") {
                flush_nodes(self, &mut node_maps, &mut batch_count).await?;
                phase = Phase::Edges;
            }

            match phase {
                Phase::Nodes => {
                    if let (Some(l), Some(r)) = (trimmed.find('{'), trimmed.rfind('}')) {
                        if r > l {
                            node_maps.push(trimmed[l..=r].to_string());
                            total_processed += 1;
                        }
                    }
                    if node_maps.len() >= batch_size {
                        flush_nodes(self, &mut node_maps, &mut batch_count).await?;
                    }
                }
                Phase::Edges => {
                    let mut ids: [u64; 2] = [0, 0];
                    let mut found = 0usize;
                    let mut rest = trimmed;
                    while found < 2 {
                        let Some(pos) = rest.find("id:") else { break };
                        rest = &rest[pos + 3..];
                        let s = rest.trim_start();
                        let mut end = 0usize;
                        for (i, ch) in s.char_indices() {
                            if !ch.is_ascii_digit() {
                                end = i;
                                break;
                            }
                        }
                        let end = if end == 0 { s.len() } else { end };
                        if let Ok(v) = s[..end].parse::<u64>() {
                            ids[found] = v;
                            found += 1;
                        }
                        rest = &s[end..];
                    }

                    if found == 2 {
                        edge_pairs.push((ids[0], ids[1]));
                        total_processed += 1;
                    }

                    if edge_pairs.len() >= batch_size {
                        flush_edges(self, &mut edge_pairs, &mut batch_count).await?;
                    }
                }
            }

            // Report progress every 5 seconds
            let now = tokio::time::Instant::now();
            if now.duration_since(last_progress_report).as_secs() >= PROGRESS_INTERVAL_SECS {
                let elapsed = now.duration_since(start_time);
                let rate = total_processed as f64 / elapsed.as_secs_f64();
                info!(
                    "Progress: {} items processed in {:?} ({:.2} items/sec, {} batches completed)",
                    crate::utils::format_number(total_processed as u64),
                    elapsed,
                    rate,
                    batch_count
                );
                last_progress_report = now;
            }
        }

        flush_nodes(self, &mut node_maps, &mut batch_count).await?;
        flush_edges(self, &mut edge_pairs, &mut batch_count).await?;

        info!(
            "Pokec Users import completed: {} statements batched into {} UNWIND queries",
            total_processed,
            batch_count
        );

        Ok(total_processed)
    }

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

        // Use longer FalkorDB per-query timeouts for large datasets.
        // This mirrors the extended timeouts used in other Falkor paths
        // (e.g. index creation, batch execution, graph_size).
        let falkor_result = match q_type {
            QueryType::Read => self.graph.ro_query(query).with_timeout(60_000).execute(),
            QueryType::Write => self.graph.query(query).with_timeout(60_000).execute(),
        };

        // Tokio-level guard: slightly above the FalkorDB per-query timeout.
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

        // Increase underlying FalkorDB timeout to 30 seconds for large datasets
        let falkor_result = self
            .graph
            .query(query)
            .with_timeout(30_000)
            .execute();
        let timeout = Duration::from_secs(30);
        let falkor_result = tokio::time::timeout(timeout, falkor_result).await;
        Self::read_reply(spawn_id, query_name, query, falkor_result)
    }

    /// Execute a batch of cypher commands individually (FalkorDB doesn't support multi-statement queries)
    pub async fn execute_batch<'a>(
        &'a mut self,
        spawn_id: &'a str,
        batch_queries: &[String],
    ) -> BenchmarkResult<()> {
        if batch_queries.is_empty() {
            return Ok(());
        }

        // Execute each query individually since FalkorDB doesn't support multi-statement queries
        for (i, query) in batch_queries.iter().enumerate() {
            OPERATION_COUNTER
                .with_label_values(&["falkor", spawn_id, "", &format!("batch_{}", i), "", ""])
                .inc();

            let falkor_result = self.graph.query(query).with_timeout(30000).execute();
            let timeout = Duration::from_secs(30);
            let falkor_result = tokio::time::timeout(timeout, falkor_result).await;

            // If any query fails, return the error
            if let Err(e) =
                Self::read_reply(spawn_id, &format!("batch_{}", i), query, falkor_result)
            {
                return Err(e);
            }
        }

        Ok(())
    }

    /// Create an index with graceful handling of "already indexed" errors
    pub async fn create_index_if_not_exists<'a>(
        &'a mut self,
        spawn_id: &'a str,
        query_name: &'a str,
        query: &'a str,
    ) -> BenchmarkResult<()> {
        OPERATION_COUNTER
            .with_label_values(&["falkor", spawn_id, "", query_name, "", ""])
            .inc();

        // Increase index creation timeout to 30 seconds for large datasets
        let falkor_result = self
            .graph
            .query(query)
            .with_timeout(30_000)
            .execute();
        let timeout = Duration::from_secs(30);
        let falkor_result = tokio::time::timeout(timeout, falkor_result).await;

        match falkor_result {
            Ok(falkor_result) => match falkor_result {
                Ok(query_result) => {
                    for row in query_result.data {
                        black_box(row);
                    }
                    Ok(())
                }
                Err(e) => {
                    let error_str = format!("{:?}", e);
                    if error_str.contains("already indexed") {
                        info!("Index already exists for query '{}', continuing", query);
                        Ok(())
                    } else {
                        let error_type = std::any::type_name_of_val(&e);
                        error!("Error executing query: {}, the error is: {:?}", query, e);
                        Err(OtherError(format!(
                            "Error (type {}) executing query: {}, the error is: {:?}",
                            error_type, query, e
                        )))
                    }
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
