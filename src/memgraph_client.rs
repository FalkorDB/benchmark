use crate::error::BenchmarkError::{Neo4rsError, OtherError};
use crate::error::BenchmarkResult;
use crate::queries_repository::PreparedQuery;
use crate::scheduler::Msg;
use crate::{
    MEMGRAPH_MSG_DEADLINE_OFFSET_GAUGE, MEMGRAPH_STORAGE_MEMORY_RES_BYTES,
    MEMGRAPH_STORAGE_MEMORY_TRACKED_BYTES, MEMGRAPH_STORAGE_PEAK_MEMORY_RES_BYTES,
    OPERATION_COUNTER,
};
use futures::stream::TryStreamExt;
use futures::{Stream, StreamExt};
use histogram::Histogram;
use neo4rs::{query, ConfigBuilder, Graph, Row};
use std::hint::black_box;
use std::pin::Pin;
use std::time::Duration;
use tokio::fs::File;
use tokio::io::{self, AsyncWriteExt};
use tokio::time::Instant;
use tracing::{error, info, trace};

#[derive(Default, Debug, Clone)]
struct MemgraphStorageInfo {
    memory_res_bytes: Option<i64>,
    peak_memory_res_bytes: Option<i64>,
    memory_tracked_bytes: Option<i64>,
}

fn parse_human_bytes_to_i64(s: &str) -> Option<i64> {
    let s = s.trim().trim_matches('"');
    if s.is_empty() {
        return None;
    }

    // Fast path: integer bytes.
    if let Ok(v) = s.parse::<i64>() {
        return Some(v);
    }

    // Float + unit (e.g. 725.66MiB)
    let mut num = String::new();
    let mut unit = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
        } else if !c.is_whitespace() {
            unit.push(c);
        }
    }

    let value = num.parse::<f64>().ok()?;
    let unit = unit.to_lowercase();

    let mul = match unit.as_str() {
        "b" | "bytes" => 1.0,
        "kb" => 1000.0,
        "kib" => 1024.0,
        "mb" => 1000.0 * 1000.0,
        "mib" => 1024.0 * 1024.0,
        "gb" => 1000.0 * 1000.0 * 1000.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };

    Some((value * mul).round() as i64)
}

fn get_row_i64(
    row: &Row,
    key: &str,
) -> Option<i64> {
    // Try a few types since Memgraph may return ints or strings depending on version.
    if let Ok(v) = row.get::<i64>(key) {
        return Some(v);
    }
    if let Ok(v) = row.get::<u64>(key) {
        return Some(v as i64);
    }
    if let Ok(v) = row.get::<String>(key) {
        // Either raw bytes ("123") or human-readable sizes ("725.66MiB").
        return parse_human_bytes_to_i64(&v).or_else(|| v.parse::<i64>().ok());
    }
    None
}

#[derive(Clone)]
pub struct MemgraphClient {
    graph: Graph,
}

impl MemgraphClient {
    pub async fn new(
        uri: String,
        user: String,
        password: String,
    ) -> BenchmarkResult<MemgraphClient> {
        // Try using ConfigBuilder with "memgraph" as database name
        // Some versions of Memgraph might expect a specific database name
        let config = ConfigBuilder::default()
            .uri(&uri)
            .user(&user)
            .password(&password)
            .db("memgraph") // Try "memgraph" as database name
            .build()
            .map_err(Neo4rsError)?;

        let graph = Graph::connect(config).await.map_err(Neo4rsError)?;

        Ok(MemgraphClient { graph })
    }

