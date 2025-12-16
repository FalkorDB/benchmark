use crate::error::BenchmarkError::{Neo4rsError, OtherError};
use crate::error::BenchmarkResult;
use crate::queries_repository::PreparedQuery;
use crate::scheduler::Msg;
use crate::{NEO4J_MSG_DEADLINE_OFFSET_GAUGE, OPERATION_COUNTER};
use futures::stream::TryStreamExt;
use futures::{Stream, StreamExt};
use histogram::Histogram;
use neo4rs::{query, ConfigBuilder, Graph, Row};
use std::hint::black_box;
use std::pin::Pin;
use std::time::Duration;
use tokio::io;
use tokio::time::Instant;
use tracing::{error, info, trace};

#[derive(Clone)]
pub struct Neo4jClient {
    graph: Graph,
}

impl Neo4jClient {
    pub async fn new(
        uri: String,
        user: String,
        password: String,
        database: Option<String>,
    ) -> BenchmarkResult<Neo4jClient> {
        let config = ConfigBuilder::default()
            .uri(&uri)
            .user(&user)
            .password(&password);

        let config = if let Some(db) = database {
            config.db(db)
        } else {
            config
        };

        let graph = Graph::connect(config.build().map_err(Neo4rsError)?)
            .await
            .map_err(Neo4rsError)?;
        Ok(Neo4jClient { graph })
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

        NEO4J_MSG_DEADLINE_OFFSET_GAUGE.set(offset);
        if offset > 0 {
            // sleep offset millis
            tokio::time::sleep(Duration::from_millis(offset as u64)).await;
        }

        let bolt_query = bolt.query.as_str();
        let bolt_params = bolt.clone().params;

        let neo4j_result = self
            .graph
            .execute(neo4rs::query(bolt_query).params(bolt_params));

        if let Some(delay) = simulate {
            if *delay > 0 {
                let delay: u64 = *delay as u64;
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            return Ok(());
        }

        let neo4j_result = tokio::time::timeout(timeout, neo4j_result).await;
        OPERATION_COUNTER
            .with_label_values(&["neo4j", worker_id, "", q_name, "", ""])
            .inc();
        match neo4j_result {
            Ok(Ok(mut stream)) => {
                while let Ok(Some(row)) = stream.next().await {
                    trace!("Row: {:?}", row);
                    black_box(row);
                }
            }
            Ok(Err(e)) => {
                OPERATION_COUNTER
                    .with_label_values(&["neo4j", worker_id, "error", q_name, "", ""])
                    .inc();
                return Err(Neo4rsError(e));
            }
            Err(_) => {
                OPERATION_COUNTER
                    .with_label_values(&["falkor", worker_id, "timeout", q_name, "", ""])
                    .inc();
                return Err(OtherError("Timeout".to_string()));
            }
        }
        Ok(())
    }

    /// Best-effort: estimate Neo4j store size (data + schema/native indexes) via JMX exposed through Cypher.
    ///
    /// This works for external endpoints (where we can't inspect the filesystem), but requires the
    /// `dbms.queryJmx` procedure to be allowed for the authenticated user.
    pub async fn collect_store_size_metrics(&self) {
        match self.store_size_bytes_via_jmx().await {
            Ok(bytes) if bytes > 0 => {
                crate::NEO4J_STORE_SIZE_BYTES.set(bytes.min(i64::MAX as u64) as i64);
            }
            Ok(_) => {
                // Keep whatever value we already had (e.g. filesystem fallback for local runs).
            }
            Err(e) => {
                // Keep whatever value we already had (e.g. filesystem fallback for local runs),
                // but make the failure visible.
                crate::NEO4J_STORE_SIZE_COLLECT_FAILURES_TOTAL.inc();
                error!("Failed to collect Neo4j store size via JMX: {:?}", e);
            }
        }
    }

    /// Best-effort: collect JVM memory usage via JMX exposed through Cypher.
    ///
    /// On external endpoints, this can be used as a proxy for "process" memory when RSS isn't accessible.
    /// If successful, this also updates `neo4j_memory_usage` (bytes) to heap+nonheap used.
    pub async fn collect_jvm_memory_metrics(&self) {
        match self.jvm_memory_used_bytes_via_jmx().await {
            Ok((heap_used, nonheap_used)) => {
                crate::NEO4J_JVM_HEAP_USED_BYTES.set(heap_used.min(i64::MAX as u64) as i64);
                crate::NEO4J_JVM_NONHEAP_USED_BYTES.set(nonheap_used.min(i64::MAX as u64) as i64);

                let total = heap_used.saturating_add(nonheap_used);
                if total > 0 {
                    // For external endpoints we otherwise don't have a value for neo4j_memory_usage.
                    crate::NEO4J_MEM_USAGE_GAUGE.set(total.min(i64::MAX as u64) as i64);
                }
            }
            Err(e) => {
                // Not fatal; just means JMX is blocked / not available.
                error!("Failed to collect Neo4j JVM memory via JMX: {:?}", e);
            }
        }
    }

    async fn jvm_memory_used_bytes_via_jmx(&self) -> BenchmarkResult<(u64, u64)> {
        // `attributes` is a map: attributeName -> { description, value }.
        // For HeapMemoryUsage / NonHeapMemoryUsage, the `value` itself is a nested map that includes `used`.
        let q = r#"
CALL dbms.queryJmx('java.lang:type=Memory') YIELD attributes
RETURN
  attributes['HeapMemoryUsage']['value']['used'] AS heap_used,
  attributes['NonHeapMemoryUsage']['value']['used'] AS nonheap_used
"#;

        let mut result = self.graph.execute(query(q)).await?;
        if let Ok(Some(row)) = result.next().await {
            let heap_used: u64 = row.get::<u64>("heap_used").or_else(|_| row.get::<i64>("heap_used").map(|v| v.max(0) as u64))?;
            let nonheap_used: u64 = row
                .get::<u64>("nonheap_used")
                .or_else(|_| row.get::<i64>("nonheap_used").map(|v| v.max(0) as u64))?;
            return Ok((heap_used, nonheap_used));
        }

        Ok((0, 0))
    }

    async fn store_size_bytes_via_jmx(&self) -> BenchmarkResult<u64> {
        // This query is a Cypher equivalent of the "Store file sizes" section in :sysinfo.
        // It returns multiple rows like (name, value). We sum all numeric values.
        //
        // The "kernel" instance naming can vary by Neo4j version/config, so try a few patterns.
        const MBEANS: [&str; 3] = [
            "org.neo4j:instance=kernel#0,name=Store file sizes",
            "org.neo4j:instance=kernel,name=Store file sizes",
            "org.neo4j:name=Store file sizes",
        ];

        for mbean in MBEANS {
            let q = format!(
                "\
CALL dbms.queryJmx('{mbean}') YIELD attributes\n\
WITH keys(attributes) AS ks, attributes\n\
UNWIND ks AS k\n\
RETURN k AS name, attributes[k]['value'] AS value\n"
            );

            let mut result = self.graph.execute(query(&q)).await?;
            let mut total: u64 = 0;

            while let Ok(Some(row)) = result.next().await {
                // Try multiple types; Neo4j may return ints/strings depending on version.
                if let Ok(v) = row.get::<i64>("value") {
                    if v > 0 {
                        total = total.saturating_add(v as u64);
                        continue;
                    }
                }
                if let Ok(v) = row.get::<u64>("value") {
                    total = total.saturating_add(v);
                    continue;
                }
                if let Ok(v) = row.get::<f64>("value") {
                    if v.is_finite() && v > 0.0 {
                        total = total.saturating_add(v.round() as u64);
                        continue;
                    }
                }
                if let Ok(v) = row.get::<String>("value") {
                    if let Ok(p) = v.parse::<u64>() {
                        total = total.saturating_add(p);
                        continue;
                    }
                }
            }

            if total > 0 {
                return Ok(total);
            }
        }

        Ok(0)
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

    /// Execute a batch of queries individually (external endpoints don't support explicit transactions)
    pub async fn execute_batch(
        &self,
        batch_queries: &[String],
        histogram: &mut Histogram,
    ) -> BenchmarkResult<()> {
        if batch_queries.is_empty() {
            return Ok(());
        }

        let start = Instant::now();

        // Execute queries individually since explicit BEGIN/COMMIT syntax is not supported
        for query in batch_queries {
            let trimmed = query.trim();
            if !trimmed.is_empty() && trimmed != ";" {
                let mut results = self.execute_query(trimmed).await?;
                while let Some(row_or_error) = results.next().await {
                    match row_or_error {
                        Ok(row) => {
                            trace!("Row: {:?}", row);
                        }
                        Err(e) => error!("Error reading batch result row: {}", e),
                    }
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

    /// Execute stream with batch processing
    pub async fn execute_query_stream_batched<S>(
        &self,
        mut stream: S,
        batch_size: usize,
        histogram: &mut Histogram,
    ) -> BenchmarkResult<usize>
    where
        S: StreamExt<Item = Result<String, io::Error>> + Unpin,
    {
        info!("Processing Neo4j queries in batches of {}", batch_size);

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

                            self.execute_batch(&current_batch, histogram).await?;
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
            self.execute_batch(&current_batch, histogram).await?;
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
}
