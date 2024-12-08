use lazy_static::lazy_static;
use prometheus::{
    register_counter_vec, register_int_counter, register_int_gauge, CounterVec, IntCounter,
    IntGauge,
};

pub mod cli;
pub mod error;
pub mod falkor;
pub mod neo4j;
pub mod neo4j_client;
pub mod process_monitor;
pub mod prometheus_endpoint;
pub mod queries_repository;
pub mod query;
pub mod scenario;
pub mod utils;

pub(crate) const REDIS_DATA_DIR: &str = "./redis-data";

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
    pub static ref FALKOR_RESTART_COUNTER: IntCounter = register_int_counter!(
        "falkordb_restarts_total",
        "Total number of restart for falkordb server",
    )
    .unwrap();
    pub static ref FALKOR_RUNNING_REQUESTS_GAUGE: IntGauge = register_int_gauge!(
        "falkordb_running_requests",
        "The number of request that run now by the falkordb server",
    )
    .unwrap();
    pub static ref FALKOR_WAITING_REQUESTS_GAUGE: IntGauge = register_int_gauge!(
        "falkordb_waiting_requests",
        "The number of request that waiting to run by the falkordb server",
    )
    .unwrap();
    pub static ref FALKOR_NODES_GAUGE: IntGauge = register_int_gauge!(
        "falkordb_nodes_total",
        "Total number of nodes in falkordb graph",
    )
    .unwrap();
    pub static ref FALKOR_RELATIONSHIPS_GAUGE: IntGauge = register_int_gauge!(
        "falkordb_relationships_total",
        "Total number of relationships in falkordb graph",
    )
    .unwrap();
}
