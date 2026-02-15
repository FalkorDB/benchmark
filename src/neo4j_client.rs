use crate::error::BenchmarkError::{Neo4rsError, OtherError};
use crate::error::BenchmarkResult;
use crate::queries_repository::PreparedQuery;
use crate::scheduler::Msg;
use crate::{NEO4J_MSG_DEADLINE_OFFSET_GAUGE, OPERATION_COUNTER};
use futures::stream::TryStreamExt;
use futures::{Stream, StreamExt};
use histogram::Histogram;
use neo4rs::{query, Graph, Row};
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
    ) -> BenchmarkResult<Neo4jClient> {
        let graph = Graph::new(&uri, user.clone(), password.clone())
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
            if count.is_multiple_of(10000) {
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
                    if count.is_multiple_of(1000) {
                        info!("{} lines processed", count);
                    }
                    histogram.increment(duration.as_micros() as u64)?;
                }
                Err(e) => eprintln!("Error reading line: {}", e),
            }
        }
        Ok(())
    }
}
