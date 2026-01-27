use crate::error::BenchmarkError::{Neo4rsError, OtherError};
use crate::error::BenchmarkResult;
use crate::queries_repository::PreparedQuery;
use crate::scheduler::Msg;
use crate::{NEO4J_MSG_DEADLINE_OFFSET_GAUGE, OPERATION_COUNTER};
use futures::stream::TryStreamExt;
use futures::{Stream, StreamExt};
use histogram::Histogram;
use neo4rs::{query, BoltList, BoltMap, BoltType, ConfigBuilder, Graph, Row};
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
    async fn run_query_no_results(
        &self,
        q: &str,
    ) -> BenchmarkResult<()> {
        self.graph.run(query(q)).await.map_err(Neo4rsError)?;
        Ok(())
    }
}

/// Parse a Cypher property map string like "{id: 1, age: 20, gender: \"male\", completion_percentage: 75}"
/// into a BoltMap for parameterized queries.
fn parse_property_map(map_str: &str) -> BenchmarkResult<BoltMap> {
    let mut bolt_map = BoltMap::new();
    
    // Remove outer braces
    let content = map_str.trim().trim_start_matches('{').trim_end_matches('}');
    
    // Simple parser for key: value pairs
    let mut current_pos = 0;
    let chars: Vec<char> = content.chars().collect();
    
    while current_pos < chars.len() {
        // Skip whitespace
        while current_pos < chars.len() && chars[current_pos].is_whitespace() {
            current_pos += 1;
        }
        if current_pos >= chars.len() {
            break;
        }
        
        // Parse key (identifier before ':')
        let key_start = current_pos;
        while current_pos < chars.len() && chars[current_pos] != ':' {
            current_pos += 1;
        }
        let key = chars[key_start..current_pos].iter().collect::<String>().trim().to_string();
        
        if current_pos >= chars.len() {
            break;
        }
        current_pos += 1; // skip ':'
        
        // Skip whitespace after ':'
        while current_pos < chars.len() && chars[current_pos].is_whitespace() {
            current_pos += 1;
        }
        
        // Parse value
        let value_start = current_pos;
        let value: BoltType = if chars[current_pos] == '"' {
            // String value
            current_pos += 1; // skip opening quote
            let str_start = current_pos;
            while current_pos < chars.len() && chars[current_pos] != '"' {
                current_pos += 1;
            }
            let str_val = chars[str_start..current_pos].iter().collect::<String>();
            current_pos += 1; // skip closing quote
            str_val.into()
        } else {
            // Numeric value
            while current_pos < chars.len() && chars[current_pos] != ',' && chars[current_pos] != '}' {
                current_pos += 1;
            }
            let num_str = chars[value_start..current_pos].iter().collect::<String>().trim().to_string();
            
            // Try to parse as i64 first, then f64
            if let Ok(int_val) = num_str.parse::<i64>() {
                int_val.into()
            } else if let Ok(float_val) = num_str.parse::<f64>() {
                float_val.into()
            } else {
                // Fallback to string if parsing fails
                num_str.into()
            }
        };
        
        bolt_map.put(key.into(), value);
        
        // Skip to next comma or end
        while current_pos < chars.len() && (chars[current_pos].is_whitespace() || chars[current_pos] == ',') {
            current_pos += 1;
        }
    }
    
    Ok(bolt_map)
}

impl Neo4jClient {

