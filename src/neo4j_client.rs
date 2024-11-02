use crate::error::BenchmarkError::Neo4rsError;
use crate::error::BenchmarkResult;
use futures::stream::TryStreamExt;
use futures::Stream;
use neo4rs::{query, Graph, Row};
use std::pin::Pin;
use tracing::trace;

// type QueryResult = Result<Row, Error>;
// type QueryStream = impl Stream<Item=QueryResult>;

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

    pub async fn execute_query_str(
        &self,
        q: &str,
    ) -> BenchmarkResult<Pin<Box<dyn Stream<Item = BenchmarkResult<Row>> + Send>>> {
        trace!("Executing query: {}", q);
        let result = self.graph.execute(query(q)).await?;
        let stream = result.into_stream().map_err(|e| e.into());
        Ok(Box::pin(stream))
    }

    pub async fn execute_query(
        &self,
        q: String,
    ) -> BenchmarkResult<Pin<Box<dyn Stream<Item = BenchmarkResult<Row>> + Send>>> {
        trace!("Executing query: {}", q);
        let result = self.graph.execute(query(q.as_str())).await?;
        let stream = result.into_stream().map_err(|e| e.into());
        Ok(Box::pin(stream))
    }
}
