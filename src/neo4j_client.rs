use crate::error::BenchmarkError::Neo4rsError;
use crate::error::BenchmarkResult;
use crate::metrics_collector::MetricsCollector;
use crate::queries_repository::QueryType;
use crate::query::QueryParam;
use futures::stream::TryStreamExt;
use futures::{Stream, StreamExt};
use histogram::Histogram;
use neo4rs::{query, Graph, Row};
use std::pin::Pin;
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
            .map_err(|e| Neo4rsError(e))?;
        Ok(Neo4jClient { graph })
    }

    pub(crate) async fn graph_size(&self) -> BenchmarkResult<(u64, u64)> {
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
    pub(crate) async fn execute_query_iterator(
        &mut self,
        iter: Box<
            dyn Iterator<Item = (String, QueryType, (String, Vec<(String, QueryParam)>))> + '_,
        >,
        metric_collector: &mut MetricsCollector,
    ) -> BenchmarkResult<()> {
        for (name, query_type, (q, params)) in iter {
            let query_and_params = format!(
                "{}, [{}]",
                q.as_str(),
                params
                    .clone()
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k, v.to_cypher_string()))
                    .collect::<Vec<String>>()
                    .join(", ")
            );
            let start = std::time::Instant::now();
            let mut result = self.graph.execute(query(q.as_str()).params(params)).await?;
            let mut rows = 0;
            while let Ok(Some(row)) = result.next().await {
                trace!("Row: {:?}", row);
                rows += 1;
                if rows % 10000 == 0 {
                    info!("{} rows executed", rows);
                }
            }

            let duration = start.elapsed();
            let stats = format!("{} rows returned", rows);
            metric_collector.record(
                duration,
                name.as_str(),
                query_type,
                query_and_params.as_str(),
                stats.as_str(),
            )?;
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

    pub(crate) async fn execute_query_stream<S>(
        &self,
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
                    while let Some(row_or_error) = results.next().await {
                        match row_or_error {
                            Ok(row) => {
                                trace!("Row: {:?}", row);
                            }
                            Err(e) => error!("Error reading row: {}", e),
                        }
                    }
                    let duration = start.elapsed();
                    histogram.increment(duration.as_micros() as u64)?;
                }
                Err(e) => eprintln!("Error reading line: {}", e),
            }
        }
        Ok(())
    }
}