    pub async fn execute_prepared_query<S: AsRef<str>>(
        &mut self,
        worker_id: S,
        msg: &Msg<PreparedQuery>,
        simulate: &Option<usize>,
    ) -> BenchmarkResult<()> {
        let Msg {
            payload: PreparedQuery { bolt, q_name, .. },
            ..
        } = msg;

        let worker_id = worker_id.as_ref();
        let q_name = q_name.as_str();
        let timeout = Duration::from_secs(60);
        let offset = msg.compute_offset_ms();

        MEMGRAPH_MSG_DEADLINE_OFFSET_GAUGE.set(offset);
        if offset > 0 {
            // sleep offset millis
            tokio::time::sleep(Duration::from_millis(offset as u64)).await;
        }

        let bolt_query = bolt.query.as_str();
        let bolt_params = bolt.clone().params;

        let memgraph_result = self
            .graph
            .execute(neo4rs::query(bolt_query).params(bolt_params));

        if let Some(delay) = simulate {
            if *delay > 0 {
                let delay: u64 = *delay as u64;
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            return Ok(());
        }

        let memgraph_result = tokio::time::timeout(timeout, memgraph_result).await;
        OPERATION_COUNTER
            .with_label_values(&["memgraph", worker_id, "", q_name, "", ""])
            .inc();
        match memgraph_result {
            Ok(Ok(mut stream)) => {
                while let Ok(Some(row)) = stream.next().await {
                    trace!("Row: {:?}", row);
                    black_box(row);
                }
            }
            Ok(Err(e)) => {
                OPERATION_COUNTER
                    .with_label_values(&["memgraph", worker_id, "error", q_name, "", ""])
                    .inc();
                return Err(Neo4rsError(e));
            }
            Err(_) => {
                OPERATION_COUNTER
                    .with_label_values(&["memgraph", worker_id, "timeout", q_name, "", ""])
                    .inc();
                return Err(OtherError("Timeout".to_string()));
            }
        }
        Ok(())
    }

    pub async fn graph_size(&self) -> BenchmarkResult<(u64, u64)> {
        let mut result = self
            .graph
            .execute(query("MATCH (n) RETURN count(n) as count"))
            .await?;
        let mut number_of_nodes: u64 = 0;
        if let Ok(Some(row)) = result.next().await {
            number_of_nodes = row.get("count")?;
        }
        let mut result = self
            .graph
            .execute(query("MATCH ()-[r]->() RETURN count(r) as count"))
            .await?;
        let mut number_of_relationships: u64 = 0;
        if let Ok(Some(row)) = result.next().await {
            number_of_relationships = row.get("count")?;
        }
        Ok((number_of_nodes, number_of_relationships))
    }

    /// Best-effort: query Memgraph for its storage/memory statistics and write them into Prometheus gauges.
    ///
    /// Uses `SHOW STORAGE INFO;` and attempts to read these columns:
    /// - memory_res
    /// - peak_memory_res
    /// - memory_tracked
    pub async fn collect_storage_info_metrics(&self) {
        // Avoid stale values when multiple runs happen in a single process.
        MEMGRAPH_STORAGE_MEMORY_RES_BYTES.set(0);
        MEMGRAPH_STORAGE_PEAK_MEMORY_RES_BYTES.set(0);
        MEMGRAPH_STORAGE_MEMORY_TRACKED_BYTES.set(0);

        match self.storage_info().await {
            Ok(info) => {
                if let Some(v) = info.memory_res_bytes {
                    MEMGRAPH_STORAGE_MEMORY_RES_BYTES.set(v);
                }
                if let Some(v) = info.peak_memory_res_bytes {
                    MEMGRAPH_STORAGE_PEAK_MEMORY_RES_BYTES.set(v);
                }
                if let Some(v) = info.memory_tracked_bytes {
                    MEMGRAPH_STORAGE_MEMORY_TRACKED_BYTES.set(v);
                }
            }
            Err(e) => {
                tracing::debug!("Failed collecting Memgraph storage info: {}", e);
            }
        }
    }

    async fn storage_info(&self) -> BenchmarkResult<MemgraphStorageInfo> {
        let mut result = self
            .graph
            .execute(query("SHOW STORAGE INFO"))
            .await
            .map_err(Neo4rsError)?;

        // Memgraph currently returns this as a key/value table:
        //   | storage info | value |
        // with values often formatted as strings like "725.66MiB".
        let mut info = MemgraphStorageInfo::default();

        while let Some(row) = result.next().await.map_err(Neo4rsError)? {
            // Preferred: key/value shape.
            let key = row
                .get::<String>("storage info")
                .or_else(|_| row.get::<String>("storage_info"))
                .ok();
            let value = row
                .get::<String>("value")
                .or_else(|_| row.get::<String>("Value"))
                .ok();

            if let (Some(k), Some(v)) = (key, value) {
                let k = k.trim().trim_matches('"');
                match k {
                    "memory_res" => {
                        info.memory_res_bytes = parse_human_bytes_to_i64(&v);
                    }
                    "peak_memory_res" => {
                        info.peak_memory_res_bytes = parse_human_bytes_to_i64(&v);
                    }
                    "memory_tracked" => {
                        info.memory_tracked_bytes = parse_human_bytes_to_i64(&v);
                    }
                    _ => {}
                }
                continue;
            }

            // Fallback: older/newer shapes with direct columns.
            if info.memory_res_bytes.is_none() {
                info.memory_res_bytes = get_row_i64(&row, "memory_res");
            }
            if info.peak_memory_res_bytes.is_none() {
                info.peak_memory_res_bytes = get_row_i64(&row, "peak_memory_res");
            }
            if info.memory_tracked_bytes.is_none() {
                info.memory_tracked_bytes = get_row_i64(&row, "memory_tracked");
            }
        }

        Ok(info)
    }

    /// Clear all user data in an external Memgraph instance.
    ///
    /// We intentionally avoid Neo4j's `cypher-shell` for Memgraph because recent versions
    /// call `db.ping` on connect, which Memgraph doesn't implement.
    pub async fn clean_db(&self) -> BenchmarkResult<()> {
        // Best-effort: drop the benchmark's known index, ignore errors if it doesn't exist.
        if let Err(e) = self.graph.run(query("DROP INDEX ON :User(id); ")).await {
            trace!("Ignoring error while dropping Memgraph index: {}", e);
        }

        // Delete everything.
        // NOTE: This can be expensive on large graphs, but it's fine for benchmark setup.
        self.graph
            .run(query("MATCH (n) DETACH DELETE n;"))
            .await
            .map_err(Neo4rsError)?;

        Ok(())
    }

    pub async fn execute_query_iterator(
        &mut self,
        iter: Box<dyn Iterator<Item = PreparedQuery> + '_>,
    ) -> BenchmarkResult<()> {
        let mut count = 0u64;
        for PreparedQuery { bolt, .. } in iter {
            let mut result = self
                .graph
                .execute(neo4rs::query(bolt.query.as_str()).params(bolt.params))
                .await?;
            while let Ok(Some(row)) = result.next().await {
                trace!("Row: {:?}", row);
                black_box(row);
            }

            count += 1;
            if count % 10000 == 0 {
                info!("Executed {} queries", count);
            }
        }
        Ok(())
    }

    pub(crate) async fn execute_query(
        &self,
        q: &str,
    ) -> BenchmarkResult<Pin<Box<dyn Stream<Item = BenchmarkResult<Row>> + Send>>> {
        trace!("Executing query: {}", q);
        let result = self.graph.execute(query(q)).await?;
        let stream = result.into_stream().map_err(|e| e.into());
        Ok(Box::pin(stream))
    }

    /// Execute a batch of queries as a single transaction
    pub async fn execute_batch(
        &self,
        _worker_id: &str,
        batch_queries: &[String],
    ) -> BenchmarkResult<()> {
        if batch_queries.is_empty() {
            return Ok(());
        }

        // Execute each query individually since Memgraph handles transactions differently
        for query_str in batch_queries {
            let mut results = self.execute_query(query_str).await?;
            while let Some(row_or_error) = results.next().await {
                match row_or_error {
                    Ok(row) => {
                        trace!("Row: {:?}", row);
                        black_box(row);
                    }
                    Err(e) => error!("Error reading batch result row: {}", e),
                }
            }
        }

        Ok(())
    }

    /// Execute a batch of queries with histogram tracking
    pub async fn execute_batch_with_histogram(
        &self,
        batch_queries: &[String],
        histogram: &mut Histogram,
    ) -> BenchmarkResult<()> {
        if batch_queries.is_empty() {
            return Ok(());
        }

        let start = Instant::now();

        // Execute each query individually
        for query_str in batch_queries {
            let mut results = self.execute_query(query_str).await?;
            while let Some(row_or_error) = results.next().await {
                match row_or_error {
                    Ok(row) => {
                        trace!("Row: {:?}", row);
                        black_box(row);
                    }
                    Err(e) => error!("Error reading batch result row: {}", e),
                }
            }
        }

        let duration = start.elapsed();
        histogram.increment(duration.as_micros() as u64)?;

        Ok(())
    }

    pub async fn execute_query_stream<S>(
        &self,
        mut stream: S,
        histogram: &mut Histogram,
    ) -> BenchmarkResult<()>
    where
        S: StreamExt<Item = Result<String, io::Error>> + Unpin,
    {
        let mut count: usize = 0;
        while let Some(line_or_error) = stream.next().await {
            match line_or_error {
                Ok(line) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() || trimmed == ";" {
                        continue;
                    }
                    let start = Instant::now();
                    let mut results = self.execute_query(line.as_str()).await?;
                    while let Some(row_or_error) = results.next().await {
                        match row_or_error {
                            Ok(row) => {
                                trace!("Row: {:?}", row);
                                black_box(row);
                            }
                            Err(e) => error!("Error reading row: {}", e),
                        }
                    }
                    let duration = start.elapsed();
                    count += 1;
                    if count % 1000 == 0 {
                        info!("{} lines processed", count);
                    }
                    histogram.increment(duration.as_micros() as u64)?;
                }
                Err(e) => eprintln!("Error reading line: {}", e),
            }
        }
        Ok(())
    }

