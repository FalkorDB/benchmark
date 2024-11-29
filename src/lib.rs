use lazy_static::lazy_static;
use prometheus::{register_counter_vec, CounterVec};

pub mod cli;
pub mod compare_template;
pub mod error;
pub mod falkor;
pub mod falkor_process;
pub mod metrics_collector;
pub mod neo4j;
pub mod neo4j_client;
pub mod prometheus_endpoint;
pub mod queries_repository;
pub mod query;
pub mod scenario;
pub mod utils;

lazy_static! {
    pub static ref OPERATION_COUNTER: CounterVec = register_counter_vec!(
        "operations_total",
        "Total number of operations processed",
        &[
            "vendor",
            "spawn_id",
            "type",
            "name",
            "dataset",
            "dataset_size"
        ]
    )
    .unwrap();
    pub static ref OPERATION_ERROR_COUNTER: CounterVec = register_counter_vec!(
        "operations_error_total",
        "Total number of operations failed",
        &[
            "vendor",
            "spawn_id",
            "type",
            "name",
            "dataset",
            "dataset_size"
        ]
    )
    .unwrap();
}
