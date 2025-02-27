use crate::queries_repository::PreparedQuery;
use falkordb::FalkorDBError;

pub type BenchmarkResult<T> = Result<T, BenchmarkError>;
#[derive(thiserror::Error, Debug)]
pub enum BenchmarkError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Failed to start Neo4j: {0} {1}")]
    FailedToSpawnProcessError(std::io::Error, String),
    #[error("Neo4j client error: {0}")]
    Neo4rsError(#[from] neo4rs::Error),
    #[error("Neo4j de serialization error: {0}")]
    Neo4rsDeError(#[from] neo4rs::DeError),
    #[error("histogram error: {0}")]
    HistogramError(#[from] histogram::Error),
    #[error("Reqwest client error: {0}")]
    ReqwestError(#[from] reqwest::Error),
    #[error("Failed to download a file error: {0}")]
    FailedToDownloadFileError(String),
    #[error("FalkorDB error: {0}")]
    FalkorDBError(#[from] FalkorDBError),
    #[error("Redis error: {0}")]
    RedisError(#[from] redis::RedisError),
    #[error("Serde error: {0}")]
    SerdeError(#[from] serde_json::Error),
    #[error("Process with name {0} not found")]
    ProcessNofFoundError(String),
    #[error("Tokio send error: {0}")]
    TokioSendError(#[from] tokio::sync::mpsc::error::SendError<PreparedQuery>),
    #[error("Tokio elapsed error: {0}")]
    TokioElapsed(#[from] tokio::time::error::Elapsed),
    #[error("Other error: {0}")]
    OtherError(String),
}
