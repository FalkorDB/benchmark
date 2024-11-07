use crate::error::BenchmarkResult;
use crate::neo4j_client::Neo4jClient;
use crate::neo4j_process::Neo4jProcess;
use crate::scenario::Spec;
use std::env;
use std::process::Output;

#[derive(Clone)]
pub struct Neo4j {
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
            uri,
            user,
            password,
            neo4j_home,
        }
    }

    pub fn neo4j_process(&self) -> Neo4jProcess {
        Neo4jProcess::new(self.neo4j_home.clone())
    }

    pub async fn start(&self) -> BenchmarkResult<()> {
        self.neo4j_process().start().await?;
        Ok(())
    }

    pub(crate) async fn clean_db(&self) -> BenchmarkResult<Output> {
        self.neo4j_process().clean_db().await
    }

    pub async fn stop(&self) -> BenchmarkResult<()> {
        self.neo4j_process().stop(true).await?;
        Ok(())
    }

    pub async fn dump<'a>(
        &self,
        spec: Spec<'a>,
    ) -> BenchmarkResult<Output> {
        self.neo4j_process().dump(spec).await
    }
    pub(crate) async fn restore_db<'a>(
        &self,
        spec: Spec<'a>,
    ) -> BenchmarkResult<Output> {
        self.neo4j_process().restore(spec).await
    }

    pub(crate) async fn client(&self) -> BenchmarkResult<Neo4jClient> {
        Neo4jClient::new(
            self.uri.to_string(),
            self.user.to_string(),
            self.password.to_string(),
        )
        .await
    }
}
