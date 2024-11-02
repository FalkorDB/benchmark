use reqwest_streams::error::StreamBodyError;

pub type BenchmarkResult<T> = Result<T, BenchmarkError>;
#[derive(thiserror::Error, Debug)]
pub enum BenchmarkError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Failed to start Neo4j: {0} {1}")]
    FailedToSpawnNeo4jError(std::io::Error, String),
    #[error("Neo4j client error: {0}")]
    Neo4rsError(#[from] neo4rs::Error),
    #[error("Reqwest client error: {0}")]
    ReqwestError(#[from] reqwest::Error),
    #[error("Stream body error: {0}")]
    StreamBodyError(#[from] StreamBodyError),
    #[error("Other error: {0}")]
    OtherError(String),
}
