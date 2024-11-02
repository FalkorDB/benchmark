use crate::error::BenchmarkError::OtherError;
use crate::error::BenchmarkResult;
use crate::neo4j_client::Neo4jClient;
use crate::neo4j_process::Neo4jProcess;
use futures::Stream;
use neo4rs::Row;
use std::env;
use std::pin::Pin;

#[derive(Clone)]
pub struct Neo4j {
    neo4j: Option<Neo4jClient>,
    uri: String,
    user: String,
    password: String,
    neo4j_home: String,
}

impl Neo4j {
    pub fn new() -> Neo4j {
        let neo4j_home =
            env::var("NEO4J_HOME").unwrap_or_else(|_| String::from("./downloads/neo4j_local"));
        let uri = env::var("NEO4J_URI").unwrap_or_else(|_| String::from("127.0.0.1:7687"));
        let user = env::var("NEO4J_USER").unwrap_or_else(|_| String::from("neo4j"));
        let password = env::var("NEO4J_PASSWORD").unwrap_or_else(|_| String::from("h6u4krd10"));
        Neo4j {
            neo4j: None,
            uri,
            user,
            password,
            neo4j_home,
        }
    }

    pub fn neo4j_process(&self) -> Neo4jProcess {
        Neo4jProcess::new(format!("{}/bin/neo4j", self.neo4j_home))
    }

    pub async fn start(&self) -> BenchmarkResult<()> {
        self.neo4j_process().start().await?;
        Ok(())
    }

    pub async fn stop(self) -> BenchmarkResult<()> {
        self.neo4j_process().stop().await?;
        Ok(())
    }

    pub async fn client(&self) -> BenchmarkResult<Neo4jClient> {
        Neo4jClient::new(
            self.uri.to_string(),
            self.user.to_string(),
            self.password.to_string(),
        )
        .await
    }
    pub async fn execute_query<'a>(
        &mut self,
        q: String,
    ) -> BenchmarkResult<Pin<Box<dyn Stream<Item = BenchmarkResult<Row>> + Send + 'a>>> {
        if self.neo4j.is_none() {
            // Initialize the Neo4j client
            let client = Neo4jClient::new(
                self.uri.to_string(),
                self.user.to_string(),
                self.password.to_string(),
            )
            .await?;
            self.neo4j = Some(client);
        }
        match self.neo4j {
            Some(ref client) => client.execute_query(q).await,
            None => Err(OtherError("Neo4j client not initialized".to_string())),
        }
    }
}