    async fn run_query_no_results(
        &self,
        q: &str,
    ) -> BenchmarkResult<()> {
        self.graph.run(query(q)).await.map_err(Neo4rsError)?;
        Ok(())
    }

    /// Fast-path loader for the Pokec "Users" dataset using UNWIND batches.
    /// See `Neo4jClient::execute_pokec_users_import_unwind` for the expected line formats.
    pub async fn execute_pokec_users_import_unwind<S>(
        &self,
        mut stream: S,
        batch_size: usize,
        histogram: &mut Histogram,
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

        async fn flush_nodes(
            client: &MemgraphClient,
            node_maps: &mut Vec<String>,
            histogram: &mut Histogram,
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
            let start = Instant::now();
            client.run_query_no_results(&q).await?;
            histogram.increment(start.elapsed().as_micros() as u64)?;
            node_maps.clear();
            Ok(())
        }

        async fn flush_edges(
            client: &MemgraphClient,
            edge_pairs: &mut Vec<(u64, u64)>,
            histogram: &mut Histogram,
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
            let start = Instant::now();
            client.run_query_no_results(&q).await?;
            histogram.increment(start.elapsed().as_micros() as u64)?;
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
                flush_nodes(self, &mut node_maps, histogram, &mut batch_count).await?;
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
                        flush_nodes(self, &mut node_maps, histogram, &mut batch_count).await?;
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
                        flush_edges(self, &mut edge_pairs, histogram, &mut batch_count).await?;
                    }
                }
            }
        }

        flush_nodes(self, &mut node_maps, histogram, &mut batch_count).await?;
        flush_edges(self, &mut edge_pairs, histogram, &mut batch_count).await?;

        info!(
            "Pokec Users import completed: {} statements batched into {} UNWIND queries",
            total_processed,
            batch_count
        );

        Ok(total_processed)
    }

    /// Execute stream with batch processing (line-by-line statements).
    pub async fn execute_query_stream_batched<S>(
        &self,
        mut stream: S,
        batch_size: usize,
        histogram: &mut Histogram,
    ) -> BenchmarkResult<usize>
    where
        S: StreamExt<Item = Result<String, io::Error>> + Unpin,
    {
        info!("Processing Memgraph queries in batches of {}", batch_size);

        let mut current_batch = Vec::with_capacity(batch_size);
        let mut total_processed = 0;
        let mut batch_count = 0;
        let start_time = tokio::time::Instant::now();
        let mut last_progress_report = start_time;
        const PROGRESS_INTERVAL_SECS: u64 = 5;

        while let Some(item_result) = stream.next().await {
            match item_result {
                Ok(item) => {
                    let trimmed = item.trim();
                    if !trimmed.is_empty() && trimmed != ";" && !trimmed.starts_with("//") {
                        current_batch.push(item);
                        total_processed += 1;

                        if current_batch.len() >= batch_size {
                            batch_count += 1;
                            let batch_start = tokio::time::Instant::now();

                            info!(
                                "Processing batch {} with {} items (total processed: {})",
                                batch_count,
                                current_batch.len(),
                                total_processed
                            );

                            self.execute_batch_with_histogram(&current_batch, histogram)
                                .await?;
                            current_batch = Vec::with_capacity(batch_size);

                            let batch_duration = batch_start.elapsed();
                            trace!("Batch {} completed in {:?}", batch_count, batch_duration);

                            // Report progress every 5 seconds
                            let now = tokio::time::Instant::now();
                            if now.duration_since(last_progress_report).as_secs()
                                >= PROGRESS_INTERVAL_SECS
                            {
                                let elapsed = now.duration_since(start_time);
                                let rate = total_processed as f64 / elapsed.as_secs_f64();
                                info!("Progress: {} items processed in {:?} ({:.2} items/sec, {} batches completed)", 
                                      crate::utils::format_number(total_processed as u64), elapsed, rate, batch_count);
                                last_progress_report = now;
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Error processing stream item: {:?}", e);
                }
            }
        }

        // Process remaining items if any
        if !current_batch.is_empty() {
            batch_count += 1;
            info!(
                "Processing final batch {} with {} items",
                batch_count,
                current_batch.len()
            );
            self.execute_batch_with_histogram(&current_batch, histogram)
                .await?;
        }

        let total_duration = start_time.elapsed();
        let final_rate = total_processed as f64 / total_duration.as_secs_f64();
        info!(
            "Completed processing {} items in {} batches over {:?} (avg {:.2} items/sec)",
            crate::utils::format_number(total_processed as u64),
            batch_count,
            total_duration,
            final_rate
        );

        Ok(total_processed)
    }

    /// Export database to a cypher file
    pub async fn export_to_file(
        &self,
        file_path: &str,
    ) -> BenchmarkResult<()> {
        info!("Exporting database to {}", file_path);

        let mut file = File::create(file_path).await?;

        // Export nodes
        let mut result = self.graph.execute(query("MATCH (n) RETURN n")).await?;
        while let Ok(Some(row)) = result.next().await {
            // This is a simplified export - in a real implementation,
            // you'd want to properly serialize the node properties
            let export_line = format!("CREATE ({:?});\n", row);
            file.write_all(export_line.as_bytes()).await?;
        }

        // Export relationships
        let mut result = self
            .graph
            .execute(query("MATCH ()-[r]->() RETURN r"))
            .await?;
        while let Ok(Some(row)) = result.next().await {
            // This is a simplified export - in a real implementation,
            // you'd want to properly serialize the relationship
            let export_line = format!("CREATE ({:?});\n", row);
            file.write_all(export_line.as_bytes()).await?;
        }

        file.flush().await?;
        info!("Database exported successfully");
        Ok(())
    }

    /// Import database from a cypher file
    pub async fn import_from_file(
        &self,
        file_path: &str,
    ) -> BenchmarkResult<()> {
        info!("Importing database from {}", file_path);

        // Read and execute each line from the file
        let content = tokio::fs::read_to_string(file_path).await?;
        for line in content.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && !trimmed.starts_with("//") {
                let mut results = self.execute_query(trimmed).await?;
                while let Some(_) = results.next().await {
                    // Process results
                }
            }
        }

        info!("Database imported successfully");
        Ok(())
    }
}