    /// Fast-path loader for the Pokec "Users" dataset.
    ///
    /// The cypher import files are line-based and consist of two phases:
    /// - Node lines: `CREATE (:User { ... });`
    /// - Edge lines: `MATCH (n:User {id: X}), (m:User {id: Y}) CREATE (n)-[e: Friend]->(m);`
    ///
    /// Instead of sending each line as a separate statement (slow), we batch them into:
    /// - Nodes: `UNWIND $batch AS row CREATE (u:User) SET u = row` (parameterized)
    /// - Edges: `UNWIND $batch AS row MATCH ... CREATE (n)-[:Friend]->(m)` (parameterized)
    ///
    /// CRITICAL: Indexes on User(id) MUST be created BEFORE calling this function for acceptable performance.
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
            "Processing Pokec Users import via parameterized UNWIND batches of {}",
            batch_size
        );

        #[derive(Copy, Clone, PartialEq, Eq)]
        enum Phase {
            Nodes,
            Edges,
        }

        let mut phase = Phase::Nodes;
        let mut node_maps: Vec<BoltMap> = Vec::with_capacity(batch_size);
        let mut edge_pairs: Vec<(u64, u64)> = Vec::with_capacity(batch_size);

        let mut total_processed: usize = 0;
        let mut batch_count: usize = 0;

        async fn flush_nodes(
            client: &Neo4jClient,
            node_maps: &mut Vec<BoltMap>,
            histogram: &mut Histogram,
            batch_count: &mut usize,
        ) -> BenchmarkResult<()> {
            if node_maps.is_empty() {
                return Ok(());
            }
            *batch_count += 1;
            
            // Use parameterized query instead of string concatenation
            let q = "UNWIND $batch AS row CREATE (u:User) SET u = row";
            
            // Convert Vec<BoltMap> to Vec<BoltType> for BoltList
            let bolt_types: Vec<BoltType> = node_maps.iter().map(|m| BoltType::Map(m.clone())).collect();
            let batch_list = BoltList::from(bolt_types);
            
            let start = Instant::now();
            client.graph.run(query(q).param("batch", BoltType::List(batch_list))).await.map_err(Neo4rsError)?;
            histogram.increment(start.elapsed().as_micros() as u64)?;
            node_maps.clear();
            Ok(())
        }

        async fn flush_edges(
            client: &Neo4jClient,
            edge_pairs: &mut Vec<(u64, u64)>,
            histogram: &mut Histogram,
            batch_count: &mut usize,
        ) -> BenchmarkResult<()> {
            if edge_pairs.is_empty() {
                return Ok(());
            }
            *batch_count += 1;
            
            // Convert edge pairs to BoltMap list for parameterized query
            let mut batch_maps = Vec::with_capacity(edge_pairs.len());
            for (src, dst) in edge_pairs.iter() {
                let mut map = BoltMap::new();
                map.put("src".into(), (*src as i64).into());
                map.put("dst".into(), (*dst as i64).into());
                batch_maps.push(map);
            }
            
            // CRITICAL: Use explicit :User label in MATCH to enable index usage.
            // Without the label, Neo4j cannot use the User(id) index and will do a full scan.
            // This is the key difference that makes edge loading fast vs. extremely slow.
            // Now using parameterized query for better performance.
            let q = "UNWIND $batch AS row MATCH (n:User {id: row.src}), (m:User {id: row.dst}) CREATE (n)-[:Friend]->(m)";
            
            // Convert Vec<BoltMap> to Vec<BoltType> for BoltList
            let bolt_types: Vec<BoltType> = batch_maps.into_iter().map(|m| BoltType::Map(m)).collect();
            let batch_list = BoltList::from(bolt_types);
            
            let start = Instant::now();
            client.graph.run(query(q).param("batch", BoltType::List(batch_list))).await.map_err(Neo4rsError)?;
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

            // Switch phase when we encounter the first edge statement.
            if phase == Phase::Nodes && trimmed.starts_with("MATCH") {
                flush_nodes(self, &mut node_maps, histogram, &mut batch_count).await?;
                phase = Phase::Edges;
            }

            match phase {
                Phase::Nodes => {
                    // Parse the property map from the CREATE statement and convert to BoltMap
                    if let (Some(l), Some(r)) = (trimmed.find('{'), trimmed.rfind('}')) {
                        if r > l {
                            // Parse the JSON-like map: {id: 1, age: 20, gender: "male", completion_percentage: 75}
                            let map_str = &trimmed[l..=r];
                            if let Ok(bolt_map) = parse_property_map(map_str) {
                                node_maps.push(bolt_map);
                                total_processed += 1;
                            }
                        }
                    }
                    if node_maps.len() >= batch_size {
                        flush_nodes(self, &mut node_maps, histogram, &mut batch_count).await?;
                    }
                }
                Phase::Edges => {
                    // Extract the two ids: `...{id: X}..., ...{id: Y}...`
                    // Keep it simple/fast: scan for `id:` and parse the following integer.
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

        // Final flush.
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
